//! Junta vídeo e áudio por ordem de tempo antes de os escrever para o FFmpeg.
//!
//! O vídeo chega quase em tempo real; a mistura de áudio chega ~60 ms depois (a latência do
//! mixer). O vídeo fica retido até haver áudio até ao seu instante, para o Matroska sair bem
//! intercalado. Só quando o áudio deixa mesmo de chegar (mixer parado) é que, ao fim de
//! `hold_max`, o vídeo segue e o buraco é preenchido com silêncio, para o FFmpeg nunca ficar à
//! espera de uma faixa vazia. Se este destino estiver atrasado (FFmpeg abaixo do tempo real), o
//! áudio continua a chegar e nunca é trocado por silêncio.

use std::collections::VecDeque;

use bytes::Bytes;

/// Formato da faixa de áudio: f32le estéreo 48 kHz.
pub(crate) const AUDIO_RATE: i64 = 48_000;
const BYTES_PER_FRAME: usize = 2 * 4;
const SILENCE_BLOCK_FRAMES: i64 = AUDIO_RATE / 100; // 10 ms

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Video { ts_us: i64, keyframe: bool, data: Bytes },
    Audio { ts_us: i64, data: Bytes },
}

pub(crate) struct Interleaver {
    video: VecDeque<(i64, bool, Bytes)>,
    audio: VecDeque<(i64, Bytes)>,
    /// Fim (µs) do último áudio entregue.
    audio_end: Option<i64>,
    /// Quando (relógio de `drain`) chegou o último pacote de áudio.
    last_audio_arrival: Option<i64>,
    hold_max_us: i64,
    pub silence_blocks: u64,
}

fn audio_duration_us(data: &[u8]) -> i64 {
    (data.len() / BYTES_PER_FRAME) as i64 * 1_000_000 / AUDIO_RATE
}

impl Interleaver {
    pub fn new(hold_max_us: i64) -> Self {
        Self {
            video: VecDeque::new(),
            audio: VecDeque::new(),
            audio_end: None,
            last_audio_arrival: None,
            hold_max_us,
            silence_blocks: 0,
        }
    }

    pub fn push_video(&mut self, ts_us: i64, keyframe: bool, data: Bytes) {
        if ts_us >= 0 {
            self.video.push_back((ts_us, keyframe, data));
        }
    }

    /// `now_us`: instante de chegada, no mesmo relógio que o `now_us` de [`Interleaver::drain`].
    pub fn push_audio(&mut self, ts_us: i64, data: Bytes, now_us: i64) {
        self.last_audio_arrival = Some(now_us);
        if ts_us < 0 {
            return;
        }
        // Já coberto por silêncio gerado enquanto o áudio não chegava.
        if self.audio_end.is_some_and(|end| ts_us + audio_duration_us(&data) <= end) {
            return;
        }
        self.audio.push_back((ts_us, data));
    }

    #[cfg(test)]
    pub fn queued(&self) -> (usize, usize) {
        (self.video.len(), self.audio.len())
    }

    fn emit_audio(&mut self, ts_us: i64, data: Bytes, out: &mut Vec<Item>) {
        self.audio_end = Some(ts_us + audio_duration_us(&data));
        out.push(Item::Audio { ts_us, data });
    }

    /// Silêncio desde o fim do áudio (ou desde `up_to`, se ainda não houve áudio) até `up_to`.
    fn fill_silence(&mut self, up_to: i64, out: &mut Vec<Item>) {
        let block = Bytes::from(vec![0u8; SILENCE_BLOCK_FRAMES as usize * BYTES_PER_FRAME]);
        let mut t = self.audio_end.unwrap_or(up_to);
        while t < up_to {
            self.emit_audio(t, block.clone(), out);
            self.silence_blocks += 1;
            t = self.audio_end.unwrap_or(up_to);
        }
        if self.audio_end.is_none() {
            // Primeiro silêncio: um bloco a começar no instante do vídeo.
            self.emit_audio(up_to, block, out);
            self.silence_blocks += 1;
        }
    }

