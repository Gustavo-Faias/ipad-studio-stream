//! Gravador MKV: grava o bitstream do iPad tal como chega (sem recodificar, 0 % GPU), com o áudio
//! do iPad em PCM, numa thread própria alimentada pelo bus.
//!
//! Cada ficheiro começa num keyframe. Muda-se de ficheiro (segmento) quando muda a configuração
//! de vídeo, a rotação ou o formato do áudio, porque o Matroska não permite mudar essas
//! propriedades a meio de uma faixa. Quando a mudança chega a meio de um GOP, o gravador pede
//! um keyframe e continua no ficheiro atual até ele chegar.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, bounded, select};
use padstudio_protocol::payload::{AudioConfig, Rotation, VideoConfig};
use serde::Serialize;

use crate::mkv::{AudioTrack, MkvWriter, Mode, Track, VideoTrack};
use crate::{AudioPacket, Controller, MediaEvent, MediaState, VideoPacket};

const QUEUE_EVENTS: usize = 2048;

#[derive(Debug, Clone)]
pub struct RecorderOptions {
    pub dir: PathBuf,
    /// Prefixo dos nomes: `<prefixo>-AAAA-MM-DD_HH-MM-SS.mkv`.
    pub prefix: String,
}

impl RecorderOptions {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), prefix: "PadStudio".into() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordedFile {
    pub path: PathBuf,
    pub video_frames: u64,
    pub audio_blocks: u64,
    pub duration_s: f64,
    pub bytes: u64,
}

pub struct Recorder {
    stop: Sender<()>,
    thread: Option<JoinHandle<io::Result<Vec<RecordedFile>>>>,
}

impl Recorder {
    /// Começa a gravar. Se a sessão já estiver a decorrer, o ficheiro começa no keyframe seguinte
    /// (pedido de imediato).
    pub fn start(controller: &Controller, opts: RecorderOptions) -> io::Result<Self> {
        fs::create_dir_all(&opts.dir)?;
        let (sub, state) = controller.subscribe(QUEUE_EVENTS);
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let mut rec = State::new(opts, controller.clone(), state);
        let thread = thread::Builder::new().name("padstudio-recorder".into()).spawn(move || {
            let result = (|| -> io::Result<()> {
                loop {
                    select! {
                        recv(sub.receiver()) -> ev => match ev {
                            Ok(ev) => rec.handle(ev)?,
                            Err(_) => return Ok(()),
                        },
                        recv(stop_rx) -> _ => return Ok(()),
                    }
                }
            })();
            let closed = rec.close_file();
            result.and(closed).map(|()| rec.files)
        })?;
        Ok(Self { stop: stop_tx, thread: Some(thread) })
    }

