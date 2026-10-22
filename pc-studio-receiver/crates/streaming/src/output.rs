//! Um destino: uma thread que alimenta um processo FFmpeg e o supervisiona.
//!
//! ```text
//!  Bus ─► Video / MixedAudio ─► Interleaver ─► Matroska live ─► stdin do FFmpeg ─► RTMP
//!                                                           stderr ◄─ progresso / erros
//! ```
//!
//! - Arranca no primeiro keyframe (pede um ao iPad).
//! - Rotação, codec ou resolução diferentes → reinicia o FFmpeg no keyframe seguinte.
//! - iPad desligado → modo de espera: repete o último keyframe a 30 fps e mantém o áudio da
//!   mistura (o microfone continua), para a plataforma não cortar a live. Ao fim de 5 min, pára.
//! - FFmpeg terminou (rede, chave errada) → volta a tentar com espera crescente (2 s … 30 s).

use std::io::{self, BufRead, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, bounded, select};
use padstudio_engine::mkv::{AudioTrack, MkvWriter, Mode, Track, VideoTrack};
use padstudio_engine::{Controller, MediaEvent, VideoPacket};
use padstudio_protocol::payload::{AudioFormat, Codec, Rotation, VideoConfig};
use serde::Serialize;

use crate::encoder::{EncoderKind, no_window};
use crate::interleave::{AUDIO_RATE, Interleaver, Item};
use crate::profile::{Destination, ffmpeg_args, redact_line};
use crate::progress::{ProgressParser, is_progress_line};

/// Quanto tempo o vídeo espera pela mistura de áudio antes de seguir com silêncio.
const HOLD_MAX_US: i64 = 250_000;
const STANDBY_FRAME: Duration = Duration::from_millis(33);
const STANDBY_MAX: Duration = Duration::from_secs(300);
/// Keyframes periódicos do iPad enquanto em direto (imagem de espera recente, recuperação rápida).
const KEYFRAME_REFRESH: Duration = Duration::from_secs(2);
const KEYFRAME_RETRY: Duration = Duration::from_secs(1);
const STABLE_AFTER: Duration = Duration::from_secs(60);
const MAX_BACKOFF_S: u64 = 30;
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Avisos do FFmpeg sem interesse para o utilizador.
const BENIGN_MESSAGES: &[&str] = &["Guessed Channel Layout"];
/// Fila de eventos por destino: ~2 s de vídeo a 120 fps + áudio. Um FFmpeg mais lento do que o
/// tempo real fica, no máximo, este tanto atrás; depois perde pacotes e recomeça num keyframe.
const QUEUE_EVENTS: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    /// À espera de vídeo do iPad (e de um keyframe).
    WaitingVideo,
    /// FFmpeg arrancado, ainda sem frames codificados (a ligar ao servidor).
    Starting,
    Live,
    /// iPad desligado: imagem congelada + áudio da mistura.
    Standby,
    /// O FFmpeg terminou; nova tentativa em breve.
    Reconnecting,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputStatus {
    pub id: String,
    pub name: String,
    /// URL sem a chave.
    pub target: String,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps_target: u32,
    pub bitrate_target_kbps: u32,
    pub state: OutputState,
    pub fps: f64,
    pub bitrate_kbps: f64,
    /// Velocidade de codificação (1,0 = tempo real; abaixo disso o PC não está a conseguir).
    pub speed: f64,
    pub dup_frames: u64,
    pub drop_frames: u64,
    pub out_time_s: f64,
    pub restarts: u32,
    pub retry_in_s: Option<u64>,
    /// Blocos de 10 ms de silêncio inseridos por falta de áudio.
    pub silence_blocks: u64,
    /// Último aviso/erro do FFmpeg (sem a chave).
    pub last_message: Option<String>,
    /// Último problema do lado do PadStudio (ex.: porque reiniciou).
    pub last_error: Option<String>,
}

