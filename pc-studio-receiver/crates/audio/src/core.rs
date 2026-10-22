//! Núcleo do mixer, sem threads nem dispositivos: entradas com timestamp → blocos misturados.
//! Determinístico, para poder ser testado com tempo simulado.

use crate::track::{CH, InputTrack, OUT_RATE, Timeline, TrackStats};
use crate::InputId;

/// Frames por bloco de saída (10 ms).
pub(crate) const BLOCK: usize = (OUT_RATE / 100) as usize;

pub(crate) struct MixerCore {
    timeline: Timeline,
    /// Próximo frame de saída a misturar.
    read_pos: i64,
    latency_frames: i64,
    tracks: [Option<InputTrack>; InputId::COUNT],
    pub master_peak: f32,
    pub blocks: u64,
}

pub(crate) struct Block {
    pub pts_local_us: i64,
    pub samples: Vec<f32>,
}

impl MixerCore {
    /// `start_us`: instante local do primeiro frame de saída; `latency_ms`: margem para as
    /// entradas chegarem (jitter buffer).
    pub fn new(start_us: i64, latency_ms: u32) -> Self {
        Self {
            timeline: Timeline { epoch_us: start_us },
            read_pos: 0,
            latency_frames: i64::from(latency_ms) * i64::from(OUT_RATE) / 1000,
            tracks: Default::default(),
            master_peak: 0.0,
            blocks: 0,
        }
    }

    pub fn push(&mut self, id: InputId, in_rate: u32, stereo: &[f32], t_us: i64) {
        let slot = &mut self.tracks[id as usize];
        if slot.as_ref().is_none_or(|t| t.in_rate != in_rate) {
            *slot = Some(InputTrack::new(in_rate));
        }
        if let Some(track) = slot.as_mut() {
            track.push(stereo, t_us, &self.timeline, self.read_pos);
        }
    }

    pub fn remove(&mut self, id: InputId) {
        self.tracks[id as usize] = None;
    }

    /// Mistura todos os blocos cuja janela terminou há mais de `latency` em relação a `now_us`.
    pub fn render_ready(&mut self, now_us: i64, gains: &[f32; InputId::COUNT]) -> Vec<Block> {
        let ready_until = self.timeline.index(now_us) - self.latency_frames;
        // Muito atrasado (PC suspenso, depurador): salta em vez de despejar segundos de áudio.
        if ready_until - self.read_pos > i64::from(OUT_RATE) {
            self.read_pos = ready_until - BLOCK as i64;
        }
        let mut out = Vec::new();
        while self.read_pos + BLOCK as i64 <= ready_until {
            let mut buf = vec![0f32; BLOCK * CH];
            for (track, &gain) in self.tracks.iter_mut().zip(gains) {
                if let Some(t) = track {
                    t.render(self.read_pos, BLOCK, &mut buf, gain);
                }
            }
            let mut peak = 0f32;
            for s in &mut buf {
                *s = soft_clip(*s);
                peak = peak.max(s.abs());
            }
            self.master_peak = self.master_peak.max(peak);
            out.push(Block { pts_local_us: self.timeline.time(self.read_pos), samples: buf });
            self.read_pos += BLOCK as i64;
            self.blocks += 1;
        }
        out
    }

    pub fn track_stats(&mut self, id: InputId) -> Option<(TrackStats, f64, f64)> {
        let read_pos = self.read_pos;
        self.tracks[id as usize].as_mut().map(|t| {
            let stats = t.stats.clone();
            t.stats.peak = 0.0;
            (stats, t.buffered_ms(read_pos), t.drift_ppm())
        })
    }

    pub fn take_master_peak(&mut self) -> f32 {
        std::mem::take(&mut self.master_peak)
    }
}

