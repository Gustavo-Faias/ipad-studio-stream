//! Conversão de PCM para f32 estéreo intercalado (o formato interno do mixer).

use padstudio_protocol::payload::{AudioConfig, AudioFormat};

/// Junta os canais de um frame em estéreo: mono duplica, estéreo mantém, mais canais fazem a média.
#[inline]
pub(crate) fn frame_to_stereo(frame: &[f32]) -> (f32, f32) {
    match frame.len() {
        0 => (0.0, 0.0),
        1 => (frame[0], frame[0]),
        2 => (frame[0], frame[1]),
        n => {
            let m = frame.iter().sum::<f32>() / n as f32;
            (m, m)
        }
    }
}

/// PCM do iPad (`AUDIO_FRAME`) → f32 estéreo.
pub(crate) fn ipad_pcm_to_stereo(cfg: &AudioConfig, data: &[u8], out: &mut Vec<f32>) {
    let channels = usize::from(cfg.channels.max(1));
    out.clear();
    match cfg.format {
        AudioFormat::S16Le => {
            let samples: Vec<f32> =
                data.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0).collect();
            push_frames(&samples, channels, out);
        }
        AudioFormat::F32Le => {
            let samples: Vec<f32> = data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            push_frames(&samples, channels, out);
        }
    }
}

pub(crate) fn push_frames(samples: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.reserve(samples.len() / channels.max(1) * 2);
    for frame in samples.chunks_exact(channels.max(1)) {
        let (l, r) = frame_to_stereo(frame);
        out.push(l);
        out.push(r);
    }
}

/// f32 → bytes little-endian (o formato de `MixedAudio`).
pub(crate) fn f32_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn linear_to_db(v: f32) -> f32 {
    if v <= 1e-6 { -120.0 } else { 20.0 * v.log10() }
}

#[cfg(test)]
mod tests {
    use padstudio_protocol::payload::AudioSource;

    use super::*;

    #[test]
    fn converts_s16_mono_and_multichannel() {
        let cfg = AudioConfig { source: AudioSource::APP, format: AudioFormat::S16Le, channels: 1, sample_rate: 48_000 };
        let mut out = Vec::new();
        ipad_pcm_to_stereo(&cfg, &[0x00, 0x40, 0x00, 0xC0], &mut out); // +0,5 e −0,5
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5]);

        let mut out = Vec::new();
        push_frames(&[0.2, 0.4, 0.6, 0.8], 4, &mut out); // 4 canais → média
        assert!((out[0] - 0.5).abs() < 1e-6 && out.len() == 2);
    }

    #[test]
    fn db_roundtrip() {
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 1e-4);
        assert!((linear_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(linear_to_db(0.0), -120.0);
    }
}
