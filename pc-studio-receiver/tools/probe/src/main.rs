//! Cliente de teste PSWP. Liga-se ao iPad (ou ao fake-sender), faz o handshake, sincroniza o
//! relógio com PING/PONG e mostra por segundo: fps, bitrate, latência, drops e keyframes.
//!
//! ```text
//! cargo run -p padstudio-probe -- --duration 10 --dump captura.h264    # fake-sender em localhost
//! cargo run -p padstudio-probe -- --usb --wait                         # iPad real por cabo (usbmuxd)
//! ffplay captura.h264
//! ```

use std::fs::File;
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use padstudio_protocol::clock::ClockSync;
use padstudio_protocol::json::{
    Bye, Configure, ConfigureAck, ErrorMsg, Hello, HelloAck, ReceiverReport, SenderStats, StreamConfig, Transport, Want,
};
use padstudio_protocol::payload::{AudioConfig, Codec, Ping, Pong, VideoConfig};
use padstudio_protocol::{Message, MessageType, PROTOCOL_VERSION, SeqTracker, StreamParser, USB_PORT, annexb};
use padstudio_usbmux::UsbmuxError;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CodecArg {
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

#[derive(Debug, Parser)]
#[command(name = "probe", version, about = "Cliente de teste PSWP")]
struct Opts {
    /// Endereço do iPad / fake-sender (ignorado com --usb).
    #[arg(long, default_value = "127.0.0.1:23456")]
    addr: String,

    /// Liga ao iPad pelo cabo, através do usbmuxd.
    #[arg(long)]
    usb: bool,

    /// Com --usb: escolhe o dispositivo pelo UDID (por omissão, o primeiro ligado por USB).
    #[arg(long)]
    udid: Option<String>,

    /// Com --usb: porta no dispositivo.
    #[arg(long, default_value_t = USB_PORT)]
    port: u16,

    /// Com --usb: espera (tentando a cada segundo) até o iPad estar ligado e a transmitir.
    #[arg(long)]
    wait: bool,

    /// PIN (modo rede local).
    #[arg(long)]
    token: Option<String>,

    /// Configuração pedida no HELLO.
    #[arg(long, value_enum)]
    codec: Option<CodecArg>,
    #[arg(long)]
    fps: Option<u32>,
    #[arg(long)]
    bitrate: Option<u64>,
    #[arg(long)]
    max_width: Option<u32>,
    #[arg(long)]
    max_height: Option<u32>,
    /// Repetição do último frame em ecrã parado (0 = desligado; mede o custo em memória, S6).
    #[arg(long)]
    min_fps: Option<u32>,

    /// Grava o vídeo recebido (Anexo-B, abre com ffplay/ffprobe).
    #[arg(long)]
    dump: Option<PathBuf>,

    /// Termina ao fim de N segundos.
    #[arg(long)]
    duration: Option<u64>,

    /// Envia CONFIGURE ao fim de N segundos, com --switch-codec / --switch-fps / --switch-bitrate /
    /// --switch-max-width / --switch-max-height (0 = nativo; compara nativo vs. escalado, spike S2).
    #[arg(long)]
    switch_at: Option<u64>,
    #[arg(long, value_enum)]
    switch_codec: Option<CodecArg>,
    #[arg(long)]
    switch_fps: Option<u32>,
    #[arg(long)]
    switch_bitrate: Option<u64>,
    #[arg(long)]
    switch_max_width: Option<u32>,
    #[arg(long)]
    switch_max_height: Option<u32>,

    /// Envia REQUEST_KEYFRAME ao fim de N segundos.
    #[arg(long)]
    keyframe_at: Option<u64>,

    /// Deixa de ler o socket ao fim de N segundos durante --stall-for segundos: a fila do iPad
    /// enche, o que testa a política de descarte e o pico de memória (spike S6).
    #[arg(long)]
    stall_at: Option<u64>,
    #[arg(long, default_value_t = 2.0)]
    stall_for: f64,
}

/// Relógio local em µs.
struct Clock(Instant);

impl Clock {
    fn now_us(&self) -> u64 {
        self.0.elapsed().as_micros() as u64 + 1
    }
}

#[derive(Default)]
struct Window {
    frames: u64,
    video_bytes: u64,
    audio_packets: u64,
    latency_ms: Vec<f64>,
    sender_ms: Vec<f64>,
}

#[derive(Default)]
struct Totals {
    frames: u64,
    keyframes: u64,
    discontinuities: u64,
    video_bytes: u64,
    audio_packets: u64,
    latency_ms: Vec<f64>,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v
}

fn fmt_ms(v: f64) -> String {
    if v.is_nan() { "—".into() } else { format!("{v:.2} ms") }
}

fn nal_names(codec: Codec, data: &[u8]) -> String {
    annexb::nals(data)
        .map(|n| match (codec, annexb::nal_type(codec, n)) {
            (Codec::H264, 7) | (Codec::Hevc, 33) => "SPS".to_string(),
            (Codec::H264, 8) | (Codec::Hevc, 34) => "PPS".to_string(),
            (Codec::Hevc, 32) => "VPS".to_string(),
            (_, t) => format!("NAL{t}"),
        })
        .collect::<Vec<_>>()
        .join("+")
}

type Link = (Box<dyn Read + Send>, Box<dyn Write + Send>, String);

fn connect_tcp(addr: &str) -> anyhow::Result<Link> {
    let stream = TcpStream::connect(addr)
        .with_context(|| format!("não foi possível ligar a {addr} — o fake-sender está ativo?"))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    let reader = stream.try_clone()?;
    Ok((Box::new(reader), Box::new(stream), addr.to_string()))
}

fn connect_usb(opts: &Opts) -> anyhow::Result<Link> {
    let mut last_error = String::new();
    loop {
        match padstudio_usbmux::connect_usb(opts.udid.as_deref(), opts.port) {
            Ok((device, tunnel)) => {
                tunnel.set_nodelay(true)?;
                tunnel.set_read_timeout(Some(Duration::from_millis(5)))?;
                let writer = tunnel.writer()?;
                let label = format!("{} {} por USB (porta {})", device.kind(), device.udid, opts.port);
                return Ok((Box::new(tunnel), Box::new(writer), label));
            }
            Err(e) if opts.wait && e.is_retryable() => {
                let msg = usb_hint(&e);
                if msg != last_error {
                    println!("{msg} — a tentar de novo a cada segundo…");
                    last_error = msg;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => anyhow::bail!("{}", usb_hint(&e)),
        }
    }
}

/// Explicação para o utilizador dos erros mais comuns por USB.
fn usb_hint(e: &UsbmuxError) -> String {
    match e {
        UsbmuxError::ConnectionRefused { port, .. } => format!(
            "o iPad está ligado, mas a transmissão do PadStudio não está ativa (porta {port} recusada). \
             Inicia a transmissão no iPad ou usa --wait"
        ),
        UsbmuxError::NoDevice(_) => "nenhum iPad ligado por USB (liga o cabo e desbloqueia o iPad)".into(),
        other => other.to_string(),
    }
}

fn main() -> anyhow::Result<ExitCode> {
    let opts = Opts::parse();
    let clock = Clock(Instant::now());

    let (mut reader, mut stream, peer) = if opts.usb { connect_usb(&opts)? } else { connect_tcp(&opts.addr)? };

    let config = StreamConfig {
        codec: opts.codec.map(Into::into),
        max_fps: opts.fps,
        bitrate_bps: opts.bitrate,
        max_width: opts.max_width,
        max_height: opts.max_height,
        min_fps: opts.min_fps,
        ..Default::default()
    };
    let hello = Hello {
        protocol: PROTOCOL_VERSION,
        client: Some("padstudio-probe".into()),
        client_version: Some(env!("CARGO_PKG_VERSION").into()),
        transport: if opts.token.is_some() { Transport::Lan } else { Transport::Usb },
        token: opts.token.clone(),
        want: Some(Want { video: Some(true), audio_app: Some(true), audio_mic: Some(false) }),
        config: (config != StreamConfig::default()).then_some(config),
    };
    Message::from_payload(&hello).with_pts(clock.now_us()).write_to(&mut stream)?;
    println!("ligado a {peer}; HELLO enviado");

    let mut dump = match &opts.dump {
        Some(path) => Some(BufWriter::new(File::create(path).with_context(|| format!("criar {}", path.display()))?)),
        None => None,
    };
    let mut dump_codec: Option<Codec> = None;
    let mut dump_started = false;

    let mut parser = StreamParser::new();
    let mut sync = ClockSync::default();
    let mut video_seq = SeqTracker::default();
    let mut video_config: Option<VideoConfig> = None;
    let mut audio_config: Option<AudioConfig> = None;
    let mut sender_stats: Option<SenderStats> = None;
    let mut rotation = 0u16;
    let mut window = Window::default();
    let mut totals = Totals::default();
    let mut seq = [0u32; 0x40];
    let mut next_seq = |t: MessageType| {
        let s = &mut seq[(t as usize) & 0x3f];
        *s += 1;
        *s - 1
    };

    let start = Instant::now();
    let mut pings_sent = 0u32;
    let mut next_ping = start;
    let mut next_tick = start + Duration::from_secs(1);
    let mut tick = 0u64;
    let mut switched = false;
    let mut keyframe_requested = false;
    let mut bye: Option<Bye> = None;
    let mut notes_seen = std::collections::HashSet::new();
    let stall_start = opts.stall_at.map(|s| start + Duration::from_secs(s));
    let stall_end = stall_start.map(|s| s + Duration::from_secs_f64(opts.stall_for.max(0.0)));
    let mut stall_announced = false;

    'main: loop {
        // Leitura (exceto durante --stall-at/--stall-for).
        let now = Instant::now();
        let stalled = stall_start.is_some_and(|s| now >= s) && stall_end.is_some_and(|e| now < e);
        if stalled {
            if !stall_announced {
                stall_announced = true;
                println!("→ a parar de ler durante {:.1} s (a fila do emissor vai encher)", opts.stall_for);
            }
            std::thread::sleep(Duration::from_millis(5));
        } else {
            match parser.read_from(&mut reader) {
                Ok(0) => {
                    println!("ligação fechada pelo emissor");
                    break;
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e).context("leitura"),
            }
        }
        let recv_us = clock.now_us();

        while let Some(msg) = parser.next_message()? {
            match msg.kind() {
                Some(MessageType::HelloAck) => {
                    let ack: HelloAck = msg.decode_payload()?;
                    let d = ack.device.unwrap_or_default();
                    let c = ack.config.unwrap_or_default();
                    println!(
                        "HELLO_ACK: {} {} · {} ({}) · ecrã {}x{} @{} · config {} {} fps {:.1} Mbps",
                        ack.server.unwrap_or_default(),
                        ack.server_version.unwrap_or_default(),
                        d.model.unwrap_or_default(),
                        d.system.unwrap_or_default(),
                        d.screen_width.unwrap_or(0),
                        d.screen_height.unwrap_or(0),
                        d.max_fps.unwrap_or(0),
                        c.codec.map_or("?", Codec::name),
                        c.max_fps.unwrap_or(0),
                        c.bitrate_bps.unwrap_or(0) as f64 / 1e6,
                    );
                }
                Some(MessageType::VideoConfig) => {
                    let c: VideoConfig = msg.decode_payload()?;
                    println!(
                        "VIDEO_CONFIG #{}: {} {}x{} @{} · {:.1} Mbps · cor {}/{}/{} {} · parâmetros {} ({} bytes)",
                        msg.header.seq,
                        c.codec.name(),
                        c.width,
                        c.height,
                        c.fps_max,
                        f64::from(c.bitrate_bps) / 1e6,
                        c.color_primaries,
                        c.transfer,
                        c.matrix,
                        if c.full_range { "full" } else { "video range" },
                        nal_names(c.codec, &c.param_sets),
                        c.param_sets.len(),
                    );
                    if dump_codec.is_some_and(|d| d != c.codec) && dump.take().is_some() {
                        println!("(o codec mudou: gravação parada para não misturar codecs)");
                    }
                    video_config = Some(c);
                }
                Some(MessageType::VideoFrame) => {
                    let Some(cfg) = &video_config else { continue };
                    let flags = msg.video_flags();
                    let gap = video_seq.observe(msg.header.seq);
                    if gap > 0 {
                        println!("  ! {gap} frames perdidos no emissor (seq {})", msg.header.seq);
                    }
                    if flags.discontinuity {
                        totals.discontinuities += 1;
                    }
                    if flags.keyframe {
                        totals.keyframes += 1;
                    }
                    if flags.rotation.degrees() != rotation {
                        rotation = flags.rotation.degrees();
                        println!("  rotação: {rotation}°");
                    }
                    window.frames += 1;
                    window.video_bytes += msg.payload.len() as u64;
                    if let Some(local_pts) = sync.remote_to_local(msg.header.pts_us) {
                        window.latency_ms.push((recv_us as i64 - local_pts) as f64 / 1000.0);
                    }
                    window.sender_ms.push(f64::from(msg.header.aux) / 1000.0);
                    if let Some(w) = dump.as_mut() {
                        dump_started |= flags.keyframe;
                        if dump_started {
                            w.write_all(&msg.payload)?;
                            dump_codec = Some(cfg.codec);
                        }
                    }
                }
                Some(MessageType::AudioConfig) => {
                    let c: AudioConfig = msg.decode_payload()?;
                    println!(
                        "AUDIO_CONFIG: fonte {} · {:?} · {} canais · {} Hz",
                        c.source.0, c.format, c.channels, c.sample_rate
                    );
                    audio_config = Some(c);
                }
                Some(MessageType::AudioFrame) => {
                    if audio_config.is_some() {
                        window.audio_packets += 1;
                    }
                }
                Some(MessageType::Pong) => {
                    let pong: Pong = msg.decode_payload()?;
                    sync.add(&pong, recv_us);
                }
                Some(MessageType::Ping) => {
                    // O iPad também pode medir o RTT: responder de imediato.
                    let ping: Ping = msg.decode_payload()?;
                    let pong = Pong { t0: ping.t0, t1: recv_us, t2: clock.now_us() };
                    Message::from_payload(&pong).with_seq(next_seq(MessageType::Pong)).write_to(&mut stream)?;
                }
                Some(MessageType::SenderStats) => sender_stats = Some(msg.decode_payload()?),
                Some(MessageType::ConfigureAck) => {
                    let ack: ConfigureAck = msg.decode_payload()?;
                    println!(
                        "CONFIGURE_ACK (pedido #{}): aplicado={} {}",
                        ack.request_seq,
                        ack.applied,
                        ack.message.unwrap_or_default()
                    );
                }
                Some(MessageType::Error) => {
                    let e: ErrorMsg = msg.decode_payload()?;
                    println!("ERROR do emissor: {} {}", e.code, e.message.unwrap_or_default());
                }
                Some(MessageType::Bye) => {
                    let b: Bye = msg.decode_payload()?;
                    println!("BYE: {:?} {}", b.reason, b.message.clone().unwrap_or_default());
                    bye = Some(b);
                    break 'main;
                }
                Some(other) => println!("{} recebido (sem tratamento no probe)", other.name()),
                None => {} // tipo desconhecido: ignorar
            }
        }

        let now = Instant::now();
        let elapsed = now - start;

        // PING: 5 rápidos ao ligar, depois a cada 500 ms.
        if now >= next_ping {
            let ping = Message::from_payload(&Ping { t0: clock.now_us() }).with_seq(next_seq(MessageType::Ping));
            ping.write_to(&mut stream)?;
            pings_sent += 1;
            next_ping = now + if pings_sent < 5 { Duration::from_millis(50) } else { Duration::from_millis(500) };
        }

        if !switched && opts.switch_at.is_some_and(|s| elapsed >= Duration::from_secs(s)) {
            switched = true;
            let cfg = StreamConfig {
                codec: opts.switch_codec.map(Into::into),
                max_fps: opts.switch_fps,
                bitrate_bps: opts.switch_bitrate,
                max_width: opts.switch_max_width,
                max_height: opts.switch_max_height,
                ..Default::default()
            };
            let s = next_seq(MessageType::Configure);
            println!("→ CONFIGURE #{s}: {}", serde_json_like(&cfg));
            Message::from_payload(&Configure(cfg)).with_seq(s).with_pts(clock.now_us()).write_to(&mut stream)?;
        }
        if !keyframe_requested && opts.keyframe_at.is_some_and(|s| elapsed >= Duration::from_secs(s)) {
            keyframe_requested = true;
            println!("→ REQUEST_KEYFRAME");
            Message::new(MessageType::RequestKeyframe, Vec::new())
                .with_seq(next_seq(MessageType::RequestKeyframe))
                .write_to(&mut stream)?;
        }

        // Estatísticas a cada segundo.
        if now >= next_tick {
            next_tick += Duration::from_secs(1);
            tick += 1;
            let w = std::mem::take(&mut window);
            let lat = sorted(w.latency_ms);
            let sender = sorted(w.sender_ms);
            let best = sync.best();
            let stats = sender_stats.as_ref();
            println!(
                "{tick:3}s │ vídeo {} fps {:5.1} Mbps │ latência p50 {} p95 {} │ emissor {} │ fila iPad {} │ drops {} │ áudio {} pkt/s │ rtt {}",
                w.frames,
                w.video_bytes as f64 * 8.0 / 1e6,
                fmt_ms(percentile(&lat, 0.5)),
                fmt_ms(percentile(&lat, 0.95)),
                fmt_ms(percentile(&sender, 0.5)),
                stats.and_then(|s| s.send_queue_ms).map_or_else(|| "—".into(), |ms| format!("{ms:.1} ms")),
                video_seq.lost(),
                w.audio_packets,
                best.map_or_else(|| "—".into(), |b| format!("{:.2} ms", b.rtt_us as f64 / 1000.0)),
            );
            if let Some(line) = stats.and_then(diag_line) {
                println!("      │ {line}");
            }
            if let Some(notes) = stats.and_then(|s| s.diag.as_ref()).and_then(|d| d.notes.as_ref()) {
                for n in notes.iter().filter(|n| notes_seen.insert((*n).clone())) {
                    println!("      │ nota do emissor: {n}");
                }
            }
            let report = ReceiverReport {
                fps_received: Some(w.frames as f64),
                fps_decoded: None,
                recv_bitrate_bps: Some(w.video_bytes * 8),
                latency_ms_p50: Some(percentile(&lat, 0.5)).filter(|v| !v.is_nan()),
                latency_ms_p95: Some(percentile(&lat, 0.95)).filter(|v| !v.is_nan()),
                rtt_ms: best.map(|b| b.rtt_us as f64 / 1000.0),
                decode_errors: Some(0),
            };
            Message::from_payload(&report)
                .with_seq(next_seq(MessageType::ReceiverReport))
                .with_pts(clock.now_us())
                .write_to(&mut stream)?;

            totals.frames += w.frames;
            totals.video_bytes += w.video_bytes;
            totals.audio_packets += w.audio_packets;
            totals.latency_ms.extend(lat);

            if opts.duration.is_some_and(|d| tick >= d) {
                let b = Bye { reason: padstudio_protocol::json::ByeReason::UserStopped, message: Some("fim do teste".into()) };
                let _ = Message::from_payload(&b).with_seq(next_seq(MessageType::Bye)).write_to(&mut stream);
                break;
            }
        }
    }

    if let Some(mut w) = dump.take() {
        w.flush()?;
    }

    let secs = start.elapsed().as_secs_f64();
    let lat = sorted(totals.latency_ms);
    println!(
        "\nResumo: {secs:.1} s · {} frames ({:.1} fps) · {:.1} Mbps · latência p50 {} / p95 {} / máx {} · drops {} · keyframes {} · descontinuidades {} · áudio {} pacotes",
        totals.frames,
        totals.frames as f64 / secs,
        totals.video_bytes as f64 * 8.0 / 1e6 / secs,
        fmt_ms(percentile(&lat, 0.5)),
        fmt_ms(percentile(&lat, 0.95)),
        fmt_ms(lat.last().copied().unwrap_or(f64::NAN)),
        video_seq.lost(),
        totals.keyframes,
        totals.discontinuities,
        totals.audio_packets,
    );
    if let Some(path) = &opts.dump {
        println!("vídeo gravado em {} (ffplay {})", path.display(), path.display());
    }
    if let Some(b) = bye.filter(|b| b.reason != padstudio_protocol::json::ByeReason::UserStopped) {
        println!("terminado pelo emissor: {:?}", b.reason);
    }
    Ok(if totals.frames > 0 { ExitCode::SUCCESS } else { ExitCode::from(2) })
}

/// Uma linha com o diagnóstico dos spikes (S1 captura, S2 encoder, S6 memória), se o emissor o enviar.
fn diag_line(s: &SenderStats) -> Option<String> {
    let d = s.diag.as_ref()?;
    let mut parts = Vec::new();
    let ms = |v: Option<f64>| v.map_or_else(|| "—".to_string(), |v| format!("{v:.1}"));
    if let Some(c) = &d.capture {
        let mut p = format!(
            "S1 captura {} fps · intervalo p50 {} / p95 {} / máx {} ms",
            c.fps.map_or_else(|| "—".to_string(), |f| format!("{f:.1}")),
            ms(c.interval_ms_p50),
            ms(c.interval_ms_p95),
            ms(c.interval_ms_max),
        );
        if let Some(h) = &c.intervals {
            p += &format!(
                " [<6:{} 6-10:{} 10-14:{} 14-20:{} 20-40:{} ≥40:{}]",
                h.lt6, h.from6to10, h.from10to14, h.from14to20, h.from20to40, h.ge40
            );
        }
        if let Some(lag) = c.lag_ms_p50 {
            p += &format!(" · atraso PTS→callback {lag:.1} ms");
        }
        if let Some(r) = c.repeats.filter(|r| *r > 0) {
            p += &format!(" · {r} repetidos");
        }
        if let (Some(w), Some(h)) = (c.width, c.height) {
            p += &format!(" · {w}x{h} {}", c.pixel_format.as_deref().unwrap_or(""));
        }
        if let Some(o) = c.orientation {
            p += &format!(" · orientação EXIF {o} → {}°", c.rotation.unwrap_or(0));
        }
        parts.push(p);
    }
    if let Some(e) = &d.encode {
        parts.push(format!(
            "S2 encoder {} {}x{}{} · {} / p95 {} / máx {} ms · em voo {} · erros {} · descartes VT {}",
            e.codec.as_deref().unwrap_or("?"),
            e.output_width.unwrap_or(0),
            e.output_height.unwrap_or(0),
            if e.scaled == Some(true) {
                format!(" (escalado de {}x{})", e.input_width.unwrap_or(0), e.input_height.unwrap_or(0))
            } else {
                " (nativo)".to_string()
            },
            ms(e.ms_p50),
            ms(e.ms_p95),
            ms(e.ms_max),
            e.in_flight_max.unwrap_or(0),
            e.errors.unwrap_or(0),
            e.encoder_drops.unwrap_or(0),
        ));
    }
    if let Some(m) = &d.memory {
        parts.push(format!(
            "S6 memória {} MB · pico {} · limite {}",
            ms(m.footprint_mb),
            ms(m.peak_mb),
            ms(m.limit_mb)
        ));
    }
    if let Some(cpu) = d.cpu_percent {
        parts.push(format!("CPU {cpu:.0} %"));
    }
    (!parts.is_empty()).then(|| parts.join("\n      │ "))
}

/// Resumo legível de uma configuração (só os campos definidos).
fn serde_json_like(cfg: &StreamConfig) -> String {
    let mut parts = Vec::new();
    if let Some(c) = cfg.codec {
        parts.push(format!("codec={}", c.name()));
    }
    if let Some(f) = cfg.max_fps {
        parts.push(format!("max_fps={f}"));
    }
    if let Some(b) = cfg.bitrate_bps {
        parts.push(format!("bitrate={:.1} Mbps", b as f64 / 1e6));
    }
    if cfg.max_width.is_some() || cfg.max_height.is_some() {
        parts.push(format!("caixa={}x{}", cfg.max_width.unwrap_or(0), cfg.max_height.unwrap_or(0)));
    }
    parts.join(" ")
}
