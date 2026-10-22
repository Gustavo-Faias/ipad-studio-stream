//! Thread do mixer: entradas do bus e do microfone → `MixedAudio` no bus, a cada 10 ms.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, bounded, select, tick, unbounded};
use padstudio_engine::{Controller, MediaEvent, MixedAudio};
use padstudio_protocol::payload::AudioConfig;
use serde::{Deserialize, Serialize};

use crate::InputId;
use crate::convert::{db_to_linear, f32_to_le_bytes, ipad_pcm_to_stereo, linear_to_db};
use crate::core::MixerCore;
use crate::mic::{Clock, ErrorSink, MicCapture, MicChunk, MicInfo, MicProblem};
use crate::track::{CH, OUT_RATE};

const MIN_GAIN_DB: f32 = -60.0;
const MAX_GAIN_DB: f32 = 12.0;
const LEVELS_INTERVAL: Duration = Duration::from_millis(50);
/// Intervalo entre tentativas de reabrir um microfone que falhou.
const MIC_RETRY: Duration = Duration::from_secs(2);

/// Que microfone do PC usar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum MicSelection {
    Default,
    Named(String),
    Off,
}

#[derive(Debug, Clone)]
pub struct MixerConfig {
    /// Margem para as entradas chegarem antes de misturar (jitter buffer). 60 ms cobre o
    /// buffer do WASAPI e o transporte do iPad com folga.
    pub latency_ms: u32,
    pub mic: MicSelection,
    /// Ganho inicial por entrada (índice = `InputId`), em dB.
    pub gains_db: [f32; InputId::COUNT],
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self { latency_ms: 60, mic: MicSelection::Default, gains_db: [0.0; InputId::COUNT] }
    }
}

/// Medidores de uma entrada (para a UI, ~20 Hz).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct InputLevel {
    /// Há áudio a chegar desta entrada.
    pub active: bool,
    /// Pico nos últimos 50 ms, depois do ganho (dBFS).
    pub peak_db: f32,
    pub gain_db: f32,
    pub muted: bool,
    /// Áudio em espera à frente da mistura.
    pub buffered_ms: f64,
    /// Correção de deriva de relógio aplicada.
    pub drift_ppm: f64,
    pub late_frames: u64,
    pub resyncs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MixerLevels {
    pub ipad_app: InputLevel,
    pub ipad_mic: InputLevel,
    pub pc_mic: InputLevel,
    pub master_peak_db: f32,
    /// Blocos de 10 ms já misturados.
    pub blocks: u64,
    pub mic: Option<MicInfo>,
    /// Problema atual do microfone (sem microfone, permissão negada, dispositivo desligado…).
    pub mic_error: Option<String>,
    /// Falhas pontuais de buffer do microfone desde o arranque.
    pub mic_xruns: u64,
}

impl MixerLevels {
    pub fn input(&self, id: InputId) -> &InputLevel {
        match id {
            InputId::IpadApp => &self.ipad_app,
            InputId::IpadMic => &self.ipad_mic,
            InputId::PcMic => &self.pc_mic,
        }
    }

    fn input_mut(&mut self, id: InputId) -> &mut InputLevel {
        match id {
            InputId::IpadApp => &mut self.ipad_app,
            InputId::IpadMic => &mut self.ipad_mic,
            InputId::PcMic => &mut self.pc_mic,
        }
    }
}

struct Shared {
    gain_db: [AtomicU32; InputId::COUNT],
    muted: [AtomicBool; InputId::COUNT],
    levels: Mutex<MixerLevels>,
    mic_error: Mutex<Option<String>>,
    mic_failed: AtomicBool,
    mic_xruns: AtomicU64,
}

impl Shared {
    fn gain_db(&self, id: InputId) -> f32 {
        f32::from_bits(self.gain_db[id as usize].load(Ordering::Relaxed))
    }

    fn linear_gains(&self) -> [f32; InputId::COUNT] {
        InputId::ALL.map(|id| if self.muted[id as usize].load(Ordering::Relaxed) { 0.0 } else { db_to_linear(self.gain_db(id)) })
    }
}

enum Cmd {
    SetMic(MicSelection),
    Stop,
}