impl OutputStatus {
    fn new(dest: &Destination, encoder: EncoderKind) -> Self {
        Self {
            id: dest.id.clone(),
            name: dest.name.clone(),
            target: dest.redacted_url(),
            encoder: encoder.label().to_string(),
            width: dest.video.width,
            height: dest.video.height,
            fps_target: dest.video.fps,
            bitrate_target_kbps: dest.video.bitrate_kbps,
            state: OutputState::WaitingVideo,
            fps: 0.0,
            bitrate_kbps: 0.0,
            speed: 0.0,
            dup_frames: 0,
            drop_frames: 0,
            out_time_s: 0.0,
            restarts: 0,
            retry_in_s: None,
            silence_blocks: 0,
            last_message: None,
            last_error: None,
        }
    }

    fn clear_rates(&mut self) {
        self.fps = 0.0;
        self.bitrate_kbps = 0.0;
        self.speed = 0.0;
    }
}

type SharedChild = Arc<Mutex<Option<Child>>>;

/// Handle de um destino ativo.
pub(crate) struct OutputHandle {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
    child: SharedChild,
    status: Arc<Mutex<OutputStatus>>,
}

impl OutputHandle {
    pub fn spawn(ctrl: Controller, dest: Destination, encoder: EncoderKind, ffmpeg: PathBuf) -> Self {
        let status = Arc::new(Mutex::new(OutputStatus::new(&dest, encoder)));
        let child: SharedChild = Arc::new(Mutex::new(None));
        let (stop_tx, stop_rx) = bounded(1);
        let name = format!("padstudio-out-{}", dest.id);
        let worker = Worker {
            ctrl,
            dest,
            encoder,
            ffmpeg,
            status: status.clone(),
            child: child.clone(),
            video: None,
            session_active: false,
            run: None,
            wait_key: true,
            restart_at: None,
            restarts: 0,
            last_key: None,
            standby_since: None,
            standby_anchor_ts: 0,
            last_video_ts: 0,
            next_standby_frame: Instant::now(),
            last_kf_request: None,
            items: Vec::new(),
        };
        let thread = thread::Builder::new().name(name).spawn(move || worker.run(stop_rx)).expect("criar a thread do destino");
        Self { stop: stop_tx, thread: Some(thread), child, status }
    }

    pub fn status(&self) -> OutputStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn request_stop(&self) {
        let _ = self.stop.try_send(());
    }

    /// Espera pela thread; se um FFmpeg encravado a bloquear, mata-o.
    pub fn join(&mut self) {
        let Some(t) = self.thread.take() else { return };
        let deadline = Instant::now() + STOP_GRACE + Duration::from_secs(2);
        while !t.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if !t.is_finished()
            && let Some(c) = self.child.lock().unwrap().as_mut() {
                let _ = c.kill();
            }
        let _ = t.join();
    }
}

impl Drop for OutputHandle {
    fn drop(&mut self) {
        self.request_stop();
        self.join();
    }
}

struct Running {
    writer: MkvWriter<BufWriter<ChildStdin>>,
    il: Interleaver,
    /// Instante local que corresponde a t = 0 no Matroska.
    origin_local: i64,
    rotation: Rotation,
    codec: Codec,
    width: u16,
    height: u16,
    stderr: Option<JoinHandle<()>>,
    started: Instant,
}

struct Worker {
    ctrl: Controller,
    dest: Destination,
    encoder: EncoderKind,
    ffmpeg: PathBuf,
    status: Arc<Mutex<OutputStatus>>,
    child: SharedChild,
    video: Option<(VideoConfig, u64)>,
    session_active: bool,
    run: Option<Running>,
    /// Só aceitar vídeo a partir do próximo keyframe (início, perda de pacotes, retoma).
    wait_key: bool,
    restart_at: Option<Instant>,
    restarts: u32,
    last_key: Option<Bytes>,
    standby_since: Option<Instant>,
    /// Timestamp do último frame real quando o iPad se desligou: a imagem de espera continua a
    /// linha temporal a partir daqui (e não da hora atual, que pode estar à frente se este
    /// destino estiver atrasado).
    standby_anchor_ts: i64,
    last_video_ts: i64,
    next_standby_frame: Instant,
    last_kf_request: Option<Instant>,
    items: Vec<Item>,
}

