//! Encoder simulado: FFmpeg (`testsrc2`) → Anexo-B no stdout → access units prontos a enviar.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::{Bytes, BytesMut};
use padstudio_protocol::annexb;
use padstudio_protocol::payload::Codec;

use crate::state::{SenderConfig, Shared};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    Nvenc,
    Amf,
    Qsv,
    Software,
}

impl EncoderKind {
    pub fn ffmpeg_name(self, codec: Codec) -> &'static str {
        match (self, codec) {
            (Self::Nvenc, Codec::H264) => "h264_nvenc",
            (Self::Nvenc, Codec::Hevc) => "hevc_nvenc",
            (Self::Amf, Codec::H264) => "h264_amf",
            (Self::Amf, Codec::Hevc) => "hevc_amf",
            (Self::Qsv, Codec::H264) => "h264_qsv",
            (Self::Qsv, Codec::Hevc) => "hevc_qsv",
            (Self::Software, Codec::H264) => "libx264",
            (Self::Software, Codec::Hevc) => "libx265",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC (NVIDIA)",
            Self::Amf => "AMF (AMD)",
            Self::Qsv => "Quick Sync (Intel)",
            Self::Software => "software (x264/x265) — sem encoder de hardware disponível",
        }
    }
}

/// Escolhe o primeiro encoder de hardware que consegue codificar 1 frame pequeno.
pub fn detect(ffmpeg: &Path) -> anyhow::Result<EncoderKind> {
    Command::new(ffmpeg)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("não encontrei o FFmpeg em \"{}\" (usa --ffmpeg <caminho>)", ffmpeg.display()))?;

    for kind in [EncoderKind::Nvenc, EncoderKind::Amf, EncoderKind::Qsv] {
        let ok = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=320x240:d=0.1"])
            .args(["-c:v", kind.ffmpeg_name(Codec::H264), "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(kind);
        }
        log!("{} indisponível", kind.label());
    }
    Ok(EncoderKind::Software)
}

