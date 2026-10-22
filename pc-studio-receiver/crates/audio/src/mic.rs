//! Captura do microfone do PC com `cpal` (WASAPI no Windows, CoreAudio no macOS).
//!
//! O stream do `cpal` não é `Send` em todas as plataformas, por isso vive numa thread própria
//! que o cria, o mantém a tocar e o larga quando lhe pedem para parar. Cada callback envia um
//! pacote já em f32 estéreo, com o instante de captura no relógio local.

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use crossbeam_channel::{Sender, bounded};
use serde::Serialize;

use crate::convert::frame_to_stereo;
use crate::mixer::MicSelection;

/// Pacote do microfone, já em f32 estéreo intercalado.
pub(crate) struct MicChunk {
    pub samples: Vec<f32>,
    pub rate: u32,
    /// Instante de captura do primeiro frame, no relógio local (µs).
    pub t_us: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MicInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: String,
}

/// Relógio local partilhado com o callback de áudio.
pub(crate) type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Problemas reportados pelo stream de captura.
pub(crate) enum MicProblem {
    /// Falha pontual de buffer (um pequeno salto no áudio); a captura continua.
    Xrun,
    /// O stream deixou de funcionar (microfone desligado, dispositivo mudou…).
    Fatal(String),
}

/// Onde reportar problemas do stream.
pub(crate) type ErrorSink = Arc<dyn Fn(MicProblem) + Send + Sync>;

/// Nomes dos microfones disponíveis.
pub fn input_devices() -> Result<Vec<String>, String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|e| format!("não foi possível listar os microfones: {e}"))?;
    Ok(devices.map(|d| d.to_string()).collect())
}

/// Nome do microfone predefinido do sistema.
pub fn default_input_device() -> Option<String> {
    cpal::default_host().default_input_device().map(|d| d.to_string())
}

/// Captura ativa. Largar este valor para a captura.
pub(crate) struct MicCapture {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
    pub info: MicInfo,
}

impl MicCapture {
    pub fn start(selection: &MicSelection, clock: Clock, on_error: ErrorSink, tx: Sender<MicChunk>) -> Result<Self, String> {
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let (ready_tx, ready_rx) = bounded::<Result<MicInfo, String>>(1);
        let selection = selection.clone();
        let thread = thread::Builder::new()
            .name("padstudio-mic".into())
            .spawn(move || match open(&selection, clock, on_error, tx) {
                Ok((stream, info)) => {
                    let _ = ready_tx.send(Ok(info));
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        let info = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "o microfone não respondeu em 5 s".to_string())??;
        Ok(Self { stop: stop_tx, thread: Some(thread), info })
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn open(selection: &MicSelection, clock: Clock, on_error: ErrorSink, tx: Sender<MicChunk>) -> Result<(cpal::Stream, MicInfo), String> {
    let host = cpal::default_host();
    let device = match selection {
        MicSelection::Off => return Err("microfone desligado".into()),
        MicSelection::Default => host.default_input_device().ok_or("não há microfone predefinido no sistema")?,
        MicSelection::Named(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.to_string() == *name)
            .ok_or_else(|| format!("microfone \"{name}\" não encontrado"))?,
    };
    let supported = device.default_input_config().map_err(|e| format!("{device}: {e}"))?;
    let format = supported.sample_format();
    let config = supported.config();
    let info = MicInfo {
        device: device.to_string(),
        sample_rate: config.sample_rate,
        channels: config.channels,
        format: format!("{format:?}"),
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, clock, on_error, tx),
        SampleFormat::I16 => build::<i16>(&device, config, clock, on_error, tx),
        SampleFormat::I32 => build::<i32>(&device, config, clock, on_error, tx),
        SampleFormat::U16 => build::<u16>(&device, config, clock, on_error, tx),
        SampleFormat::F64 => build::<f64>(&device, config, clock, on_error, tx),
        other => return Err(format!("formato de amostra {other:?} não suportado")),
    }?;
    stream.play().map_err(|e| format!("não foi possível iniciar o microfone: {e}"))?;
    Ok((stream, info))
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, clock: Clock, on_error: ErrorSink, tx: Sender<MicChunk>) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let rate = config.sample_rate;
    let mut frame = vec![0f32; channels];
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                let ts = info.timestamp();
                let delay_us = ts.callback.duration_since(ts.capture).as_micros() as i64;
                let t_us = clock() - delay_us;
                let mut samples = Vec::with_capacity(data.len() / channels * 2);
                for raw in data.chunks_exact(channels) {
                    for (dst, s) in frame.iter_mut().zip(raw) {
                        *dst = f32::from_sample_(*s);
                    }
                    let (l, r) = frame_to_stereo(&frame);
                    samples.push(l);
                    samples.push(r);
                }
                let _ = tx.try_send(MicChunk { samples, rate, t_us }); // fila cheia: perde-se (o mixer realinha)
            },
            move |e: cpal::Error| {
                on_error(match e.kind() {
                    cpal::ErrorKind::Xrun => MicProblem::Xrun,
                    _ => MicProblem::Fatal(format!("erro no microfone: {e}")),
                })
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| format!("não foi possível abrir o microfone: {e}"))
}
