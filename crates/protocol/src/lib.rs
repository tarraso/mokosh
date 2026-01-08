//! # GodotNetLink Protocol
//!
//! Core protocol definitions for GodotNetLink networking library.
//!
//! This crate provides:
//! - `Envelope`: The wire format for all network messages
//! - `EnvelopeFlags`: Message flags for reliability, encryption, and compression
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

pub mod envelope;
pub mod error;

pub use envelope::{Envelope, EnvelopeFlags, ENVELOPE_HEADER_SIZE};
pub use error::{EnvelopeError, Result};
