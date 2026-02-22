//! Control messages for Mokosh protocol
//!
//! Control messages (route_id < 100) handle protocol-level operations:
//! - HELLO/HELLO_OK/HELLO_ERROR: version negotiation and connection establishment
//! - AUTH_REQUEST/AUTH_RESPONSE: authentication (future)
//! - DISCONNECT: graceful disconnection
//! - PING/PONG: keepalive and latency measurement (future)

use serde::{Deserialize, Serialize};

/// Control message route IDs (< 100)
pub mod routes {
    /// HELLO message (Client → Server): initial handshake
    pub const HELLO: u16 = 1;

    /// HELLO_OK message (Server → Client): handshake accepted
    pub const HELLO_OK: u16 = 2;

    /// HELLO_ERROR message (Server → Client): handshake rejected
    pub const HELLO_ERROR: u16 = 3;

    /// AUTH_REQUEST message (Client → Server): authentication request (future)
    pub const AUTH_REQUEST: u16 = 10;

    /// AUTH_RESPONSE message (Server → Client): authentication response (future)
    pub const AUTH_RESPONSE: u16 = 11;

    /// DISCONNECT message (bidirectional): graceful disconnection (future)
    pub const DISCONNECT: u16 = 20;

    /// PING message (bidirectional): keepalive request (future)
    pub const PING: u16 = 30;

    /// PONG message (bidirectional): keepalive response (future)
    pub const PONG: u16 = 31;
}

/// Game messages start from route_id >= 100
pub const GAME_MESSAGES_START: u16 = 100;

/// HELLO message sent by client to initiate connection
///
/// This is the first message sent after WebSocket connection is established.
/// The client proposes its protocol version and indicates the minimum version it supports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    /// Client's preferred protocol version
    pub protocol_version: u16,

    /// Minimum protocol version the client supports
    pub min_protocol_version: u16,

    /// Codec ID the client will use (JSON=1, Postcard=2, Raw=3)
    pub codec_id: u8,

    /// Schema hash for message structure compatibility
    pub schema_hash: u64,
}

/// HELLO_OK message sent by server when handshake is accepted
///
/// The server confirms the negotiated protocol version and provides
/// information about authentication requirements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloOk {
    /// Server's protocol version (negotiated version to use)
    pub server_version: u16,

    /// Session ID (empty until authentication is complete)
    pub session_id: String,

    /// Whether authentication is required before sending game messages
    pub auth_required: bool,

    /// Available authentication methods (e.g., ["steam", "google", "passcode"])
    pub available_auth_methods: Vec<String>,
}

/// HELLO_ERROR message sent by server when handshake is rejected
///
/// Provides the reason for rejection and a human-readable error message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloError {
    /// The reason for rejection
    pub reason: ErrorReason,

    /// Human-readable error message
    pub message: String,

    /// Expected schema hash (for SchemaMismatch errors)
    ///
    /// When the server rejects a connection due to schema mismatch,
    /// this field contains the server's expected schema hash for debugging.
    #[serde(default)]
    pub expected_schema_hash: u64,
}

/// Reasons why a HELLO handshake might be rejected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorReason {
    /// Client and server protocol versions are incompatible
    VersionMismatch,

    /// Client and server message schemas are incompatible
    SchemaMismatch,

    /// Server has reached maximum connection capacity
    ServerFull,

    /// Server is undergoing maintenance
    Maintenance,
}

impl std::fmt::Display for ErrorReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorReason::VersionMismatch => write!(f, "VersionMismatch"),
            ErrorReason::SchemaMismatch => write!(f, "SchemaMismatch"),
            ErrorReason::ServerFull => write!(f, "ServerFull"),
            ErrorReason::Maintenance => write!(f, "Maintenance"),
        }
    }
}

/// AUTH_REQUEST message sent by client to authenticate
///
/// Contains the authentication method and credentials/token data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthRequest {
    /// Authentication method (e.g., "passcode", "steam", "google")
    pub method: String,

    /// Authentication credentials (format depends on method)
    /// - passcode: UTF-8 string passcode
    /// - steam: Steam session ticket bytes
    /// - google: Google OAuth token
    pub credentials: Vec<u8>,
}

/// AUTH_RESPONSE message sent by server after authentication attempt
///
/// Indicates success or failure and optionally provides a session ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Whether authentication was successful
    pub success: bool,

    /// Session ID assigned by server (only present on success)
    pub session_id: Option<String>,

    /// Error message (only present on failure)
    pub error_message: Option<String>,
}

/// DISCONNECT message sent by either client or server for graceful disconnection
///
/// Provides a reason code and optional message for the disconnection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disconnect {
    /// The reason for disconnection
    pub reason: DisconnectReason,

    /// Optional human-readable message
    pub message: String,
}

