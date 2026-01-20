//! Codec abstraction for payload serialization/deserialization
//!
//! This module provides a pluggable codec system that allows applications
//! to choose their preferred serialization format (JSON, Postcard, Raw bytes).
//!
//! # Codec IDs
//!
//! - `1`: JSON (serde_json)
//! - `2`: Postcard (serde postcard)
//! - `3`: Raw (no serialization, pass-through bytes)
//!
//! # Usage
//!
//! ```
//! use godot_netlink_protocol::codec::{Codec, JsonCodec};
//! use bytes::Bytes;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize, Debug, PartialEq)]
//! struct MyMessage {
//!     field: String,
//! }
//!
//! let codec = JsonCodec;
//! let message = MyMessage { field: "test".into() };
//!
//! // Encode
//! let bytes = codec.encode(&message).unwrap();
//!
//! // Decode
//! let decoded: MyMessage = codec.decode(&bytes).unwrap();
//! assert_eq!(message, decoded);
//! ```

use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};

use crate::error::{ProtocolError, Result};

/// Codec trait for serializing/deserializing message payloads
///
/// Implementations must be thread-safe (Send + Sync) as they may be
/// shared across multiple connections.
pub trait Codec: Send + Sync {
    /// Returns the codec ID (1=JSON, 2=Postcard, 3=Raw)
    fn id(&self) -> u8;

    /// Returns a human-readable name for this codec
    fn name(&self) -> &'static str;

    /// Encodes a serializable message into bytes
    fn encode<T: Serialize>(&self, message: &T) -> Result<Bytes>;

    /// Decodes bytes into a deserializable message
    fn decode<T: DeserializeOwned>(&self, bytes: &Bytes) -> Result<T>;
}

/// JSON codec (codec_id = 1)
///
/// Uses serde_json for human-readable serialization.
/// Best for debugging, interoperability, and web clients.
///
/// # Example
///
/// ```
/// use godot_netlink_protocol::codec::{Codec, JsonCodec};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct Position { x: f32, y: f32 }
///
/// let codec = JsonCodec;
/// let pos = Position { x: 1.0, y: 2.0 };
/// let bytes = codec.encode(&pos).unwrap();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    fn id(&self) -> u8 {
        1
    }

    fn name(&self) -> &'static str {
        "JSON"
    }

    fn encode<T: Serialize>(&self, message: &T) -> Result<Bytes> {
        let vec = serde_json::to_vec(message)
            .map_err(|e| ProtocolError::CodecError(format!("JSON encode failed: {}", e)))?;
        Ok(Bytes::from(vec))
    }

    fn decode<T: DeserializeOwned>(&self, bytes: &Bytes) -> Result<T> {
        serde_json::from_slice(bytes)
            .map_err(|e| ProtocolError::CodecError(format!("JSON decode failed: {}", e)))
    }
}

/// Postcard codec (codec_id = 2)
///
/// Uses postcard for compact binary serialization.
/// Best for production use with bandwidth constraints.
///
/// # Example
///
/// ```
/// use godot_netlink_protocol::codec::{Codec, PostcardCodec};
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct PlayerInput { seq: u32, jump: bool }
///
/// let codec = PostcardCodec;
/// let input = PlayerInput { seq: 42, jump: true };
/// let bytes = codec.encode(&input).unwrap();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PostcardCodec;

impl Codec for PostcardCodec {
    fn id(&self) -> u8 {
        2
    }

    fn name(&self) -> &'static str {
        "Postcard"
    }

    fn encode<T: Serialize>(&self, message: &T) -> Result<Bytes> {
        let vec = postcard::to_allocvec(message)
            .map_err(|e| ProtocolError::CodecError(format!("Postcard encode failed: {}", e)))?;
        Ok(Bytes::from(vec))
    }

    fn decode<T: DeserializeOwned>(&self, bytes: &Bytes) -> Result<T> {
        postcard::from_bytes(bytes)
            .map_err(|e| ProtocolError::CodecError(format!("Postcard decode failed: {}", e)))
    }
}

