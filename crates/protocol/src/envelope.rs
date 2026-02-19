use crate::error::{EnvelopeError, EnvelopeResult as Result};
use bitflags::bitflags;
use bytes::{Buf, BufMut, Bytes, BytesMut};

bitflags! {
    /// Envelope flags indicating message properties
    ///
    /// - bit 0: RELIABLE - guaranteed delivery required
    /// - bit 1: ENCRYPTED - payload is encrypted
    /// - bit 2: COMPRESSED - payload is compressed
    /// - bits 3-7: reserved
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct EnvelopeFlags: u8 {
        /// Message requires guaranteed delivery
        const RELIABLE = 0b0000_0001;
        /// Payload is encrypted
        const ENCRYPTED = 0b0000_0010;
        /// Payload is compressed
        const COMPRESSED = 0b0000_0100;
    }
}

/// Network message envelope for GodotNetLink protocol
///
/// The Envelope wraps all network messages and provides:
/// - Protocol version negotiation
/// - Message routing
/// - Codec identification
/// - Schema compatibility checking
/// - Message ordering and correlation (for RPC)
/// - Flags for reliability, encryption, and compression
///
/// Wire format (big-endian):
/// ```text
/// ┌─────────────────┬──────┬───────────┐
/// │ protocol_version│ u16  │  2 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ codec_id        │ u8   │  1 byte   │
/// ├─────────────────┼──────┼───────────┤
/// │ schema_hash     │ u64  │  8 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ route_id        │ u16  │  2 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ msg_id          │ u64  │  8 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ correlation_id  │ u64  │  8 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ flags           │ u8   │  1 byte   │
/// ├─────────────────┼──────┼───────────┤
/// │ payload_len     │ u32  │  4 bytes  │
/// ├─────────────────┼──────┼───────────┤
/// │ payload         │ [u8] │  N bytes  │
/// └─────────────────┴──────┴───────────┘
/// Total header: 34 bytes
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    /// Protocol version number
    pub protocol_version: u16,

    /// Codec identifier (JSON=1, Postcard=2, Raw=3)
    pub codec_id: u8,

    /// Message schema hash for compatibility checking
    pub schema_hash: u64,

    /// Route/message type identifier
    pub route_id: u16,

    /// Unique message identifier for ordering and deduplication
    pub msg_id: u64,

    /// Correlation ID for RPC requests/responses (0 if not RPC)
    pub correlation_id: u64,

    /// Message flags (reliability, encryption, compression)
    pub flags: EnvelopeFlags,

    /// Length of the payload in bytes
    pub payload_len: u32,

    /// Actual message payload
    pub payload: Bytes,
}

/// Size of the envelope header in bytes (excluding payload)
pub const ENVELOPE_HEADER_SIZE: usize = 34;

