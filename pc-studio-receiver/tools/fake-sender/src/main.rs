//! iPad simulado (PSWP v1).
//!
//! Gera vídeo de teste com o FFmpeg (`testsrc2`), codificado com o encoder de hardware disponível
//! (NVENC / AMF / Quick Sync, ou x264 como último recurso), e serve-o exatamente como o iPad:
//! HELLO/HELLO_ACK, VIDEO_CONFIG, VIDEO_FRAME (Anexo-B, keyframes com parâmetros in-band),
//! áudio PCM, PING/PONG, SENDER_STATS, CONFIGURE e a política de descarte da fila (§7.1).
//!
//! ```text
//! cargo run -p padstudio-fake-sender -- --fps 120
//! cargo run -p padstudio-probe -- --duration 10
//! ```

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Log com o tempo desde o arranque.
macro_rules! log {
    ($($arg:tt)*) => {{
        let t = $crate::START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
        eprintln!("[fake-sender {t:7.2}s] {}", format_args!($($arg)*));
    }};
}

mod audio;
mod encoder;
mod session;
mod state;

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use padstudio_protocol::json::{SenderStats, ThermalState};
use padstudio_protocol::payload::{Codec, Rotation};
use padstudio_protocol::{LAN_PORT, Message, MessageType, USB_PORT};

use crate::encoder::EncoderKind;
use crate::session::Outgoing;
use crate::state::{SenderConfig, Shared};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CodecArg {
    H264,
    Hevc,
}

impl From<CodecArg> for Codec {
    fn from(c: CodecArg) -> Self {
        match c {
            CodecArg::H264 => Codec::H264,
            CodecArg::Hevc => Codec::Hevc,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EncoderArg {
    Auto,
    Nvenc,
    Amf,
    Qsv,
    Software,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "fake-sender", version, about = "iPad simulado: serve PSWP com vídeo de teste do FFmpeg")]
pub struct Opts {
    /// Endereço de escuta. Por omissão 127.0.0.1:23456, como o iPad no túnel USB.
    #[arg(long)]
    pub listen: Option<SocketAddr>,

    /// Modo rede local: escuta em 0.0.0.0:23457 e exige o PIN (--token).
    #[arg(long)]
    pub lan: bool,

    /// PIN de 6 dígitos do modo rede local (gerado se omitido).
    #[arg(long)]
    pub token: Option<String>,

    #[arg(long, value_enum, default_value_t = CodecArg::H264)]
    pub codec: CodecArg,

    #[arg(long, default_value_t = 120)]
    pub fps: u32,

    /// Bitrate alvo em bit/s.
    #[arg(long, default_value_t = 30_000_000)]
    pub bitrate: u32,

    /// Ecrã simulado, LARGURAxALTURA. Por omissão, o do iPad Pro 11" (2018, iPad8,1) a 120 Hz.
    #[arg(long, default_value = "2388x1668", value_parser = parse_screen)]
    pub screen: (u32, u32),

    /// Largura máxima (o ecrã simulado é escalado mantendo o aspeto; 0 = nativo).
    #[arg(long, default_value_t = 1920)]
    pub max_width: u32,

    #[arg(long, default_value_t = 1440)]
    pub max_height: u32,

    #[arg(long, value_enum, default_value_t = EncoderArg::Auto)]
    pub encoder: EncoderArg,

    /// Executável do FFmpeg.
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg: PathBuf,

    /// Não enviar áudio.
    #[arg(long)]
    pub no_audio: bool,

    /// Rotação inicial em graus (0, 90, 180, 270).
    #[arg(long, default_value_t = 0)]
    pub rotation: u16,

    /// Roda mais 90° a cada N segundos (testa a rotação no PC).
    #[arg(long)]
    pub rotate_every: Option<u64>,

    /// A cada N segundos simula uma fila cheia: descarta até ao keyframe seguinte (DISCONTINUITY).
    #[arg(long)]
    pub simulate_drops: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    START.get_or_init(Instant::now);
    let opts = Opts::parse();

    let rotation = Rotation::from_degrees(opts.rotation).context("--rotation tem de ser 0, 90, 180 ou 270")?;
    if opts.fps == 0 || opts.fps > 240 {
        bail!("--fps tem de estar entre 1 e 240");
    }

    let encoder_kind = match opts.encoder {
        EncoderArg::Auto => encoder::detect(&opts.ffmpeg)?,
        EncoderArg::Nvenc => EncoderKind::Nvenc,
        EncoderArg::Amf => EncoderKind::Amf,
        EncoderArg::Qsv => EncoderKind::Qsv,
        EncoderArg::Software => EncoderKind::Software,
    };
    log!("encoder: {}", encoder_kind.label());

    let token = if opts.lan {
        Some(opts.token.clone().unwrap_or_else(random_pin))
    } else {
        opts.token.clone()
    };
    let listen = opts.listen.unwrap_or_else(|| {
        if opts.lan { SocketAddr::from(([0, 0, 0, 0], LAN_PORT)) } else { SocketAddr::from(([127, 0, 0, 1], USB_PORT)) }
    });

    let config = SenderConfig {
        screen: opts.screen,
        codec: opts.codec.into(),
        fps: opts.fps,
        bitrate_bps: opts.bitrate,
        max_width: opts.max_width,
        max_height: opts.max_height,
    };
    let shared = Arc::new(Shared::new(opts, encoder_kind, config, token, rotation));

    let listener = TcpListener::bind(listen).with_context(|| format!("não foi possível escutar em {listen}"))?;
    match &shared.token {
        Some(pin) => log!("à escuta em {listen} (rede local, PIN {pin})"),
        None => log!("à escuta em {listen}"),
    }

    spawn("encoder", {
        let s = shared.clone();
        move || encoder::supervise(s)
    });
    if !shared.opts.no_audio {
        spawn("audio", {
            let s = shared.clone();
            move || audio::run(s)
        });
    }
    spawn("stats", {
        let s = shared.clone();
        move || stats_loop(s, listen)
    });

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let s = shared.clone();
                spawn("session", move || session::handle_connection(s, stream));
            }
            Err(e) => log!("accept falhou: {e}"),
        }
    }
    Ok(())
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    thread::Builder::new().name(name.into()).spawn(f).expect("criar thread");
}

