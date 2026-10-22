//! Mixer na linha de comandos: liga-se ao iPad (ou ao fake-sender), mistura com o microfone do
//! PC e mostra os medidores. Com `--selftest` verifica falhas de áudio, ganhos e o microfone.
//!
//! ```text
//! cargo run -p padstudio-audio --example mixer -- --list-mics
//! cargo run -p padstudio-audio --example mixer -- --seconds 10
//! cargo run -p padstudio-audio --example mixer -- --usb --mic "Microfone (Realtek)" --wav mistura.wav
//! cargo run -p padstudio-audio --example mixer -- --selftest
//! ```

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use padstudio_audio::{InputId, InputLevel, MicSelection, Mixer, MixerConfig, MixerLevels, default_input_device, input_devices, linear_to_db};
use padstudio_engine::{Endpoint, Engine, EngineConfig, EngineEvent, EngineStatus, MediaEvent, Subscription};

struct Args {
    endpoint: Endpoint,
    seconds: u64,
    mic: MicSelection,
    wav: Option<PathBuf>,
    list_mics: bool,
    selftest: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        endpoint: Endpoint::tcp("127.0.0.1:23456"),
        seconds: 10,
        mic: MicSelection::Default,
        wav: None,
        list_mics: false,
        selftest: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("falta o valor de {arg}"));
        match arg.as_str() {
            "--usb" => a.endpoint = Endpoint::usb(),
            "--tcp" => a.endpoint = Endpoint::tcp(value()?),
            "--seconds" => a.seconds = value()?.parse().map_err(|_| "--seconds inválido")?,
            "--mic" => {
                a.mic = match value()?.as_str() {
                    "default" => MicSelection::Default,
                    "off" => MicSelection::Off,
                    name => MicSelection::Named(name.to_string()),
                }
            }
            "--wav" => a.wav = Some(value()?.into()),
            "--list-mics" => a.list_mics = true,
            "--selftest" => a.selftest = true,
            other => return Err(format!("opção desconhecida: {other}")),
        }
    }
    Ok(a)
}

/// WAV f32 estéreo 48 kHz; os tamanhos são escritos no fim.
struct WavWriter {
    out: BufWriter<File>,
    data_bytes: u32,
}

impl WavWriter {
    fn create(path: &PathBuf) -> std::io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        let (rate, ch, bits) = (48_000u32, 2u16, 32u16);
        out.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
        out.write_all(&16u32.to_le_bytes())?;
        out.write_all(&3u16.to_le_bytes())?; // IEEE float
        out.write_all(&ch.to_le_bytes())?;
        out.write_all(&rate.to_le_bytes())?;
        out.write_all(&(rate * u32::from(ch) * u32::from(bits) / 8).to_le_bytes())?;
        out.write_all(&(ch * bits / 8).to_le_bytes())?;
        out.write_all(&bits.to_le_bytes())?;
        out.write_all(b"data\0\0\0\0")?;
        Ok(Self { out, data_bytes: 0 })
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.data_bytes += data.len() as u32;
        self.out.write_all(data)
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.out.seek(SeekFrom::Start(4))?;
        self.out.write_all(&(36 + self.data_bytes).to_le_bytes())?;
        self.out.seek(SeekFrom::Start(40))?;
        self.out.write_all(&self.data_bytes.to_le_bytes())?;
        self.out.flush()
    }
}

fn level(l: &InputLevel) -> String {
    if !l.active {
        return "—".into();
    }
    let mute = if l.muted { " [mudo]" } else { "" };
    format!("{:6.1} dB (ganho {:+.0}, fila {:3.0} ms, deriva {:+4.0} ppm){mute}", l.peak_db, l.gain_db, l.buffered_ms, l.drift_ppm)
}

fn print_levels(t: f64, l: &MixerLevels, blocks_per_s: f64) {
    println!(
        "{t:5.1}s │ jogo {} │ mic PC {} │ mistura {:6.1} dB │ {blocks_per_s:3.0} blocos/s",
        level(&l.ipad_app),
        level(&l.pc_mic),
        l.master_peak_db,
    );
}