/// O mixer: uma thread que subscreve o bus do motor e publica `MediaEvent::MixedAudio`.
pub struct Mixer {
    shared: Arc<Shared>,
    cmd: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Mixer {
    pub fn start(controller: &Controller, config: MixerConfig) -> Self {
        let shared = Arc::new(Shared {
            gain_db: config.gains_db.map(|db| AtomicU32::new(db.clamp(MIN_GAIN_DB, MAX_GAIN_DB).to_bits())),
            muted: Default::default(),
            levels: Mutex::new(MixerLevels::default()),
            mic_error: Mutex::new(None),
            mic_failed: AtomicBool::new(false),
            mic_xruns: AtomicU64::new(0),
        });
        let (cmd_tx, cmd_rx) = unbounded();
        let (ctrl, sh) = (controller.clone(), shared.clone());
        let thread = thread::Builder::new()
            .name("padstudio-mixer".into())
            .spawn(move || run(ctrl, sh, config, cmd_rx))
            .expect("criar a thread do mixer");
        Self { shared, cmd: cmd_tx, thread: Some(thread) }
    }

    /// Ganho de uma entrada, em dB (−60 a +12). Aplica-se no bloco seguinte, com rampa.
    pub fn set_gain_db(&self, input: InputId, db: f32) {
        self.shared.gain_db[input as usize].store(db.clamp(MIN_GAIN_DB, MAX_GAIN_DB).to_bits(), Ordering::Relaxed);
    }

    pub fn gain_db(&self, input: InputId) -> f32 {
        self.shared.gain_db(input)
    }

    pub fn set_muted(&self, input: InputId, muted: bool) {
        self.shared.muted[input as usize].store(muted, Ordering::Relaxed);
    }

    pub fn is_muted(&self, input: InputId) -> bool {
        self.shared.muted[input as usize].load(Ordering::Relaxed)
    }

    /// Troca (ou desliga) o microfone do PC.
    pub fn set_mic(&self, selection: MicSelection) {
        let _ = self.cmd.send(Cmd::SetMic(selection));
    }

    /// Medidores atuais (atualizados a cada 50 ms).
    pub fn levels(&self) -> MixerLevels {
        self.shared.levels.lock().unwrap().clone()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn ipad_input(source: u8) -> Option<InputId> {
    match source {
        0 => Some(InputId::IpadApp),
        1 => Some(InputId::IpadMic),
        _ => None,
    }
}

fn start_mic(selection: &MicSelection, ctrl: &Controller, shared: &Arc<Shared>, tx: &Sender<MicChunk>) -> Option<MicCapture> {
    *shared.mic_error.lock().unwrap() = None;
    shared.mic_failed.store(false, Ordering::SeqCst);
    if *selection == MicSelection::Off {
        return None;
    }
    let clock: Clock = {
        let c = ctrl.clone();
        Arc::new(move || c.now_us() as i64)
    };
    let on_error: ErrorSink = {
        let s = shared.clone();
        Arc::new(move |problem| match problem {
            MicProblem::Xrun => {
                s.mic_xruns.fetch_add(1, Ordering::Relaxed);
            }
            MicProblem::Fatal(e) => {
                *s.mic_error.lock().unwrap() = Some(e);
                s.mic_failed.store(true, Ordering::SeqCst);
            }
        })
    };
    match MicCapture::start(selection, clock, on_error, tx.clone()) {
        Ok(m) => Some(m),
        Err(e) => {
            *shared.mic_error.lock().unwrap() = Some(e);
            shared.mic_failed.store(true, Ordering::SeqCst);
            None
        }
    }
}

fn run(ctrl: Controller, shared: Arc<Shared>, config: MixerConfig, cmd_rx: Receiver<Cmd>) {
    // Só áudio: subscreve o bus diretamente (sem pedir keyframes como um consumidor de vídeo).
    let sub = ctrl.bus().subscribe(1024);
    let mut audio_cfg: [Option<AudioConfig>; 2] = [None, None];
    for (source, cfg) in ctrl.media_state().audio {
        if let Some(slot) = audio_cfg.get_mut(usize::from(source)) {
            *slot = Some(cfg);
        }
    }

    let now = || ctrl.now_us() as i64;
    let mut core = MixerCore::new(now(), config.latency_ms);
    let (mic_tx, mic_rx) = bounded::<MicChunk>(64);
    let mut mic_selection = config.mic.clone();
    let mut mic = start_mic(&mic_selection, &ctrl, &shared, &mic_tx);
    let mut mic_attempt = Instant::now();
    let ticker = tick(Duration::from_millis(5));
    let mut scratch = Vec::new();
    let mut next_levels = Instant::now() + LEVELS_INTERVAL;

    loop {
        select! {
            recv(sub.receiver()) -> ev => match ev {
                Ok(MediaEvent::AudioConfig(c)) => {
                    if let Some(slot) = audio_cfg.get_mut(usize::from(c.source.0)) {
                        *slot = Some(c);
                    }
                }
                Ok(MediaEvent::Audio(p)) => {
                    let source = p.source.0;
                    if let (Some(id), Some(Some(cfg))) = (ipad_input(source), audio_cfg.get(usize::from(source))) {
                        ipad_pcm_to_stereo(cfg, &p.data, &mut scratch);
                        // Instante de captura no iPad → relógio local; antes da 1.ª medição, a chegada.
                        let t = ctrl.remote_to_local(p.pts_us).unwrap_or(p.recv_us as i64);
                        core.push(id, cfg.sample_rate, &scratch, t);
                    }
                }
                Ok(MediaEvent::SessionStarted(_) | MediaEvent::SessionEnded { .. }) => {
                    core.remove(InputId::IpadApp);
                    core.remove(InputId::IpadMic);
                    audio_cfg = [None, None];
                }
                Ok(_) => {}
                Err(_) => break, // o motor terminou
            },
            recv(mic_rx) -> chunk => {
                if let Ok(c) = chunk {
                    core.push(InputId::PcMic, c.rate, &c.samples, c.t_us);
                }
            },
            recv(cmd_rx) -> cmd => match cmd {
                Ok(Cmd::SetMic(selection)) => {
                    drop(mic.take()); // pára a captura anterior antes de abrir outra
                    core.remove(InputId::PcMic);
                    mic_selection = selection;
                    mic = start_mic(&mic_selection, &ctrl, &shared, &mic_tx);
                    mic_attempt = Instant::now();
                }
                Ok(Cmd::Stop) | Err(_) => break,
            },
            recv(ticker) -> _ => {}
        }

        // Microfone que falhou (desligado, sem permissão): volta a tentar de 2 em 2 s.
        if shared.mic_failed.load(Ordering::SeqCst) && mic_selection != MicSelection::Off && mic_attempt.elapsed() >= MIC_RETRY {
            drop(mic.take());
            core.remove(InputId::PcMic);
            mic = start_mic(&mic_selection, &ctrl, &shared, &mic_tx);
            mic_attempt = Instant::now();
        }

        for block in core.render_ready(now(), &shared.linear_gains()) {
            let frames = block.samples.len() / CH;
            ctrl.bus().publish(MediaEvent::MixedAudio(MixedAudio {
                pts_local_us: block.pts_local_us.max(0) as u64,
                sample_rate: OUT_RATE,
                channels: CH as u8,
                frames,
                data: Bytes::from(f32_to_le_bytes(&block.samples)),
            }));
        }

        if Instant::now() >= next_levels {
            next_levels = Instant::now() + LEVELS_INTERVAL;
            let mut levels = MixerLevels { blocks: core.blocks, ..Default::default() };
            for id in InputId::ALL {
                let level = levels.input_mut(id);
                level.gain_db = shared.gain_db(id);
                level.muted = shared.muted[id as usize].load(Ordering::Relaxed);
                level.peak_db = -120.0;
                if let Some((stats, buffered_ms, drift_ppm)) = core.track_stats(id) {
                    level.active = buffered_ms > 0.0 || stats.peak > 0.0;
                    level.peak_db = linear_to_db(stats.peak);
                    level.buffered_ms = buffered_ms;
                    level.drift_ppm = drift_ppm;
                    level.late_frames = stats.late_frames;
                    level.resyncs = stats.resyncs;
                }
            }
            levels.master_peak_db = linear_to_db(core.take_master_peak());
            levels.mic = mic.as_ref().map(|m| m.info.clone());
            levels.mic_error = shared.mic_error.lock().unwrap().clone();
            levels.mic_xruns = shared.mic_xruns.load(Ordering::Relaxed);
            *shared.levels.lock().unwrap() = levels;
        }
    }
    drop(mic);
}