/// Argumentos do FFmpeg: fonte de teste em tempo real, encoder de baixa latência sem B-frames e
/// keyframe a cada segundo, em Anexo-B cru no stdout.
pub fn ffmpeg_args(kind: EncoderKind, cfg: &SenderConfig) -> Vec<String> {
    let (w, h) = cfg.output_size();
    let fps = cfg.fps.max(1);
    let bitrate = cfg.bitrate_bps.to_string();
    let bufsize = (cfg.bitrate_bps / fps * 2).max(100_000).to_string(); // ~2 frames: limita picos

    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-re", "-f", "lavfi", "-i"]);
    push(&[&format!("testsrc2=size={w}x{h}:rate={fps}")]);
    push(&["-pix_fmt", "yuv420p", "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]);
    push(&["-color_range", "tv", "-c:v", kind.ffmpeg_name(cfg.codec)]);
    match kind {
        EncoderKind::Nvenc => push(&["-preset", "p1", "-tune", "ull", "-zerolatency", "1", "-delay", "0", "-rc", "cbr", "-forced-idr", "1"]),
        EncoderKind::Amf => push(&["-usage", "ultralowlatency", "-quality", "speed", "-rc", "cbr", "-header_insertion_mode", "idr"]),
        EncoderKind::Qsv => push(&["-preset", "veryfast", "-async_depth", "1"]),
        EncoderKind::Software => match cfg.codec {
            Codec::H264 => push(&["-preset", "ultrafast", "-tune", "zerolatency"]),
            Codec::Hevc => push(&[
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x265-params",
                &format!("bframes=0:keyint={fps}:repeat-headers=1:log-level=error"),
            ]),
        },
    }
    push(&["-bf", "0", "-g", &fps.to_string(), "-b:v", &bitrate, "-maxrate", &bitrate, "-bufsize", &bufsize]);
    let format = match cfg.codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
    };
    push(&["-flush_packets", "1", "-f", format, "pipe:1"]);
    a
}

/// Mantém o FFmpeg a correr; reinicia-o quando a configuração muda ou quando termina sozinho.
pub fn supervise(shared: Arc<Shared>) {
    loop {
        let generation = shared.encoder_generation.load(Ordering::SeqCst);
        let cfg = shared.config.lock().unwrap().clone();
        let (w, h) = cfg.output_size();
        log!(
            "encoder: {} {w}x{h} @ {} fps, {:.1} Mbps",
            shared.encoder_kind.ffmpeg_name(cfg.codec),
            cfg.fps,
            f64::from(cfg.bitrate_bps) / 1e6
        );
        let started = Instant::now();
        if let Err(e) = run(&shared, &cfg, generation) {
            log!("encoder: {e:#}");
        }
        if shared.encoder_generation.load(Ordering::SeqCst) != generation {
            continue; // reconfiguração pedida
        }
        let wait = if started.elapsed() < Duration::from_secs(3) { 5 } else { 1 };
        log!("encoder: o FFmpeg terminou; a reiniciar em {wait} s");
        thread::sleep(Duration::from_secs(wait));
    }
}

fn run(shared: &Shared, cfg: &SenderConfig, generation: u64) -> anyhow::Result<()> {
    let mut child = Command::new(&shared.opts.ffmpeg)
        .args(ffmpeg_args(shared.encoder_kind, cfg))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("não foi possível arrancar \"{}\"", shared.opts.ffmpeg.display()))?;
    let mut stdout = child.stdout.take().expect("stdout com pipe");
    *shared.encoder_child.lock().unwrap() = Some(child);
    if shared.encoder_generation.load(Ordering::SeqCst) != generation {
        shared.restart_encoder(); // a reconfiguração chegou entre o spawn e o registo
    }

    let mut nals = NalStream::default();
    let mut au = AuBuilder::new(cfg.codec);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        nals.push(&buf[..n], |nal| {
            if let Some(done) = au.push_nal(nal) {
                shared.on_encoded_au(done, cfg, generation);
            }
        });
        if let Some(done) = nals.pending_head().and_then(|head| au.peek(head)) {
            shared.on_encoded_au(done, cfg, generation);
        }
    }

    if let Some(mut child) = shared.encoder_child.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Divide um fluxo Anexo-B contínuo em NAL units completas.
#[derive(Default)]
pub struct NalStream {
    buf: Vec<u8>,
    /// Início da NAL atual (logo a seguir ao start code).
    nal_start: Option<usize>,
    /// Primeira posição ainda não verificada como início de start code.
    scan: usize,
}

impl NalStream {
    pub fn push(&mut self, data: &[u8], mut on_nal: impl FnMut(&[u8])) {
        self.buf.extend_from_slice(data);
        let mut i = self.scan;
        while i + 3 <= self.buf.len() {
            if self.buf[i] == 0 && self.buf[i + 1] == 0 && self.buf[i + 2] == 1 {
                if let Some(start) = self.nal_start {
                    let mut end = i;
                    while end > start && self.buf[end - 1] == 0 {
                        end -= 1;
                    }
                    if end > start {
                        on_nal(&self.buf[start..end]);
                    }
                }
                i += 3;
                self.nal_start = Some(i);
            } else {
                i += 1;
            }
        }
        self.scan = i;
        let keep_from = self.nal_start.unwrap_or(self.scan);
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.scan -= keep_from;
            self.nal_start = self.nal_start.map(|s| s - keep_from);
        }
    }

    /// Primeiros bytes da NAL que ainda está a chegar (para decidir cedo se começa um AU novo).
    pub fn pending_head(&self) -> Option<&[u8]> {
        self.nal_start.map(|s| &self.buf[s..]).filter(|head| !head.is_empty())
    }
}

/// Um access unit pronto para `VIDEO_FRAME`.
pub struct EncodedAu {
    /// Anexo-B com start codes de 4 bytes, sem AUD; keyframes começam pelos parâmetros.
    pub data: Bytes,
    pub keyframe: bool,
    /// Parâmetros (VPS/SPS/PPS) quando é keyframe.
    pub param_sets: Option<Bytes>,
}