impl Envelope {
    /// Creates a new envelope with the given parameters
    ///
    /// # Deprecation Notice
    ///
    /// **For game messages (route_id >= 100)**, prefer using the type-safe API:
    /// - `Client::send_message<T: GameMessage>()`
    /// - `Server::send_message<T: GameMessage>()`
    ///
    /// This low-level API should only be used for:
    /// - Control messages (route_id < 100)
    /// - Custom protocol extensions
    /// - Testing and debugging
    ///
    /// The type-safe API provides:
    /// - Automatic route_id assignment
    /// - Automatic schema_hash generation
    /// - Compile-time type checking
    /// - No manual boilerplate
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        protocol_version: u16,
        codec_id: u8,
        schema_hash: u64,
        route_id: u16,
        msg_id: u64,
        correlation_id: u64,
        flags: EnvelopeFlags,
        payload: Bytes,
    ) -> Self {
        let payload_len = payload.len() as u32;
        Self {
            protocol_version,
            codec_id,
            schema_hash,
            route_id,
            msg_id,
            correlation_id,
            flags,
            payload_len,
            payload,
        }
    }

    /// Creates a new envelope with empty correlation_id (not an RPC)
    ///
    /// # Deprecation Notice
    ///
    /// **For game messages (route_id >= 100)**, prefer using the type-safe API:
    /// - `Client::send_message<T: GameMessage>()`
    /// - `Server::send_message<T: GameMessage>()`
    ///
    /// See `Envelope::new()` for more details.
    pub fn new_simple(
        protocol_version: u16,
        codec_id: u8,
        schema_hash: u64,
        route_id: u16,
        msg_id: u64,
        flags: EnvelopeFlags,
        payload: Bytes,
    ) -> Self {
        Self::new(
            protocol_version,
            codec_id,
            schema_hash,
            route_id,
            msg_id,
            0, // no correlation_id
            flags,
            payload,
        )
    }

    /// Validates the envelope
    pub fn validate(&self) -> Result<()> {
        // Check payload length matches
        if self.payload.len() != self.payload_len as usize {
            return Err(EnvelopeError::PayloadLengthMismatch {
                expected: self.payload_len,
                actual: self.payload.len(),
            });
        }

        Ok(())
    }

    /// Serializes the envelope to bytes (big-endian)
    pub fn to_bytes(&self) -> Bytes {
        let total_size = ENVELOPE_HEADER_SIZE + self.payload.len();
        let mut buf = BytesMut::with_capacity(total_size);

        buf.put_u16(self.protocol_version);
        buf.put_u8(self.codec_id);
        buf.put_u64(self.schema_hash);
        buf.put_u16(self.route_id);
        buf.put_u64(self.msg_id);
        buf.put_u64(self.correlation_id);
        buf.put_u8(self.flags.bits());
        buf.put_u32(self.payload_len);
        buf.put_slice(&self.payload);

        buf.freeze()
    }

    /// Deserializes an envelope from bytes (big-endian)
    pub fn from_bytes(mut data: Bytes) -> Result<Self> {
        if data.len() < ENVELOPE_HEADER_SIZE {
            return Err(EnvelopeError::BufferTooShort {
                need: ENVELOPE_HEADER_SIZE,
                have: data.len(),
            });
        }

        let protocol_version = data.get_u16();
        let codec_id = data.get_u8();
        let schema_hash = data.get_u64();
        let route_id = data.get_u16();
        let msg_id = data.get_u64();
        let correlation_id = data.get_u64();
        let flags_byte = data.get_u8();
        let payload_len = data.get_u32();

        // Parse flags
        let flags = EnvelopeFlags::from_bits(flags_byte)
            .ok_or(EnvelopeError::InvalidFlags(flags_byte))?;

        // Check if we have enough data for the payload
        if data.len() < payload_len as usize {
            return Err(EnvelopeError::BufferTooShort {
                need: payload_len as usize,
                have: data.len(),
            });
        }

        // Extract payload
        let payload = data.copy_to_bytes(payload_len as usize);

        let envelope = Self {
            protocol_version,
            codec_id,
            schema_hash,
            route_id,
            msg_id,
            correlation_id,
            flags,
            payload_len,
            payload,
        };

        envelope.validate()?;
        Ok(envelope)
    }

    /// Returns the total size of the envelope (header + payload)
    #[inline]
    pub fn total_size(&self) -> usize {
        ENVELOPE_HEADER_SIZE + self.payload.len()
    }

    /// Checks if the envelope has the RELIABLE flag set
    #[inline]
    pub fn is_reliable(&self) -> bool {
        self.flags.contains(EnvelopeFlags::RELIABLE)
    }

    /// Checks if the envelope has the ENCRYPTED flag set
    #[inline]
    pub fn is_encrypted(&self) -> bool {
        self.flags.contains(EnvelopeFlags::ENCRYPTED)
    }

    /// Checks if the envelope has the COMPRESSED flag set
    #[inline]
    pub fn is_compressed(&self) -> bool {
        self.flags.contains(EnvelopeFlags::COMPRESSED)
    }

    /// Checks if this envelope is an RPC request/response
    #[inline]
    pub fn is_rpc(&self) -> bool {
        self.correlation_id != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_new() {
        let payload = Bytes::from_static(b"test payload");
        let envelope = Envelope::new_simple(
            1,                    // protocol_version
            2,                    // codec_id (Postcard)
            0x1234567890ABCDEF,   // schema_hash
            100,                  // route_id
            42,                   // msg_id
            EnvelopeFlags::RELIABLE,
            payload.clone(),
        );

        assert_eq!(envelope.protocol_version, 1);
        assert_eq!(envelope.codec_id, 2);
        assert_eq!(envelope.schema_hash, 0x1234567890ABCDEF);
        assert_eq!(envelope.route_id, 100);
        assert_eq!(envelope.msg_id, 42);
        assert_eq!(envelope.correlation_id, 0);
        assert_eq!(envelope.flags, EnvelopeFlags::RELIABLE);
        assert_eq!(envelope.payload_len, 12);
        assert_eq!(envelope.payload, payload);
    }

    #[test]
    fn test_envelope_serialization() {
        let payload = Bytes::from_static(b"hello");
        let envelope = Envelope::new_simple(
            256,                  // protocol_version (v1.0)
            1,                    // codec_id (JSON)
            0xAABBCCDDEEFF0011,   // schema_hash
            200,                  // route_id
            999,                  // msg_id
            EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED,
            payload,
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(envelope, deserialized);
    }

    #[test]
    fn test_envelope_flags() {
        let payload = Bytes::from_static(b"test");

        let envelope = Envelope::new_simple(
            1, 1, 0, 100, 1,
            EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
            payload,
        );

        assert!(envelope.is_reliable());
        assert!(!envelope.is_encrypted());
        assert!(envelope.is_compressed());
        assert!(!envelope.is_rpc());
    }

    #[test]
    fn test_envelope_rpc() {
        let payload = Bytes::from_static(b"rpc call");
        let envelope = Envelope::new(
            1, 1, 0, 100, 1,
            12345, // correlation_id for RPC
            EnvelopeFlags::RELIABLE,
            payload,
        );

        assert!(envelope.is_rpc());
        assert_eq!(envelope.correlation_id, 12345);
    }

    #[test]
    fn test_envelope_validation_success() {
        let payload = Bytes::from_static(b"valid");
        let envelope = Envelope::new_simple(1, 1, 0, 100, 1, EnvelopeFlags::empty(), payload);
        assert!(envelope.validate().is_ok());
    }

    #[test]
    fn test_envelope_buffer_too_short() {
        let short_buffer = Bytes::from_static(&[1, 2, 3]);
        let result = Envelope::from_bytes(short_buffer);
        assert!(matches!(result, Err(EnvelopeError::BufferTooShort { .. })));
    }

    #[test]
    fn test_envelope_invalid_flags() {
        let mut buf = BytesMut::with_capacity(ENVELOPE_HEADER_SIZE + 5);

        // Valid header but invalid flags (0xFF has reserved bits set)
        buf.put_u16(1);              // protocol_version
        buf.put_u8(1);               // codec_id
        buf.put_u64(0);              // schema_hash
        buf.put_u16(100);            // route_id
        buf.put_u64(1);              // msg_id
        buf.put_u64(0);              // correlation_id
        buf.put_u8(0xFF);            // invalid flags (reserved bits set)
        buf.put_u32(5);              // payload_len
        buf.put_slice(b"hello");     // payload

        let result = Envelope::from_bytes(buf.freeze());
        assert!(matches!(result, Err(EnvelopeError::InvalidFlags(_))));
    }

    #[test]
    fn test_envelope_total_size() {
        let payload = Bytes::from_static(b"test payload");
        let envelope = Envelope::new_simple(1, 1, 0, 100, 1, EnvelopeFlags::empty(), payload);
        assert_eq!(envelope.total_size(), ENVELOPE_HEADER_SIZE + 12);
    }

    #[test]
    fn test_empty_payload() {
        let payload = Bytes::new();
        let envelope = Envelope::new_simple(1, 1, 0, 100, 1, EnvelopeFlags::empty(), payload);

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.payload_len, 0);
        assert_eq!(deserialized.payload.len(), 0);
    }

    #[test]
    fn test_large_payload() {
        let large_payload = vec![0xAB; 65536]; // 64KB
        let payload = Bytes::from(large_payload);
        let envelope = Envelope::new_simple(
            1, 1, 0, 100, 1,
            EnvelopeFlags::COMPRESSED,
            payload.clone(),
        );

        let bytes = envelope.to_bytes();
        let deserialized = Envelope::from_bytes(bytes).expect("Failed to deserialize");

        assert_eq!(deserialized.payload, payload);
        assert_eq!(deserialized.payload_len, 65536);
    }
}
