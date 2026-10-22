//! Payloads JSON (PROTOCOL.md §5). Campos opcionais são omitidos ao serializar; campos
//! desconhecidos são ignorados ao descodificar; valores de enumeração desconhecidos viram `Unknown`.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::payload::{Codec, Payload};
use crate::{MessageType, ProtocolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Usb,
    Lan,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BitrateMode {
    Vbr,
    Cbr,
    #[serde(other)]
    Unknown,
}

/// Configuração do encoder do iPad: objeto de `CONFIGURE`, `HELLO.config` e `HELLO_ACK.config`.
/// Campos em falta mantêm o valor atual.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamConfig {
    pub codec: Option<Codec>,
    pub max_fps: Option<u32>,
    pub min_fps: Option<u32>,
    pub bitrate_bps: Option<u64>,
    pub bitrate_mode: Option<BitrateMode>,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub keyframe_interval_s: Option<f64>,
    pub audio_app: Option<bool>,
    pub audio_mic: Option<bool>,
}

impl StreamConfig {
    /// Aplica por cima de `self` os campos definidos em `other`.
    pub fn merge(&mut self, other: &StreamConfig) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(codec, max_fps, min_fps, bitrate_bps, bitrate_mode, max_width, max_height, keyframe_interval_s, audio_app, audio_mic);
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Want {
    pub video: Option<bool>,
    pub audio_app: Option<bool>,
    pub audio_mic: Option<bool>,
}

/// `HELLO` (PC → iPad).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub client: Option<String>,
    pub client_version: Option<String>,
    pub transport: Transport,
    pub token: Option<String>,
    pub want: Option<Want>,
    pub config: Option<StreamConfig>,
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub model: Option<String>,
    pub system: Option<String>,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
    pub max_fps: Option<u32>,
}

/// `HELLO_ACK` (iPad → PC).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol: u32,
    pub server: Option<String>,
    pub server_version: Option<String>,
    pub session_id: Option<String>,
    pub clock: Option<String>,
    pub device: Option<DeviceInfo>,
    pub codecs: Option<Vec<Codec>>,
    pub config: Option<StreamConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    UserStopped,
    Replaced,
    Unauthorized,
    ProtocolVersion,
    Timeout,
    Shutdown,
    Error,
    #[serde(other)]
    Unknown,
}

/// `BYE` (ambos os sentidos). Quem envia fecha o socket a seguir.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bye {
    pub reason: ByeReason,
    pub message: Option<String>,
}

/// `ERROR` (ambos os sentidos).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: String,
    pub message: Option<String>,
    pub fatal: Option<bool>,
    pub ref_type: Option<u8>,
    pub ref_seq: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastState {
    Active,
    Paused,
    Finishing,
    #[serde(other)]
    Unknown,
}

/// `SENDER_STATE` (iPad → PC).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SenderState {
    pub state: BroadcastState,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
    #[serde(other)]
    Unknown,
}

/// `SENDER_STATS` (iPad → PC, ~1 Hz).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SenderStats {
    pub fps_capture: Option<f64>,
    pub fps_encoded: Option<f64>,
    pub bitrate_bps: Option<u64>,
    pub target_bitrate_bps: Option<u64>,
    pub frames_dropped: Option<u64>,
    pub keyframes: Option<u64>,
    pub send_queue_bytes: Option<u64>,
    pub send_queue_ms: Option<f64>,
    pub encode_ms_avg: Option<f64>,
    pub thermal: Option<ThermalState>,
    pub memory_mb: Option<f64>,
    pub low_power_mode: Option<bool>,
    /// Diagnóstico dos spikes do passo 4 (PROTOCOL.md §5.6.1). Emissores antigos não o enviam.
    pub diag: Option<SenderDiagnostics>,
}

/// `SENDER_STATS.diag`: medições da última janela (~1 s) na extensão do iPad.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SenderDiagnostics {
    pub capture: Option<CaptureDiagnostics>,
    pub encode: Option<EncodeDiagnostics>,
    pub memory: Option<MemoryDiagnostics>,
    pub cpu_percent: Option<f64>,
    pub notes: Option<Vec<String>>,
}

/// Intervalos entre frames entregues pelo ReplayKit, por faixa (ms). 120 Hz ≈ 8,3 ms; 60 Hz ≈ 16,7 ms.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IntervalHistogram {
    #[serde(default)]
    pub lt6: u64,
    #[serde(default, rename = "6to10")]
    pub from6to10: u64,
    #[serde(default, rename = "10to14")]
    pub from10to14: u64,
    #[serde(default, rename = "14to20")]
    pub from14to20: u64,
    #[serde(default, rename = "20to40")]
    pub from20to40: u64,
    #[serde(default)]
    pub ge40: u64,
}

