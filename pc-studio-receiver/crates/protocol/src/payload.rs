//! Payloads binários (PROTOCOL.md §4), flags e o trait [`Payload`].

use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use crate::header::{read_u16, read_u32, read_u64};
use crate::{MessageType, ProtocolError};

/// Payload tipado de uma mensagem (binário ou JSON).
pub trait Payload: Sized {
    const TYPE: MessageType;
    fn encode_payload(&self) -> Bytes;
    fn decode_payload(payload: &Bytes) -> Result<Self, ProtocolError>;
}

fn need(payload: &[u8], kind: MessageType, min: usize) -> Result<(), ProtocolError> {
    if payload.len() < min {
        return Err(ProtocolError::ShortPayload { kind: kind.name(), len: payload.len(), min });
    }
    Ok(())
}

// ── Enumerações ──────────────────────────────────────────────────────────────

/// Codec de vídeo. Em JSON: `"h264"` / `"hevc"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Codec {
    H264 = 1,
    Hevc = 2,
}

impl Codec {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::H264),
            2 => Some(Self::Hevc),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
        }
    }
}

/// Rotação que o recetor tem de aplicar para mostrar a imagem direita (sentido horário).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Rotation {
    #[default]
    R0 = 0,
    R90 = 1,
    R180 = 2,
    R270 = 3,
}

impl Rotation {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x3 {
            0 => Self::R0,
            1 => Self::R90,
            2 => Self::R180,
            _ => Self::R270,
        }
    }

    pub fn from_degrees(degrees: u16) -> Option<Self> {
        match degrees % 360 {
            0 => Some(Self::R0),
            90 => Some(Self::R90),
            180 => Some(Self::R180),
            270 => Some(Self::R270),
            _ => None,
        }
    }

    pub fn degrees(self) -> u16 {
        self as u16 * 90
    }

    /// Verdadeiro quando largura e altura trocam ao mostrar a imagem.
    pub fn swaps_dimensions(self) -> bool {
        matches!(self, Self::R90 | Self::R270)
    }
}

/// Fonte de áudio (4 bits). Valores desconhecidos são preservados.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AudioSource(pub u8);

impl AudioSource {
    pub const APP: Self = Self(0);
    pub const MIC: Self = Self(1);
}

/// Formato PCM (sempre intercalado).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AudioFormat {
    S16Le = 1,
    F32Le = 2,
}

impl AudioFormat {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::S16Le),
            2 => Some(Self::F32Le),
            _ => None,
        }
    }

    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::S16Le => 2,
            Self::F32Le => 4,
        }
    }
}

// ── Flags ────────────────────────────────────────────────────────────────────

/// Flags de `VIDEO_FRAME` (PROTOCOL.md §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoFlags {
    pub keyframe: bool,
    pub params_inband: bool,
    pub discontinuity: bool,
    pub rotation: Rotation,
}

impl VideoFlags {
    pub const KEYFRAME: u8 = 0x01;
    pub const PARAMS_INBAND: u8 = 0x02;
    pub const DISCONTINUITY: u8 = 0x04;
    pub const ROTATION_SHIFT: u8 = 4;
    pub const ROTATION_MASK: u8 = 0x30;

    pub fn to_u8(self) -> u8 {
        (if self.keyframe { Self::KEYFRAME } else { 0 })
            | (if self.params_inband { Self::PARAMS_INBAND } else { 0 })
            | (if self.discontinuity { Self::DISCONTINUITY } else { 0 })
            | ((self.rotation as u8) << Self::ROTATION_SHIFT)
    }

    pub fn from_u8(f: u8) -> Self {
        Self {
            keyframe: f & Self::KEYFRAME != 0,
            params_inband: f & Self::PARAMS_INBAND != 0,
            discontinuity: f & Self::DISCONTINUITY != 0,
            rotation: Rotation::from_bits((f & Self::ROTATION_MASK) >> Self::ROTATION_SHIFT),
        }
    }
}

/// Flags de `AUDIO_FRAME` (PROTOCOL.md §4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioFlags {
    pub source: AudioSource,
    pub discontinuity: bool,
}

impl AudioFlags {
    pub const SOURCE_MASK: u8 = 0x0f;
    pub const DISCONTINUITY: u8 = 0x10;

    pub fn to_u8(self) -> u8 {
        (self.source.0 & Self::SOURCE_MASK) | if self.discontinuity { Self::DISCONTINUITY } else { 0 }
    }

    pub fn from_u8(f: u8) -> Self {
        Self { source: AudioSource(f & Self::SOURCE_MASK), discontinuity: f & Self::DISCONTINUITY != 0 }
    }
}

