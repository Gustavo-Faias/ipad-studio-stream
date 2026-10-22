use std::process::Child;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use padstudio_protocol::json::{BitrateMode, StreamConfig};
use padstudio_protocol::payload::{Codec, Rotation, VideoConfig};

use crate::Opts;
use crate::encoder::{EncodedAu, EncoderKind};
use crate::session::Session;

/// Relógio monotónico em µs (equivalente ao host time do iPad).
pub struct Clock {
    start: Instant,
}

impl Clock {
    /// Desvio inicial para os valores parecerem um uptime real.
    const BASE_US: u64 = 1_000_000_000;

    pub fn now_us(&self) -> u64 {
        Self::BASE_US + self.start.elapsed().as_micros() as u64
    }
}

/// Configuração efetiva do encoder simulado.
#[derive(Debug, Clone, PartialEq)]
pub struct SenderConfig {
    /// Ecrã simulado (largura, altura) na orientação nativa.
    pub screen: (u32, u32),
    pub codec: Codec,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

impl SenderConfig {
    /// O ecrã simulado escalado para caber em max_width × max_height (dimensões pares).
    pub fn output_size(&self) -> (u32, u32) {
        let (sw, sh) = self.screen;
        let mw = if self.max_width == 0 { sw } else { self.max_width.min(sw) };
        let mh = if self.max_height == 0 { sh } else { self.max_height.min(sh) };
        let scale = (f64::from(mw) / f64::from(sw)).min(f64::from(mh) / f64::from(sh));
        let even = |v: f64| (((v / 2.0).round() as u32) * 2).max(2);
        (even(f64::from(sw) * scale), even(f64::from(sh) * scale))
    }

    pub fn to_stream_config(&self, audio: bool) -> StreamConfig {
        StreamConfig {
            codec: Some(self.codec),
            max_fps: Some(self.fps),
            min_fps: Some(0),
            bitrate_bps: Some(u64::from(self.bitrate_bps)),
            bitrate_mode: Some(BitrateMode::Cbr),
            max_width: Some(self.max_width),
            max_height: Some(self.max_height),
            keyframe_interval_s: Some(1.0),
            audio_app: Some(audio),
            audio_mic: Some(false),
        }
    }

    /// Aplica um pedido do PC. Devolve notas sobre o que foi ajustado ou ignorado.
    pub fn apply(&mut self, req: &StreamConfig) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(codec) = req.codec {
            self.codec = codec;
        }
        if let Some(fps) = req.max_fps {
            self.fps = fps.clamp(1, 240);
            if self.fps != fps {
                notes.push(format!("max_fps ajustado a {}", self.fps));
            }
        }
        if let Some(b) = req.bitrate_bps {
            self.bitrate_bps = b.clamp(500_000, 200_000_000) as u32;
            if u64::from(self.bitrate_bps) != b {
                notes.push(format!("bitrate_bps ajustado a {}", self.bitrate_bps));
            }
        }
        if let Some(w) = req.max_width {
            self.max_width = w;
        }
        if let Some(h) = req.max_height {
            self.max_height = h;
        }
        if req.min_fps.is_some() || req.keyframe_interval_s.is_some() || req.bitrate_mode.is_some() {
            notes.push("min_fps, keyframe_interval_s e bitrate_mode são fixos no fake-sender".into());
        }
        notes
    }
}

#[derive(Default)]
pub struct VideoState {
    pub config: Option<VideoConfig>,
    /// Muda sempre que a VIDEO_CONFIG muda (cada sessão reenvia-a antes do keyframe seguinte).
    pub id: u64,
}

#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub video_bytes: AtomicU64,
    pub keyframes: AtomicU64,
}

pub struct Shared {
    pub opts: Opts,
    pub clock: Clock,
    pub encoder_kind: EncoderKind,
    pub config: Mutex<SenderConfig>,
    pub encoder_generation: AtomicU64,
    pub encoder_child: Mutex<Option<Child>>,
    pub video: Mutex<VideoState>,
    pub client: Mutex<Option<Arc<Session>>>,
    pub rotation: AtomicU8,
    pub stats: Stats,
    pub next_session_id: AtomicU64,
    pub token: Option<String>,
}

