//! Estado da app: motor, mixer, streaming, gravador, definições e preview.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use padstudio_audio::{InputId, Mixer, MixerConfig, MixerLevels};
use padstudio_engine::recorder::{RecordedFile, Recorder};
use padstudio_engine::{Controller, Engine, EngineConfig, EngineStatus, Telemetry};
use padstudio_streaming::{OutputStatus, StreamManager, StreamOptions};
use serde::Serialize;

use crate::preview::{self, PreviewInfo};
use crate::settings::Settings;

pub struct Recording {
    pub recorder: Recorder,
    pub started: Instant,
    pub dir: PathBuf,
}

pub struct AppState {
    pub engine: Engine,
    pub ctrl: Controller,
    pub mixer: Mixer,
    /// Criado em segundo plano (deteção do FFmpeg e do encoder demora ~1 s).
    pub streams: Mutex<Option<Arc<StreamManager>>>,
    pub streaming_error: Mutex<Option<String>>,
    pub recording: Mutex<Option<Recording>>,
    pub last_recordings: Mutex<Vec<RecordedFile>>,
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    pub default_record_dir: PathBuf,
    pub preview: Option<PreviewInfo>,
    pub preview_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingView {
    pub elapsed_s: f64,
    pub dir: PathBuf,
}

/// Tudo o que a UI mostra, enviado ~10 vezes por segundo.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub status: EngineStatus,
    pub telemetry: Telemetry,
    pub mixer: MixerLevels,
    pub outputs: Vec<OutputStatus>,
    pub encoder: Option<String>,
    pub streaming_error: Option<String>,
    pub recording: Option<RecordingView>,
    pub last_recordings: Vec<RecordedFile>,
}

impl AppState {
    pub fn new(settings_path: PathBuf, default_record_dir: PathBuf) -> Self {
        let settings = Settings::load(&settings_path);
        let engine = Engine::start(EngineConfig {
            endpoint: settings.endpoint.to_endpoint(),
            stream: settings.ipad.clone(),
            ..Default::default()
        });
        let ctrl = engine.controller();
        let mixer = Mixer::start(&ctrl, MixerConfig { mic: settings.mic.clone(), gains_db: settings.gains_db, ..Default::default() });
        for id in InputId::ALL {
            mixer.set_muted(id, settings.muted[id as usize]);
        }
        let (preview, preview_error) = match preview::start(ctrl.clone()) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("preview indisponível: {e}"))),
        };
        Self {
            engine,
            ctrl,
            mixer,
            streams: Mutex::new(None),
            streaming_error: Mutex::new(None),
            recording: Mutex::new(None),
            last_recordings: Mutex::new(Vec::new()),
            settings: Mutex::new(settings),
            settings_path,
            default_record_dir,
            preview,
            preview_error,
        }
    }

    /// Deteta o FFmpeg/encoder (chamar numa thread à parte).
    pub fn init_streaming(&self) {
        match StreamManager::new(&self.ctrl, StreamOptions::default()) {
            Ok(m) => *self.streams.lock().unwrap() = Some(Arc::new(m)),
            Err(e) => *self.streaming_error.lock().unwrap() = Some(e),
        }
    }

    pub fn streams(&self) -> Result<Arc<StreamManager>, String> {
        if let Some(m) = self.streams.lock().unwrap().as_ref() {
            return Ok(m.clone());
        }
        Err(self.streaming_error.lock().unwrap().clone().unwrap_or_else(|| "o FFmpeg ainda está a ser verificado".into()))
    }

    pub fn save_settings(&self) -> Result<(), String> {
        self.settings.lock().unwrap().save(&self.settings_path)
    }

    pub fn snapshot(&self) -> Snapshot {
        let streams = self.streams.lock().unwrap().clone();
        Snapshot {
            status: self.ctrl.status(),
            telemetry: self.ctrl.telemetry(),
            mixer: self.mixer.levels(),
            outputs: streams.as_ref().map(|m| m.statuses()).unwrap_or_default(),
            encoder: streams.as_ref().map(|m| m.encoder().label().to_string()),
            streaming_error: self.streaming_error.lock().unwrap().clone(),
            recording: self
                .recording
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| RecordingView { elapsed_s: r.started.elapsed().as_secs_f64(), dir: r.dir.clone() }),
            last_recordings: self.last_recordings.lock().unwrap().clone(),
        }
    }

    /// Fecha tudo por ordem: gravação, destinos, mixer, motor.
    pub fn shutdown(&self) {
        if let Some(r) = self.recording.lock().unwrap().take() {
            let _ = r.recorder.stop();
        }
        if let Some(m) = self.streams.lock().unwrap().take() {
            m.stop_all();
        }
    }
}
