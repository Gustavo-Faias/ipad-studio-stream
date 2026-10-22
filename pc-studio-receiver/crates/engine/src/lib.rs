//! Motor do PadStudio (ver `docs/ARCHITECTURE.md` §4.2).
//!
//! ```text
//!   iPad ──(USB/usbmuxd ou TCP)──► sessão PSWP ──► Bus ──► preview (passo 3)
//!                                     │                ├──► gravador MKV
//!                                     │                └──► streaming FFmpeg (passo 2f)
//!                                     └──► telemetria + estado (UI)
//! ```
//!
//! Tudo corre em threads normais (sem runtime async), com canais `crossbeam`:
//! - [`Engine::start`] lança o supervisor, que procura o iPad, liga-se, faz o handshake e volta
//!   a tentar a cada segundo quando a ligação cai.
//! - [`Controller`] (clonável) envia comandos, lê estado/telemetria e cria subscrições do bus.
//! - [`Bus`] distribui [`MediaEvent`]s sem copiar dados; um consumidor lento recebe `Gap` e deve
//!   esperar pelo keyframe seguinte.
//! - [`recorder::Recorder`] grava em MKV a partir de uma subscrição.

#![forbid(unsafe_code)]

mod bus;
mod devices;
mod engine;
mod media;
pub mod mkv;
pub mod recorder;
mod session;
mod telemetry;

pub use bus::{Bus, BusClosed, Subscription};
pub use devices::model_name;
pub use engine::{Controller, Endpoint, Engine, EngineConfig, EngineEvent, EngineStatus, WaitReason};
pub use media::{AudioPacket, MediaEvent, MediaState, MixedAudio, SessionInfo, TransportKind, VideoPacket};
pub use telemetry::Telemetry;

use std::time::Instant;

/// Relógio monotónico local em µs (o domínio de `recv_us`).
#[derive(Debug)]
pub struct LocalClock {
    start: Instant,
}

impl LocalClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }

    pub fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64 + 1
    }
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::new()
    }
}