/// Reasons for disconnection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectReason {
    /// Client requested disconnection
    ClientRequested,

    /// Server is shutting down
    ServerShutdown,

    /// Connection idle timeout
    Timeout,

    /// Protocol error
    ProtocolError,

    /// Authentication failed
    AuthenticationFailed,

    /// Replay attack detected (duplicate message ID)
    ReplayAttack,

    /// Rate limit exceeded
    RateLimitExceeded,

    /// Message too old (TTL expired)
    MessageTooOld,

    /// Protocol violation (generic)
    ProtocolViolation,
}

impl std::fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisconnectReason::ClientRequested => write!(f, "ClientRequested"),
            DisconnectReason::ServerShutdown => write!(f, "ServerShutdown"),
            DisconnectReason::Timeout => write!(f, "Timeout"),
            DisconnectReason::ProtocolError => write!(f, "ProtocolError"),
            DisconnectReason::AuthenticationFailed => write!(f, "AuthenticationFailed"),
            DisconnectReason::ReplayAttack => write!(f, "ReplayAttack"),
            DisconnectReason::RateLimitExceeded => write!(f, "RateLimitExceeded"),
            DisconnectReason::MessageTooOld => write!(f, "MessageTooOld"),
            DisconnectReason::ProtocolViolation => write!(f, "ProtocolViolation"),
        }
    }
}

/// PING message for keepalive and latency measurement
///
/// The sender includes a timestamp, and the receiver should echo it back in a PONG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    /// Timestamp when PING was sent (milliseconds since epoch)
    pub timestamp: u64,
}

/// PONG message in response to PING
///
/// Echoes back the timestamp from the PING message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    /// Timestamp from the original PING message
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello_serialization() {
        let hello = Hello {
            protocol_version: 0x0100,
            min_protocol_version: 0x0100,
            codec_id: 1,
            schema_hash: 0x1234567890ABCDEF,
        };

        let json = serde_json::to_string(&hello).unwrap();
        let deserialized: Hello = serde_json::from_str(&json).unwrap();

        assert_eq!(hello, deserialized);
    }

    #[test]
    fn test_hello_ok_serialization() {
        let hello_ok = HelloOk {
            server_version: 0x0100,
            session_id: String::new(),
            auth_required: false,
            available_auth_methods: vec![],
        };

        let json = serde_json::to_string(&hello_ok).unwrap();
        let deserialized: HelloOk = serde_json::from_str(&json).unwrap();

        assert_eq!(hello_ok, deserialized);
    }

    #[test]
    fn test_hello_error_serialization() {
        let hello_error = HelloError {
            reason: ErrorReason::VersionMismatch,
            message: "Client version too old".to_string(),
            expected_schema_hash: 0,
        };

        let json = serde_json::to_string(&hello_error).unwrap();
        let deserialized: HelloError = serde_json::from_str(&json).unwrap();

        assert_eq!(hello_error, deserialized);
    }

    #[test]
    fn test_hello_error_with_schema_hash() {
        let hello_error = HelloError {
            reason: ErrorReason::SchemaMismatch,
            message: "Schema mismatch".to_string(),
            expected_schema_hash: 0x1234_5678_90AB_CDEF,
        };

        let json = serde_json::to_string(&hello_error).unwrap();
        let deserialized: HelloError = serde_json::from_str(&json).unwrap();

        assert_eq!(hello_error, deserialized);
        assert_eq!(deserialized.expected_schema_hash, 0x1234_5678_90AB_CDEF);
    }

    #[test]
    fn test_error_reason_display() {
        assert_eq!(ErrorReason::VersionMismatch.to_string(), "VersionMismatch");
        assert_eq!(ErrorReason::ServerFull.to_string(), "ServerFull");
    }

    #[test]
    fn test_auth_request_serialization() {
        let auth_request = AuthRequest {
            method: "passcode".to_string(),
            credentials: b"my-secret-passcode".to_vec(),
        };

        let json = serde_json::to_string(&auth_request).unwrap();
        let deserialized: AuthRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(auth_request, deserialized);
        assert_eq!(auth_request.method, "passcode");
        assert_eq!(auth_request.credentials, b"my-secret-passcode");
    }

    #[test]
    fn test_auth_response_success() {
        let auth_response = AuthResponse {
            success: true,
            session_id: Some("session-123".to_string()),
            error_message: None,
        };

        let json = serde_json::to_string(&auth_response).unwrap();
        let deserialized: AuthResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(auth_response, deserialized);
        assert!(auth_response.success);
        assert_eq!(auth_response.session_id, Some("session-123".to_string()));
    }

    #[test]
    fn test_auth_response_failure() {
        let auth_response = AuthResponse {
            success: false,
            session_id: None,
            error_message: Some("Invalid passcode".to_string()),
        };

        let json = serde_json::to_string(&auth_response).unwrap();
        let deserialized: AuthResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(auth_response, deserialized);
        assert!(!auth_response.success);
        assert_eq!(auth_response.error_message, Some("Invalid passcode".to_string()));
    }
}
