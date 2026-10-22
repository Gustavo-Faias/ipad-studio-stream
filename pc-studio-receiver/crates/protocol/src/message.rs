use std::io::{self, IoSlice, Write};

use bytes::{BufMut, Bytes, BytesMut};

use crate::payload::{AudioFlags, Payload, VideoFlags};
use crate::{HEADER_LEN, Header, MessageType, ProtocolError};

/// Uma mensagem PSWP: cabeçalho + payload partilhado (clonar é barato, não copia bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub header: Header,
    pub payload: Bytes,
}

impl Message {
    /// Mensagem de um tipo conhecido com flags/seq/aux/pts a zero (ver `with_*`).
    pub fn new(msg_type: MessageType, payload: impl Into<Bytes>) -> Self {
        Self::raw(msg_type as u8, payload)
    }

    /// Mensagem com um tipo em bruto (inclui tipos desconhecidos/experimentais).
    pub fn raw(msg_type: u8, payload: impl Into<Bytes>) -> Self {
        let payload = payload.into();
        let header = Header { msg_type, payload_len: payload.len() as u32, ..Header::default() };
        Self { header, payload }
    }

    /// Mensagem a partir de um payload tipado (binário ou JSON).
    pub fn from_payload<P: Payload>(payload: &P) -> Self {
        Self::new(P::TYPE, payload.encode_payload())
    }

    /// `VIDEO_FRAME` com um access unit Anexo-B.
    pub fn video_frame(au: Bytes, flags: VideoFlags, seq: u32, aux: u32, pts_us: u64) -> Self {
        Self::new(MessageType::VideoFrame, au).with_flags(flags.to_u8()).with_seq(seq).with_aux(aux).with_pts(pts_us)
    }

    /// `AUDIO_FRAME` com PCM intercalado.
    pub fn audio_frame(pcm: Bytes, flags: AudioFlags, seq: u32, aux: u32, pts_us: u64) -> Self {
        Self::new(MessageType::AudioFrame, pcm).with_flags(flags.to_u8()).with_seq(seq).with_aux(aux).with_pts(pts_us)
    }

    pub fn with_flags(mut self, flags: u8) -> Self {
        self.header.flags = flags;
        self
    }

    pub fn with_seq(mut self, seq: u32) -> Self {
        self.header.seq = seq;
        self
    }

    pub fn with_aux(mut self, aux: u32) -> Self {
        self.header.aux = aux;
        self
    }

    pub fn with_pts(mut self, pts_us: u64) -> Self {
        self.header.pts_us = pts_us;
        self
    }

    pub fn kind(&self) -> Option<MessageType> {
        self.header.kind()
    }

    /// Descodifica o payload como `P`, validando o tipo da mensagem.
    pub fn decode_payload<P: Payload>(&self) -> Result<P, ProtocolError> {
        if self.header.msg_type != P::TYPE as u8 {
            return Err(ProtocolError::UnexpectedType(self.header.msg_type));
        }
        P::decode_payload(&self.payload)
    }

    pub fn video_flags(&self) -> VideoFlags {
        VideoFlags::from_u8(self.header.flags)
    }

    pub fn audio_flags(&self) -> AudioFlags {
        AudioFlags::from_u8(self.header.flags)
    }

    pub fn encoded_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }

    /// Cabeçalho com `payload_len` coerente com o payload atual.
    fn wire_header(&self) -> [u8; HEADER_LEN] {
        Header { payload_len: self.payload.len() as u32, ..self.header }.to_bytes()
    }

    pub fn encode_into(&self, out: &mut BytesMut) {
        out.reserve(self.encoded_len());
        out.put_slice(&self.wire_header());
        out.put_slice(&self.payload);
    }

    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&self.wire_header());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Escreve cabeçalho + payload numa única chamada vetorizada (sem copiar o payload).
    pub fn write_to<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        let header = self.wire_header();
        let mut bufs = [IoSlice::new(&header), IoSlice::new(&self.payload)];
        let mut slices: &mut [IoSlice<'_>] = &mut bufs;
        while !slices.is_empty() {
            match w.write_vectored(slices) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => IoSlice::advance_slices(&mut slices, n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
