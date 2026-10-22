//! Cliente `usbmuxd` (ver `docs/PROTOCOL.md` §9).
//!
//! O usbmuxd é o serviço da Apple que multiplexa ligações TCP sobre o cabo USB:
//! - Windows: TCP `127.0.0.1:27015` (serviço *Apple Mobile Device*, do iTunes ou da app Dispositivos Apple).
//! - macOS: unix socket `/var/run/usbmuxd`.
//! - Ambos: a variável `USBMUXD_SOCKET_ADDRESS` (`UNIX:/caminho` ou `host:porta`) sobrepõe-se.
//!
//! ```no_run
//! use padstudio_usbmux::UsbmuxClient;
//!
//! let mut client = UsbmuxClient::connect()?;
//! for device in client.list_devices()? {
//!     println!("{} ({:?})", device.udid, device.connection);
//! }
//! // Túnel TCP para a porta 23456 do primeiro iPad ligado por USB:
//! let (device, tunnel) = padstudio_usbmux::connect_usb(None, 23456)?;
//! # Ok::<(), padstudio_usbmux::UsbmuxError>(())
//! ```

#![forbid(unsafe_code)]

pub mod lockdown;

use std::fmt;
use std::io::{self, Cursor, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use plist::{Dictionary, Value};

/// Endereço do usbmuxd no Windows.
pub const DEFAULT_TCP_ADDR: &str = "127.0.0.1:27015";
/// Socket do usbmuxd no macOS / Linux.
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/usbmuxd";
/// Variável de ambiente (convenção do libimobiledevice) que indica outro endereço.
pub const ENV_ADDR: &str = "USBMUXD_SOCKET_ADDRESS";

const HEADER_LEN: usize = 16;
const VERSION_PLIST: u32 = 1;
const MESSAGE_PLIST: u32 = 8;
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

const PROG_NAME: &str = "padstudio";
const BUNDLE_ID: &str = "com.padstudio.receiver";
const LIBUSBMUX_VERSION: u64 = 3;

/// Códigos de `Result` do usbmuxd (1 = comando inválido e 6 = versão inválida chegam como `Rejected`).
const RESULT_OK: u64 = 0;
const RESULT_BAD_DEVICE: u64 = 2;
const RESULT_CONNECTION_REFUSED: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum UsbmuxError {
    #[error(
        "o usbmuxd não está disponível em {addr} ({source}). No Windows é preciso o iTunes ou a app \
         \"Dispositivos Apple\" (serviço Apple Mobile Device); no macOS faz parte do sistema"
    )]
    Unavailable {
        addr: String,
        #[source]
        source: io::Error,
    },

    #[error("erro de E/S com o usbmuxd: {0}")]
    Io(#[from] io::Error),

    #[error("plist inválido do usbmuxd: {0}")]
    Plist(#[from] plist::Error),

    #[error("resposta inesperada do usbmuxd: {0}")]
    Protocol(String),

    #[error("o dispositivo {device_id} recusou a ligação à porta {port} (não há nada à escuta nessa porta)")]
    ConnectionRefused { device_id: u32, port: u16 },

    #[error("dispositivo {0} desconhecido (foi desligado?)")]
    BadDevice(u32),

    #[error("o usbmuxd rejeitou o pedido (código {0})")]
    Rejected(u64),

    #[error("nenhum iPad/iPhone ligado por USB{}", .0.as_ref().map(|u| format!(" com o UDID {u}")).unwrap_or_default())]
    NoDevice(Option<String>),
}

impl UsbmuxError {
    /// Erros que se resolvem sozinhos com o tempo (ligar o cabo, iniciar a transmissão).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::NoDevice(_) | Self::ConnectionRefused { .. } | Self::BadDevice(_))
    }
}

pub type Result<T> = std::result::Result<T, UsbmuxError>;

// ── Endereço ─────────────────────────────────────────────────────────────────

/// Onde está o usbmuxd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbmuxAddr {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

impl UsbmuxAddr {
    /// `USBMUXD_SOCKET_ADDRESS` se existir; senão, o predefinido do sistema operativo.
    pub fn from_env_or_default() -> Self {
        std::env::var(ENV_ADDR).ok().and_then(|v| Self::parse(&v)).unwrap_or_else(Self::platform_default)
    }

