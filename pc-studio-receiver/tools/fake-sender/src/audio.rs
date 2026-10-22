//! Áudio simulado da app: tom de 440 Hz com um bip de 1 kHz nos primeiros 100 ms de cada segundo
//! (útil mais tarde para verificar a sincronização A/V). PCM s16le estéreo a 48 kHz, 1024 frames
//! por pacote (~21,3 ms), como o ReplayKit.

use std::f64::consts::TAU;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::{BufMut, BytesMut};
use padstudio_protocol::payload::{AudioConfig, AudioFormat, AudioSource};

use crate::state::Shared;

pub const CONFIG: AudioConfig =
    AudioConfig { source: AudioSource::APP, format: AudioFormat::S16Le, channels: 2, sample_rate: 48_000 };

const FRAMES_PER_PACKET: usize = 1024;

pub fn run(shared: Arc<Shared>) {
    let rate = u64::from(CONFIG.sample_rate);
    let period = Duration::from_micros(FRAMES_PER_PACKET as u64 * 1_000_000 / rate);
    let mut next = Instant::now();
    let mut sample: u64 = 0;
    let mut phase = 0.0f64;

    loop {
        let mut pcm = BytesMut::with_capacity(FRAMES_PER_PACKET * CONFIG.bytes_per_frame());
        for _ in 0..FRAMES_PER_PACKET {
            let beep = sample % rate < rate / 10;
            let (freq, amp) = if beep { (1000.0, 0.3) } else { (440.0, 0.1) };
            let v = (phase.sin() * amp * f64::from(i16::MAX)) as i16;
            phase = (phase + TAU * freq / rate as f64) % TAU;
            pcm.put_i16_le(v);
            pcm.put_i16_le(v);
            sample += 1;
        }
        let pts = shared.clock.now_us();
        if let Some(client) = shared.current_client() {
            client.on_audio(pcm.freeze(), pts);
        }

        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else if now - next > Duration::from_millis(100) {
            next = now; // atrasou-se muito (ex.: PC suspenso): recomeça o compasso
        }
    }
}
