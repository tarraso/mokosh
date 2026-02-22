# mokosh-protocol

Core protocol definitions for Mokosh networking library.

## Overview

This crate provides the foundational protocol layer for Mokosh, including:

- **Envelope**: Wire format for all network messages with a fixed 34-byte header
- **EnvelopeFlags**: Bitflags for message properties (RELIABLE, ENCRYPTED, COMPRESSED)
- **Control Messages**: HELLO, HELLO_OK, HELLO_ERROR for connection handshake
- **ConnectionState**: Protocol state machine with validated transitions
  - Client: Closed → Connecting → HelloSent → Connected
  - Server: Closed → Connecting → Connected
- **Version Negotiation**: Protocol version compatibility checking
- **Error types**: Comprehensive error handling for envelope parsing and validation

## Features

### Envelope Structure

The Envelope wraps all network messages and provides:

- Protocol version negotiation
- Message routing via route IDs
- Codec identification (JSON, Postcard, Raw, or custom)
- Schema compatibility checking via hash
- Message ordering and correlation for RPC
- Flags for reliability, encryption, and compression

### Wire Format

The envelope uses a fixed 34-byte header followed by variable-length payload:

```
┌─────────────────┬──────┬───────────┐
│ protocol_version│ u16  │  2 bytes  │
├─────────────────┼──────┼───────────┤
│ codec_id        │ u8   │  1 byte   │
├─────────────────┼──────┼───────────┤
│ schema_hash     │ u64  │  8 bytes  │
├─────────────────┼──────┼───────────┤
│ route_id        │ u16  │  2 bytes  │
├─────────────────┼──────┼───────────┤
│ msg_id          │ u64  │  8 bytes  │
├─────────────────┼──────┼───────────┤
│ correlation_id  │ u64  │  8 bytes  │
├─────────────────┼──────┼───────────┤
│ flags           │ u8   │  1 byte   │
├─────────────────┼──────┼───────────┤
│ payload_len     │ u32  │  4 bytes  │
├─────────────────┼──────┼───────────┤
│ payload         │ [u8] │  N bytes  │
└─────────────────┴──────┴───────────┘
```

All multi-byte integers are encoded in big-endian format.

## Usage

### Basic Envelope Usage

```rust
use mokosh_protocol::{Envelope, EnvelopeFlags};
use bytes::Bytes;

// Create an envelope
let payload = Bytes::from_static(b"Hello, Godot!");
let envelope = Envelope::new_simple(
    256,                    // protocol_version (v1.0)
    1,                      // codec_id (JSON)
    0x1234567890ABCDEF,     // schema_hash
    100,                    // route_id
    1,                      // msg_id
    EnvelopeFlags::RELIABLE,
    payload,
);

// Serialize to bytes
let bytes = envelope.to_bytes();

// Deserialize from bytes
let received = Envelope::from_bytes(bytes).unwrap();
assert_eq!(received.payload, Bytes::from_static(b"Hello, Godot!"));

// Check flags
assert!(received.is_reliable());
assert!(!received.is_encrypted());
```

### Version Negotiation

```rust
use mokosh_protocol::{
    negotiate_version,
    CURRENT_PROTOCOL_VERSION,
    MIN_PROTOCOL_VERSION,
};

// Client supports v1.0-v1.5, Server supports v1.0-v2.0
let negotiated = negotiate_version(
    0x0105, // client version
    0x0100, // client min
    0x0200, // server version
    0x0100, // server min
).unwrap();

assert_eq!(negotiated, 0x0105); // Use v1.5
```

### Control Messages

```rust
use mokosh_protocol::{Hello, messages::routes};

// Client sends HELLO
let hello = Hello {
    protocol_version: 0x0100,
    min_protocol_version: 0x0100,
    codec_id: 1,
    schema_hash: 0,
};

// Serialize and send in envelope with route_id = routes::HELLO
```

## Envelope Flags

- `RELIABLE` (bit 0): Message requires guaranteed delivery
- `ENCRYPTED` (bit 1): Payload is encrypted
- `COMPRESSED` (bit 2): Payload is compressed
- Bits 3-7: Reserved for future use

Flags can be combined using bitwise OR:

```rust
let flags = EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED;
```

## Codec IDs

Standard codec identifiers:

- `1`: JSON codec (human-readable, debugging)
- `2`: Postcard codec (compact binary, Rust-friendly)
- `3`: RawBytes codec (user-managed serialization)
- Custom codecs can use IDs 4-255

## Testing

The crate includes comprehensive tests:

```bash
cargo test --package mokosh-protocol
```

Tests cover:
- Serialization/deserialization round-trips
- Flag operations
- Validation logic
- Error cases (buffer too short, invalid flags, etc.)
- Edge cases (empty payload, large payload)
- RPC correlation

## License

MIT OR Apache-2.0