    /// Formato do libimobiledevice: `UNIX:/caminho` ou `host:porta`.
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(path) = s.strip_prefix("UNIX:") {
            return Self::unix(path);
        }
        s.to_socket_addrs().ok()?.next().map(Self::Tcp)
    }

    #[cfg(unix)]
    fn unix(path: &str) -> Option<Self> {
        Some(Self::Unix(path.into()))
    }

    #[cfg(not(unix))]
    fn unix(_path: &str) -> Option<Self> {
        None
    }

    #[cfg(unix)]
    pub fn platform_default() -> Self {
        Self::Unix(DEFAULT_SOCKET_PATH.into())
    }

    #[cfg(not(unix))]
    pub fn platform_default() -> Self {
        Self::Tcp(DEFAULT_TCP_ADDR.parse().expect("endereço válido"))
    }
}

impl fmt::Display for UsbmuxAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(a) => write!(f, "{a}"),
            #[cfg(unix)]
            Self::Unix(p) => write!(f, "UNIX:{}", p.display()),
        }
    }
}

// ── Socket ───────────────────────────────────────────────────────────────────

enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Stream {
    fn open(addr: &UsbmuxAddr) -> io::Result<Self> {
        match addr {
            UsbmuxAddr::Tcp(a) => {
                let s = TcpStream::connect_timeout(a, CONNECT_TIMEOUT)?;
                s.set_nodelay(true)?;
                Ok(Self::Tcp(s))
            }
            #[cfg(unix)]
            UsbmuxAddr::Unix(p) => Ok(Self::Unix(std::os::unix::net::UnixStream::connect(p)?)),
        }
    }

    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(d),
            #[cfg(unix)]
            Self::Unix(s) => s.set_read_timeout(d),
        }
    }

    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_write_timeout(d),
            #[cfg(unix)]
            Self::Unix(s) => s.set_write_timeout(d),
        }
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(match self {
            Self::Tcp(s) => Self::Tcp(s.try_clone()?),
            #[cfg(unix)]
            Self::Unix(s) => Self::Unix(s.try_clone()?),
        })
    }

    fn shutdown(&self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(s) => s.shutdown(Shutdown::Both),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write_vectored(bufs),
            #[cfg(unix)]
            Self::Unix(s) => s.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

// ── Mensagens ────────────────────────────────────────────────────────────────

/// A porta em ordem de rede, lida como inteiro nativo (o que o usbmuxd espera em `PortNumber`).
pub fn port_number(port: u16) -> u16 {
    u16::from_ne_bytes(port.to_be_bytes())
}

fn request_dict(message_type: &str) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), Value::String(message_type.into()));
    d.insert("ClientVersionString".into(), Value::String(concat!("padstudio ", env!("CARGO_PKG_VERSION")).into()));
    d.insert("ProgName".into(), Value::String(PROG_NAME.into()));
    d.insert("BundleID".into(), Value::String(BUNDLE_ID.into()));
    d.insert("kLibUSBMuxVersion".into(), Value::Integer(LIBUSBMUX_VERSION.into()));
    d
}