    /// Entrega, por ordem de tempo, tudo o que já pode ser escrito. `now_us` é relativo à origem.
    pub fn drain(&mut self, now_us: i64, out: &mut Vec<Item>) {
        loop {
            let a = self.audio.front().map(|x| x.0);
            let v = self.video.front().map(|x| x.0);
            match (a, v) {
                (Some(at), Some(vt)) if at <= vt => {
                    let (ts, data) = self.audio.pop_front().expect("frente");
                    self.emit_audio(ts, data, out);
                }
                (_, Some(vt)) => {
                    let covered = self.audio_end.is_some_and(|end| end >= vt);
                    // Já há áudio posterior em fila: o áudio deste instante não vai chegar.
                    let skipped = a.is_some_and(|at| at > vt);
                    // Só se o áudio deixou de chegar (não se este destino estiver atrasado).
                    let audio_absent = self.last_audio_arrival.is_none_or(|t| now_us - t > self.hold_max_us);
                    let timed_out = audio_absent && vt < now_us - self.hold_max_us;
                    if !(covered || skipped || timed_out) {
                        break;
                    }
                    if timed_out && !covered && !skipped {
                        self.fill_silence(vt, out);
                    }
                    let (ts_us, keyframe, data) = self.video.pop_front().expect("frente");
                    out.push(Item::Video { ts_us, keyframe, data });
                }
                (Some(_), None) => {
                    let (ts, data) = self.audio.pop_front().expect("frente");
                    self.emit_audio(ts, data, out);
                }
                (None, None) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(ms: usize) -> Bytes {
        Bytes::from(vec![1u8; ms * 48 * BYTES_PER_FRAME])
    }

    fn labels(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Video { ts_us, .. } => format!("v{}", ts_us / 1000),
                Item::Audio { ts_us, data } if data.iter().all(|b| *b == 0) => format!("s{}", ts_us / 1000),
                Item::Audio { ts_us, .. } => format!("a{}", ts_us / 1000),
            })
            .collect()
    }

    #[test]
    fn video_waits_for_lagging_audio_then_interleaves() {
        let mut il = Interleaver::new(250_000);
        let mut out = Vec::new();
        il.push_video(0, true, Bytes::new());
        il.push_video(25_000, false, Bytes::new());
        il.drain(20_000, &mut out);
        assert!(out.is_empty(), "sem áudio ainda: o vídeo espera");

        il.push_audio(0, audio(10), 70_000);
        il.push_audio(10_000, audio(10), 75_000);
        il.drain(80_000, &mut out);
        assert_eq!(labels(&out), ["a0", "v0", "a10"], "v25 espera por áudio até 25 ms (só há até 20)");

        il.push_audio(20_000, audio(10), 85_000);
        out.clear();
        il.drain(90_000, &mut out);
        assert_eq!(labels(&out), ["a20", "v25"]);
    }

    #[test]
    fn lagging_destination_never_replaces_real_audio_with_silence() {
        // O FFmpeg deste destino está 2 s atrasado: o vídeo em fila é "antigo", mas o áudio
        // continua a chegar. Não se pode inventar silêncio por cima do áudio verdadeiro.
        let mut il = Interleaver::new(250_000);
        let mut out = Vec::new();
        let now = 2_000_000;
        il.push_video(0, true, Bytes::new());
        il.push_video(16_000, false, Bytes::new());
        il.push_audio(0, audio(10), now - 5_000);
        il.drain(now, &mut out);
        assert_eq!(labels(&out), ["a0", "v0"], "v16 espera pelo áudio, mesmo atrasado");
        il.push_audio(10_000, audio(10), now + 1_000);
        il.push_audio(20_000, audio(10), now + 2_000);
        out.clear();
        il.drain(now + 3_000, &mut out);
        assert_eq!(labels(&out), ["a10", "v16", "a20"]);
        assert_eq!(il.silence_blocks, 0);
    }

    #[test]
    fn missing_audio_is_filled_with_silence_after_hold() {
        let mut il = Interleaver::new(250_000);
        let mut out = Vec::new();
        il.push_video(0, true, Bytes::new());
        il.push_video(30_000, false, Bytes::new());
        il.drain(100_000, &mut out);
        assert!(out.is_empty());
        il.drain(300_000, &mut out); // 300 ms depois: o mixer não está a mandar áudio
        assert_eq!(labels(&out), ["s0", "v0", "s10", "s20", "v30"]);
        assert_eq!(il.silence_blocks, 3);

        // O áudio real que chegar depois e já estiver coberto pelo silêncio é descartado.
        il.push_audio(0, audio(10), 310_000);
        assert_eq!(il.queued().1, 0);
    }

    #[test]
    fn later_audio_releases_earlier_video() {
        let mut il = Interleaver::new(250_000);
        let mut out = Vec::new();
        il.push_audio(0, audio(10), 0);
        il.drain(0, &mut out);
        il.push_video(40_000, true, Bytes::new()); // o áudio de 10–40 ms perdeu-se
        il.push_audio(50_000, audio(10), 44_000);
        out.clear();
        il.drain(45_000, &mut out);
        assert_eq!(labels(&out), ["v40", "a50"]);
    }
}
