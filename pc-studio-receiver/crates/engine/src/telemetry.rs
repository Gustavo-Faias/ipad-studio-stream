use padstudio_protocol::json::SenderStats;
use serde::Serialize;

/// Fotografia da telemetria, atualizada a cada segundo (serializável para a UI).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Telemetry {
    pub connected: bool,
    pub target: Option<String>,
    pub device_name: Option<String>,
    pub device_system: Option<String>,

    pub codec: Option<String>,
    pub width: u16,
    pub height: u16,
    pub fps_max: u16,
    /// Rotação a aplicar para mostrar a imagem direita (sentido horário).
    pub rotation_deg: u16,

    /// Frames de vídeo recebidos no último segundo.
    pub fps: f64,
    /// Tudo o que chegou pelo cabo/rede (cabeçalhos incluídos).
    pub link_bitrate_bps: u64,
    pub video_bitrate_bps: u64,
    pub audio_packets_per_s: f64,

    /// Captura no iPad → receção no PC.
    pub latency_total_p50_ms: Option<f64>,
    pub latency_total_p95_ms: Option<f64>,
    /// Parte da latência gasta no iPad (entrega do ReplayKit + encode), reportada em `aux`.
    pub latency_sender_p50_ms: Option<f64>,
    /// Transporte (fila no iPad, cabo, usbmuxd).
    pub latency_network_p50_ms: Option<f64>,
    pub rtt_ms: Option<f64>,
    /// relógio_iPad − relógio_PC.
    pub clock_offset_ms: Option<f64>,

    pub frames_total: u64,
    pub keyframes_total: u64,
    /// Frames que o iPad descartou (saltos no `seq`).
    pub frames_lost: u64,
    pub discontinuities: u64,

    /// Último `SENDER_STATS` do iPad (temperatura, fila, fps de captura…).
    pub sender: Option<SenderStats>,
    pub session_uptime_s: f64,
}

pub(crate) fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v
}

pub(crate) fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[((sorted.len() - 1) as f64 * p).round() as usize])
}
