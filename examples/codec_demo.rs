//! Codec demonstration
//!
//! This example demonstrates using different codecs (JSON, Postcard, Raw)
//! for encoding and decoding messages in the GodotNetLink protocol.
//!
//! Run with:
//! ```sh
//! cargo run --example codec_demo
//! ```

use bytes::Bytes;
use godot_netlink_protocol::{
    codec::{Codec, JsonCodec, PostcardCodec, RawCodec},
    codec_registry::{CodecRegistry, CodecType},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PlayerInput {
    sequence: u32,
    direction_x: f32,
    direction_y: f32,
    jump: bool,
    shoot: bool,
}

fn main() {
    println!("=== GodotNetLink Codec Demo ===\n");

    // Create test message
    let input = PlayerInput {
        sequence: 42,
        direction_x: 0.75,
        direction_y: -0.25,
        jump: true,
        shoot: false,
    };
    println!("Original message: {:?}\n", input);

    // Demonstrate JSON codec
    println!("--- JSON Codec (ID=1) ---");
    demo_json_codec(&input);
    println!();

    // Demonstrate Postcard codec
    println!("--- Postcard Codec (ID=2) ---");
    demo_postcard_codec(&input);
    println!();

    // Demonstrate Raw codec
    println!("--- Raw Codec (ID=3) ---");
    demo_raw_codec();
    println!();

    // Demonstrate codec registry
    println!("--- Codec Registry ---");
    demo_codec_registry(&input);
    println!();

    // Compare codec sizes
    println!("--- Size Comparison ---");
    compare_codec_sizes(&input);
}

fn demo_json_codec(input: &PlayerInput) {
    let codec = JsonCodec;
    println!("Codec: {} (ID={})", codec.name(), codec.id());

    // Encode
    let bytes = codec.encode(input).expect("Failed to encode");
    println!("Encoded ({} bytes): {}", bytes.len(), String::from_utf8_lossy(&bytes));

    // Decode
    let decoded: PlayerInput = codec.decode(&bytes).expect("Failed to decode");
    println!("Decoded: {:?}", decoded);

    // Verify
    assert_eq!(input, &decoded);
    println!("✓ Encode/decode successful");
}

fn demo_postcard_codec(input: &PlayerInput) {
    let codec = PostcardCodec;
    println!("Codec: {} (ID={})", codec.name(), codec.id());

    // Encode
    let bytes = codec.encode(input).expect("Failed to encode");
    println!("Encoded ({} bytes): {:02x?}", bytes.len(), bytes);

    // Decode
    let decoded: PlayerInput = codec.decode(&bytes).expect("Failed to decode");
    println!("Decoded: {:?}", decoded);

    // Verify
    assert_eq!(input, &decoded);
    println!("✓ Encode/decode successful");
}

fn demo_raw_codec() {
    let codec = RawCodec;
    println!("Codec: {} (ID={})", codec.name(), codec.id());

    // Raw codec is for binary data that doesn't use serde
    let raw_data = Bytes::from_static(b"\x01\x02\x03\x04\x05");
    println!("Raw data ({} bytes): {:02x?}", raw_data.len(), raw_data);

    // Encode (pass-through)
    let encoded = codec.encode_raw(raw_data.clone()).expect("Failed to encode");
    println!("Encoded ({} bytes): {:02x?}", encoded.len(), encoded);

    // Decode (pass-through)
    let decoded = codec.decode_raw(&encoded).expect("Failed to decode");
    println!("Decoded ({} bytes): {:02x?}", decoded.len(), decoded);

    // Verify
    assert_eq!(raw_data, decoded);
    println!("✓ Encode/decode successful");
}

fn demo_codec_registry(input: &PlayerInput) {
    let registry = CodecRegistry::default();
    println!("Available codecs: {:?}", registry.codec_ids());

    // Look up codec by ID
    println!("\nLooking up codec ID=1:");
    let json_codec = registry.get(1).expect("Codec not found");
    println!("  Name: {}", json_codec.name());

    // Encode using registry codec
    let bytes = json_codec.encode(input).expect("Failed to encode");
    println!("  Encoded: {} bytes", bytes.len());

    // Try all codecs
    println!("\nTrying all registered codecs:");
    for codec_id in registry.codec_ids() {
        let codec = registry.get(codec_id).unwrap();
        match codec {
            CodecType::Json(_) | CodecType::Postcard(_) => {
                let bytes = codec.encode(input).expect("Failed to encode");
                let decoded: PlayerInput = codec.decode(&bytes).expect("Failed to decode");
                assert_eq!(input, &decoded);
                println!("  {} (ID={}): {} bytes ✓", codec.name(), codec.id(), bytes.len());
            }
            CodecType::Raw(_) => {
                println!("  {} (ID={}): skipped (not for serde types)", codec.name(), codec.id());
            }
        }
    }
}

fn compare_codec_sizes(input: &PlayerInput) {
    let json_codec = JsonCodec;
    let postcard_codec = PostcardCodec;

    let json_bytes = json_codec.encode(input).expect("Failed to encode");
    let postcard_bytes = postcard_codec.encode(input).expect("Failed to encode");

    println!("Message: {:?}", input);
    println!("  JSON:     {} bytes", json_bytes.len());
    println!("  Postcard: {} bytes", postcard_bytes.len());
    println!(
        "  Savings:  {} bytes ({:.1}% smaller)",
        json_bytes.len() - postcard_bytes.len(),
        100.0 * (1.0 - postcard_bytes.len() as f64 / json_bytes.len() as f64)
    );

    // Show actual encoded data
    println!("\nJSON representation:");
    println!("  {}", String::from_utf8_lossy(&json_bytes));
    println!("\nPostcard representation (hex):");
    println!("  {:02x?}", postcard_bytes);
}
