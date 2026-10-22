//! Testa o cliente contra um usbmuxd falso (TCP local), sem hardware. O servidor falso
//! descodifica e codifica as mensagens de forma independente do código do crate.

use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use padstudio_usbmux::{ConnectionType, DeviceEvent, UsbmuxAddr, UsbmuxClient, UsbmuxError, port_number};
use plist::{Dictionary, Value};

fn read_request(s: &mut TcpStream) -> (u32, Dictionary) {
    let mut header = [0u8; 16];
    s.read_exact(&mut header).unwrap();
    let len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 1, "versão plist");
    assert_eq!(u32::from_le_bytes(header[8..12].try_into().unwrap()), 8, "mensagem plist");
    let tag = u32::from_le_bytes(header[12..16].try_into().unwrap());
    let mut body = vec![0u8; len - 16];
    s.read_exact(&mut body).unwrap();
    let dict = Value::from_reader(Cursor::new(body)).unwrap().into_dictionary().unwrap();
    for key in ["ClientVersionString", "ProgName", "kLibUSBMuxVersion"] {
        assert!(dict.contains_key(key), "falta {key}");
    }
    (tag, dict)
}

fn encode(tag: u32, dict: Dictionary) -> Vec<u8> {
    let mut body = Vec::new();
    Value::Dictionary(dict).to_writer_xml(&mut body).unwrap();
    let mut out = Vec::new();
    for v in [(16 + body.len()) as u32, 1, 8, tag] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&body);
    out
}

fn dict(entries: &[(&str, Value)]) -> Dictionary {
    entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn int(v: u64) -> Value {
    Value::Integer(v.into())
}

fn string(s: &str) -> Value {
    Value::String(s.into())
}

fn result(n: u64) -> Dictionary {
    dict(&[("MessageType", string("Result")), ("Number", int(n))])
}

fn device_record(id: u64, udid: &str, connection: &str) -> Value {
    let props = dict(&[
        ("ConnectionType", string(connection)),
        ("DeviceID", int(id)),
        ("ProductID", int(0x12ab)),
        ("SerialNumber", string(udid)),
    ]);
    Value::Dictionary(dict(&[
        ("DeviceID", int(id)),
        ("MessageType", string("Attached")),
        ("Properties", Value::Dictionary(props)),
    ]))
}

/// Servidor falso numa porta livre; `handler` trata uma única ligação.
fn mock(handler: impl FnOnce(TcpStream) + Send + 'static) -> UsbmuxAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || handler(listener.accept().unwrap().0));
    UsbmuxAddr::Tcp(addr)
}

#[test]
fn list_devices() {
    let addr = mock(|mut s| {
        let (tag, req) = read_request(&mut s);
        assert_eq!(req.get("MessageType").and_then(Value::as_string), Some("ListDevices"));
        let list = Value::Array(vec![device_record(3, "UDID-USB", "USB"), device_record(9, "UDID-WIFI", "Network")]);
        s.write_all(&encode(tag, dict(&[("DeviceList", list)]))).unwrap();
    });
    let devices = UsbmuxClient::connect_to(&addr).unwrap().list_devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!((devices[0].device_id, devices[0].udid.as_str()), (3, "UDID-USB"));
    assert_eq!(devices[0].connection, ConnectionType::Usb);
    assert_eq!(devices[1].connection, ConnectionType::Network);
}

#[test]
fn connect_hands_over_bytes_sent_right_after_result() {
    let addr = mock(|mut s| {
        let (tag, req) = read_request(&mut s);
        assert_eq!(req.get("MessageType").and_then(Value::as_string), Some("Connect"));
        assert_eq!(req.get("DeviceID").and_then(Value::as_unsigned_integer), Some(7));
        let port = req.get("PortNumber").and_then(Value::as_unsigned_integer).unwrap();
        assert_eq!(port, u64::from(port_number(23456)));
        // Result + os primeiros bytes do dispositivo na mesma escrita.
        let mut out = encode(tag, result(0));
        out.extend_from_slice(b"PS-hello");
        s.write_all(&out).unwrap();
        // Depois, eco.
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).unwrap();
        s.write_all(&buf).unwrap();
    });
    let mut tunnel = UsbmuxClient::connect_to(&addr).unwrap().connect_device(7, 23456).unwrap();
    let mut first = [0u8; 8];
    tunnel.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"PS-hello");
    let mut writer = tunnel.writer().unwrap();
    writer.write_all(b"ping").unwrap();
    let mut echo = [0u8; 4];
    tunnel.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"ping");
}

#[test]
fn connect_refused_is_retryable() {
    let addr = mock(|mut s| {
        let (tag, _) = read_request(&mut s);
        s.write_all(&encode(tag, result(3))).unwrap();
    });
    let err = UsbmuxClient::connect_to(&addr).unwrap().connect_device(7, 23456).err().unwrap();
    assert!(matches!(err, UsbmuxError::ConnectionRefused { device_id: 7, port: 23456 }), "{err}");
    assert!(err.is_retryable());
}

#[test]
fn listen_delivers_attach_and_detach() {
    let addr = mock(|mut s| {
        let (tag, req) = read_request(&mut s);
        assert_eq!(req.get("MessageType").and_then(Value::as_string), Some("Listen"));
        let mut out = encode(tag, result(0));
        out.extend(encode(0, device_record(5, "UDID-A", "USB").into_dictionary().unwrap()));
        s.write_all(&out).unwrap();
        s.write_all(&encode(0, dict(&[("MessageType", string("Detached")), ("DeviceID", int(5))]))).unwrap();
        s.write_all(&encode(0, dict(&[("MessageType", string("Paired")), ("DeviceID", int(5))]))).unwrap();
    });
    let mut events = UsbmuxClient::connect_to(&addr).unwrap().listen().unwrap();
    match events.next_event().unwrap() {
        Some(DeviceEvent::Attached(d)) => assert_eq!((d.device_id, d.udid.as_str()), (5, "UDID-A")),
        other => panic!("esperava Attached, recebi {other:?}"),
    }
    assert_eq!(events.next_event().unwrap(), Some(DeviceEvent::Detached { device_id: 5 }));
    assert_eq!(events.next_event().unwrap(), Some(DeviceEvent::Paired { device_id: 5 }));
}

#[test]
fn unavailable_service_has_helpful_error() {
    // Porta onde ninguém escuta.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let err = UsbmuxClient::connect_to(&UsbmuxAddr::Tcp(port)).err().unwrap();
    assert!(matches!(err, UsbmuxError::Unavailable { .. }));
    assert!(err.to_string().contains("Dispositivos Apple"));
}
