//! Utilitários Anexo-B (H.264 / HEVC): dividir access units em NAL units e classificá-las.

use bytes::{BufMut, BytesMut};

use crate::payload::Codec;

/// Start code de 4 bytes usado no fio.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Itera as NAL units de um buffer Anexo-B (sem start codes nem zeros finais).
pub fn nals(data: &[u8]) -> NalIter<'_> {
    let pos = find_start_code(data, 0).map_or(data.len(), |i| i + 3);
    NalIter { data, pos }
}

pub struct NalIter<'a> {
    data: &'a [u8],
    /// Início da próxima NAL (logo a seguir a um start code).
    pos: usize,
}

impl<'a> Iterator for NalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        while self.pos < self.data.len() {
            let start = self.pos;
            let mut end = match find_start_code(self.data, start) {
                Some(i) => {
                    self.pos = i + 3;
                    i
                }
                None => {
                    self.pos = self.data.len();
                    self.data.len()
                }
            };
            while end > start && self.data[end - 1] == 0 {
                end -= 1; // zero inicial de um start code de 4 bytes, ou trailing_zero_8bits
            }
            if end > start {
                return Some(&self.data[start..end]);
            }
        }
        None
    }
}

/// Índice do próximo `00 00 01` a partir de `from`.
pub fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    if data.len() < 3 {
        return None;
    }
    (from..data.len() - 2).find(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
}

/// Tipo da NAL: H.264 → 5 bits baixos; HEVC → bits 1..6 do primeiro byte.
pub fn nal_type(codec: Codec, nal: &[u8]) -> u8 {
    match (codec, nal.first()) {
        (_, None) => 0,
        (Codec::H264, Some(b)) => b & 0x1f,
        (Codec::Hevc, Some(b)) => (b >> 1) & 0x3f,
    }
}

/// VPS / SPS / PPS.
pub fn is_param_set(codec: Codec, nal_type: u8) -> bool {
    match codec {
        Codec::H264 => matches!(nal_type, 7 | 8),
        Codec::Hevc => matches!(nal_type, 32..=34),
    }
}

/// IDR (H.264) / IRAP (HEVC): ponto onde um decoder pode começar.
pub fn is_keyframe(codec: Codec, nal_type: u8) -> bool {
    match codec {
        Codec::H264 => nal_type == 5,
        Codec::Hevc => (16..=21).contains(&nal_type),
    }
}

/// Access Unit Delimiter (não vai no fio).
pub fn is_aud(codec: Codec, nal_type: u8) -> bool {
    match codec {
        Codec::H264 => nal_type == 9,
        Codec::Hevc => nal_type == 35,
    }
}

/// Acrescenta uma NAL com start code de 4 bytes.
pub fn put_nal(out: &mut BytesMut, nal: &[u8]) {
    out.reserve(START_CODE.len() + nal.len());
    out.put_slice(&START_CODE);
    out.put_slice(nal);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mixed_start_codes_and_keeps_emulation_prevention() {
        // 4 bytes, 3 bytes, NAL com 00 00 03 (não é start code), zeros finais.
        let data = [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB, 0, 0, 0, 1, 0x65, 0, 0, 3, 0x01, 0, 0];
        let got: Vec<&[u8]> = nals(&data).collect();
        assert_eq!(got, vec![&[0x67, 0xAA][..], &[0x68, 0xBB][..], &[0x65, 0, 0, 3, 0x01][..]]);
    }

    #[test]
    fn empty_and_garbage_inputs() {
        assert_eq!(nals(&[]).count(), 0);
        assert_eq!(nals(&[1, 2, 3]).count(), 0);
        assert_eq!(nals(&[0, 0, 1]).count(), 0);
    }

    #[test]
    fn classifies_hevc() {
        assert!(is_keyframe(Codec::Hevc, nal_type(Codec::Hevc, &[0x26, 0x01]))); // IDR_W_RADL (19)
        assert!(is_param_set(Codec::Hevc, nal_type(Codec::Hevc, &[0x40, 0x01]))); // VPS (32)
        assert!(is_aud(Codec::Hevc, nal_type(Codec::Hevc, &[0x46, 0x01]))); // AUD (35)
    }
}
