//! Supervisor: procura o iPad, liga-se, faz o handshake, mantém a sessão viva e volta a tentar.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use padstudio_protocol::json::{Bye, ByeReason, Configure, Hello, StreamConfig, Transport, Want};
use padstudio_protocol::payload::{Ping, SetBitrate};
use padstudio_protocol::{Message, MessageType, PROTOCOL_VERSION, USB_PORT};
use padstudio_usbmux::{UsbmuxError, lockdown};
use serde::Serialize;

use crate::LocalClock;
use crate::bus::{Bus, Subscription};
use crate::devices::display_name;
use crate::media::{MediaEvent, MediaState, TransportKind};
use crate::session::{self, Session, SharedWriter};
use crate::telemetry::Telemetry;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// Sem bytes do iPad durante este tempo → ligação morta (PROTOCOL.md §7.4).
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_TICK: Duration = Duration::from_millis(20);
const READ_TIMEOUT: Duration = Duration::from_millis(100);
const KEYFRAME_COALESCE: Duration = Duration::from_millis(100);

/// Onde está o iPad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Por cabo, via usbmuxd (o primeiro iPad, ou o do UDID indicado).
    Usb { udid: Option<String> },
    /// TCP direto: rede local (com PIN) ou o `fake-sender` em localhost.
    Tcp { addr: String, token: Option<String> },
}

impl Endpoint {
    pub fn usb() -> Self {
        Self::Usb { udid: None }
    }

    pub fn tcp(addr: impl Into<String>) -> Self {
        Self::Tcp { addr: addr.into(), token: None }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub endpoint: Endpoint,
    /// Configuração pedida ao iPad no HELLO (campos vazios = predefinições do iPad).
    pub stream: StreamConfig,
    pub retry_interval: Duration,
    pub client_name: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            endpoint: Endpoint::usb(),
            stream: StreamConfig::default(),
            retry_interval: Duration::from_secs(1),
            client_name: "pc-studio-receiver".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// Nenhum iPad ligado por cabo.
    NoDevice,
    /// iPad ligado (ou endereço acessível), mas a transmissão não está ativa.
    BroadcastInactive,
    /// Falta o serviço Apple Mobile Device / usbmuxd.
    UsbmuxUnavailable,
    /// Endereço de rede inacessível.
    Unreachable,
    /// A sessão anterior acabou; a tentar de novo.
    SessionEnded,
    Error,
}

/// Estado do motor, para a UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineStatus {
    Starting,
    Waiting { reason: WaitReason, message: String, device: Option<String> },
    Connecting { target: String },
    Streaming { target: String, device: Option<String> },
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EngineEvent {
    Status(EngineStatus),
    Info(String),
    Warning(String),
}

enum Command {
    Configure(StreamConfig),
    RequestKeyframe,
    SetBitrate(u32),
    SetEndpoint(Endpoint),
    Shutdown,
}

/// Estado partilhado entre o supervisor, a thread de leitura e os `Controller`s.
pub(crate) struct EngineShared {
    pub clock: LocalClock,
    pub bus: Bus,
    pub media: Mutex<MediaState>,
    pub status: Mutex<EngineStatus>,
    pub telemetry: Mutex<Telemetry>,
    pub next_config_id: AtomicU64,
    /// relógio_iPad − relógio_local (µs), da melhor amostra de PING/PONG da sessão atual.
    pub clock_offset_us: Mutex<Option<i64>>,
    events: Sender<EngineEvent>,
}

impl EngineShared {
    fn emit(&self, e: EngineEvent) {
        let _ = self.events.try_send(e); // sem leitor, os eventos mais recentes perdem-se
    }

    pub fn info(&self, m: impl Into<String>) {
        self.emit(EngineEvent::Info(m.into()));
    }

    pub fn warn(&self, m: impl Into<String>) {
        self.emit(EngineEvent::Warning(m.into()));
    }
}

/// Handle clonável para comandar o motor e consumir media/telemetria.
#[derive(Clone)]
pub struct Controller {
    cmd: Sender<Command>,
    shared: Arc<EngineShared>,
}

impl Controller {
    /// Muda a configuração do encoder no iPad (aplica já, ou no próximo HELLO).
    pub fn configure(&self, cfg: StreamConfig) {
        let _ = self.cmd.send(Command::Configure(cfg));
    }

