//! Uma entrada do mixer: reamostragem para 48 kHz, colocação na linha temporal pelo instante de
//! captura e correção contínua da deriva de relógio.
//!
//! Cada entrada tem o seu relógio de amostragem (DAC/ADC do iPad, microfone do PC) e nenhum é
//! exatamente o relógio do PC. Em vez de deixar a fila crescer ou esvaziar, compara-se onde cada
//! pacote *devia* ficar (pelo seu timestamp) com onde vai ficar (pela contagem de amostras) e
//! ajusta-se o rácio do resampler em poucas centenas de ppm. Erros grandes (pausa, salto de
//! relógio) realinham de imediato.

use std::collections::VecDeque;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction};

/// Taxa e canais da saída do mixer.
pub const OUT_RATE: u32 = 48_000;
pub(crate) const CH: usize = 2;

/// Frames de entrada por chamada ao resampler (~5 ms).
const CHUNK: usize = 256;
/// Um salto maior do que isto nos timestamps de entrada é uma descontinuidade.
const INPUT_GAP_US: f64 = 40_000.0;
/// Erro de alinhamento a partir do qual se realinha de imediato (30 ms).
const RESYNC_FRAMES: f64 = 1_440.0;
/// Ganho proporcional do controlo de deriva (rácio relativo por frame de erro).
const KP: f64 = 2e-5;
/// Correção máxima do rácio (±0,3 %, inaudível em voz e jogo).
const MAX_CORRECTION: f64 = 0.003;
/// Suavização do erro (média exponencial).
const EMA: f64 = 0.02;

/// Converte instantes locais (µs) em índices de frame de saída e vice-versa.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeline {
    pub epoch_us: i64,
}

impl Timeline {
    pub fn index_f(&self, t_us: i64) -> f64 {
        (t_us - self.epoch_us) as f64 * f64::from(OUT_RATE) / 1e6
    }

    pub fn index(&self, t_us: i64) -> i64 {
        self.index_f(t_us).round() as i64
    }

    pub fn time(&self, index: i64) -> i64 {
        self.epoch_us + (index as f64 * 1e6 / f64::from(OUT_RATE)).round() as i64
    }
}

#[derive(Debug, Clone, Default)]
pub struct TrackStats {
    /// Pico (depois do ganho) desde a última leitura.
    pub peak: f32,
    /// Frames que chegaram depois de a sua janela já ter sido misturada.
    pub late_frames: u64,
    /// Realinhamentos (descontinuidades ou erro > 30 ms).
    pub resyncs: u64,
    pub frames_in: u64,
}

pub(crate) struct InputTrack {
    pub in_rate: u32,
    resampler: Async<f32>,
    delay: i64,
    pending: Vec<f32>,
    in_cursor_us: Option<f64>,
    write_pos: Option<i64>,
    ring: VecDeque<f32>,
    ring_start: i64,
    out_scratch: Vec<f32>,
    err_filtered: f64,
    ratio_rel: f64,
    gain: f32,
    anchored_once: bool,
    pub stats: TrackStats,
}

impl InputTrack {
    pub fn new(in_rate: u32) -> Self {
        let params = SincInterpolationParameters::new(64, WindowFunction::BlackmanHarris2)
            .oversampling_factor(128)
            .interpolation(SincInterpolationType::Linear);
        let ratio = f64::from(OUT_RATE) / f64::from(in_rate.max(1));
        let resampler = Async::<f32>::new_sinc(ratio, 1.0 + 2.0 * MAX_CORRECTION, &params, CHUNK, CH, FixedAsync::Input)
            .expect("parâmetros do resampler válidos");
        let delay = resampler.output_delay() as i64;
        let out_scratch = vec![0.0; resampler.output_frames_max() * CH];
        Self {
            in_rate,
            resampler,
            delay,
            pending: Vec::with_capacity(CHUNK * CH * 4),
            in_cursor_us: None,
            write_pos: None,
            ring: VecDeque::with_capacity(OUT_RATE as usize * CH / 2),
            ring_start: 0,
            out_scratch,
            err_filtered: 0.0,
            ratio_rel: 1.0,
            gain: 1.0,
            anchored_once: false,
            stats: TrackStats::default(),
        }
    }

    /// Correção de deriva atual em ppm (positivo = a entrada é mais lenta do que o nominal).
    pub fn drift_ppm(&self) -> f64 {
        (self.ratio_rel - 1.0) * 1e6
    }

    /// Áudio já reamostrado à espera de ser misturado, em ms à frente de `read_pos`.
    pub fn buffered_ms(&self, read_pos: i64) -> f64 {
        let end = self.ring_start + (self.ring.len() / CH) as i64;
        (end - read_pos).max(0) as f64 * 1000.0 / f64::from(OUT_RATE)
    }

    fn reset_stream(&mut self) {
        self.resampler.reset();
        let _ = self.resampler.set_resample_ratio_relative(1.0, false);
        self.ratio_rel = 1.0;
        self.err_filtered = 0.0;
        self.pending.clear();
        self.in_cursor_us = None;
        self.write_pos = None;
        if self.anchored_once {
            self.stats.resyncs += 1;
        }
    }