/// S1 — ritmo de captura do ReplayKit.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CaptureDiagnostics {
    pub fps: Option<f64>,
    pub interval_ms_p50: Option<f64>,
    pub interval_ms_p95: Option<f64>,
    pub interval_ms_max: Option<f64>,
    pub intervals: Option<IntervalHistogram>,
    pub lag_ms_p50: Option<f64>,
    pub lag_ms_max: Option<f64>,
    pub repeats: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pixel_format: Option<String>,
    pub orientation: Option<u32>,
    pub rotation: Option<u32>,
}

/// S2 — custo do encoder VideoToolbox.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EncodeDiagnostics {
    pub codec: Option<String>,
    pub input_width: Option<u32>,
    pub input_height: Option<u32>,
    pub output_width: Option<u32>,
    pub output_height: Option<u32>,
    pub scaled: Option<bool>,
    pub ms_p50: Option<f64>,
    pub ms_p95: Option<f64>,
    pub ms_max: Option<f64>,
    pub in_flight_max: Option<u64>,
    pub errors: Option<u64>,
    pub encoder_drops: Option<u64>,
    pub sessions: Option<u64>,
}

/// S6 — memória da extensão (limite ~50 MB).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MemoryDiagnostics {
    pub footprint_mb: Option<f64>,
    pub peak_mb: Option<f64>,
    pub available_mb: Option<f64>,
    pub limit_mb: Option<f64>,
}

/// `RECEIVER_REPORT` (PC → iPad, ~1 Hz).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReceiverReport {
    pub fps_received: Option<f64>,
    pub fps_decoded: Option<f64>,
    pub recv_bitrate_bps: Option<u64>,
    pub latency_ms_p50: Option<f64>,
    pub latency_ms_p95: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub decode_errors: Option<u64>,
}

/// `CONFIGURE` (PC → iPad): o payload é o próprio objeto de configuração.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Configure(pub StreamConfig);

/// `CONFIGURE_ACK` (iPad → PC).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigureAck {
    pub request_seq: u32,
    pub applied: bool,
    pub config: Option<StreamConfig>,
    pub message: Option<String>,
}

macro_rules! json_payloads {
    ($($ty:ty => $kind:ident),* $(,)?) => {$(
        impl Payload for $ty {
            const TYPE: MessageType = MessageType::$kind;

            fn encode_payload(&self) -> Bytes {
                Bytes::from(serde_json::to_vec(self).expect("os tipos do protocolo serializam sempre"))
            }

            fn decode_payload(p: &Bytes) -> Result<Self, ProtocolError> {
                serde_json::from_slice(p)
                    .map_err(|e| ProtocolError::Json { kind: MessageType::$kind.name(), message: e.to_string() })
            }
        }
    )*};
}

json_payloads! {
    Hello => Hello,
    HelloAck => HelloAck,
    Bye => Bye,
    ErrorMsg => Error,
    SenderState => SenderState,
    SenderStats => SenderStats,
    ReceiverReport => ReceiverReport,
    Configure => Configure,
    ConfigureAck => ConfigureAck,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `diag` tal como a extensão Swift o escreve (`SenderDiagnostics` em PadStudioKit).
    #[test]
    fn sender_stats_with_diag_from_swift() {
        let json = r#"{"fps_capture":119.6,"thermal":"fair","memory_mb":21.7,
            "diag":{"capture":{"fps":119.6,"interval_ms_p50":8.33,"interval_ms_max":25.1,
                "intervals":{"lt6":0,"6to10":117,"10to14":1,"14to20":0,"20to40":1,"ge40":0},
                "lag_ms_p50":3.2,"repeats":0,"width":1668,"height":2388,"pixel_format":"420f","orientation":6,"rotation":90},
              "encode":{"codec":"h264","input_width":1668,"input_height":2388,"output_width":1340,"output_height":1920,
                "scaled":true,"ms_p50":3.9,"ms_max":6.1,"in_flight_max":1,"errors":0,"encoder_drops":0,"sessions":1},
              "memory":{"footprint_mb":21.7,"peak_mb":24.0,"available_mb":28.3,"limit_mb":50.0},
              "cpu_percent":12.5,"notes":["VT MaxFrameDelayCount recusado (-12900)"],"future_field":1}}"#;
        let s: SenderStats = serde_json::from_str(json).unwrap();
        let d = s.diag.unwrap();
        let c = d.capture.unwrap();
        assert_eq!(c.intervals.unwrap().from6to10, 117);
        assert_eq!(c.rotation, Some(90));
        assert_eq!(d.encode.unwrap().output_height, Some(1920));
        assert_eq!(d.memory.unwrap().limit_mb, Some(50.0));
        assert_eq!(d.notes.unwrap().len(), 1);
        assert_eq!(s.thermal, Some(ThermalState::Fair));
    }
}