fn encode_message(tag: u32, dict: Dictionary) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    Value::Dictionary(dict).to_writer_xml(&mut body)?;
    let len = u32::try_from(HEADER_LEN + body.len()).map_err(|_| UsbmuxError::Protocol("mensagem demasiado grande".into()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&VERSION_PLIST.to_le_bytes());
    out.extend_from_slice(&MESSAGE_PLIST.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// Acumula bytes do socket e extrai mensagens completas. Um timeout nunca parte uma mensagem.
#[derive(Default)]
struct MessageReader {
    buf: Vec<u8>,
}

impl MessageReader {
    fn take(&mut self) -> Result<Option<(u32, Dictionary)>> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = read_u32(&self.buf, 0) as usize;
        if !(HEADER_LEN..=MAX_MESSAGE).contains(&len) {
            return Err(UsbmuxError::Protocol(format!("tamanho de mensagem inválido: {len}")));
        }
        if self.buf.len() < len {
            return Ok(None);
        }
        let (version, message, tag) = (read_u32(&self.buf, 4), read_u32(&self.buf, 8), read_u32(&self.buf, 12));
        if version != VERSION_PLIST || message != MESSAGE_PLIST {
            return Err(UsbmuxError::Protocol(format!("formato binário não suportado (versão {version}, tipo {message})")));
        }
        let value = Value::from_reader(Cursor::new(&self.buf[HEADER_LEN..len]))?;
        self.buf.drain(..len);
        let dict = value.into_dictionary().ok_or_else(|| UsbmuxError::Protocol("plist sem dicionário".into()))?;
        Ok(Some((tag, dict)))
    }

    /// Lê até haver uma mensagem completa; `Ok(None)` se o timeout do socket expirar.
    fn read(&mut self, stream: &mut Stream) -> Result<Option<(u32, Dictionary)>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(m) = self.take()? {
                return Ok(Some(m));
            }
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::Error::new(ErrorKind::UnexpectedEof, "o usbmuxd fechou a ligação").into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if is_timeout(&e) => return Ok(None),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn result_number(reply: &Dictionary) -> Result<u64> {
    match reply.get("MessageType").and_then(Value::as_string) {
        Some("Result") => reply
            .get("Number")
            .and_then(Value::as_unsigned_integer)
            .ok_or_else(|| UsbmuxError::Protocol("Result sem Number".into())),
        other => Err(UsbmuxError::Protocol(format!("esperava Result, recebi {other:?}"))),
    }
}

// ── Dispositivos ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionType {
    Usb,
    /// Sincronização por Wi-Fi do iTunes/Finder (o usbmuxd também a tunela, com mais latência).
    Network,
    Other(String),
}

/// Um dispositivo visto pelo usbmuxd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Identificador da ligação atual (muda a cada ligação do cabo).
    pub device_id: u32,
    /// UDID (estável).
    pub udid: String,
    pub connection: ConnectionType,
    pub product_id: Option<u64>,
    pub location_id: Option<u64>,
    /// Velocidade do barramento USB em bit/s (480 Mb/s = USB 2, 5 Gb/s = USB 3).
    pub speed_bps: Option<u64>,
}

impl Device {
    fn from_record(d: &Dictionary) -> Result<Self> {
        let props = d.get("Properties").and_then(Value::as_dictionary);
        let get = |k: &str| d.get(k).or_else(|| props.and_then(|p| p.get(k)));
        let device_id = get("DeviceID")
            .and_then(Value::as_unsigned_integer)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| UsbmuxError::Protocol("dispositivo sem DeviceID".into()))?;
        let connection = match get("ConnectionType").and_then(Value::as_string) {
            Some("USB") => ConnectionType::Usb,
            Some("Network") => ConnectionType::Network,
            Some(other) => ConnectionType::Other(other.into()),
            None => ConnectionType::Other(String::new()),
        };
        Ok(Self {
            device_id,
            udid: get("SerialNumber").and_then(Value::as_string).unwrap_or_default().to_string(),
            connection,
            product_id: get("ProductID").and_then(Value::as_unsigned_integer),
            location_id: get("LocationID").and_then(Value::as_unsigned_integer),
            speed_bps: get("ConnectionSpeed").and_then(Value::as_unsigned_integer),
        })
    }

    pub fn is_usb(&self) -> bool {
        self.connection == ConnectionType::Usb
    }

    /// Tipo provável a partir do ProductID USB da Apple (heurística).
    pub fn kind(&self) -> &'static str {
        match self.product_id {
            Some(0x12ab) => "iPad",
            Some(0x12a8) => "iPhone",
            Some(0x12aa) => "iPod touch",
            _ => "dispositivo Apple",
        }
    }
}

/// Eventos de `Listen`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    Attached(Device),
    Detached { device_id: u32 },
    /// O utilizador tocou em "Confiar neste computador".
    Paired { device_id: u32 },
    Other(String),
}

// ── Cliente ──────────────────────────────────────────────────────────────────

/// Uma ligação ao usbmuxd. Cada `listen`/`connect_device` consome o cliente, porque a ligação
/// passa a ser usada para eventos ou para o túnel.
pub struct UsbmuxClient {
    stream: Stream,
    reader: MessageReader,
    next_tag: u32,
}

impl UsbmuxClient {
    /// Liga-se ao usbmuxd no endereço predefinido (ou em `USBMUXD_SOCKET_ADDRESS`).
    pub fn connect() -> Result<Self> {
        Self::connect_to(&UsbmuxAddr::from_env_or_default())
    }

    pub fn connect_to(addr: &UsbmuxAddr) -> Result<Self> {
        let stream = Stream::open(addr).map_err(|source| UsbmuxError::Unavailable { addr: addr.to_string(), source })?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
        Ok(Self { stream, reader: MessageReader::default(), next_tag: 1 })
    }