    /// Acrescenta frames estéreo capturados no instante local `t_us` (primeiro frame).
    pub fn push(&mut self, stereo: &[f32], t_us: i64, timeline: &Timeline, read_pos: i64) {
        let frames = stereo.len() / CH;
        if frames == 0 {
            return;
        }
        self.stats.frames_in += frames as u64;

        let continuous = self.in_cursor_us.is_some_and(|c| (t_us as f64 - c).abs() <= INPUT_GAP_US);
        if !continuous {
            self.reset_stream();
        }

        if let Some(wp) = self.write_pos {
            // Onde este pacote vai ficar (contagem de amostras) vs. onde devia ficar (timestamp).
            let pending_frames = (self.pending.len() / CH) as f64;
            let out_per_in = f64::from(OUT_RATE) / f64::from(self.in_rate) * self.ratio_rel;
            let predicted = wp as f64 + self.delay as f64 + pending_frames * out_per_in;
            let err = timeline.index_f(t_us) - predicted;
            if err.abs() > RESYNC_FRAMES {
                self.reset_stream();
            } else {
                self.err_filtered += EMA * (err - self.err_filtered);
                let rel = 1.0 + (self.err_filtered * KP).clamp(-MAX_CORRECTION, MAX_CORRECTION);
                if (rel - self.ratio_rel).abs() > 1e-7 && self.resampler.set_resample_ratio_relative(rel, true).is_ok() {
                    self.ratio_rel = rel;
                }
            }
        }
        if self.write_pos.is_none() {
            // Os primeiros `delay` frames de saída do resampler são o seu atraso interno.
            self.write_pos = Some(timeline.index(t_us) - self.delay);
            self.anchored_once = true;
        }
        self.in_cursor_us = Some(t_us as f64 + frames as f64 * 1e6 / f64::from(self.in_rate));
        self.pending.extend_from_slice(stereo);
        self.resample_pending(read_pos);
    }

    fn resample_pending(&mut self, read_pos: i64) {
        loop {
            let need = self.resampler.input_frames_next();
            if self.pending.len() / CH < need {
                return;
            }
            let out_capacity = self.out_scratch.len() / CH;
            let result = {
                let input = InterleavedSlice::new(&self.pending[..need * CH], CH, need).expect("tamanho de entrada");
                let mut output = InterleavedSlice::new_mut(&mut self.out_scratch, CH, out_capacity).expect("tamanho de saída");
                self.resampler.process_into_buffer(&input, &mut output, None)
            };
            let Ok((used, produced)) = result else {
                self.reset_stream();
                return;
            };
            self.pending.drain(..used * CH);
            self.write_out(produced, read_pos);
        }
    }

    /// Escreve `n` frames de `out_scratch` na linha temporal, a partir de `write_pos`.
    fn write_out(&mut self, n: usize, read_pos: i64) {
        let wp = self.write_pos.expect("ancorado");
        self.write_pos = Some(wp + n as i64);

        // O que cai antes de `read_pos` já foi misturado: chegou tarde.
        let skip = ((read_pos - wp).max(0) as usize).min(n);
        self.stats.late_frames += skip as u64;
        if skip == n {
            return;
        }
        let start = wp + skip as i64;
        let ring_end = self.ring_start + (self.ring.len() / CH) as i64;
        if self.ring.is_empty() || start <= self.ring_start {
            self.ring.clear();
            self.ring_start = start;
        } else if start < ring_end {
            self.ring.truncate(((start - self.ring_start) as usize) * CH);
        } else if start > ring_end {
            let gap = ((start - ring_end) as usize) * CH;
            self.ring.extend(std::iter::repeat_n(0.0, gap));
        }
        self.ring.extend(self.out_scratch[skip * CH..n * CH].iter().copied());
    }

    /// Soma `frames` frames a partir de `start` em `out`, com rampa de ganho até `target_gain`.
    pub fn render(&mut self, start: i64, frames: usize, out: &mut [f32], target_gain: f32) {
        let (g0, g1) = (self.gain, target_gain);
        self.gain = g1;
        let ring_frames = (self.ring.len() / CH) as i64;
        let mut peak = 0f32;
        for i in 0..frames {
            let rel = start + i as i64 - self.ring_start;
            if rel < 0 || rel >= ring_frames {
                continue;
            }
            let g = g0 + (g1 - g0) * (i as f32 / frames as f32);
            for c in 0..CH {
                let s = self.ring[rel as usize * CH + c] * g;
                out[i * CH + c] += s;
                peak = peak.max(s.abs());
            }
        }
        self.stats.peak = self.stats.peak.max(peak);

        let end = start + frames as i64;
        let consumed = (end - self.ring_start).clamp(0, ring_frames) as usize;
        self.ring.drain(..consumed * CH);
        self.ring_start = if self.ring.is_empty() { end.max(self.ring_start) } else { self.ring_start + consumed as i64 };
    }
}
