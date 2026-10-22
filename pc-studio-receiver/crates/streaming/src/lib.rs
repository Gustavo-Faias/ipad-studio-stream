//! Multi-streaming do PadStudio: um processo FFmpeg por destino (Twitch, YouTube, TikTok ou RTMP
//! personalizado), alimentado pelo vídeo do iPad e pela mistura de áudio do motor.
//!
//! ```text
//!   Bus ──► destino Twitch  ─► Matroska (stdin) ─► FFmpeg: decode GPU → 60 fps → 16:9 → NVENC/AMF → RTMP
//!       ├─► destino YouTube ─► …
//!       └─► destino TikTok  ─► … 9:16
//! ```
//!
//! Cada destino é independente: ligar, desligar ou falhar um não afeta os outros. As chaves de
//! stream nunca aparecem nos logs nem no estado (ver [`redact_url`]).

#![forbid(unsafe_code)]

mod encoder;
mod interleave;
mod manager;
mod output;
mod profile;
mod progress;

pub use encoder::{EncoderKind, detect as detect_encoder};
pub use manager::{StreamManager, StreamOptions};
pub use output::{OutputState, OutputStatus};
pub use profile::{Destination, Layout, VideoProfile, redact_url};