/// Junta NAL units em access units. Um AU novo começa no primeiro slice de uma imagem
/// (`first_mb_in_slice == 0` em H.264, `first_slice_segment_in_pic_flag` em HEVC) ou nas NAL
/// não-VCL que o precedem (AUD, parâmetros, SEI). Não depende de AUD nem de filtros do FFmpeg.
/// Garante parâmetros in-band em cada keyframe, como o iPad tem de fazer (o VideoToolbox não os põe).
///
/// Consequência: um AU só sai quando chegam os primeiros bytes do seguinte (~1 frame de atraso,
/// só no simulador; o iPad recebe cada frame completo num callback do VideoToolbox).
pub struct AuBuilder {
    codec: Codec,
    au: BytesMut,
    params: BytesMut,
    keyframe: bool,
    has_slices: bool,
    cached_params: Option<Bytes>,
}

impl AuBuilder {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            au: BytesMut::new(),
            params: BytesMut::new(),
            keyframe: false,
            has_slices: false,
            cached_params: None,
        }
    }

    fn is_slice(&self, nal_type: u8) -> bool {
        match self.codec {
            Codec::H264 => (1..=5).contains(&nal_type),
            Codec::Hevc => nal_type < 32,
        }
    }

    /// A NAL (ou o seu início) abre um access unit novo?
    fn starts_new_au(&self, nal: &[u8]) -> bool {
        let t = annexb::nal_type(self.codec, nal);
        match (self.codec, self.is_slice(t)) {
            // O 1.º bit do cabeçalho do slice a 1 ⇔ first_mb_in_slice = 0 (ue(v)).
            (Codec::H264, true) => nal.get(1).is_some_and(|b| b & 0x80 != 0),
            // SEI, AUD, SPS, PPS antes do 1.º slice.
            (Codec::H264, false) => matches!(t, 6..=9),
            // first_slice_segment_in_pic_flag é o 1.º bit depois dos 2 bytes de cabeçalho.
            (Codec::Hevc, true) => nal.get(2).is_some_and(|b| b & 0x80 != 0),
            // VPS, SPS, PPS, AUD, SEI de prefixo.
            (Codec::Hevc, false) => matches!(t, 32..=35 | 39),
        }
    }

    /// Fecha já o AU atual se a NAL que está a chegar (ainda incompleta) abre um novo.
    pub fn peek(&mut self, head: &[u8]) -> Option<EncodedAu> {
        if self.has_slices && self.starts_new_au(head) { self.finish() } else { None }
    }

    /// Acrescenta uma NAL completa; devolve o AU anterior se esta abrir um novo.
    pub fn push_nal(&mut self, nal: &[u8]) -> Option<EncodedAu> {
        let finished = if self.has_slices && self.starts_new_au(nal) { self.finish() } else { None };
        let t = annexb::nal_type(self.codec, nal);
        let is_slice = self.is_slice(t);

        if annexb::is_aud(self.codec, t) {
            return finished; // o AUD não vai no fio
        }
        if annexb::is_param_set(self.codec, t) {
            annexb::put_nal(&mut self.params, nal);
        }
        if is_slice {
            self.has_slices = true;
            self.keyframe |= annexb::is_keyframe(self.codec, t);
        }
        annexb::put_nal(&mut self.au, nal);
        finished
    }

    fn finish(&mut self) -> Option<EncodedAu> {
        self.has_slices = false;
        let keyframe = std::mem::take(&mut self.keyframe);
        let params = std::mem::take(&mut self.params).freeze();
        let au = std::mem::take(&mut self.au);
        if au.is_empty() {
            return None;
        }
        if !keyframe {
            return Some(EncodedAu { data: au.freeze(), keyframe, param_sets: None });
        }
        if !params.is_empty() {
            self.cached_params = Some(params.clone());
            return Some(EncodedAu { data: au.freeze(), keyframe, param_sets: Some(params) });
        }
        // Keyframe sem parâmetros: acrescenta os últimos conhecidos à frente.
        let cached = self.cached_params.clone();
        let mut data = BytesMut::with_capacity(cached.as_ref().map_or(0, Bytes::len) + au.len());
        if let Some(p) = &cached {
            data.extend_from_slice(p);
        }
        data.extend_from_slice(&au);
        Some(EncodedAu { data: data.freeze(), keyframe, param_sets: cached })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUD: [u8; 2] = [0x09, 0xF0];

    fn stream(nals: &[&[u8]]) -> Vec<u8> {
        nals.iter().flat_map(|n| [&[0u8, 0, 0, 1][..], n].concat()).collect()
    }

    fn split(codec: Codec, data: &[u8], chunk: usize) -> Vec<EncodedAu> {
        let mut nals = NalStream::default();
        let mut builder = AuBuilder::new(codec);
        let mut aus = Vec::new();
        for c in data.chunks(chunk) {
            nals.push(c, |nal| aus.extend(builder.push_nal(nal)));
            aus.extend(nals.pending_head().and_then(|head| builder.peek(head)));
        }
        aus
    }

    #[test]
    fn h264_splits_on_first_slice_and_reinjects_params() {
        let sps: &[u8] = &[0x67, 0x64, 0x00, 0x34];
        let pps: &[u8] = &[0x68, 0xEE];
        let idr_a: &[u8] = &[0x65, 0x88, 0x10]; // first_mb_in_slice = 0
        let idr_b: &[u8] = &[0x65, 0x40, 0x22]; // 2.º slice do mesmo frame (first_mb ≠ 0)
        let p: &[u8] = &[0x41, 0x9A];
        let idr2: &[u8] = &[0x65, 0x99];
        let next: &[u8] = &[0x41, 0x9B]; // só serve para fechar o AU anterior
        let data = stream(&[&AUD, sps, pps, idr_a, idr_b, &AUD, p, idr2, next]);

        for chunk in [1, 2, 3, 5, 7, 64] {
            let aus = split(Codec::H264, &data, chunk);
            assert_eq!(aus.len(), 3, "pedaços de {chunk}");
            assert!(aus[0].keyframe && aus[0].param_sets.is_some());
            assert_eq!(aus[0].data.as_ref(), stream(&[sps, pps, idr_a, idr_b]).as_slice(), "AUD removido, 2 slices juntos");
            assert!(!aus[1].keyframe);
            assert_eq!(aus[1].data.as_ref(), stream(&[p]).as_slice());
            // O segundo IDR veio sem SPS/PPS: o builder repõe-nos à frente.
            assert!(aus[2].keyframe);
            assert_eq!(aus[2].data.as_ref(), stream(&[sps, pps, idr2]).as_slice());
        }
    }

    #[test]
    fn hevc_splits_on_first_slice_segment() {
        let vps: &[u8] = &[0x40, 0x01, 0x0C];
        let sps: &[u8] = &[0x42, 0x01, 0x01];
        let pps: &[u8] = &[0x44, 0x01, 0xC1];
        let idr: &[u8] = &[0x26, 0x01, 0xAF]; // IDR_W_RADL, first_slice_segment_in_pic_flag = 1
        let trail: &[u8] = &[0x02, 0x01, 0xD0]; // TRAIL_R
        let suffix_sei: &[u8] = &[0x50, 0x01, 0x05]; // SEI de sufixo: pertence ao AU atual
        let next: &[u8] = &[0x02, 0x01, 0xD1];
        let aus = split(Codec::Hevc, &stream(&[vps, sps, pps, idr, suffix_sei, trail, next]), 4);
        assert_eq!(aus.len(), 2);
        assert!(aus[0].keyframe);
        assert_eq!(aus[0].param_sets.as_deref(), Some(stream(&[vps, sps, pps]).as_slice()));
        assert_eq!(aus[0].data.as_ref(), stream(&[vps, sps, pps, idr, suffix_sei]).as_slice());
        assert_eq!(aus[1].data.as_ref(), stream(&[trail]).as_slice());
    }
}