    fn request(&mut self, message_type: &str, extra: Dictionary) -> Result<Dictionary> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1).max(1);
        let mut dict = request_dict(message_type);
        for (k, v) in extra {
            dict.insert(k, v);
        }
        self.stream.write_all(&encode_message(tag, dict)?)?;
        loop {
            match self.reader.read(&mut self.stream)? {
                Some((t, reply)) if t == tag => return Ok(reply),
                Some(_) => continue, // mensagem de outro pedido: ignorar
                None => return Err(UsbmuxError::Protocol(format!("sem resposta a {message_type} em 5 s"))),
            }
        }
    }

    /// Dispositivos ligados neste momento (USB e rede).
    pub fn list_devices(&mut self) -> Result<Vec<Device>> {
        let reply = self.request("ListDevices", Dictionary::new())?;
        let list = reply
            .get("DeviceList")
            .and_then(Value::as_array)
            .ok_or_else(|| UsbmuxError::Protocol("ListDevices sem DeviceList".into()))?;
        list.iter().filter_map(Value::as_dictionary).map(Device::from_record).collect()
    }

    /// Passa a receber eventos de ligação/desligação. Os dispositivos já ligados chegam logo como `Attached`.
    pub fn listen(mut self) -> Result<DeviceEvents> {
        let reply = self.request("Listen", Dictionary::new())?;
        match result_number(&reply)? {
            RESULT_OK => {}
            n => return Err(UsbmuxError::Rejected(n)),
        }
        self.stream.set_read_timeout(None)?;
        Ok(DeviceEvents { stream: self.stream, reader: self.reader })
    }

    /// Abre um túnel TCP para `port` no dispositivo. A ligação ao usbmuxd passa a ser o túnel.
    pub fn connect_device(mut self, device_id: u32, port: u16) -> Result<Tunnel> {
        let mut extra = Dictionary::new();
        extra.insert("DeviceID".into(), Value::Integer(u64::from(device_id).into()));
        extra.insert("PortNumber".into(), Value::Integer(u64::from(port_number(port)).into()));
        let reply = self.request("Connect", extra)?;
        match result_number(&reply)? {
            RESULT_OK => {}
            RESULT_CONNECTION_REFUSED => return Err(UsbmuxError::ConnectionRefused { device_id, port }),
            RESULT_BAD_DEVICE => return Err(UsbmuxError::BadDevice(device_id)),
            n => return Err(UsbmuxError::Rejected(n)),
        }
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(None)?;
        // Bytes que o dispositivo já tenha enviado a seguir ao Result pertencem ao túnel.
        Ok(Tunnel { stream: self.stream, pending: self.reader.buf, pending_pos: 0 })
    }
}

/// Liga-se à `port` do primeiro dispositivo ligado por USB (ou do que tem o `udid` indicado).
pub fn connect_usb(udid: Option<&str>, port: u16) -> Result<(Device, Tunnel)> {
    let mut client = UsbmuxClient::connect()?;
    let device = client
        .list_devices()?
        .into_iter()
        .filter(Device::is_usb)
        .find(|d| udid.is_none_or(|u| d.udid.eq_ignore_ascii_case(u)))
        .ok_or_else(|| UsbmuxError::NoDevice(udid.map(str::to_string)))?;
    let tunnel = client.connect_device(device.device_id, port)?;
    Ok((device, tunnel))
}

/// Fluxo de eventos de `Listen`.
pub struct DeviceEvents {
    stream: Stream,
    reader: MessageReader,
}

impl DeviceEvents {
    /// Com timeout, `next_event` devolve `Ok(None)` quando expira (útil para verificar uma flag de paragem).
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }

    pub fn next_event(&mut self) -> Result<Option<DeviceEvent>> {
        let Some((_, msg)) = self.reader.read(&mut self.stream)? else { return Ok(None) };
        let device_id = || {
            msg.get("DeviceID")
                .and_then(Value::as_unsigned_integer)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| UsbmuxError::Protocol("evento sem DeviceID".into()))
        };
        Ok(Some(match msg.get("MessageType").and_then(Value::as_string) {
            Some("Attached") => DeviceEvent::Attached(Device::from_record(&msg)?),
            Some("Detached") => DeviceEvent::Detached { device_id: device_id()? },
            Some("Paired") => DeviceEvent::Paired { device_id: device_id()? },
            other => DeviceEvent::Other(other.unwrap_or_default().to_string()),
        }))
    }
}

