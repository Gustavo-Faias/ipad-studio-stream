//! Multi-streaming na linha de comandos: motor + mixer + um FFmpeg por destino.
//!
//! ```text
//! cargo run -p padstudio-streaming --example stream -- --usb --twitch-key live_xxx --youtube-key yyyy
//! cargo run -p padstudio-streaming --example stream -- --tiktok-server rtmp://… --tiktok-key zzz --mic default
//! cargo run -p padstudio-streaming --example stream -- --twitch-url rtmp://127.0.0.1:19351/live/teste --seconds 20
//! ```
//!
//! As chaves nunca são impressas: o estado mostra só o servidor.

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use padstudio_audio::{MicSelection, Mixer, MixerConfig};
use padstudio_engine::{Endpoint, Engine, EngineConfig, EngineEvent, EngineStatus};
use padstudio_streaming::{Destination, EncoderKind, Layout, OutputStatus, StreamManager, StreamOptions};

struct Args {
    endpoint: Endpoint,
    seconds: u64,
    destinations: Vec<Destination>,
    mic: MicSelection,
    encoder: Option<EncoderKind>,
}

fn parse_args() -> Result<Args, String> {
    let mut endpoint = Endpoint::tcp("127.0.0.1:23456");
    let mut seconds = 30;
    let (mut twitch, mut youtube, mut tiktok_url) = (None, None, None);
    let (mut tiktok_server, mut tiktok_key) = (None, None);
    let mut tiktok_vertical = true;
    let mut layout = None;
    let mut mic = MicSelection::Off;
    let mut encoder = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("falta o valor de {arg}"));
        match arg.as_str() {
            "--usb" => endpoint = Endpoint::usb(),
            "--tcp" => endpoint = Endpoint::tcp(value()?),
            "--seconds" => seconds = value()?.parse().map_err(|_| "--seconds inválido")?,
            "--twitch-key" => twitch = Some(Destination::twitch(&value()?)),
            "--twitch-url" => twitch = Some(with_url(Destination::twitch(""), value()?)),
            "--youtube-key" => youtube = Some(Destination::youtube(&value()?)),
            "--youtube-url" => youtube = Some(with_url(Destination::youtube(""), value()?)),
            "--tiktok-server" => tiktok_server = Some(value()?),
            "--tiktok-key" => tiktok_key = Some(value()?),
            "--tiktok-url" => tiktok_url = Some(value()?),
            "--tiktok-horizontal" => tiktok_vertical = false,
            "--layout" => {
                layout = Some(match value()?.as_str() {
                    "fit-blur" => Layout::FitBlur,
                    "fit-black" => Layout::FitBlack,
                    "crop" => Layout::Crop,
                    other => return Err(format!("layout desconhecido: {other}")),
                })
            }
            "--mic" => {
                mic = match value()?.as_str() {
                    "default" => MicSelection::Default,
                    "off" => MicSelection::Off,
                    name => MicSelection::Named(name.into()),
                }
            }
            "--encoder" => {
                encoder = match value()?.as_str() {
                    "auto" => None,
                    "nvenc" => Some(EncoderKind::Nvenc),
                    "amf" => Some(EncoderKind::Amf),
                    "qsv" => Some(EncoderKind::Qsv),
                    "videotoolbox" => Some(EncoderKind::VideoToolbox),
                    "software" => Some(EncoderKind::Software),
                    other => return Err(format!("encoder desconhecido: {other}")),
                }
            }
            other => return Err(format!("opção desconhecida: {other}")),
        }
    }
    let tiktok = match (tiktok_url, tiktok_server, tiktok_key) {
        (Some(url), _, _) => Some(with_url(Destination::tiktok("", "", tiktok_vertical), url)),
        (None, Some(server), Some(key)) => Some(Destination::tiktok(&server, &key, tiktok_vertical)),
        (None, None, None) => None,
        _ => return Err("o TikTok precisa de --tiktok-server e --tiktok-key (ou --tiktok-url)".into()),
    };
    let mut destinations: Vec<Destination> = [twitch, youtube, tiktok].into_iter().flatten().collect();
    if destinations.is_empty() {
        return Err("indica pelo menos um destino (--twitch-key, --youtube-key, --tiktok-…)".into());
    }
    if let Some(l) = layout {
        for d in &mut destinations {
            d.video.layout = l;
        }
    }
    Ok(Args { endpoint, seconds, destinations, mic, encoder })
}

fn with_url(mut d: Destination, url: String) -> Destination {
    d.url = url;
    d
}

fn line(s: &OutputStatus) -> String {
    let state = format!("{:?}", s.state).to_lowercase();
    let retry = s.retry_in_s.map(|r| format!(" (nova tentativa em {r} s)")).unwrap_or_default();
    let note = s.last_error.as_deref().or(s.last_message.as_deref()).map(|m| format!(" · {m}")).unwrap_or_default();
    format!(
        "  {:<8} {:<13} {:5.1} fps {:6.0} kbps {:4.2}x · dup {} drop {} · silêncio {}{retry}{note}",
        s.name, state, s.fps, s.bitrate_kbps, s.speed, s.dup_frames, s.drop_frames, s.silence_blocks
    )
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    let engine = Engine::start(EngineConfig { endpoint: args.endpoint.clone(), ..Default::default() });
    let ctrl = engine.controller();
    let manager = match StreamManager::new(&ctrl, StreamOptions { encoder: args.encoder, ..Default::default() }) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("encoder: {}", manager.encoder().label());
    let mixer = Mixer::start(&ctrl, MixerConfig { mic: args.mic.clone(), ..Default::default() });

    for d in &args.destinations {
        println!(
            "destino {}: {} · {}x{} @ {} fps · {} kbps · {:?}",
            d.name,
            d.redacted_url(),
            d.video.width,
            d.video.height,
            d.video.fps,
            d.video.bitrate_kbps,
            d.video.layout
        );
        manager.start(d.clone());
    }

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(args.seconds) {
        thread::sleep(Duration::from_secs(1));
        while let Ok(ev) = engine.events().try_recv() {
            match ev {
                EngineEvent::Status(EngineStatus::Waiting { message, .. }) => println!("[iPad] {message}"),
                EngineEvent::Status(EngineStatus::Streaming { target, .. }) => println!("[iPad] ligado: {target}"),
                EngineEvent::Info(m) => println!("[info] {m}"),
                EngineEvent::Warning(m) => println!("[aviso] {m}"),
                _ => {}
            }
        }
        let t = ctrl.telemetry();
        println!(
            "{:4.0}s · iPad {} {:.0} fps rot {}° · mistura {:.1} dB",
            start.elapsed().as_secs_f64(),
            if t.connected { "ligado" } else { "desligado" },
            t.fps,
            t.rotation_deg,
            mixer.levels().master_peak_db
        );
        for s in manager.statuses() {
            println!("{}", line(&s));
        }
    }

    println!("a terminar…");
    manager.stop_all();
    for s in manager.statuses() {
        println!("{}", line(&s));
    }
    mixer.stop();
    engine.shutdown();
    ExitCode::SUCCESS
}