fn parse_screen(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("formato LARGURAxALTURA, ex.: 2388x1668")?;
    let parse = |v: &str| v.trim().parse::<u32>().ok().filter(|n| (16..=8192).contains(n) && n % 2 == 0);
    match (parse(w), parse(h)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err("dimensões pares entre 16 e 8192".into()),
    }
}

fn random_pin() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{:06}", (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) % 1_000_000)
}

/// A cada segundo: SENDER_STATS para o PC, rotação/descartes simulados e uma linha de estado.
fn stats_loop(shared: Arc<Shared>, listen: SocketAddr) {
    let mut last_frames = 0u64;
    let mut last_bytes = 0u64;
    let mut tick = 0u64;
    loop {
        thread::sleep(Duration::from_secs(1));
        tick += 1;
        let frames = shared.stats.frames.load(Ordering::Relaxed);
        let bytes = shared.stats.video_bytes.load(Ordering::Relaxed);
        let fps = frames - last_frames;
        let bitrate = (bytes - last_bytes) * 8;
        (last_frames, last_bytes) = (frames, bytes);

        if shared.opts.rotate_every.is_some_and(|n| n > 0 && tick.is_multiple_of(n)) {
            let r = (shared.rotation.load(Ordering::Relaxed) + 1) % 4;
            shared.rotation.store(r, Ordering::Relaxed);
            log!("rotação → {}°", Rotation::from_bits(r).degrees());
        }

        let Some(client) = shared.current_client() else {
            log!("{fps} fps · {:.1} Mbps · à espera de ligação em {listen}", bitrate as f64 / 1e6);
            continue;
        };

        if shared.opts.simulate_drops.is_some_and(|n| n > 0 && tick.is_multiple_of(n)) {
            let n = client.simulate_drop();
            log!("descarte simulado: {n} frames na fila, a descartar até ao próximo keyframe");
        }

        let (queue_bytes, queue_ms) = client.queue.video_backlog();
        let target = shared.config.lock().unwrap().bitrate_bps;
        let stats = SenderStats {
            fps_capture: Some(fps as f64),
            fps_encoded: Some(fps as f64),
            bitrate_bps: Some(bitrate),
            target_bitrate_bps: Some(u64::from(target)),
            frames_dropped: Some(client.dropped.load(Ordering::Relaxed)),
            keyframes: Some(shared.stats.keyframes.load(Ordering::Relaxed)),
            send_queue_bytes: Some(queue_bytes as u64),
            send_queue_ms: Some(queue_ms),
            thermal: Some(ThermalState::Nominal),
            low_power_mode: Some(false),
            ..Default::default()
        };
        let msg = Message::from_payload(&stats)
            .with_seq(client.next_seq(MessageType::SenderStats))
            .with_pts(shared.clock.now_us());
        client.queue.push_control(Outgoing::Msg(msg));

        let latency = client
            .last_report
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|r| r.latency_ms_p50)
            .map_or_else(|| "—".to_string(), |ms| format!("{ms:.2} ms"));
        log!(
            "{fps} fps · {:.1} Mbps · fila {queue_ms:.1} ms · drops {} · PC {} · latência p50 no PC {latency}",
            bitrate as f64 / 1e6,
            client.dropped.load(Ordering::Relaxed),
            client.peer,
        );
    }
}