// ── Túnel ────────────────────────────────────────────────────────────────────

/// Ligação TCP transparente até à porta do dispositivo.
pub struct Tunnel {
    stream: Stream,
    pending: Vec<u8>,
    pending_pos: usize,
}

impl Tunnel {
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }

    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(d)
    }

    /// `TCP_NODELAY` no troço até ao usbmuxd (Windows). No unix socket não se aplica.
    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        match &self.stream {
            Stream::Tcp(s) => s.set_nodelay(on),
            #[cfg(unix)]
            Stream::Unix(_) => Ok(()),
        }
    }

    /// Um segundo handle só para escrever (ex.: thread de envio). As leituras continuam neste.
    pub fn writer(&self) -> io::Result<TunnelWriter> {
        Ok(TunnelWriter { stream: self.stream.try_clone()? })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown()
    }
}

impl Read for Tunnel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending_pos < self.pending.len() {
            let n = buf.len().min(self.pending.len() - self.pending_pos);
            buf[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
            self.pending_pos += n;
            if self.pending_pos == self.pending.len() {
                self.pending = Vec::new();
                self.pending_pos = 0;
            }
            return Ok(n);
        }
        self.stream.read(buf)
    }
}

impl Write for Tunnel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.stream.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Handle de escrita de um [`Tunnel`].
pub struct TunnelWriter {
    stream: Stream,
}

impl TunnelWriter {
    /// Fecha o túnel nos dois sentidos (desbloqueia uma leitura pendente noutra thread).
    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown()
    }
}

impl Write for TunnelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.stream.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_number_is_network_order_read_natively() {
        let expected = if cfg!(target_endian = "little") { 41051 } else { 23456 };
        assert_eq!(port_number(23456), expected); // 0x5BA0 → 0xA05B
        assert_eq!(port_number(62078).to_ne_bytes(), 62078u16.to_be_bytes());
    }

    #[test]
    fn message_roundtrip_survives_any_split() {
        let mut d = request_dict("ListDevices");
        d.insert("Extra".into(), Value::Integer(7u64.into()));
        let bytes = encode_message(42, d).unwrap();
        assert_eq!(read_u32(&bytes, 0) as usize, bytes.len());
        assert_eq!((read_u32(&bytes, 4), read_u32(&bytes, 8), read_u32(&bytes, 12)), (1, 8, 42));

        for split in [1, 15, 16, 17, bytes.len() - 1] {
            let mut r = MessageReader::default();
            r.buf.extend_from_slice(&bytes[..split]);
            assert!(r.take().unwrap().is_none(), "incompleta em {split}");
            r.buf.extend_from_slice(&bytes[split..]);
            let (tag, dict) = r.take().unwrap().unwrap();
            assert_eq!(tag, 42);
            assert_eq!(dict.get("MessageType").and_then(Value::as_string), Some("ListDevices"));
            assert_eq!(dict.get("kLibUSBMuxVersion").and_then(Value::as_unsigned_integer), Some(3));
            assert!(r.buf.is_empty());
        }
    }

    #[test]
    fn parses_usb_device_record() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>DeviceID</key><integer>7</integer>
  <key>MessageType</key><string>Attached</string>
  <key>Properties</key><dict>
    <key>ConnectionSpeed</key><integer>480000000</integer>
    <key>ConnectionType</key><string>USB</string>
    <key>DeviceID</key><integer>7</integer>
    <key>LocationID</key><integer>336592896</integer>
    <key>ProductID</key><integer>4779</integer>
    <key>SerialNumber</key><string>00008103-001A2B3C4D5E6F70</string>
  </dict>
</dict></plist>"#;
        let dict = Value::from_reader(Cursor::new(&xml[..])).unwrap().into_dictionary().unwrap();
        let d = Device::from_record(&dict).unwrap();
        assert_eq!(d.device_id, 7);
        assert_eq!(d.udid, "00008103-001A2B3C4D5E6F70");
        assert!(d.is_usb());
        assert_eq!(d.kind(), "iPad");
        assert_eq!(d.speed_bps, Some(480_000_000));
    }

    #[test]
    fn rejects_bad_lengths() {
        let mut r = MessageReader::default();
        r.buf.extend_from_slice(&8u32.to_le_bytes());
        r.buf.extend_from_slice(&[0; 12]);
        assert!(matches!(r.take(), Err(UsbmuxError::Protocol(_))));
    }
}
