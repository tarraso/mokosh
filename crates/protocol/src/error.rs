use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq)]
pub enum EnvelopeError {
    #[error("Invalid envelope: {0}")]
    Invalid(String),

    #[error("Payload length mismatch: expected {expected}, got {actual}")]
    PayloadLengthMismatch { expected: u32, actual: usize },

    #[error("Buffer too short: need {need} bytes, have {have}")]
    BufferTooShort { need: usize, have: usize },

    #[error("Unsupported protocol version: {0}")]
    UnsupportedVersion(u16),

    #[error("Invalid flags: {0}")]
    InvalidFlags(u8),
}

pub type Result<T> = std::result::Result<T, EnvelopeError>;
