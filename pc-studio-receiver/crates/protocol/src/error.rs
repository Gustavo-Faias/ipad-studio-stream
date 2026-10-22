/// Erros do protocolo. [`ProtocolError::code`] devolve o código usado nos vetores de teste.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// Os dois primeiros bytes não são "PS": o fluxo está dessincronizado.
    #[error("magic inválido {0:02x?} (esperado \"PS\")")]
    BadMagic([u8; 2]),

    /// `payload_len` acima de 16 MiB.
    #[error("payload demasiado grande: {0} bytes (máximo 16 MiB)")]
    PayloadTooLarge(u32),

    /// Payload binário mais curto do que o mínimo do tipo.
    #[error("payload de {kind} com {len} bytes (mínimo {min})")]
    ShortPayload { kind: &'static str, len: usize, min: usize },

    /// Valor fora do domínio num payload binário (ex.: codec desconhecido).
    #[error("valor inválido em {field}: {value}")]
    InvalidValue { field: &'static str, value: u64 },

    /// Pediu-se para descodificar um payload de um tipo diferente do da mensagem.
    #[error("tipo de mensagem inesperado: 0x{0:02x}")]
    UnexpectedType(u8),

    /// Payload JSON que não corresponde ao esquema do tipo.
    #[error("JSON inválido em {kind}: {message}")]
    Json { kind: &'static str, message: String },
}

impl ProtocolError {
    /// Código estável (igual ao campo `error` dos vetores inválidos).
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadMagic(_) => "bad_magic",
            Self::PayloadTooLarge(_) => "payload_too_large",
            Self::ShortPayload { .. } | Self::InvalidValue { .. } | Self::UnexpectedType(_) | Self::Json { .. } => {
                "bad_payload"
            }
        }
    }

    /// Erros que obrigam a fechar a ligação (o fluxo de bytes deixou de ser fiável).
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::BadMagic(_) | Self::PayloadTooLarge(_))
    }
}