/// Picos (lineares) dos blocos de `MixedAudio` recebidos; grava-os no WAV, se pedido.
fn drain_blocks(sub: &Subscription, wav: &mut Option<WavWriter>) -> Vec<f32> {
    let mut peaks = Vec::new();
    while let Some(ev) = sub.try_recv() {
        if let MediaEvent::MixedAudio(m) = ev {
            let peak = m.data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs()).fold(0f32, f32::max);
            peaks.push(peak);
            if let Some(w) = wav.as_mut() {
                let _ = w.write(&m.data);
            }
        }
    }
    peaks
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    if args.list_mics {
        let default = default_input_device();
        match input_devices() {
            Ok(list) if list.is_empty() => println!("nenhum microfone encontrado"),
            Ok(list) => {
                for name in list {
                    let mark = if Some(&name) == default.as_ref() { " (predefinido)" } else { "" };
                    println!("  {name}{mark}");
                }
            }
            Err(e) => eprintln!("{e}"),
        }
        return ExitCode::SUCCESS;
    }

    let engine = Engine::start(EngineConfig { endpoint: args.endpoint.clone(), ..Default::default() });
    let ctrl = engine.controller();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(ctrl.status(), EngineStatus::Streaming { .. }) {
        while let Ok(ev) = engine.events().try_recv() {
            if let EngineEvent::Status(EngineStatus::Waiting { message, .. }) = ev {
                println!("[à espera] {message}");
            }
        }
        if Instant::now() > deadline {
            eprintln!("sem ligação ao iPad/fake-sender ao fim de 30 s");
            return ExitCode::FAILURE;
        }
        thread::sleep(Duration::from_millis(100));
    }
    println!("ligado; a iniciar o mixer…");

    let sub = ctrl.bus().subscribe(8192);
    let mixer = Mixer::start(&ctrl, MixerConfig { mic: args.mic.clone(), ..Default::default() });
    let mut wav = match &args.wav {
        Some(p) => match WavWriter::create(p) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("não foi possível criar {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let seconds = if args.selftest { 8 } else { args.seconds };
    let start = Instant::now();
    let mut phase_a = Vec::new(); // 0,5–3 s: só o iPad, 0 dB
    let mut phase_c = Vec::new(); // 5,5–8 s: só o iPad, −12 dB
    let mut mic_peak_db = -120f32;
    let mut mic_delivering = false;
    let mut blocks_total = 0usize;
    if args.selftest {
        mixer.set_muted(InputId::PcMic, true);
    }
    while start.elapsed() < Duration::from_secs(seconds) {
        thread::sleep(Duration::from_millis(500));
        let t = start.elapsed().as_secs_f64();
        let peaks = drain_blocks(&sub, &mut wav);
        blocks_total += peaks.len();
        let levels = mixer.levels();
        print_levels(t, &levels, peaks.len() as f64 * 2.0);
        if args.selftest {
            match t {
                t if (0.9..3.1).contains(&t) => phase_a.extend(peaks),
                t if (3.4..5.1).contains(&t) => {
                    mic_peak_db = mic_peak_db.max(levels.pc_mic.peak_db);
                    mic_delivering |= levels.pc_mic.active && levels.pc_mic.buffered_ms > 0.0;
                }
                t if (5.9..8.1).contains(&t) => phase_c.extend(peaks),
                _ => {}
            }
            if (2.9..3.1).contains(&t) {
                println!("        → microfone do PC ligado");
                mixer.set_muted(InputId::PcMic, false);
            }
            if (4.9..5.1).contains(&t) {
                println!("        → microfone desligado, jogo a −12 dB");
                mixer.set_muted(InputId::PcMic, true);
                mixer.set_gain_db(InputId::IpadApp, -12.0);
            }
        }
    }

    let levels = mixer.levels();
    if let Some(info) = &levels.mic {
        println!("microfone: {} · {} Hz · {} canais · {}", info.device, info.sample_rate, info.channels, info.format);
    }
    if let Some(e) = &levels.mic_error {
        println!("microfone: {e}");
    }
    if levels.mic_xruns > 0 {
        println!("microfone: {} falha(s) pontual(is) de buffer (xrun)", levels.mic_xruns);
    }
    println!("blocos misturados: {blocks_total} em {:.1} s", start.elapsed().as_secs_f64());
    if let Some(w) = wav.take() {
        let _ = w.finish();
        println!("mistura gravada em {}", args.wav.as_ref().unwrap().display());
    }

    let mut code = ExitCode::SUCCESS;
    if args.selftest {
        let dropouts = phase_a.iter().filter(|p| **p < 0.05).count();
        let (a, c) = (median(phase_a.clone()), median(phase_c));
        let gain_change = linear_to_db(c) - linear_to_db(a);
        let checks = [
            (format!("sem falhas de áudio ({dropouts} blocos em silêncio em {})", phase_a.len()), dropouts == 0 && phase_a.len() > 200),
            (format!("nível do jogo {:.1} dBFS (o fake-sender gera um tom a −20 dBFS)", linear_to_db(a)), (-22.0..-18.0).contains(&linear_to_db(a))),
            (format!("ganho −12 dB aplicado: variação medida {gain_change:+.1} dB"), (-13.0..-11.0).contains(&gain_change)),
            // Com a sala em silêncio, a supressão de ruído do Windows pode entregar zeros exatos:
            // o que se verifica é que o áudio chega; o pico é informativo.
            (format!("microfone do PC a entregar áudio (pico {mic_peak_db:.1} dBFS)"), levels.mic.is_some() && mic_delivering),
            ("microfone sem erros persistentes".to_string(), levels.mic_error.is_none()),
        ];
        println!("\nautoteste:");
        for (label, ok) in &checks {
            println!("  [{}] {label}", if *ok { "ok" } else { "FALHA" });
            if !ok {
                code = ExitCode::FAILURE;
            }
        }
    }
    mixer.stop();
    engine.shutdown();
    code
}
