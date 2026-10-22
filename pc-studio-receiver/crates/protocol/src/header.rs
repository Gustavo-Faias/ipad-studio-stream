use crate::ProtocolError;

/// Bytes iniciais de cada mensagem: "PS".
pub const MAGIC: [u8; 2] = *b"PS";

/// Tamanho fixo do cabeçalho.
pub const HEADER_LEN: usize = 24;

/// Tamanho máximo de um payload (16 MiB).
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// Tipos de mensagem conhecidos (PROTOCOL.md §3). Tipos desconhecidos são ignorados pelo recetor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    Hello = 0x01,
    HelloAck = 0x02,
    Ping = 0x03,
    Pong = 0x04,
    Bye = 0x05,
    Error = 0x06,
    SenderState = 0x07,
    VideoConfig = 0x10,
    VideoFrame = 0x11,
    AudioConfig = 0x12,
    AudioFrame = 0x13,
    SenderStats = 0x20,
    ReceiverReport = 0x21,
    Configure = 0x30,
    ConfigureAck = 0x31,
    RequestKeyframe = 0x32,
    SetBitrate = 0x33,
}

impl MessageType {
    pub const ALL: [MessageType; 17] = [
        Self::Hello,
        Self::HelloAck,
        Self::Ping,
        Self::Pong,
        Self::Bye,
        Self::Error,
        Self::SenderState,
        Self::VideoConfig,
        Self::VideoFrame,
        Self::AudioConfig,
        Self::AudioFrame,
        Self::SenderStats,
        Self::ReceiverReport,
        Self::Configure,
        Self::ConfigureAck,
        Self::RequestKeyframe,
        Self::SetBitrate,
    ];

    pub fn from_u8(value: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|t| *t as u8 == value)
    }

    /// Nome tal como aparece na especificação (ex.: `"VIDEO_FRAME"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Hello => "HELLO",
            Self::HelloAck => "HELLO_ACK",
            Self::Ping => "PING",
            Self::Pong => "PONG",
            Self::Bye => "BYE",
            Self::Error => "ERROR",
            Self::SenderState => "SENDER_STATE",
            Self::VideoConfig => "VIDEO_CONFIG",
            Self::VideoFrame => "VIDEO_FRAME",
            Self::AudioConfig => "AUDIO_CONFIG",
            Self::AudioFrame => "AUDIO_FRAME",
            Self::SenderStats => "SENDER_STATS",
            Self::ReceiverReport => "RECEIVER_REPORT",
            Self::Configure => "CONFIGURE",
            Self::ConfigureAck => "CONFIGURE_ACK",
            Self::RequestKeyframe => "REQUEST_KEYFRAME",
            Self::SetBitrate => "SET_BITRATE",
        }
    }

    /// Tipos cujo payload é um objeto JSON.
    pub fn is_json(self) -> bool {
        matches!(
            self,
            Self::Hello
                | Self::HelloAck
                | Self::Bye
                | Self::Error
                | Self::SenderState
                | Self::SenderStats
                | Self::ReceiverReport
                | Self::Configure
                | Self::ConfigureAck
        )
    }
}

/// Cabeçalho de 24 bytes, little-endian (PROTOCOL.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    /// Tipo em bruto (pode ser desconhecido; ver [`Header::kind`]).
    pub msg_type: u8,
    pub flags: u8,
    pub payload_len: u32,
    pub seq: u32,
    pub aux: u32,
    pub pts_us: u64,
}

impl Header {
    /// Tipo conhecido, ou `None` para tipos a ignorar.
    pub fn kind(&self) -> Option<MessageType> {
        MessageType::from_u8(self.msg_type)
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC);
        b[2] = self.msg_type;
        b[3] = self.flags;
        b[4..8].copy_from_slice(&self.payload_len.to_le_bytes());
        b[8..12].copy_from_slice(&self.seq.to_le_bytes());
        b[12..16].copy_from_slice(&self.aux.to_le_bytes());
        b[16..24].copy_from_slice(&self.pts_us.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; HEADER_LEN]) -> Result<Self, ProtocolError> {
        check_magic(&b[..2])?;
        let payload_len = read_u32(b, 4);
        if payload_len > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }
        Ok(Self {
            msg_type: b[2],
            flags: b[3],
            payload_len,
            seq: read_u32(b, 8),
            aux: read_u32(b, 12),
            pts_us: read_u64(b, 16),
        })
    }
}

/// Valida os dois primeiros bytes. `b` tem de ter pelo menos 2 bytes.
pub(crate) fn check_magic(b: &[u8]) -> Result<(), ProtocolError> {
    if b[..2] == MAGIC { Ok(()) } else { Err(ProtocolError::BadMagic([b[0], b[1]])) }
}

pub(crate) fn read_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

pub(crate) fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

pub(crate) fn read_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}