impl Shared {
    pub fn new(opts: Opts, encoder_kind: EncoderKind, config: SenderConfig, token: Option<String>, rotation: Rotation) -> Self {
        Self {
            opts,
            clock: Clock { start: Instant::now() },
            encoder_kind,
            config: Mutex::new(config),
            encoder_generation: AtomicU64::new(0),
            encoder_child: Mutex::new(None),
            video: Mutex::new(VideoState::default()),
            client: Mutex::new(None),
            rotation: AtomicU8::new(rotation as u8),
            stats: Stats::default(),
            next_session_id: AtomicU64::new(1),
            token,
        }
    }

    pub fn audio_enabled(&self) -> bool {
        !self.opts.no_audio
    }

    pub fn current_client(&self) -> Option<Arc<Session>> {
        self.client.lock().unwrap().clone()
    }

    pub fn effective_config(&self) -> StreamConfig {
        self.config.lock().unwrap().to_stream_config(self.audio_enabled())
    }

    /// Aplica um pedido de configuração; reinicia o encoder se alguma coisa mudou.
    pub fn apply_config(&self, req: &StreamConfig) -> (StreamConfig, Vec<String>) {
        let mut cfg = self.config.lock().unwrap();
        let before = cfg.clone();
        let notes = cfg.apply(req);
        let changed = *cfg != before;
        let after = cfg.clone();
        drop(cfg);
        if changed {
            let (w, h) = after.output_size();
            log!(
                "nova configuração: {} {w}x{h} @ {} fps, {:.1} Mbps — a reiniciar o encoder",
                after.codec.name(),
                after.fps,
                f64::from(after.bitrate_bps) / 1e6
            );
            self.restart_encoder();
        }
        (after.to_stream_config(self.audio_enabled()), notes)
    }

    pub fn restart_encoder(&self) {
        self.encoder_generation.fetch_add(1, Ordering::SeqCst);
        if let Some(child) = self.encoder_child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    /// Chamado pelo leitor do encoder para cada access unit completo.
    pub fn on_encoded_au(&self, au: EncodedAu, cfg: &SenderConfig, generation: u64) {
        if generation != self.encoder_generation.load(Ordering::SeqCst) {
            return; // saída de um encoder que já foi substituído
        }
        self.stats.frames.fetch_add(1, Ordering::Relaxed);
        self.stats.video_bytes.fetch_add(au.data.len() as u64, Ordering::Relaxed);
        if au.keyframe {
            self.stats.keyframes.fetch_add(1, Ordering::Relaxed);
        }
        let pts = self.clock.now_us();

        let (video_config, config_id) = {
            let mut v = self.video.lock().unwrap();
            if let Some(param_sets) = au.param_sets.as_ref().filter(|_| au.keyframe) {
                let (w, h) = cfg.output_size();
                let candidate = VideoConfig {
                    codec: cfg.codec,
                    bit_depth: 8,
                    fps_max: cfg.fps as u16,
                    width: w as u16,
                    height: h as u16,
                    bitrate_bps: cfg.bitrate_bps,
                    color_primaries: 1, // BT.709 (o FFmpeg é configurado com estas cores)
                    transfer: 1,
                    matrix: 1,
                    full_range: false,
                    param_sets: param_sets.clone(),
                };
                if v.config.as_ref() != Some(&candidate) {
                    v.config = Some(candidate);
                    v.id += 1;
                }
            }
            match &v.config {
                Some(c) => (c.clone(), v.id),
                None => return, // ainda sem parâmetros: espera pelo primeiro keyframe
            }
        };

        if let Some(client) = self.current_client() {
            let rotation = Rotation::from_bits(self.rotation.load(Ordering::Relaxed));
            client.on_video(&au, pts, rotation, &video_config, config_id);
        }
    }
}
