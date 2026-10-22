//! Escritor Matroska (MKV) para H.264/HEVC + PCM, sem dependências.
//!
//! - [`Mode::Live`]: tamanhos "desconhecidos" e escrita imediata, para alimentar o FFmpeg pelo stdin
//!   (passo 2f) sem latência extra.
//! - [`Mode::File`]: clusters com tamanho conhecido, `Cues` (índice de keyframes) e `Duration`
//!   escritos no fim, para um ficheiro com seek. Se o processo morrer a meio, o ficheiro continua
//!   legível: o segmento fica com tamanho "desconhecido".
//!
//! O vídeo vai em AVCC/HVCC (prefixos de tamanho de 4 bytes) com `CodecPrivate` avcC/hvcC, como
//! manda a especificação. Timestamps em unidades de 100 µs (`TimestampScale` = 100 000 ns), para
//! não arredondar 120 fps a milissegundos.

use std::io::{self, Seek, SeekFrom, Write};

use padstudio_protocol::annexb;
use padstudio_protocol::payload::{AudioConfig, AudioFormat, Codec, Rotation, VideoConfig};

/// Nanossegundos por unidade de tempo do ficheiro.
pub const TIMESTAMP_SCALE_NS: u64 = 100_000;
const TICK_US: i64 = (TIMESTAMP_SCALE_NS / 1_000) as i64;
/// Um cluster novo a cada keyframe ou, no máximo, a cada 2 s.
const MAX_CLUSTER_TICKS: i64 = 2_000_000 / TICK_US;
const SEEKHEAD_RESERVED: usize = 96;
const UNKNOWN_SIZE: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const FLAG_LACING: u32 = 0x9C;
    pub const LANGUAGE: u32 = 0x22_B59C;
    pub const NAME: u32 = 0x536E;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const PROJECTION: u32 = 0x7670;
    pub const PROJECTION_TYPE: u32 = 0x7671;
    pub const PROJECTION_POSE_ROLL: u32 = 0x7675;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const BIT_DEPTH: u32 = 0x6264;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
    pub const VOID: u32 = 0xEC;
}

// ── EBML ─────────────────────────────────────────────────────────────────────

fn put_id(out: &mut Vec<u8>, id: u32) {
    let b = id.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap_or(3);
    out.extend_from_slice(&b[skip..]);
}

/// Tamanho em VINT com o menor comprimento possível (o valor "todos a 1" é reservado).
fn put_size(out: &mut Vec<u8>, size: u64) {
    let n = (1..=8usize).find(|&n| size < (1u64 << (7 * n)) - 1).expect("tamanho EBML < 2^56");
    let v = size | (1u64 << (7 * n));
    out.extend_from_slice(&v.to_be_bytes()[8 - n..]);
}

fn el(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    put_id(out, id);
    put_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn el_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let b = v.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap_or(7);
    el(out, id, &b[skip..]);
}

fn el_float(out: &mut Vec<u8>, id: u32, v: f64) {
    el(out, id, &v.to_be_bytes());
}

fn el_str(out: &mut Vec<u8>, id: u32, s: &str) {
    el(out, id, s.as_bytes());
}

fn master(out: &mut Vec<u8>, id: u32, body: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    body(&mut inner);
    el(out, id, &inner);
}

/// Elemento `Void` com exatamente `total` bytes (mínimo 2, máximo 128).
fn put_void(out: &mut Vec<u8>, total: usize) {
    assert!((2..=128).contains(&total), "Void de {total} bytes");
    put_id(out, id::VOID);
    put_size(out, (total - 2) as u64);
    out.resize(out.len() + total - 2, 0);
}

