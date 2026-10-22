use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use padstudio_engine::Controller;

use crate::encoder::{EncoderKind, detect};
use crate::output::{OutputHandle, OutputStatus};
use crate::profile::Destination;

#[derive(Debug, Clone)]
pub struct StreamOptions {
    /// Executável do FFmpeg (na app final, o sidecar distribuído com ela).
    pub ffmpeg: PathBuf,
    /// `None` = detetar o melhor encoder de hardware.
    pub encoder: Option<EncoderKind>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self { ffmpeg: "ffmpeg".into(), encoder: None }
    }
}

/// Gere um processo FFmpeg por destino. Ligar/desligar um destino não mexe nos outros.
pub struct StreamManager {
    ctrl: Controller,
    ffmpeg: PathBuf,
    encoder: EncoderKind,
    outputs: Mutex<BTreeMap<String, OutputHandle>>,
}

impl StreamManager {
    /// Verifica o FFmpeg e escolhe o encoder (demora ~1 s na primeira vez).
    pub fn new(ctrl: &Controller, opts: StreamOptions) -> Result<Self, String> {
        let encoder = match opts.encoder {
            Some(e) => e,
            None => detect(&opts.ffmpeg)?,
        };
        Ok(Self { ctrl: ctrl.clone(), ffmpeg: opts.ffmpeg, encoder, outputs: Mutex::new(BTreeMap::new()) })
    }

    pub fn encoder(&self) -> EncoderKind {
        self.encoder
    }

    /// Liga um destino (se já estiver ligado com o mesmo `id`, reinicia-o com as novas definições).
    pub fn start(&self, dest: Destination) {
        let previous = self.outputs.lock().unwrap().remove(&dest.id);
        drop(previous); // pára o anterior (Drop espera pela thread)
        let id = dest.id.clone();
        let handle = OutputHandle::spawn(self.ctrl.clone(), dest, self.encoder, self.ffmpeg.clone());
        self.outputs.lock().unwrap().insert(id, handle);
    }

    /// Desliga um destino. Devolve `false` se não estava ligado.
    pub fn stop(&self, id: &str) -> bool {
        let handle = self.outputs.lock().unwrap().remove(id);
        handle.is_some()
    }

    /// Desliga todos (em paralelo).
    pub fn stop_all(&self) {
        let mut all = std::mem::take(&mut *self.outputs.lock().unwrap());
        for h in all.values() {
            h.request_stop();
        }
        for h in all.values_mut() {
            h.join();
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.outputs.lock().unwrap().contains_key(id)
    }

    pub fn statuses(&self) -> Vec<OutputStatus> {
        self.outputs.lock().unwrap().values().map(OutputHandle::status).collect()
    }
}

impl Drop for StreamManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
