//! Sincronização de relógio estilo NTP com PING/PONG (PROTOCOL.md §4.2 e §6).
//!
//! O originador (normalmente o PC) envia `PING{t0}`; o respondente (iPad) responde
//! `PONG{t0, t1, t2}`; o originador regista `t3` ao receber. Fica a amostra de menor RTT numa
//! janela deslizante, que é a que tem menos ruído de filas.

use std::collections::VecDeque;

use crate::payload::Pong;

/// Uma medição. `offset_us`: relógio_respondente ≈ relógio_originador + offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSample {
    pub rtt_us: i64,
    pub offset_us: i64,
}

impl ClockSample {
    pub fn from_pong(pong: &Pong, t3: u64) -> Self {
        let (t0, t1, t2, t3) = (pong.t0 as i128, pong.t1 as i128, pong.t2 as i128, t3 as i128);
        let rtt = (t3 - t0) - (t2 - t1);
        let offset = ((t1 - t0) + (t2 - t3)) / 2;
        Self { rtt_us: rtt as i64, offset_us: offset as i64 }
    }
}

#[derive(Debug, Clone)]
pub struct ClockSync {
    window: usize,
    samples: VecDeque<ClockSample>,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self::new(16)
    }
}

impl ClockSync {
    pub fn new(window: usize) -> Self {
        Self { window: window.max(1), samples: VecDeque::with_capacity(window) }
    }

    /// Regista um PONG recebido no instante local `t3` (µs).
    pub fn add(&mut self, pong: &Pong, t3: u64) -> ClockSample {
        let sample = ClockSample::from_pong(pong, t3);
        if self.samples.len() == self.window {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        sample
    }

    /// Amostra de menor RTT na janela.
    pub fn best(&self) -> Option<ClockSample> {
        self.samples.iter().copied().min_by_key(|s| s.rtt_us)
    }

    /// Converte um instante do relógio remoto (respondente) para o relógio local, em µs.
    pub fn remote_to_local(&self, remote_us: u64) -> Option<i64> {
        self.best().map(|s| remote_us as i64 - s.offset_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_offset_and_prefers_lowest_rtt() {
        // Relógio remoto = local + 1 000 000 µs. Ida 200 µs, volta 200 µs, processamento 50 µs.
        let mut sync = ClockSync::new(4);
        let s = sync.add(&Pong { t0: 1_000, t1: 1_001_200, t2: 1_001_250 }, 1_450);
        assert_eq!(s, ClockSample { rtt_us: 400, offset_us: 1_000_000 });

        // Amostra assimétrica (fila na volta): RTT maior, offset enviesado, é ignorada.
        sync.add(&Pong { t0: 5_000, t1: 1_005_200, t2: 1_005_250 }, 9_450);
        assert_eq!(sync.best().unwrap().offset_us, 1_000_000);
        assert_eq!(sync.remote_to_local(2_000_000), Some(1_000_000));
    }
}
