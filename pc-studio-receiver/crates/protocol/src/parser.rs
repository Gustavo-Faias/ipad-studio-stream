use std::io::{self, Read};

use bytes::BytesMut;

use crate::header::check_magic;
use crate::{HEADER_LEN, Header, Message, ProtocolError};

const READ_CHUNK: usize = 64 * 1024;

/// Parser incremental: aceita bytes em pedaços de qualquer tamanho e devolve mensagens completas.
///
/// Os payloads devolvidos partilham a memória do buffer interno ([`bytes::Bytes`]), por isso a
/// extração não copia dados. Depois de um erro, a ligação deve ser fechada.
pub struct StreamParser {
    buf: BytesMut,
    scratch: Box<[u8]>,
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamParser {
    pub fn new() -> Self {
        Self { buf: BytesMut::with_capacity(256 * 1024), scratch: vec![0u8; READ_CHUNK].into_boxed_slice() }
    }

    /// Acrescenta bytes recebidos.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Lê uma vez de `r` para o buffer interno. Devolve 0 em fim de fluxo.
    pub fn read_from<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<usize> {
        let n = r.read(&mut self.scratch)?;
        self.buf.extend_from_slice(&self.scratch[..n]);
        Ok(n)
    }

    /// Acesso direto ao buffer (ex.: `tokio::io::AsyncReadExt::read_buf`, que escreve sem cópia extra).
    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buf
    }

    /// Próxima mensagem completa, `None` se faltarem bytes.
    pub fn next_message(&mut self) -> Result<Option<Message>, ProtocolError> {
        if self.buf.len() >= 2 {
            check_magic(&self.buf)?; // falha cedo, sem esperar pelos 24 bytes
        }
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let header = Header::decode(self.buf[..HEADER_LEN].try_into().expect("24 bytes"))?;
        let total = HEADER_LEN + header.payload_len as usize;
        if self.buf.len() < total {
            self.buf.reserve(total - self.buf.len());
            return Ok(None);
        }
        let mut frame = self.buf.split_to(total);
        let payload = frame.split_off(HEADER_LEN).freeze();
        Ok(Some(Message { header, payload }))
    }

    /// Bytes recebidos que ainda não formam uma mensagem completa.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}