impl Worker {
    fn status(&self) -> std::sync::MutexGuard<'_, OutputStatus> {
        self.status.lock().unwrap()
    }

    fn set_state(&self, state: OutputState) {
        self.status().state = state;
    }

    fn request_keyframe(&mut self, force: bool) {
        if force || self.last_kf_request.is_none_or(|t| t.elapsed() >= KEYFRAME_RETRY) {
            self.ctrl.request_keyframe();
            self.last_kf_request = Some(Instant::now());
        }
    }

    fn restart_due(&self) -> bool {
        self.restart_at.is_none_or(|t| Instant::now() >= t)
    }

    fn run(mut self, stop_rx: Receiver<()>) {
        let (sub, state) = self.ctrl.subscribe(QUEUE_EVENTS);
        self.video = state.video;
        self.session_active = state.session.is_some();
        loop {
            select! {
                recv(stop_rx) -> _ => break,
                recv(sub.receiver()) -> ev => match ev {
                    Ok(ev) => self.on_event(ev),
                    Err(_) => break,
                },
                default(Duration::from_millis(10)) => {}
            }
            self.tick();
        }
        self.stop_process(true);
        let mut s = self.status();
        s.state = OutputState::Stopped;
        s.retry_in_s = None;
        s.clear_rates();
    }

    fn on_event(&mut self, ev: MediaEvent) {
        match ev {
            MediaEvent::SessionStarted(_) => {
                self.session_active = true;
                self.wait_key = true; // retoma (ou começa) no próximo keyframe
                self.request_keyframe(true);
            }
            MediaEvent::SessionEnded { .. } => {
                self.session_active = false;
                if self.run.is_some() {
                    self.standby_since = Some(Instant::now());
                    self.standby_anchor_ts = self.last_video_ts;
                    self.next_standby_frame = Instant::now() + STANDBY_FRAME;
                    self.set_state(OutputState::Standby);
                }
            }
            MediaEvent::VideoConfig { config, id } => self.video = Some((config, id)),
            MediaEvent::Video(p) => self.on_video(p),
            MediaEvent::MixedAudio(m) => {
                let now_local = self.ctrl.now_us() as i64;
                if let Some(r) = self.run.as_mut() {
                    r.il.push_audio(m.pts_local_us as i64 - r.origin_local, m.data, now_local - r.origin_local);
                }
            }
            MediaEvent::Gap(_) => {
                self.wait_key = true;
                self.request_keyframe(true);
            }
            MediaEvent::AudioConfig(_) | MediaEvent::Audio(_) => {} // a live usa a mistura
        }
    }

    fn on_video(&mut self, p: VideoPacket) {
        let (cfg, cfg_id) = match &self.video {
            Some((c, id)) => (c.clone(), *id),
            None => return,
        };
        if p.config_id != cfg_id {
            return;
        }
        let key = p.flags.keyframe;
        if key {
            self.last_key = Some(p.data.clone());
        }
        let ts_local = self.ctrl.remote_to_local(p.pts_us).unwrap_or(p.recv_us as i64);

        let incompatible = self.run.as_ref().is_some_and(|r| {
            r.rotation != p.flags.rotation || r.codec != cfg.codec || (r.width, r.height) != (cfg.width, cfg.height)
        });
        if incompatible {
            if !key {
                self.request_keyframe(false);
                return;
            }
            self.status().last_error = Some("a orientação ou a resolução do iPad mudou: FFmpeg reiniciado".into());
            self.stop_process(true);
            self.restart_at = None;
        }

        if self.run.is_none() {
            if !self.restart_due() {
                return;
            }
            if !key {
                self.request_keyframe(false);
                return;
            }
            if !self.start_process(&cfg, p.flags.rotation, ts_local) {
                return;
            }
        }
        if self.wait_key {
            if !key {
                return;
            }
            self.wait_key = false;
        }
        if self.standby_since.take().is_some() {
            self.set_state(OutputState::Live);
        }
        if let Some(r) = self.run.as_mut() {
            let ts = ts_local - r.origin_local;
            r.il.push_video(ts, key, p.data);
            self.last_video_ts = self.last_video_ts.max(ts);
        }
    }

    fn start_process(&mut self, cfg: &VideoConfig, rotation: Rotation, origin_local: i64) -> bool {
        let Some(video_track) = VideoTrack::from_config(cfg, Rotation::R0) else {
            self.schedule_restart("VIDEO_CONFIG sem SPS/PPS válidos".into());
            return false;
        };
        let tracks = [
            Track::Video(video_track),
            Track::Audio(AudioTrack {
                name: "Mistura".into(),
                sample_rate: AUDIO_RATE as u32,
                channels: 2,
                format: AudioFormat::F32Le,
            }),
        ];

        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(ffmpeg_args(&self.dest, self.encoder, rotation))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        no_window(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.schedule_restart(format!("não foi possível arrancar o FFmpeg ({e})"));
                return false;
            }
        };
        let stdin = child.stdin.take().expect("stdin com pipe");
        let stderr = child.stderr.take().expect("stderr com pipe");
        let stderr_thread = {
            let (status, url) = (self.status.clone(), self.dest.url.clone());
            thread::Builder::new().name("padstudio-ffmpeg-log".into()).spawn(move || read_stderr(stderr, status, url)).ok()
        };
        *self.child.lock().unwrap() = Some(child);

        match MkvWriter::new(BufWriter::with_capacity(256 * 1024, stdin), Mode::Live, &tracks, "PadStudio") {
            Ok(writer) => {
                self.run = Some(Running {
                    writer,
                    il: Interleaver::new(HOLD_MAX_US),
                    origin_local,
                    rotation,
                    codec: cfg.codec,
                    width: cfg.width,
                    height: cfg.height,
                    stderr: stderr_thread,
                    started: Instant::now(),
                });
                self.wait_key = false;
                let mut s = self.status();
                s.state = OutputState::Starting;
                s.retry_in_s = None;
                s.clear_rates();
                true
            }
            Err(e) => {
                self.kill_child();
                if let Some(t) = stderr_thread {
                    let _ = t.join();
                }
                self.schedule_restart(format!("o FFmpeg fechou a entrada ({e})"));
                false
            }
        }
    }

    fn kill_child(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn child_exited(&self) -> Option<String> {
        let mut guard = self.child.lock().unwrap();
        match guard.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            Ok(None) => None,
            Err(e) => Some(e.to_string()),
        }
    }

    /// Fecha o stdin (o FFmpeg termina a saída e sai); se não sair a tempo, mata-o.
    fn stop_process(&mut self, graceful: bool) {
        let Some(r) = self.run.take() else { return };
        let Running { writer, stderr, .. } = r;
        drop(writer);
        let deadline = Instant::now() + if graceful { STOP_GRACE } else { Duration::ZERO };
        while self.child_exited().is_none() && self.child.lock().unwrap().is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        self.kill_child();
        if let Some(t) = stderr {
            let _ = t.join();
        }
        self.standby_since = None;
    }

    fn schedule_restart(&mut self, reason: String) {
        self.restarts += 1;
        let backoff = (1u64 << self.restarts.min(5)).min(MAX_BACKOFF_S);
        self.restart_at = Some(Instant::now() + Duration::from_secs(backoff));
        self.wait_key = true;
        let mut s = self.status();
        s.state = OutputState::Reconnecting;
        s.restarts = self.restarts;
        s.retry_in_s = Some(backoff);
        s.last_error = Some(redact_line(&reason, &self.dest.url));
        s.clear_rates();
    }

    fn tick(&mut self) {
        let now = Instant::now();

        // O FFmpeg terminou sozinho (servidor fechou, rede, chave errada…)?
        if self.run.is_some()
            && let Some(code) = self.child_exited() {
                self.stop_process(false);
                self.schedule_restart(format!("o FFmpeg terminou ({code})"));
            }

        // À espera de nova tentativa: contagem decrescente e pedido de keyframe quando for altura.
        if self.run.is_none()
            && let Some(t) = self.restart_at {
                let left = t.saturating_duration_since(now);
                self.status().retry_in_s = (!left.is_zero()).then(|| left.as_secs() + 1);
                if left.is_zero() && self.session_active {
                    self.request_keyframe(false);
                }
            }

        // Modo de espera: repete o último keyframe para a plataforma não cortar a live.
        if let Some(since) = self.standby_since {
            if since.elapsed() > STANDBY_MAX {
                self.status().last_error = Some("iPad desligado há mais de 5 min: transmissão parada".into());
                self.stop_process(true);
                self.set_state(OutputState::WaitingVideo);
            } else if now >= self.next_standby_frame {
                self.next_standby_frame = now + STANDBY_FRAME;
                let ts = self.standby_anchor_ts + since.elapsed().as_micros() as i64;
                if let (Some(key), Some(r)) = (self.last_key.clone(), self.run.as_mut()) {
                    r.il.push_video(ts, true, key);
                    self.last_video_ts = self.last_video_ts.max(ts);
                }
            }
        }

        if self.run.is_some()
            && let Err(e) = self.write_ready() {
                self.stop_process(false);
                self.schedule_restart(format!("falha ao enviar dados ao FFmpeg ({e})"));
            }

        if let Some(r) = &self.run {
            if r.started.elapsed() > STABLE_AFTER {
                self.restarts = 0; // estável há 1 min: a próxima falha volta a esperar só 2 s
            }
            if self.session_active
                && self.standby_since.is_none()
                && self.last_kf_request.is_none_or(|t| t.elapsed() >= KEYFRAME_REFRESH)
            {
                self.request_keyframe(true);
            }
        }
    }

    fn write_ready(&mut self) -> io::Result<()> {
        let Some(r) = self.run.as_mut() else { return Ok(()) };
        let now_rel = self.ctrl.now_us() as i64 - r.origin_local;
        let mut items = std::mem::take(&mut self.items);
        r.il.drain(now_rel, &mut items);
        let wrote = !items.is_empty();
        let mut result = Ok(());
        for item in items.drain(..) {
            result = match item {
                Item::Video { ts_us, keyframe, data } => r.writer.write_video_annexb(1, ts_us, keyframe, &data),
                Item::Audio { ts_us, data } => r.writer.write_block(2, ts_us, true, &data),
            };
            if result.is_err() {
                break;
            }
        }
        items.clear();
        self.items = items;
        result?;
        if wrote {
            r.writer.flush()?;
        }
        let silence = r.il.silence_blocks;
        self.status().silence_blocks = silence;
        Ok(())
    }
}

/// Lê o stderr do FFmpeg: progresso → estatísticas; o resto → última mensagem (sem a chave).
fn read_stderr(stderr: ChildStderr, status: Arc<Mutex<OutputStatus>>, url: String) {
    let mut reader = BufReader::new(stderr);
    let mut parser = ProgressParser::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // O FFmpeg no Windows nem sempre escreve UTF-8 válido: nunca parar de ler por isso.
        let text = String::from_utf8_lossy(&buf);
        let line = text.trim();
        if line.is_empty() {
            continue;
        }
        if is_progress_line(line) {
            if let Some(p) = parser.line(line) {
                let mut s = status.lock().unwrap();
                s.fps = p.fps;
                s.bitrate_kbps = p.bitrate_kbps;
                s.speed = p.speed;
                s.dup_frames = p.dup_frames;
                s.drop_frames = p.drop_frames;
                s.out_time_s = p.out_time_us as f64 / 1e6;
                if s.state == OutputState::Starting && p.frame > 0 {
                    s.state = OutputState::Live;
                }
            }
        } else if !BENIGN_MESSAGES.iter().any(|m| line.contains(m)) {
            status.lock().unwrap().last_message = Some(redact_line(line, &url));
        }
    }
}