    /// Pede um keyframe (agrupado: no máximo 1 a cada 100 ms).
    pub fn request_keyframe(&self) {
        let _ = self.cmd.send(Command::RequestKeyframe);
    }

    pub fn set_bitrate(&self, bitrate_bps: u32) {
        let _ = self.cmd.send(Command::SetBitrate(bitrate_bps));
    }

    /// Muda para outra ligação (USB ↔ rede); a sessão atual termina.
    pub fn set_endpoint(&self, endpoint: Endpoint) {
        let _ = self.cmd.send(Command::SetEndpoint(endpoint));
    }

    pub fn status(&self) -> EngineStatus {
        self.shared.status.lock().unwrap().clone()
    }

    pub fn telemetry(&self) -> Telemetry {
        self.shared.telemetry.lock().unwrap().clone()
    }

    pub fn media_state(&self) -> MediaState {
        self.shared.media.lock().unwrap().clone()
    }

    pub fn bus(&self) -> &Bus {
        &self.shared.bus
    }

    /// Relógio local (o domínio de `recv_us`).
    pub fn now_us(&self) -> u64 {
        self.shared.clock.now_us()
    }

    /// Converte um instante do relógio do iPad (`pts_us`) para o relógio local.
    /// `None` antes da primeira medição de PING/PONG da sessão.
    pub fn remote_to_local(&self, remote_us: u64) -> Option<i64> {
        self.shared.clock_offset_us.lock().unwrap().map(|offset| remote_us as i64 - offset)
    }

    /// Subscreve o bus e devolve as configurações já conhecidas. Se houver vídeo a decorrer, pede
    /// um keyframe para o novo consumidor poder começar logo.
    pub fn subscribe(&self, capacity: usize) -> (Subscription, MediaState) {
        let sub = self.shared.bus.subscribe(capacity);
        let state = self.media_state();
        if state.video.is_some() {
            self.request_keyframe();
        }
        (sub, state)
    }
}

/// O motor: um supervisor numa thread própria. Largar o `Engine` encerra-o.
pub struct Engine {
    controller: Controller,
    events: Receiver<EngineEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(config: EngineConfig) -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = bounded(1024);
        let shared = Arc::new(EngineShared {
            clock: LocalClock::new(),
            bus: Bus::new(),
            media: Mutex::new(MediaState::default()),
            status: Mutex::new(EngineStatus::Starting),
            telemetry: Mutex::new(Telemetry::default()),
            next_config_id: AtomicU64::new(0),
            clock_offset_us: Mutex::new(None),
            events: ev_tx,
        });
        let supervisor = Supervisor {
            cfg: config,
            cmd_rx,
            shared: shared.clone(),
            shutdown: false,
            device_names: HashMap::new(),
            last_keyframe_request: None,
        };
        let thread = thread::Builder::new()
            .name("padstudio-engine".into())
            .spawn(move || supervisor.run())
            .expect("criar a thread do motor");
        Self { controller: Controller { cmd: cmd_tx, shared }, events: ev_rx, thread: Some(thread) }
    }

    pub fn controller(&self) -> Controller {
        self.controller.clone()
    }