// ── Payloads binários ────────────────────────────────────────────────────────

/// `PING`: relógio de quem envia, em µs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub t0: u64,
}

impl Payload for Ping {
    const TYPE: MessageType = MessageType::Ping;

    fn encode_payload(&self) -> Bytes {
        Bytes::copy_from_slice(&self.t0.to_le_bytes())
    }

    fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
        need(p, Self::TYPE, 8)?;
        Ok(Self { t0: read_u64(p, 0) })
    }
}

/// `PONG`: `t0` copiado do PING, `t1` receção e `t2` envio no relógio de quem responde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub t0: u64,
    pub t1: u64,
    pub t2: u64,
}

impl Payload for Pong {
    const TYPE: MessageType = MessageType::Pong;

    fn encode_payload(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(24);
        b.put_u64_le(self.t0);
        b.put_u64_le(self.t1);
        b.put_u64_le(self.t2);
        b.freeze()
    }

    fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
        need(p, Self::TYPE, 24)?;
        Ok(Self { t0: read_u64(p, 0), t1: read_u64(p, 8), t2: read_u64(p, 16) })
    }
}

/// `VIDEO_CONFIG` (PROTOCOL.md §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfig {
    pub codec: Codec,
    pub bit_depth: u8,
    pub fps_max: u16,
    /// Largura sem rotação.
    pub width: u16,
    /// Altura sem rotação.
    pub height: u16,
    pub bitrate_bps: u32,
    /// Códigos ITU-T H.273.
    pub color_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
    /// VPS (HEVC) + SPS + PPS em Anexo-B.
    pub param_sets: Bytes,
}

impl VideoConfig {
    pub const FIXED_LEN: usize = 16;
}

impl Payload for VideoConfig {
    const TYPE: MessageType = MessageType::VideoConfig;

    fn encode_payload(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(Self::FIXED_LEN + self.param_sets.len());
        b.put_u8(self.codec as u8);
        b.put_u8(self.bit_depth);
        b.put_u16_le(self.fps_max);
        b.put_u16_le(self.width);
        b.put_u16_le(self.height);
        b.put_u32_le(self.bitrate_bps);
        b.put_u8(self.color_primaries);
        b.put_u8(self.transfer);
        b.put_u8(self.matrix);
        b.put_u8(self.full_range as u8);
        b.put_slice(&self.param_sets);
        b.freeze()
    }

    fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
        need(p, Self::TYPE, Self::FIXED_LEN)?;
        let codec = Codec::from_u8(p[0]).ok_or(ProtocolError::InvalidValue { field: "codec", value: p[0].into() })?;
        Ok(Self {
            codec,
            bit_depth: p[1],
            fps_max: read_u16(p, 2),
            width: read_u16(p, 4),
            height: read_u16(p, 6),
            bitrate_bps: read_u32(p, 8),
            color_primaries: p[12],
            transfer: p[13],
            matrix: p[14],
            full_range: p[15] != 0,
            param_sets: p.slice(Self::FIXED_LEN..),
        })
    }
}

/// `AUDIO_CONFIG` (PROTOCOL.md §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioConfig {
    pub source: AudioSource,
    pub format: AudioFormat,
    pub channels: u8,
    pub sample_rate: u32,
}

impl AudioConfig {
    /// Bytes por frame de áudio (todas as amostras de um instante).
    pub fn bytes_per_frame(&self) -> usize {
        self.format.bytes_per_sample() * self.channels as usize
    }
}

impl Payload for AudioConfig {
    const TYPE: MessageType = MessageType::AudioConfig;

    fn encode_payload(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(8);
        b.put_u8(self.source.0);
        b.put_u8(self.format as u8);
        b.put_u8(self.channels);
        b.put_u8(0);
        b.put_u32_le(self.sample_rate);
        b.freeze()
    }

    fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
        need(p, Self::TYPE, 8)?;
        let format = AudioFormat::from_u8(p[1]).ok_or(ProtocolError::InvalidValue { field: "format", value: p[1].into() })?;
        Ok(Self { source: AudioSource(p[0]), format, channels: p[2], sample_rate: read_u32(p, 4) })
    }
}

/// `SET_BITRATE`: novo bitrate alvo, aplicado em direto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetBitrate {
    pub bitrate_bps: u32,
}

impl Payload for SetBitrate {
    const TYPE: MessageType = MessageType::SetBitrate;

    fn encode_payload(&self) -> Bytes {
        Bytes::copy_from_slice(&self.bitrate_bps.to_le_bytes())
    }

    fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
        need(p, Self::TYPE, 4)?;
        Ok(Self { bitrate_bps: read_u32(p, 0) })
    }
}
