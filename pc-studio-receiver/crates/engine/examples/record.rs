//! Motor + gravador na linha de comandos: liga-se ao iPad (ou ao fake-sender), mostra a
//! telemetria a cada segundo e grava em MKV.
//!
//! ```text
//! cargo run -p padstudio-engine --example record -- --seconds 10 --out gravacoes
//! cargo run -p padstudio-engine --example record -- --usb --seconds 30 --out gravacoes
//! cargo run -p padstudio-engine --example record -- --tcp 192.168.1.20:23457 --token 123456 --no-record
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use padstudio_engine::recorder::{Recorder, RecorderOptions};
use padstudio_engine::{Endpoint, Engine, EngineConfig, EngineEvent, EngineStatus, Telemetry};

struct Args {
    endpoint: Endpoint,
    seconds: u64,
    out: PathBuf,
    record: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { endpoint: Endpoint::tcp("127.0.0.1:23456"), seconds: 10, out: "gravacoes".into(), record: true };
    let mut token = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("falta o valor de {a}"));
        match a.as_str() {
            "--usb" => args.endpoint = Endpoint::usb(),
            "--udid" => args.endpoint = Endpoint::Usb { udid: Some(value()?) },
            "--tcp" => args.endpoint = Endpoint::tcp(value()?),
            "--token" => token = Some(value()?),
            "--seconds" => args.seconds = value()?.parse().map_err(|_| "--seconds inválido")?,
            "--out" => args.out = value()?.into(),
            "--no-record" => args.record = false,
            other => return Err(format!("opção desconhecida: {other}")),
        }
    }
    if let (Endpoint::Tcp { token: t, .. }, Some(pin)) = (&mut args.endpoint, token) {
        *t = Some(pin);
    }
    Ok(args)
}

fn print_event(ev: &EngineEvent) {
    match ev {
        EngineEvent::Status(EngineStatus::Waiting { message, .. }) => println!("[à espera] {message}"),
        EngineEvent::Status(EngineStatus::Connecting { target }) => println!("[a ligar] {target}"),
        EngineEvent::Status(EngineStatus::Streaming { target, .. }) => println!("[ligado] {target}"),
        EngineEvent::Status(other) => println!("[estado] {other:?}"),
        EngineEvent::Info(m) => println!("[info] {m}"),
        EngineEvent::Warning(m) => println!("[aviso] {m}"),
    }
}

fn ms(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |v| format!("{v:.2}"))
}

fn line(s: u64, t: &Telemetry) -> String {
    format!(
        "{s:3}s │ {} {}x{} rot {}° │ {:5.1} fps {:5.1} Mbps │ latência p50 {} ms (iPad {} + transporte {}) p95 {} │ rtt {} ms │ perdidos {} │ áudio {:.0}/s",
        t.codec.as_deref().unwrap_or("?"),
        t.width,
        t.height,
        t.rotation_deg,
        t.fps,
        t.link_bitrate_bps as f64 / 1e6,
        ms(t.latency_total_p50_ms),
        ms(t.latency_sender_p50_ms),
        ms(t.latency_network_p50_ms),
        ms(t.latency_total_p95_ms),
        ms(t.rtt_ms),
        t.frames_lost,
        t.audio_packets_per_s,
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

    // Espera até 60 s pela ligação.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        while let Ok(ev) = engine.events().try_recv() {
            print_event(&ev);
        }
        if matches!(ctrl.status(), EngineStatus::Streaming { .. }) {
            break;
        }
        if Instant::now() > deadline {
            eprintln!("sem ligação ao fim de 60 s");
            return ExitCode::FAILURE;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let recorder = if args.record {
        match Recorder::start(&ctrl, RecorderOptions::new(&args.out)) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("não foi possível gravar em {}: {e}", args.out.display());
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    for s in 1..=args.seconds {
        thread::sleep(Duration::from_secs(1));
        while let Ok(ev) = engine.events().try_recv() {
            print_event(&ev);
        }
        println!("{}", line(s, &ctrl.telemetry()));
    }

    let mut code = ExitCode::SUCCESS;
    if let Some(r) = recorder {
        match r.stop() {
            Ok(files) => {
                for f in &files {
                    println!(
                        "gravado: {} ({} frames, {} blocos de áudio, {:.2} s, {:.1} MB)",
                        f.path.display(),
                        f.video_frames,
                        f.audio_blocks,
                        f.duration_s,
                        f.bytes as f64 / 1e6
                    );
                }
                if files.is_empty() {
                    println!("nada gravado (não chegou nenhum keyframe?)");
                    code = ExitCode::FAILURE;
                }
            }
            Err(e) => {
                eprintln!("erro na gravação: {e}");
                code = ExitCode::FAILURE;
            }
        }
    }
    engine.shutdown();
    code
}