    /// Mudanças de estado e mensagens para o utilizador.
    pub fn events(&self) -> &Receiver<EngineEvent> {
        &self.events
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.controller.cmd.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Supervisor ───────────────────────────────────────────────────────────────

struct Link {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// Fecha o socket (desbloqueia a thread de leitura).
    closer: Box<dyn Fn() + Send>,
    target: String,
    transport: TransportKind,
    device_name: Option<String>,
}

struct Wait {
    reason: WaitReason,
    message: String,
    device: Option<String>,
}

impl Wait {
    fn new(reason: WaitReason, message: impl Into<String>) -> Self {
        Self { reason, message: message.into(), device: None }
    }
}

struct Supervisor {
    cfg: EngineConfig,
    cmd_rx: Receiver<Command>,
    shared: Arc<EngineShared>,
    shutdown: bool,
    /// Nome do modelo por `DeviceID` do usbmuxd (consulta ao lockdownd uma vez por ligação do cabo).
    device_names: HashMap<u32, String>,
    last_keyframe_request: Option<Instant>,
}

impl Supervisor {
    fn run(mut self) {
        while !self.shutdown {
            match self.connect() {
                Ok(link) => {
                    let reason = self.run_session(link);
                    self.end_session(&reason);
                    if !self.shutdown {
                        self.set_status(EngineStatus::Waiting {
                            reason: WaitReason::SessionEnded,
                            message: format!("Sessão terminada: {reason}"),
                            device: None,
                        });
                        self.wait(Duration::from_millis(300));
                    }
                }
                Err(w) => {
                    self.set_status(EngineStatus::Waiting { reason: w.reason, message: w.message, device: w.device });
                    let retry = self.cfg.retry_interval;
                    self.wait(retry);
                }
            }
        }
        self.set_status(EngineStatus::Stopped);
    }

    fn set_status(&self, status: EngineStatus) {
        let mut current = self.shared.status.lock().unwrap();
        if *current != status {
            *current = status.clone();
            drop(current);
            self.shared.emit(EngineEvent::Status(status));
        }
    }

    /// Espera até `d`, a tratar comandos. Volta mais cedo se a ligação mudar ou para encerrar.
    fn wait(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.cmd_rx.recv_timeout(left) {
                Ok(Command::Configure(c)) => self.cfg.stream.merge(&c),
                Ok(Command::SetBitrate(b)) => self.cfg.stream.bitrate_bps = Some(b.into()),
                Ok(Command::SetEndpoint(e)) => {
                    self.cfg.endpoint = e;
                    return;
                }
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.shutdown = true;
                    return;
                }
                Ok(Command::RequestKeyframe) => {}
                Err(RecvTimeoutError::Timeout) => return,
            }
        }
    }

    fn connect(&mut self) -> Result<Link, Wait> {
        match self.cfg.endpoint.clone() {
            Endpoint::Usb { udid } => self.connect_usb(udid.as_deref()),
            Endpoint::Tcp { addr, .. } => connect_tcp(&addr),
        }
    }

    fn connect_usb(&mut self, udid: Option<&str>) -> Result<Link, Wait> {
        match padstudio_usbmux::connect_usb(udid, USB_PORT) {
            Ok((device, tunnel)) => {
                let name = self.device_name(device.device_id);
                let io = |e: std::io::Error| Wait::new(WaitReason::Error, format!("túnel USB: {e}"));
                tunnel.set_nodelay(true).map_err(io)?;
                tunnel.set_read_timeout(Some(READ_TIMEOUT)).map_err(io)?;
                let writer = tunnel.writer().map_err(io)?;
                let closer = tunnel.writer().map_err(io)?;
                Ok(Link {
                    reader: Box::new(tunnel),
                    writer: Box::new(writer),
                    closer: Box::new(move || {
                        let _ = closer.shutdown();
                    }),
                    target: format!("{name} por USB"),
                    transport: TransportKind::Usb,
                    device_name: Some(name),
                })
            }
            Err(UsbmuxError::ConnectionRefused { device_id, .. }) => {
                let name = self.device_name(device_id);
                Err(Wait {
                    reason: WaitReason::BroadcastInactive,
                    message: format!("{name} ligado por USB. Inicia a transmissão do PadStudio no iPad."),
                    device: Some(name),
                })
            }
            Err(UsbmuxError::NoDevice(_) | UsbmuxError::BadDevice(_)) => {
                Err(Wait::new(WaitReason::NoDevice, "Liga o iPad ao PC por cabo e desbloqueia-o."))
            }
            Err(e @ UsbmuxError::Unavailable { .. }) => Err(Wait::new(WaitReason::UsbmuxUnavailable, e.to_string())),
            Err(e) => Err(Wait::new(WaitReason::Error, e.to_string())),
        }
    }

    fn device_name(&mut self, device_id: u32) -> String {
        if let Some(n) = self.device_names.get(&device_id) {
            return n.clone();
        }
        let name = match lockdown::query(device_id) {
            Ok(info) => display_name(info.product_type.as_deref(), "iPad"),
            Err(_) => "iPad".to_string(),
        };
        self.device_names.insert(device_id, name.clone());
        name
    }

