use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use padstudio_protocol::json::{DeviceInfo, StreamConfig};
use padstudio_protocol::payload::{AudioConfig, AudioSource, VideoConfig, VideoFlags};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Usb,
    Lan,
}

/// Informação de uma sessão, a partir do `HELLO_ACK`.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: Option<String>,
    /// Descrição da ligação (ex.: "iPad Pro 11\" (2018) por USB").
    pub target: String,
    pub transport: TransportKind,
    pub server: Option<String>,
    pub device: DeviceInfo,
    /// Nome legível do modelo, quando conhecido.
    pub device_name: Option<String>,
    /// Configuração efetiva do encoder no iPad.
    pub config: StreamConfig,
}

/// Um access unit de vídeo (Anexo-B, tal como chegou).
#[derive(Debug, Clone)]
pub struct VideoPacket {
    pub data: Bytes,
    pub flags: VideoFlags,
    pub seq: u32,
    /// Instante de captura no relógio do iPad (µs).
    pub pts_us: u64,
    /// Instante de receção no relógio local ([`crate::LocalClock`]).
    pub recv_us: u64,
    /// Identificador da `VIDEO_CONFIG` a que pertence.
    pub config_id: u64,
}

/// Um pacote de áudio PCM intercalado (formato no último `AUDIO_CONFIG` da mesma fonte).
#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub source: AudioSource,
    pub data: Bytes,
    pub discontinuity: bool,
    pub seq: u32,
    pub pts_us: u64,
    pub recv_us: u64,
}

/// Áudio misturado pelo mixer (`padstudio-audio`): f32le intercalado, sempre 48 kHz estéreo.
#[derive(Debug, Clone)]
pub struct MixedAudio {
    /// Instante do primeiro frame no relógio **local** ([`crate::LocalClock`]), já alinhado com a
    /// captura no iPad. Para juntar com vídeo, converte o `pts_us` do vídeo com
    /// [`crate::Controller::remote_to_local`].
    pub pts_local_us: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub frames: usize,
    pub data: Bytes,
}

/// O que circula no [`crate::Bus`].
#[derive(Debug, Clone)]
pub enum MediaEvent {
    SessionStarted(Arc<SessionInfo>),
    SessionEnded { reason: String },
    VideoConfig { config: VideoConfig, id: u64 },
    Video(VideoPacket),
    AudioConfig(AudioConfig),
    Audio(AudioPacket),
    /// Mistura do iPad + microfone do PC, publicada pelo mixer.
    MixedAudio(MixedAudio),
    /// Esta subscrição perdeu `n` eventos porque a fila encheu. No vídeo: esperar pelo próximo keyframe.
    Gap(u64),
}

/// Estado atual dos fluxos, para quem se subscreve a meio de uma sessão.
#[derive(Debug, Clone, Default)]
pub struct MediaState {
    pub session: Option<Arc<SessionInfo>>,
    pub video: Option<(VideoConfig, u64)>,
    /// Por fonte de áudio.
    pub audio: BTreeMap<u8, AudioConfig>,
}