// ── Faixas ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrack {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    /// avcC / hvcC.
    pub codec_private: Vec<u8>,
    pub rotation: Rotation,
    pub color_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl VideoTrack {
    /// A partir de uma `VIDEO_CONFIG`; `None` se os parâmetros estiverem incompletos.
    pub fn from_config(cfg: &VideoConfig, rotation: Rotation) -> Option<Self> {
        let codec_private = match cfg.codec {
            Codec::H264 => avcc_from_annexb(&cfg.param_sets)?,
            Codec::Hevc => hvcc_from_annexb(&cfg.param_sets, cfg.bit_depth)?,
        };
        Some(Self {
            codec: cfg.codec,
            width: cfg.width,
            height: cfg.height,
            codec_private,
            rotation,
            color_primaries: cfg.color_primaries,
            transfer: cfg.transfer,
            matrix: cfg.matrix,
            full_range: cfg.full_range,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub format: AudioFormat,
}

impl AudioTrack {
    pub fn from_config(cfg: &AudioConfig, name: impl Into<String>) -> Self {
        Self { name: name.into(), sample_rate: cfg.sample_rate, channels: cfg.channels, format: cfg.format }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Track {
    Video(VideoTrack),
    Audio(AudioTrack),
}

/// `ProjectionPoseRoll` para que os leitores rodem a imagem `rotation` graus no sentido horário.
/// (Sinal validado contra `ffmpeg -display_rotation` + `ffprobe`; ver testes de integração.)
pub fn projection_roll(rotation: Rotation) -> f64 {
    match rotation {
        Rotation::R0 => 0.0,
        Rotation::R90 => -90.0,
        Rotation::R180 => 180.0,
        Rotation::R270 => 90.0,
    }
}

fn write_track_entry(out: &mut Vec<u8>, number: u64, track: &Track) {
    master(out, id::TRACK_ENTRY, |e| {
        el_uint(e, id::TRACK_NUMBER, number);
        el_uint(e, id::TRACK_UID, 0x5AD0_0000 + number);
        el_uint(e, id::FLAG_LACING, 0);
        el_str(e, id::LANGUAGE, "und");
        match track {
            Track::Video(v) => {
                el_uint(e, id::TRACK_TYPE, 1);
                el_str(e, id::CODEC_ID, match v.codec {
                    Codec::H264 => "V_MPEG4/ISO/AVC",
                    Codec::Hevc => "V_MPEGH/ISO/HEVC",
                });
                el(e, id::CODEC_PRIVATE, &v.codec_private);
                master(e, id::VIDEO, |vid| {
                    el_uint(vid, id::PIXEL_WIDTH, v.width.into());
                    el_uint(vid, id::PIXEL_HEIGHT, v.height.into());
                    master(vid, id::COLOUR, |c| {
                        el_uint(c, id::MATRIX_COEFFICIENTS, v.matrix.into());
                        el_uint(c, id::RANGE, if v.full_range { 2 } else { 1 });
                        el_uint(c, id::TRANSFER_CHARACTERISTICS, v.transfer.into());
                        el_uint(c, id::PRIMARIES, v.color_primaries.into());
                    });
                    if v.rotation != Rotation::R0 {
                        master(vid, id::PROJECTION, |p| {
                            el_uint(p, id::PROJECTION_TYPE, 0); // retangular
                            el_float(p, id::PROJECTION_POSE_ROLL, projection_roll(v.rotation));
                        });
                    }
                });
            }
            Track::Audio(a) => {
                el_uint(e, id::TRACK_TYPE, 2);
                el_str(e, id::NAME, &a.name);
                let (codec, bits) = match a.format {
                    AudioFormat::S16Le => ("A_PCM/INT/LIT", 16),
                    AudioFormat::F32Le => ("A_PCM/FLOAT/IEEE", 32),
                };
                el_str(e, id::CODEC_ID, codec);
                master(e, id::AUDIO, |au| {
                    el_float(au, id::SAMPLING_FREQUENCY, f64::from(a.sample_rate));
                    el_uint(au, id::CHANNELS, a.channels.into());
                    el_uint(au, id::BIT_DEPTH, bits);
                });
            }
        }
    });
}

// ── Escritor ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    File,
}

struct Cluster {
    ticks: i64,
    /// Corpo do cluster (só em `Mode::File`; em `Live` os blocos vão direto para a saída).
    body: Vec<u8>,
    starts_with_keyframe: bool,
}

pub struct MkvWriter<W: Write> {
    w: W,
    mode: Mode,
    pos: u64,
    segment_size_pos: u64,
    segment_data_start: u64,
    seekhead_pos: u64,
    duration_pos: u64,
    info_rel: u64,
    tracks_rel: u64,
    video_track: Option<u64>,
    cluster: Option<Cluster>,
    cues: Vec<(i64, u64)>,
    max_ticks: i64,
    /// Último timestamp por faixa (índice = número da faixa), para os manter estritamente crescentes.
    last_ticks: Vec<Option<i64>>,
    blocks: u64,
    scratch: Vec<u8>,
}

impl<W: Write> MkvWriter<W> {
    /// Escreve o cabeçalho com as faixas (numeradas a partir de 1, pela ordem dada).
    pub fn new(mut w: W, mode: Mode, tracks: &[Track], app: &str) -> io::Result<Self> {
        let mut h = Vec::with_capacity(1024);
        master(&mut h, id::EBML, |e| {
            el_uint(e, id::EBML_VERSION, 1);
            el_uint(e, id::EBML_READ_VERSION, 1);
            el_uint(e, id::EBML_MAX_ID_LENGTH, 4);
            el_uint(e, id::EBML_MAX_SIZE_LENGTH, 8);
            el_str(e, id::DOC_TYPE, "matroska");
            el_uint(e, id::DOC_TYPE_VERSION, 4);
            el_uint(e, id::DOC_TYPE_READ_VERSION, 2);
        });

        put_id(&mut h, id::SEGMENT);
        let segment_size_pos = h.len() as u64;
        h.extend_from_slice(&UNKNOWN_SIZE);
        let segment_data_start = h.len() as u64;

        let seekhead_pos = h.len() as u64;
        if mode == Mode::File {
            put_void(&mut h, SEEKHEAD_RESERVED); // o SeekHead é escrito aqui no fim
        }

        let info_rel = h.len() as u64 - segment_data_start;
        let mut info = Vec::new();
        el_uint(&mut info, id::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
        el_str(&mut info, id::MUXING_APP, app);
        el_str(&mut info, id::WRITING_APP, app);
        let duration_offset = (mode == Mode::File).then(|| {
            put_id(&mut info, id::DURATION);
            put_size(&mut info, 8);
            let off = info.len();
            info.extend_from_slice(&0f64.to_be_bytes());
            off
        });
        put_id(&mut h, id::INFO);
        put_size(&mut h, info.len() as u64);
        let duration_pos = duration_offset.map_or(0, |off| (h.len() + off) as u64);
        h.extend_from_slice(&info);

        let tracks_rel = h.len() as u64 - segment_data_start;
        master(&mut h, id::TRACKS, |t| {
            for (i, track) in tracks.iter().enumerate() {
                write_track_entry(t, i as u64 + 1, track);
            }
        });

        w.write_all(&h)?;
        let video_track = tracks.iter().position(|t| matches!(t, Track::Video(_))).map(|i| i as u64 + 1);
        Ok(Self {
            w,
            mode,
            pos: h.len() as u64,
            segment_size_pos,
            segment_data_start,
            seekhead_pos,
            duration_pos,
            info_rel,
            tracks_rel,
            video_track,
            cluster: None,
            cues: Vec::new(),
            max_ticks: 0,
            last_ticks: vec![None; tracks.len() + 1],
            blocks: 0,
            scratch: Vec::new(),
        })
    }

    fn write_raw(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    /// Escreve um bloco. `ts_us` é relativo ao início do ficheiro; `data` já no formato final.
    /// Dentro de cada faixa os timestamps ficam estritamente crescentes: dois blocos no mesmo
    /// instante (fontes que entregam em rajadas) ficam separados por 1 unidade (100 µs).
    pub fn write_block(&mut self, track: u64, ts_us: i64, keyframe: bool, data: &[u8]) -> io::Result<()> {
        let mut ticks = ts_us.div_euclid(TICK_US).max(0);
        if let Some(slot) = self.last_ticks.get_mut(track as usize) {
            if let Some(last) = *slot {
                ticks = ticks.max(last + 1);
            }
            *slot = Some(ticks);
        }
        let video_key = keyframe && Some(track) == self.video_track;
        let new_cluster = match &self.cluster {
            None => true,
            Some(c) => video_key || ticks - c.ticks > MAX_CLUSTER_TICKS,
        };
        if new_cluster {
            self.start_cluster(ticks, video_key)?;
        }
        let cluster_ticks = self.cluster.as_ref().map_or(0, |c| c.ticks);
        let rel = (ticks - cluster_ticks).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;

        let mut hdr = Vec::with_capacity(16);
        put_id(&mut hdr, id::SIMPLE_BLOCK);
        put_size(&mut hdr, 4 + data.len() as u64);
        hdr.push(0x80 | track as u8); // número da faixa em VINT de 1 byte
        hdr.extend_from_slice(&rel.to_be_bytes());
        hdr.push(if keyframe { 0x80 } else { 0 });
        match self.mode {
            Mode::Live => {
                self.write_raw(&hdr)?;
                self.write_raw(data)?;
            }
            Mode::File => {
                let c = self.cluster.as_mut().expect("cluster aberto");
                c.body.extend_from_slice(&hdr);
                c.body.extend_from_slice(data);
            }
        }
        self.max_ticks = self.max_ticks.max(ticks);
        self.blocks += 1;
        Ok(())
    }

    /// Converte um access unit Anexo-B para prefixos de 4 bytes e escreve-o.
    pub fn write_video_annexb(&mut self, track: u64, ts_us: i64, keyframe: bool, au: &[u8]) -> io::Result<()> {
        let mut buf = std::mem::take(&mut self.scratch);
        buf.clear();
        annexb_to_length_prefixed(au, &mut buf);
        let result = self.write_block(track, ts_us, keyframe, &buf);
        self.scratch = buf;
        result
    }

    fn start_cluster(&mut self, ticks: i64, starts_with_keyframe: bool) -> io::Result<()> {
        self.flush_cluster()?;
        let mut head = Vec::new();
        el_uint(&mut head, id::TIMESTAMP, ticks as u64);
        let body = match self.mode {
            Mode::Live => {
                let mut h = Vec::new();
                put_id(&mut h, id::CLUSTER);
                h.extend_from_slice(&UNKNOWN_SIZE);
                h.extend_from_slice(&head);
                self.write_raw(&h)?;
                Vec::new()
            }
            Mode::File => {
                let mut body = Vec::with_capacity(512 * 1024);
                body.extend_from_slice(&head);
                body
            }
        };
        self.cluster = Some(Cluster { ticks, body, starts_with_keyframe });
        Ok(())
    }

    fn flush_cluster(&mut self) -> io::Result<()> {
        let Some(c) = self.cluster.take() else { return Ok(()) };
        if self.mode == Mode::File {
            if c.starts_with_keyframe {
                self.cues.push((c.ticks, self.pos - self.segment_data_start));
            }
            let mut h = Vec::new();
            put_id(&mut h, id::CLUSTER);
            put_size(&mut h, c.body.len() as u64);
            self.write_raw(&h)?;
            self.write_raw(&c.body)?;
        }
        Ok(())
    }

    /// Duração escrita até agora (µs).
    pub fn duration_us(&self) -> i64 {
        self.max_ticks * TICK_US
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Bytes escritos na saída (sem contar o cluster ainda em memória).
    pub fn bytes_written(&self) -> u64 {
        self.pos
    }

    /// Em `Mode::Live`, passa para a saída o que estiver em buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

impl<W: Write + Seek> MkvWriter<W> {
    /// Fecha o ficheiro: último cluster, Cues, tamanho do segmento, Duration e SeekHead.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush_cluster()?;
        if self.mode == Mode::Live {
            self.w.flush()?;
            return Ok(self.w);
        }

        let cues_rel = self.pos - self.segment_data_start;
        let has_cues = !self.cues.is_empty() && self.video_track.is_some();
        if has_cues {
            let track = self.video_track.unwrap_or(1);
            let mut c = Vec::new();
            master(&mut c, id::CUES, |cs| {
                for &(ticks, pos) in &self.cues {
                    master(cs, id::CUE_POINT, |p| {
                        el_uint(p, id::CUE_TIME, ticks as u64);
                        master(p, id::CUE_TRACK_POSITIONS, |t| {
                            el_uint(t, id::CUE_TRACK, track);
                            el_uint(t, id::CUE_CLUSTER_POSITION, pos);
                        });
                    });
                }
            });
            self.write_raw(&c)?;
        }
        let end = self.pos;

        self.w.seek(SeekFrom::Start(self.segment_size_pos))?;
        self.w.write_all(&((1u64 << 56) | (end - self.segment_data_start)).to_be_bytes())?;

        self.w.seek(SeekFrom::Start(self.duration_pos))?;
        self.w.write_all(&(self.max_ticks as f64).to_be_bytes())?;

        let mut sh = Vec::new();
        master(&mut sh, id::SEEK_HEAD, |s| {
            seek_entry(s, id::INFO, self.info_rel);
            seek_entry(s, id::TRACKS, self.tracks_rel);
            if has_cues {
                seek_entry(s, id::CUES, cues_rel);
            }
        });
        let rest = SEEKHEAD_RESERVED - sh.len();
        if rest > 0 {
            put_void(&mut sh, rest);
        }
        self.w.seek(SeekFrom::Start(self.seekhead_pos))?;
        self.w.write_all(&sh)?;

        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

fn seek_entry(out: &mut Vec<u8>, target: u32, pos: u64) {
    master(out, id::SEEK, |s| {
        let mut target_id = Vec::new();
        put_id(&mut target_id, target);
        el(s, id::SEEK_ID, &target_id);
        el_uint(s, id::SEEK_POSITION, pos);
    });
}

// ── Codec ────────────────────────────────────────────────────────────────────

/// Anexo-B → NAL units com prefixo de tamanho de 4 bytes (big-endian).
pub fn annexb_to_length_prefixed(au: &[u8], out: &mut Vec<u8>) {
    for nal in annexb::nals(au) {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
}

/// `AVCDecoderConfigurationRecord` (ISO 14496-15 §5.3.3) a partir de SPS/PPS em Anexo-B.
pub fn avcc_from_annexb(param_sets: &[u8]) -> Option<Vec<u8>> {
    let (mut sps, mut pps) = (Vec::new(), Vec::new());
    for nal in annexb::nals(param_sets) {
        match annexb::nal_type(Codec::H264, nal) {
            7 => sps.push(nal),
            8 => pps.push(nal),
            _ => {}
        }
    }
    let first = *sps.first()?;
    if first.len() < 4 || pps.is_empty() {
        return None;
    }
    let mut out = vec![1, first[1], first[2], first[3], 0xFF, 0xE0 | (sps.len() as u8 & 0x1F)];
    for s in &sps {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(pps.len() as u8);
    for p in &pps {
        out.extend_from_slice(&(p.len() as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    Some(out)
}

/// `HEVCDecoderConfigurationRecord` (ISO 14496-15 §8.3.3) a partir de VPS/SPS/PPS em Anexo-B.
/// Perfil, tier e nível vêm do `profile_tier_level` do SPS; assume 4:2:0.
pub fn hvcc_from_annexb(param_sets: &[u8], bit_depth: u8) -> Option<Vec<u8>> {
    let mut arrays: [(u8, Vec<&[u8]>); 3] = [(32, Vec::new()), (33, Vec::new()), (34, Vec::new())];
    for nal in annexb::nals(param_sets) {
        let t = annexb::nal_type(Codec::Hevc, nal);
        if let Some(a) = arrays.iter_mut().find(|a| a.0 == t) {
            a.1.push(nal);
        }
    }
    let sps = *arrays[1].1.first()?;
    if arrays[2].1.is_empty() {
        return None;
    }
    let rbsp = unescape_rbsp(sps);
    if rbsp.len() < 15 {
        return None;
    }
    // rbsp[0..2] cabeçalho NAL; [2] vps_id | max_sub_layers_minus1 | temporal_id_nesting;
    // [3..15] general profile_tier_level (12 bytes).
    let max_sub_layers = ((rbsp[2] >> 1) & 0x7) + 1;
    let nesting = rbsp[2] & 1;
    let depth = bit_depth.saturating_sub(8) & 0x7;

    let mut out = vec![1, rbsp[3]];
    out.extend_from_slice(&rbsp[4..14]); // compatibility (4) + constraint (6)
    out.push(rbsp[14]); // general_level_idc
    out.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    out.push(0xFC); // parallelismType = 0
    out.push(0xFC | 1); // chromaFormat 4:2:0
    out.push(0xF8 | depth);
    out.push(0xF8 | depth);
    out.extend_from_slice(&[0, 0]); // avgFrameRate
    out.push((max_sub_layers << 3) | (nesting << 2) | 3); // lengthSizeMinusOne = 3
    let present: Vec<_> = arrays.iter().filter(|a| !a.1.is_empty()).collect();
    out.push(present.len() as u8);
    for (nal_type, list) in present {
        out.push(0x80 | nal_type); // array_completeness = 1
        out.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for n in list {
            out.extend_from_slice(&(n.len() as u16).to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Some(out)
}

/// Codec string RFC 6381 para WebCodecs / MSE (ex.: `avc1.640034`, `hvc1.1.6.L93.B0`).
pub fn codec_string(cfg: &VideoConfig) -> Option<String> {
    let find = |t: u8| annexb::nals(&cfg.param_sets).find(|n| annexb::nal_type(cfg.codec, n) == t);
    match cfg.codec {
        Codec::H264 => {
            let sps = find(7).filter(|s| s.len() >= 4)?;
            Some(format!("avc1.{:02x}{:02x}{:02x}", sps[1], sps[2], sps[3]))
        }
        Codec::Hevc => {
            let r = unescape_rbsp(find(33)?);
            if r.len() < 15 {
                return None;
            }
            let (space, tier, profile) = (r[3] >> 6, (r[3] >> 5) & 1, r[3] & 0x1f);
            let compat = u32::from_be_bytes([r[4], r[5], r[6], r[7]]).reverse_bits();
            let mut s = format!(
                "hvc1.{}{profile}.{compat:X}.{}{}",
                ["", "A", "B", "C"][usize::from(space)],
                if tier == 1 { "H" } else { "L" },
                r[14]
            );
            let constraints = &r[8..14];
            if let Some(last) = constraints.iter().rposition(|b| *b != 0) {
                for b in &constraints[..=last] {
                    s.push_str(&format!(".{b:X}"));
                }
            }
            Some(s)
        }
    }
}

/// Remove os bytes de prevenção de emulação (`00 00 03` → `00 00`).
fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Leitor EBML mínimo para validar a estrutura escrita.
    fn read_vint(b: &[u8], at: usize) -> (u64, usize, bool) {
        let first = b[at];
        let len = first.leading_zeros() as usize + 1;
        let mut v = u64::from(first) & ((1u64 << (8 - len)) - 1);
        let mut all_ones = v == (1u64 << (8 - len)) - 1;
        for i in 1..len {
            v = (v << 8) | u64::from(b[at + i]);
            all_ones &= b[at + i] == 0xFF;
        }
        (v, len, all_ones)
    }

    fn read_id(b: &[u8], at: usize) -> (u32, usize) {
        let len = b[at].leading_zeros() as usize + 1;
        let mut v = 0u32;
        for i in 0..len {
            v = (v << 8) | u32::from(b[at + i]);
        }
        (v, len)
    }

    /// Filho EBML: id, início do elemento, início do corpo, tamanho do corpo.
    #[derive(Debug, Clone, Copy)]
    struct Child {
        id: u32,
        start: usize,
        body: usize,
        size: usize,
    }

    fn children(b: &[u8], mut at: usize, end: usize) -> Vec<Child> {
        let mut out = Vec::new();
        while at < end {
            let (id, il) = read_id(b, at);
            let (size, sl, unknown) = read_vint(b, at + il);
            let body = at + il + sl;
            let size = if unknown { end - body } else { size as usize };
            out.push(Child { id, start: at, body, size });
            at = body + size;
        }
        out
    }

    fn inner(b: &[u8], c: &Child) -> Vec<Child> {
        children(b, c.body, c.body + c.size)
    }

    fn uint(b: &[u8], c: &Child) -> u64 {
        b[c.body..c.body + c.size].iter().fold(0, |a, &x| (a << 8) | u64::from(x))
    }

    fn sample_tracks() -> Vec<Track> {
        let ps = [&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x34, 0xAC][..], &[0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0xB0]].concat();
        let cfg = VideoConfig {
            codec: Codec::H264,
            bit_depth: 8,
            fps_max: 120,
            width: 1920,
            height: 1342,
            bitrate_bps: 30_000_000,
            color_primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            param_sets: ps.into(),
        };
        vec![
            Track::Video(VideoTrack::from_config(&cfg, Rotation::R90).unwrap()),
            Track::Audio(AudioTrack { name: "app".into(), sample_rate: 48_000, channels: 2, format: AudioFormat::S16Le }),
        ]
    }

    #[test]
    fn avcc_matches_spec_layout() {
        let ps = [&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x34, 0xAC][..], &[0, 0, 1, 0x68, 0xEE, 0x3C]].concat();
        let avcc = avcc_from_annexb(&ps).unwrap();
        assert_eq!(avcc, [1, 0x64, 0x00, 0x34, 0xFF, 0xE1, 0, 5, 0x67, 0x64, 0x00, 0x34, 0xAC, 1, 0, 3, 0x68, 0xEE, 0x3C]);
        assert!(avcc_from_annexb(&[0, 0, 0, 1, 0x68, 0xEE]).is_none(), "sem SPS");
    }

    #[test]
    fn codec_strings_for_webcodecs() {
        let h264 = VideoConfig {
            codec: Codec::H264,
            bit_depth: 8,
            fps_max: 120,
            width: 1920,
            height: 1342,
            bitrate_bps: 0,
            color_primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
            param_sets: [&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x34, 0xAC][..], &[0, 0, 0, 1, 0x68, 0xEE]].concat().into(),
        };
        assert_eq!(codec_string(&h264).as_deref(), Some("avc1.640034"));
        let sps = [0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xB0, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5D];
        let hevc = VideoConfig {
            codec: Codec::Hevc,
            param_sets: [&[0, 0, 0, 1, 0x40, 0x01, 0x0C][..], &[0, 0, 0, 1], &sps, &[0, 0, 0, 1, 0x44, 0x01, 0xC1]].concat().into(),
            ..h264
        };
        assert_eq!(codec_string(&hevc).as_deref(), Some("hvc1.1.6.L93.B0"));
    }

    #[test]
    fn hvcc_reads_profile_tier_level_after_unescaping() {
        // SPS: cabeçalho 42 01; 01 = vps 0, 1 sub-layer, nesting 1; PTL: 01 (Main),
        // compat 60 00 00 00, constraint 90 00 00 00 00 00 (com EPB), level 0x5D (93 = 3.1).
        let sps = [0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x5D];
        let ps = [&[0, 0, 0, 1, 0x40, 0x01, 0x0C][..], &[0, 0, 0, 1], &sps, &[0, 0, 0, 1, 0x44, 0x01, 0xC1]].concat();
        let h = hvcc_from_annexb(&ps, 8).unwrap();
        assert_eq!(h[0], 1);
        assert_eq!(h[1], 0x01, "profile_space/tier/profile_idc");
        assert_eq!(&h[2..6], &[0x60, 0, 0, 0], "compat");
        assert_eq!(&h[6..12], &[0x90, 0, 0, 0, 0, 0], "constraint sem EPB");
        assert_eq!(h[12], 0x5D, "level");
        assert_eq!(h[21], (1 << 3) | (1 << 2) | 3);
        assert_eq!(h[22], 3, "VPS, SPS, PPS");
        assert_eq!(h[23], 0x80 | 32);
    }

    #[test]
    fn file_mode_has_consistent_structure_and_patched_sizes() {
        let tracks = sample_tracks();
        let mut w = MkvWriter::new(Cursor::new(Vec::new()), Mode::File, &tracks, "teste").unwrap();
        let key = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        let p = [0, 0, 0, 1, 0x41, 0x9A];
        for i in 0..250i64 {
            let ts = i * 8_333; // 120 fps
            w.write_video_annexb(1, ts, i % 120 == 0, if i % 120 == 0 { &key } else { &p }).unwrap();
            if i % 3 == 0 {
                w.write_block(2, ts, true, &[0u8; 64]).unwrap();
            }
        }
        let bytes = w.finish().unwrap().into_inner();

        let top = children(&bytes, 0, bytes.len());
        assert_eq!(top.iter().map(|c| c.id).collect::<Vec<_>>(), [id::EBML, id::SEGMENT]);
        let segment = top[1];
        assert_eq!(segment.body + segment.size, bytes.len(), "tamanho do segmento corrigido no fim");

        let seg = inner(&bytes, &segment);
        let ids: Vec<u32> = seg.iter().map(|c| c.id).collect();
        assert_eq!(&ids[..4], &[id::SEEK_HEAD, id::VOID, id::INFO, id::TRACKS]);
        assert_eq!(seg[0].start + 96, seg[2].start, "SeekHead + Void ocupam exatamente o espaço reservado");
        assert_eq!(*ids.last().unwrap(), id::CUES);
        let clusters: Vec<&Child> = seg.iter().filter(|c| c.id == id::CLUSTER).collect();
        assert_eq!(clusters.len(), 3, "um cluster por keyframe (0, 120, 240)");

        // Cues apontam para os clusters (posição relativa ao início dos dados do segmento).
        let points = inner(&bytes, seg.last().unwrap());
        assert_eq!(points.len(), 3);
        for (point, cluster) in points.iter().zip(&clusters) {
            let ctp = inner(&bytes, point).into_iter().find(|c| c.id == id::CUE_TRACK_POSITIONS).unwrap();
            let pos = inner(&bytes, &ctp).into_iter().find(|c| c.id == id::CUE_CLUSTER_POSITION).unwrap();
            assert_eq!(uint(&bytes, &pos) as usize, cluster.start - segment.body);
        }

        // O SeekHead aponta para Info, Tracks e Cues.
        let seeks = inner(&bytes, &seg[0]);
        let targets: Vec<(u64, u64)> = seeks
            .iter()
            .map(|s| {
                let f = inner(&bytes, s);
                (uint(&bytes, &f[0]), uint(&bytes, &f[1]))
            })
            .collect();
        let rel = |id: u32| (seg.iter().find(|c| c.id == id).unwrap().start - segment.body) as u64;
        assert_eq!(targets, [
            (u64::from(id::INFO), rel(id::INFO)),
            (u64::from(id::TRACKS), rel(id::TRACKS)),
            (u64::from(id::CUES), rel(id::CUES)),
        ]);

        // Duration = último timestamp (249 × 8,333 ms) em unidades de 100 µs.
        let info = seg.iter().find(|c| c.id == id::INFO).unwrap();
        let dur = inner(&bytes, info).into_iter().find(|c| c.id == id::DURATION).unwrap();
        let d = f64::from_be_bytes(bytes[dur.body..dur.body + 8].try_into().unwrap());
        assert_eq!(d, (249 * 8_333 / 100) as f64);
    }

    #[test]
    fn live_mode_streams_unknown_sized_clusters() {
        let tracks = sample_tracks();
        let mut w = MkvWriter::new(Vec::new(), Mode::Live, &tracks, "teste").unwrap();
        w.write_video_annexb(1, 0, true, &[0, 0, 0, 1, 0x65, 0x88]).unwrap();
        w.write_block(2, 1_000, true, &[1, 2, 3, 4]).unwrap();
        let header_and_blocks = w.bytes_written();
        let bytes = w.w;
        assert_eq!(bytes.len() as u64, header_and_blocks, "nada fica retido em memória");
        // O segmento e o cluster têm tamanho desconhecido.
        let segment_pos = bytes.windows(4).position(|x| x == [0x18, 0x53, 0x80, 0x67]).unwrap();
        assert_eq!(&bytes[segment_pos + 4..segment_pos + 12], &UNKNOWN_SIZE);
        let cluster_pos = bytes.windows(4).position(|x| x == [0x1F, 0x43, 0xB6, 0x75]).unwrap();
        assert_eq!(&bytes[cluster_pos + 4..cluster_pos + 12], &UNKNOWN_SIZE);
    }

    #[test]
    fn timestamps_are_strictly_increasing_per_track() {
        let tracks = sample_tracks();
        let mut w = MkvWriter::new(Vec::new(), Mode::Live, &tracks, "teste").unwrap();
        let au = [0, 0, 0, 1, 0x41, 0x9A];
        w.write_video_annexb(1, 0, true, &au).unwrap();
        w.write_video_annexb(1, 30, false, &au).unwrap(); // mesmo tick (100 µs) que o anterior
        w.write_video_annexb(1, 50, false, &au).unwrap();
        w.write_block(2, 0, true, &[0; 4]).unwrap(); // outra faixa: independente
        assert_eq!(w.last_ticks[1], Some(2));
        assert_eq!(w.last_ticks[2], Some(0));
    }

    #[test]
    fn vint_sizes_are_minimal_and_avoid_reserved_values() {
        for (v, expected) in [(0u64, vec![0x80]), (126, vec![0xFE]), (127, vec![0x40, 0x7F]), (16_382, vec![0x7F, 0xFE])] {
            let mut out = Vec::new();
            put_size(&mut out, v);
            assert_eq!(out, expected, "{v}");
        }
    }
}