    fn run_session(&mut self, link: Link) -> String {
        let Link { reader, writer, closer, target, transport, device_name } = link;
        self.set_status(EngineStatus::Connecting { target: target.clone() });
        let session = Arc::new(Session::new(target.clone(), transport, device_name));
        let writer: SharedWriter = Arc::new(Mutex::new(writer));

        let token = match &self.cfg.endpoint {
            Endpoint::Tcp { token, .. } => token.clone(),
            Endpoint::Usb { .. } => None,
        };
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client: Some(self.cfg.client_name.clone()),
            client_version: Some(env!("CARGO_PKG_VERSION").into()),
            transport: match transport {
                TransportKind::Usb => Transport::Usb,
                TransportKind::Lan => Transport::Lan,
            },
            token,
            want: Some(Want { video: Some(true), audio_app: Some(true), audio_mic: Some(true) }),
            config: (self.cfg.stream != StreamConfig::default()).then(|| self.cfg.stream.clone()),
        };
        let msg = Message::from_payload(&hello)
            .with_seq(session.next_seq(MessageType::Hello))
            .with_pts(self.shared.clock.now_us());
        if let Err(e) = session::send(&writer, &msg) {
            closer();
            return format!("falha ao enviar HELLO ({e})");
        }

        let reader_thread = {
            let (s, eng, w) = (session.clone(), self.shared.clone(), writer.clone());
            thread::Builder::new()
                .name("padstudio-reader".into())
                .spawn(move || session::reader_loop(reader, s, eng, w))
        };
        let reader_thread = match reader_thread {
            Ok(t) => t,
            Err(e) => {
                closer();
                return format!("thread de leitura: {e}");
            }
        };