/// Limitador suave: linear até 0,9 e depois compressão até 1,0 (nunca satura de forma dura).
#[inline]
fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.9 { x } else { x.signum() * (0.9 + 0.1 * ((a - 0.9) / 0.1).tanh()) }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    const START: i64 = 1_000_000;
    const UNITY: [f32; InputId::COUNT] = [1.0; InputId::COUNT];

    /// Gera `frames` de um seno estéreo a partir da fase dada.
    fn sine(frames: usize, rate: u32, freq: f64, amp: f32, phase: &mut f64) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            let s = (phase.sin() as f32) * amp;
            v.push(s);
            v.push(s);
            *phase = (*phase + TAU * freq / f64::from(rate)) % TAU;
        }
        v
    }

    fn impulse(frames: usize, at: usize) -> Vec<f32> {
        let mut v = vec![0.0; frames * 2];
        v[at * 2] = 0.8;
        v[at * 2 + 1] = 0.8;
        v
    }

    fn first_peak(blocks: &[Block], threshold: f32) -> Option<i64> {
        let t0 = blocks.first()?.pts_local_us;
        let all: Vec<f32> = blocks.iter().flat_map(|b| b.samples.iter().step_by(2).copied()).collect();
        let i = all.iter().position(|s| s.abs() > threshold)?;
        Some(t0 + (i as f64 * 1e6 / 48_000.0) as i64)
    }

    #[test]
    fn inputs_at_different_rates_are_aligned_by_capture_time() {
        // Impulso no mesmo instante de captura numa entrada a 44,1 kHz (iPad) e noutra a 48 kHz
        // (microfone) que chega 15 ms depois: no resultado ficam no mesmo sítio.
        let mut a = MixerCore::new(START, 60);
        let mut b = MixerCore::new(START, 60);
        let click_us = START + 200_000;
        for (core, rate, arrive_late) in [(&mut a, 44_100u32, 0i64), (&mut b, 48_000, 15_000)] {
            let chunk = rate as usize / 100; // pacotes de 10 ms
            let mut t = START;
            let mut out = Vec::new();
            for i in 0..60 {
                let frames_from_start = (t - START) as f64 * f64::from(rate) / 1e6;
                let click_at = ((click_us - START) as f64 * f64::from(rate) / 1e6 - frames_from_start).round() as i64;
                let data = if (0..chunk as i64).contains(&click_at) { impulse(chunk, click_at as usize) } else { vec![0.0; chunk * 2] };
                core.push(InputId::PcMic, rate, &data, t);
                out.extend(core.render_ready(t + arrive_late + 10_000, &UNITY));
                t = START + ((i + 1) as f64 * chunk as f64 * 1e6 / f64::from(rate)) as i64;
            }
            out.extend(core.render_ready(t + 200_000, &UNITY));
            let peak_at = first_peak(&out, 0.3).expect("impulso no resultado");
            eprintln!("{rate} Hz: impulso a {} µs do instante de captura", peak_at - click_us);
            assert!((peak_at - click_us).abs() < 500, "{rate} Hz: impulso a {} µs do instante certo", peak_at - click_us);
        }
    }

    #[test]
    fn clock_drift_is_absorbed_without_resyncs_or_gaps() {
        // Entrada nominal de 48 kHz que na verdade produz 300 ppm mais amostras por segundo real.
        let mut core = MixerCore::new(START, 60);
        let rate = 48_000u32;
        let chunk = 480;
        let real_rate = 48_000.0 * (1.0 + 300e-6);
        let mut phase = 0.0;
        let mut silent_blocks_after_warmup = 0;
        let mut produced = 0usize;
        for _ in 0..3_000 {
            // 30 s
            let t = START + (produced as f64 * 1e6 / real_rate) as i64;
            core.push(InputId::IpadApp, rate, &sine(chunk, rate, 440.0, 0.5, &mut phase), t);
            produced += chunk;
            for block in core.render_ready(t + 10_000, &UNITY) {
                let peak = block.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
                if block.pts_local_us > START + 500_000 && peak < 0.3 {
                    silent_blocks_after_warmup += 1;
                }
            }
        }
        let (stats, buffered_ms, drift_ppm) = core.track_stats(InputId::IpadApp).unwrap();
        eprintln!("deriva compensada {drift_ppm:.0} ppm, fila {buffered_ms:.1} ms, realinhamentos {}", stats.resyncs);
        assert_eq!(stats.resyncs, 0, "a deriva não devia provocar realinhamentos");
        assert_eq!(silent_blocks_after_warmup, 0, "sem falhas de áudio");
        assert!((-400.0..-200.0).contains(&drift_ppm), "deriva compensada: {drift_ppm:.0} ppm");
        assert!(buffered_ms < 80.0, "a fila não cresce: {buffered_ms:.1} ms");
    }

    #[test]
    fn gain_ramps_and_mute_silences() {
        let mut core = MixerCore::new(START, 20);
        let mut phase = 0.0;
        let mut gains = UNITY;
        let mut peaks = Vec::new();
        for i in 0..100 {
            let t = START + i * 10_000;
            core.push(InputId::PcMic, 48_000, &sine(480, 48_000, 1_000.0, 0.5, &mut phase), t);
            if i == 40 {
                gains[InputId::PcMic as usize] = 0.5; // −6 dB
            }
            if i == 70 {
                gains[InputId::PcMic as usize] = 0.0; // mute
            }
            for b in core.render_ready(t + 10_000, &gains) {
                peaks.push((b.pts_local_us, b.samples.iter().fold(0f32, |m, s| m.max(s.abs()))));
            }
        }
        let peak_near = |t: i64| peaks.iter().min_by_key(|(p, _)| (p - t).abs()).unwrap().1;
        assert!((peak_near(START + 200_000) - 0.5).abs() < 0.02);
        assert!((peak_near(START + 550_000) - 0.25).abs() < 0.02);
        assert!(peak_near(START + 900_000) < 1e-4);
    }

    #[test]
    fn late_and_discontinuous_input_is_handled() {
        let mut core = MixerCore::new(START, 30);
        let mut phase = 0.0;
        // 0,5 s normal, depois um salto de 300 ms (pausa no iPad), depois normal.
        let mut t = START;
        for i in 0..100 {
            if i == 50 {
                t += 300_000;
            }
            core.push(InputId::IpadApp, 48_000, &sine(480, 48_000, 440.0, 0.5, &mut phase), t);
            core.render_ready(t + 10_000, &UNITY);
            t += 10_000;
        }
        // Um pacote muito antigo chega fora de tempo: é descartado, não volta atrás no tempo.
        core.push(InputId::IpadApp, 48_000, &sine(480, 48_000, 440.0, 0.5, &mut phase), START);
        let (stats, _, _) = core.track_stats(InputId::IpadApp).unwrap();
        assert!(stats.resyncs >= 2, "pausa e pacote antigo realinham: {stats:?}");
        assert!(stats.late_frames > 0, "{stats:?}");
    }

    #[test]
    fn soft_clip_never_exceeds_full_scale() {
        for x in [-5.0f32, -1.0, -0.95, 0.0, 0.5, 0.9, 0.95, 1.2, 10.0] {
            let y = soft_clip(x);
            assert!(y.abs() <= 1.0 && y.signum() == x.signum() || x == 0.0, "{x} → {y}");
        }
        assert_eq!(soft_clip(0.5), 0.5);
    }
}
