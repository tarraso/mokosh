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

/// Result type for envelope operations
pub type EnvelopeResult<T> = std::result::Result<T, EnvelopeError>;

#[derive(Error, Debug, Clone, PartialEq)]
pub enum ProtocolError {
    #[error("Invalid state transition from {from} to {to}")]
    InvalidStateTransition {
        from: crate::state::ConnectionState,
        to: crate::state::ConnectionState,
    },

    #[error("Protocol version mismatch: client [{client_min}..{client_max}], server [{server_min}..{server_max}]")]
    VersionMismatch {
        client_min: u16,
        client_max: u16,
        server_min: u16,
        server_max: u16,
    },

    #[error("Envelope error: {0}")]
    Envelope(#[from] EnvelopeError),

    #[error("JSON serialization error: {0}")]
    JsonError(String),
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        ProtocolError::JsonError(e.to_string())
    }
}

/// Result type for protocol-level operations
pub type Result<T> = std::result::Result<T, ProtocolError>;
