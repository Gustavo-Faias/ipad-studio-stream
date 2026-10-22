//! Mixer de áudio do PadStudio: áudio do iPad (app + microfone do iPad) + microfone do PC,
//! reamostrado para 48 kHz estéreo, com ganhos em tempo real, medidores e correção de deriva.
//!
//! ```text
//!   Bus (AUDIO_FRAME do iPad, pts do iPad → relógio local) ─┐
//!   Microfone do PC (cpal, instante de captura)  ───────────┼─► linha temporal 48 kHz ─► Bus (MixedAudio)
//!                                                           │   (latência fixa ~60 ms)
//!   Ganhos / mute (UI) ─────────────────────────────────────┘
//! ```
//!
//! Cada entrada é colocada na linha temporal pelo seu instante de captura, por isso o áudio do
//! jogo, o microfone e o vídeo ficam sincronizados. O `pts_local_us` da mistura está no mesmo
//! relógio que `Controller::remote_to_local(pts_do_vídeo)`.

#![forbid(unsafe_code)]

mod convert;
mod core;
mod mic;
mod mixer;
mod track;

pub use convert::{db_to_linear, linear_to_db};
pub use mic::{MicInfo, default_input_device, input_devices};
pub use mixer::{InputLevel, Mixer, MixerConfig, MixerLevels, MicSelection};
pub use track::OUT_RATE;

use serde::{Deserialize, Serialize};

/// Entradas do mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum InputId {
    /// Som do jogo/app no iPad.
    IpadApp = 0,
    /// Microfone do iPad (se ativado na folha de transmissão).
    IpadMic = 1,
    /// Microfone do PC.
    PcMic = 2,
}

impl InputId {
    pub const COUNT: usize = 3;
    pub const ALL: [InputId; 3] = [Self::IpadApp, Self::IpadMic, Self::PcMic];

    pub fn label(self) -> &'static str {
        match self {
            Self::IpadApp => "Jogo (iPad)",
            Self::IpadMic => "Microfone (iPad)",
            Self::PcMic => "Microfone (PC)",
        }
    }
}
