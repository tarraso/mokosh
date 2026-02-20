//! # GodotNetLink Protocol
//!
//! Core protocol definitions for GodotNetLink networking library.
//!
//! This crate provides:
//! - `Envelope`: The wire format for all network messages
//! - `EnvelopeFlags`: Message flags for reliability, encryption, and compression
//! - `ConnectionState`: Protocol state machine
//! - Control messages: HELLO, HELLO_OK, HELLO_ERROR
//! - Version negotiation logic
//! - Error types for envelope parsing and validation
//!
//! ## Example
//!
//! ```
//! use godot_netlink_protocol::{Envelope, EnvelopeFlags};
//! use bytes::Bytes;
//!
//! // Create an envelope
//! let payload = Bytes::from_static(b"Hello, Godot!");
//! let envelope = Envelope::new_simple(
//!     256,                    // protocol_version (v1.0)
//!     1,                      // codec_id (JSON)
//!     0x1234567890ABCDEF,     // schema_hash
//!     100,                    // route_id
//!     1,                      // msg_id
//!     EnvelopeFlags::RELIABLE,
//!     payload,
//! );
//!
//! // Serialize to bytes
//! let bytes = envelope.to_bytes();
//!
//! // Deserialize from bytes
//! let received = Envelope::from_bytes(bytes).unwrap();
//! assert_eq!(received.payload, Bytes::from_static(b"Hello, Godot!"));
//! ```

pub mod auth;
pub mod codec;
pub mod codec_registry;
pub mod envelope;
pub mod error;
pub mod message_registry;
pub mod messages;
pub mod state;
pub mod transport;
pub mod version;

pub use codec_registry::CodecType;
pub use envelope::{Envelope, EnvelopeFlags, ENVELOPE_HEADER_SIZE};
pub use error::{EnvelopeError, ProtocolError, Result};
pub use message_registry::{calculate_global_schema_hash, GameMessage, MessageRegistry};
pub use messages::{Disconnect, DisconnectReason, ErrorReason, Hello, HelloError, HelloOk, Ping, Pong};
pub use state::ConnectionState;
pub use transport::Transport;
pub use version::{negotiate_version, CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION};

/// Session identifier for routing envelopes to specific clients
///
/// This is used internally by the server to track and route messages to
/// individual client connections. The SessionId is NOT part of the wire
/// protocol - it's only used for internal message routing.
pub type SessionId = uuid::Uuid;

/// Envelope tagged with a session ID for internal routing
///
/// This type is used internally by the server transport and event loop
/// to route messages to specific clients. The SessionId is not transmitted
/// over the network - only the Envelope is sent on the wire.
///
/// # Example
///
/// ```
/// use godot_netlink_protocol::{SessionEnvelope, Envelope, EnvelopeFlags};
/// use bytes::Bytes;
/// use uuid::Uuid;
///
/// let session_id = Uuid::new_v4();
/// let envelope = Envelope::new_simple(
///     1, 1, 0, 100, 1,
///     EnvelopeFlags::RELIABLE,
///     Bytes::from_static(b"test"),
/// );
///
/// let session_envelope = SessionEnvelope::new(session_id, envelope);
/// ```
#[derive(Debug, Clone)]
pub struct SessionEnvelope {
    /// Session identifier for routing
    pub session_id: SessionId,

    /// The actual envelope to send/receive
    pub envelope: Envelope,
}

impl SessionEnvelope {
    /// Creates a new SessionEnvelope
    pub fn new(session_id: SessionId, envelope: Envelope) -> Self {
        Self {
            session_id,
            envelope,
        }
    }
}