/// Raw codec (codec_id = 3)
///
/// Pass-through codec for raw byte payloads.
/// Use when you need full control over serialization or working with binary protocols.
///
/// # Example
///
/// ```
/// use godot_netlink_protocol::codec::{Codec, RawCodec};
/// use bytes::Bytes;
///
/// let codec = RawCodec;
/// let data = Bytes::from_static(b"\x01\x02\x03\x04");
///
/// // Raw codec passes through bytes unchanged
/// let encoded = codec.encode(&data).unwrap();
/// assert_eq!(data, encoded);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RawCodec;

impl RawCodec {
    /// Encode raw bytes (pass-through)
    pub fn encode_raw(&self, bytes: Bytes) -> Result<Bytes> {
        Ok(bytes)
    }

    /// Decode raw bytes (pass-through)
    pub fn decode_raw(&self, bytes: &Bytes) -> Result<Bytes> {
        Ok(bytes.clone())
    }
}

impl Codec for RawCodec {
    fn id(&self) -> u8 {
        3
    }

    fn name(&self) -> &'static str {
        "Raw"
    }

    fn encode<T: Serialize>(&self, _message: &T) -> Result<Bytes> {
        // For raw codec with serde types, we need special handling
        // This is primarily for Bytes wrapper types
        Err(ProtocolError::CodecError(
            "Raw codec does not support serde types. Use encode_raw() for Bytes.".into(),
        ))
    }

    fn decode<T: DeserializeOwned>(&self, _bytes: &Bytes) -> Result<T> {
        Err(ProtocolError::CodecError(
            "Raw codec does not support serde types. Use decode_raw() for Bytes.".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestMessage {
        id: u32,
        name: String,
        active: bool,
    }

    #[test]
    fn test_json_codec() {
        let codec = JsonCodec;
        assert_eq!(codec.id(), 1);
        assert_eq!(codec.name(), "JSON");

        let msg = TestMessage {
            id: 42,
            name: "test".into(),
            active: true,
        };

        let bytes = codec.encode(&msg).unwrap();
        let decoded: TestMessage = codec.decode(&bytes).unwrap();

        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_postcard_codec() {
        let codec = PostcardCodec;
        assert_eq!(codec.id(), 2);
        assert_eq!(codec.name(), "Postcard");

        let msg = TestMessage {
            id: 42,
            name: "test".into(),
            active: true,
        };

        let bytes = codec.encode(&msg).unwrap();
        let decoded: TestMessage = codec.decode(&bytes).unwrap();

        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_raw_codec() {
        let codec = RawCodec;
        assert_eq!(codec.id(), 3);
        assert_eq!(codec.name(), "Raw");

        let data = Bytes::from_static(b"\x01\x02\x03\x04");
        let encoded = codec.encode_raw(data.clone()).unwrap();
        let decoded = codec.decode_raw(&encoded).unwrap();

        assert_eq!(data, decoded);
    }

    #[test]
    fn test_raw_codec_serde_error() {
        let codec = RawCodec;
        let msg = TestMessage {
            id: 42,
            name: "test".into(),
            active: true,
        };

        // Should error when trying to use serde with raw codec
        assert!(codec.encode(&msg).is_err());
    }

    #[test]
    fn test_json_vs_postcard_size() {
        let msg = TestMessage {
            id: 42,
            name: "test".into(),
            active: true,
        };

        let json_bytes = JsonCodec.encode(&msg).unwrap();
        let postcard_bytes = PostcardCodec.encode(&msg).unwrap();

        // Postcard should be more compact than JSON
        assert!(postcard_bytes.len() < json_bytes.len());
        println!(
            "JSON: {} bytes, Postcard: {} bytes",
            json_bytes.len(),
            postcard_bytes.len()
        );
    }
}
