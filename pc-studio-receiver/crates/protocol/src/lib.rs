//! PSWP — PadStudio Wire Protocol v1.
//!
//! Especificação: `docs/PROTOCOL.md`. Testado contra os vetores partilhados com o lado Swift em
//! `protocol/test-vectors/v1.json` (`tests/vectors.rs`).
//!
//! O crate não faz I/O de rede (é "sans-IO"): o [`StreamParser`] recebe bytes de qualquer
//! transporte (USB via usbmuxd, TCP na rede local, testes) e devolve [`Message`]s cujo payload é
//! um [`bytes::Bytes`] partilhado, sem cópias, pronto a distribuir pelo preview, gravador e streaming.
//!
//! ```
//! use padstudio_protocol::{Message, StreamParser, payload::Ping};
//!
//! let bytes = Message::from_payload(&Ping { t0: 42 }).with_seq(1).to_vec();
//! let mut parser = StreamParser::new();
//! parser.push(&bytes);
//! let msg = parser.next_message().unwrap().unwrap();
//! assert_eq!(msg.decode_payload::<Ping>().unwrap().t0, 42);
//! ```

#![forbid(unsafe_code)]

pub mod annexb;
pub mod clock;
mod error;
mod header;
pub mod json;
mod message;
mod parser;
pub mod payload;
mod seq;

pub use error::ProtocolError;
pub use header::{HEADER_LEN, Header, MAGIC, MAX_PAYLOAD, MessageType};
pub use message::Message;
pub use parser::StreamParser;
pub use payload::Payload;
pub use seq::SeqTracker;

/// Versão do protocolo implementada.
pub const PROTOCOL_VERSION: u32 = 1;

/// Porta TCP escutada no iPad para o túnel USB (só loopback no iPad).
pub const USB_PORT: u16 = 23456;

/// Porta TCP escutada no iPad para a rede local.
pub const LAN_PORT: u16 = 23457;

/// Serviço Bonjour anunciado pelo iPad na rede local.
pub const BONJOUR_SERVICE: &str = "_padstudio._tcp";
