//! Consulta mínima ao `lockdownd` (porta 62078 de qualquer iPad/iPhone), sem emparelhamento.
//!
//! Serve para identificar o dispositivo na UI (nome, modelo, versão do sistema) antes de a
//! transmissão começar, e para testar o túnel do usbmuxd com um serviço que existe sempre.
//! Sem emparelhamento, o dispositivo só responde a alguns valores; os restantes ficam `None`.
//! Só leitura: não altera nada no dispositivo.

use std::io::{Cursor, Read, Write};
use std::time::Duration;

use plist::{Dictionary, Value};

use crate::{Result, Tunnel, UsbmuxClient, UsbmuxError};

pub const LOCKDOWN_PORT: u16 = 62078;
const LABEL: &str = "padstudio";
const MAX_REPLY: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockdownInfo {
    /// `com.apple.mobile.lockdown` num dispositivo normal.
    pub service_type: String,
    pub device_name: Option<String>,
    /// Ex.: `iPad16,6`.
    pub product_type: Option<String>,
    /// Versão do sistema, ex.: `26.0`.
    pub product_version: Option<String>,
    /// `iPad`, `iPhone`, …
    pub device_class: Option<String>,
    pub hardware_model: Option<String>,
}

/// Abre um túnel até ao lockdownd do dispositivo e lê a informação pública.
pub fn query(device_id: u32) -> Result<LockdownInfo> {
    let mut tunnel = UsbmuxClient::connect()?.connect_device(device_id, LOCKDOWN_PORT)?;
    tunnel.set_read_timeout(Some(Duration::from_secs(5)))?;

    let reply = call(&mut tunnel, "QueryType", None)?;
    let service_type = reply
        .get("Type")
        .and_then(Value::as_string)
        .ok_or_else(|| UsbmuxError::Protocol("QueryType sem Type".into()))?
        .to_string();

    let mut info = LockdownInfo { service_type, ..Default::default() };
    let slots = [
        ("DeviceName", &mut info.device_name),
        ("ProductType", &mut info.product_type),
        ("ProductVersion", &mut info.product_version),
        ("DeviceClass", &mut info.device_class),
        ("HardwareModel", &mut info.hardware_model),
    ];
    for (key, slot) in slots {
        match get_value(&mut tunnel, key) {
            Ok(v) => *slot = v,
            Err(_) => break, // o lockdownd fechou a ligação: fica o que já se leu
        }
    }
    let _ = tunnel.shutdown();
    Ok(info)
}

/// `GetValue` de uma chave; `None` se o dispositivo a recusar sem emparelhamento.
fn get_value(tunnel: &mut Tunnel, key: &str) -> Result<Option<String>> {
    let reply = call(tunnel, "GetValue", Some(key))?;
    if reply.contains_key("Error") {
        return Ok(None);
    }
    Ok(reply.get("Value").and_then(Value::as_string).map(str::to_string))
}

/// Protocolo do lockdownd: 4 bytes (big-endian) de tamanho + plist XML, nos dois sentidos.
fn call(tunnel: &mut Tunnel, request: &str, key: Option<&str>) -> Result<Dictionary> {
    let mut d = Dictionary::new();
    d.insert("Label".into(), Value::String(LABEL.into()));
    d.insert("Request".into(), Value::String(request.into()));
    if let Some(k) = key {
        d.insert("Key".into(), Value::String(k.into()));
    }
    let mut body = Vec::new();
    Value::Dictionary(d).to_writer_xml(&mut body)?;
    let mut msg = (body.len() as u32).to_be_bytes().to_vec();
    msg.extend_from_slice(&body);
    tunnel.write_all(&msg)?;

    let mut len = [0u8; 4];
    tunnel.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_REPLY {
        return Err(UsbmuxError::Protocol(format!("resposta do lockdownd demasiado grande ({len} bytes)")));
    }
    let mut reply = vec![0u8; len];
    tunnel.read_exact(&mut reply)?;
    Value::from_reader(Cursor::new(reply))?
        .into_dictionary()
        .ok_or_else(|| UsbmuxError::Protocol("resposta do lockdownd sem dicionário".into()))
}