    /// Para, fecha o ficheiro atual e devolve tudo o que foi gravado.
    pub fn stop(mut self) -> io::Result<Vec<RecordedFile>> {
        self.finish()
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    fn finish(&mut self) -> io::Result<Vec<RecordedFile>> {
        let _ = self.stop.try_send(());
        match self.thread.take() {
            Some(t) => t.join().map_err(|_| io::Error::other("a thread do gravador terminou com pânico"))?,
            None => Ok(Vec::new()),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct OpenFile {
    writer: MkvWriter<BufWriter<File>>,
    path: PathBuf,
    origin_pts: u64,
    config_id: u64,
    rotation: Rotation,
    audio_sig: Vec<(u8, AudioConfig)>,
    audio_tracks: BTreeMap<u8, u64>,
    frames: u64,
    audio_blocks: u64,
}

struct State {
    opts: RecorderOptions,
    controller: Controller,
    video: Option<(VideoConfig, u64)>,
    audio: BTreeMap<u8, AudioConfig>,
    file: Option<OpenFile>,
    wait_keyframe: bool,
    split_requested: bool,
    files: Vec<RecordedFile>,
}

impl State {
    fn new(opts: RecorderOptions, controller: Controller, state: MediaState) -> Self {
        Self {
            opts,
            controller,
            video: state.video,
            audio: state.audio,
            file: None,
            wait_keyframe: false,
            split_requested: false,
            files: Vec::new(),
        }
    }

    fn handle(&mut self, ev: MediaEvent) -> io::Result<()> {
        match ev {
            MediaEvent::SessionStarted(_) | MediaEvent::SessionEnded { .. } => {
                self.close_file()?;
                self.video = None;
                self.audio.clear();
            }
            MediaEvent::VideoConfig { config, id } => self.video = Some((config, id)),
            MediaEvent::AudioConfig(c) => {
                self.audio.insert(c.source.0, c);
                if self.file.as_ref().is_some_and(|f| f.audio_sig != self.audio_sig()) {
                    self.request_split();
                }
            }
            MediaEvent::Video(p) => self.on_video(&p)?,
            MediaEvent::Audio(p) => self.on_audio(&p)?,
            MediaEvent::MixedAudio(_) => {} // o gravador guarda o áudio original do iPad
            MediaEvent::Gap(_) => {
                // Perdemos frames: o próximo P-frame já não é descodificável.
                self.wait_keyframe = true;
                self.controller.request_keyframe();
            }
        }
        Ok(())
    }

    fn audio_sig(&self) -> Vec<(u8, AudioConfig)> {
        self.audio.iter().map(|(k, v)| (*k, *v)).collect()
    }

    fn request_split(&mut self) {
        if !self.split_requested {
            self.split_requested = true;
            self.controller.request_keyframe();
        }
    }

    fn on_video(&mut self, p: &VideoPacket) -> io::Result<()> {
        let Some((_, config_id)) = &self.video else { return Ok(()) };
        if p.config_id != *config_id {
            return Ok(()); // frame de uma configuração antiga
        }
        if self.wait_keyframe {
            if !p.flags.keyframe {
                return Ok(());
            }
            self.wait_keyframe = false;
        }

        let config_changed = self.file.as_ref().is_none_or(|f| f.config_id != p.config_id);
        let props_changed = self.file.as_ref().is_some_and(|f| f.rotation != p.flags.rotation || f.audio_sig != self.audio_sig());
        if p.flags.keyframe && (config_changed || props_changed) {
            self.close_file()?;
            self.open_file(p)?;
            self.split_requested = false;
        } else if config_changed {
            self.request_split(); // ainda sem keyframe da nova configuração: não há onde escrever
            return Ok(());
        } else if props_changed {
            self.request_split(); // continua no ficheiro atual até ao keyframe
        }

        let f = self.file.as_mut().expect("ficheiro aberto");
        let ts = p.pts_us as i64 - f.origin_pts as i64;
        if ts >= 0 {
            f.writer.write_video_annexb(1, ts, p.flags.keyframe, &p.data)?;
            f.frames += 1;
        }
        Ok(())
    }

    fn on_audio(&mut self, p: &AudioPacket) -> io::Result<()> {
        let Some(f) = self.file.as_mut() else { return Ok(()) };
        let Some(&track) = f.audio_tracks.get(&p.source.0) else { return Ok(()) };
        let ts = p.pts_us as i64 - f.origin_pts as i64;
        if ts >= 0 {
            f.writer.write_block(track, ts, true, &p.data)?;
            f.audio_blocks += 1;
        }
        Ok(())
    }

    fn open_file(&mut self, first: &VideoPacket) -> io::Result<()> {
        let (config, config_id) = self.video.as_ref().expect("configuração de vídeo");
        let video = VideoTrack::from_config(config, first.flags.rotation)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "VIDEO_CONFIG sem parâmetros válidos"))?;
        let mut tracks = vec![Track::Video(video)];
        let mut audio_tracks = BTreeMap::new();
        for (source, cfg) in &self.audio {
            let name = if *source == 0 { "Áudio do iPad" } else { "Microfone do iPad" };
            audio_tracks.insert(*source, tracks.len() as u64 + 1);
            tracks.push(Track::Audio(AudioTrack::from_config(cfg, name)));
        }
        let path = self.next_path();
        let out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
        let writer = MkvWriter::new(out, Mode::File, &tracks, concat!("PadStudio ", env!("CARGO_PKG_VERSION")))?;
        self.file = Some(OpenFile {
            writer,
            path,
            origin_pts: first.pts_us,
            config_id: *config_id,
            rotation: first.flags.rotation,
            audio_sig: self.audio_sig(),
            audio_tracks,
            frames: 0,
            audio_blocks: 0,
        });
        Ok(())
    }

    fn close_file(&mut self) -> io::Result<()> {
        let Some(f) = self.file.take() else { return Ok(()) };
        let duration_us = f.writer.duration_us();
        let file = f.writer.finish()?.into_inner().map_err(|e| e.into_error())?;
        let bytes = file.metadata()?.len();
        self.files.push(RecordedFile {
            path: f.path,
            video_frames: f.frames,
            audio_blocks: f.audio_blocks,
            duration_s: duration_us as f64 / 1e6,
            bytes,
        });
        Ok(())
    }

    fn next_path(&self) -> PathBuf {
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let mut n = 1;
        loop {
            let name = if n == 1 {
                format!("{}-{stamp}.mkv", self.opts.prefix)
            } else {
                format!("{}-{stamp}-{n}.mkv", self.opts.prefix)
            };
            let path = self.opts.dir.join(name);
            if !path.exists() {
                return path;
            }
            n += 1;
        }
    }
}