        let reason = self.control_loop(&session, &writer, &target);
        session.end(reason);
        closer();
        let _ = reader_thread.join();
        session.end_reason()
    }

    fn control_loop(&mut self, s: &Session, writer: &SharedWriter, target: &str) -> String {
        let start = Instant::now();
        let mut streaming = false;
        let mut pings = 0u32;
        let mut next_ping = start;
        let mut next_tick = start + Duration::from_secs(1);
        loop {
            match self.cmd_rx.recv_timeout(CONTROL_TICK) {
                Ok(cmd) => {
                    if let Some(reason) = self.handle_online(cmd, s, writer) {
                        return reason;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.shutdown = true;
                    return "motor encerrado".into();
                }
            }
            if !s.is_alive() {
                return s.end_reason();
            }
            let now = Instant::now();

            if !streaming {
                if s.hello_ack.lock().unwrap().is_some() {
                    streaming = true;
                    self.set_status(EngineStatus::Streaming { target: target.into(), device: s.device_label() });
                    self.shared.info(format!("Ligado a {target}"));
                } else if now - start > HANDSHAKE_TIMEOUT {
                    return "o iPad não respondeu ao HELLO em 3 s".into();
                }
            }

            // PING: 5 rápidos ao ligar (sincronização inicial do relógio), depois a cada 500 ms.
            if now >= next_ping {
                let ping = Message::from_payload(&Ping { t0: self.shared.clock.now_us() }).with_seq(s.next_seq(MessageType::Ping));
                if let Err(e) = session::send(writer, &ping) {
                    return format!("ligação perdida ao escrever ({e})");
                }
                pings += 1;
                next_ping = now + if pings < 5 { Duration::from_millis(50) } else { Duration::from_millis(500) };
            }

            let idle = now.saturating_duration_since(*s.last_rx.lock().unwrap());
            if idle > IDLE_TIMEOUT {
                self.send_bye(s, writer, ByeReason::Timeout, "sem dados do iPad há 3 s");
                return "sem dados do iPad há 3 s".into();
            }

            if now >= next_tick {
                next_tick += Duration::from_secs(1);
                let (telemetry, report) = s.snapshot();
                *self.shared.telemetry.lock().unwrap() = telemetry;
                if streaming {
                    let msg = Message::from_payload(&report)
                        .with_seq(s.next_seq(MessageType::ReceiverReport))
                        .with_pts(self.shared.clock.now_us());
                    let _ = session::send(writer, &msg);
                }
            }
        }
    }

    fn handle_online(&mut self, cmd: Command, s: &Session, writer: &SharedWriter) -> Option<String> {
        let now_us = self.shared.clock.now_us();
        match cmd {
            Command::Configure(c) => {
                self.cfg.stream.merge(&c);
                let msg = Message::from_payload(&Configure(c)).with_seq(s.next_seq(MessageType::Configure)).with_pts(now_us);
                let _ = session::send(writer, &msg);
            }
            Command::RequestKeyframe => {
                if self.last_keyframe_request.is_none_or(|t| t.elapsed() >= KEYFRAME_COALESCE) {
                    self.last_keyframe_request = Some(Instant::now());
                    let msg = Message::new(MessageType::RequestKeyframe, Bytes::new())
                        .with_seq(s.next_seq(MessageType::RequestKeyframe));
                    let _ = session::send(writer, &msg);
                }
            }
            Command::SetBitrate(bitrate_bps) => {
                self.cfg.stream.bitrate_bps = Some(bitrate_bps.into());
                let msg = Message::from_payload(&SetBitrate { bitrate_bps }).with_seq(s.next_seq(MessageType::SetBitrate));
                let _ = session::send(writer, &msg);
            }
            Command::SetEndpoint(e) => {
                if e != self.cfg.endpoint {
                    self.cfg.endpoint = e;
                    self.send_bye(s, writer, ByeReason::UserStopped, "o PC mudou de ligação");
                    return Some("ligação alterada no PC".into());
                }
            }
            Command::Shutdown => {
                self.shutdown = true;
                self.send_bye(s, writer, ByeReason::Shutdown, "o PC encerrou o recetor");
                return Some("recetor encerrado".into());
            }
        }
        None
    }

    fn send_bye(&self, s: &Session, writer: &SharedWriter, reason: ByeReason, message: &str) {
        let bye = Bye { reason, message: Some(message.into()) };
        let _ = session::send(writer, &Message::from_payload(&bye).with_seq(s.next_seq(MessageType::Bye)));
    }

    fn end_session(&self, reason: &str) {
        *self.shared.media.lock().unwrap() = MediaState::default();
        *self.shared.clock_offset_us.lock().unwrap() = None;
        *self.shared.telemetry.lock().unwrap() = Telemetry::default();
        self.shared.bus.publish(MediaEvent::SessionEnded { reason: reason.into() });
        self.shared.info(format!("Sessão terminada: {reason}"));
    }
}

fn connect_tcp(addr: &str) -> Result<Link, Wait> {
    let unreachable = |m: String| Wait::new(WaitReason::Unreachable, m);
    let sock = addr
        .to_socket_addrs()
        .map_err(|e| unreachable(format!("endereço inválido {addr}: {e}")))?
        .next()
        .ok_or_else(|| unreachable(format!("endereço inválido {addr}")))?;
    let stream = match TcpStream::connect_timeout(&sock, Duration::from_secs(2)) {
        Ok(s) => s,
        // No Windows, uma porta fechada em localhost pode esgotar o tempo em vez de recusar.
        Err(e) if e.kind() == ErrorKind::ConnectionRefused || (e.kind() == ErrorKind::TimedOut && sock.ip().is_loopback()) => {
            return Err(Wait::new(
                WaitReason::BroadcastInactive,
                format!("{addr} recusou a ligação: a transmissão não está ativa."),
            ));
        }
        Err(e) => return Err(unreachable(format!("{addr} inacessível ({e})"))),
    };
    let io = |e: std::io::Error| Wait::new(WaitReason::Error, format!("socket: {e}"));
    stream.set_nodelay(true).map_err(io)?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(io)?;
    let reader = stream.try_clone().map_err(io)?;
    let closer = stream.try_clone().map_err(io)?;
    Ok(Link {
        reader: Box::new(reader),
        writer: Box::new(stream),
        closer: Box::new(move || {
            let _ = closer.shutdown(Shutdown::Both);
        }),
        target: addr.to_string(),
        transport: TransportKind::Lan,
        device_name: None,
    })
}
