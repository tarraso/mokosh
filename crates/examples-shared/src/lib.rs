//! Shared code for Mokosh examples
//!
//! This crate contains game-specific simulation logic that is shared between
//! different client and server implementations (Godot, Bevy, native).
//!
//! All code here must be WASM-compatible (no I/O, no platform-specific dependencies).

pub mod platformer;

/// Pre-shared key used by the UDP examples to authenticate and encrypt every
/// datagram (see `platformer_server_udp` / `platformer_client_udp`).
///
/// ⚠️ **DEMO KEY ONLY.** This value is hard-coded so the two example binaries
/// agree out of the box. A real deployment MUST generate a random 32-byte key,
/// keep it secret, and distribute it to clients out-of-band — never commit it to
/// source control.
pub const DEMO_UDP_PSK: [u8; 32] = [
    0x6d, 0x6f, 0x6b, 0x6f, 0x73, 0x68, 0x2d, 0x64, 0x65, 0x6d, 0x6f, 0x2d, 0x75, 0x64, 0x70, 0x2d,
    0x70, 0x73, 0x6b, 0x2d, 0x64, 0x6f, 0x2d, 0x6e, 0x6f, 0x74, 0x2d, 0x75, 0x73, 0x65, 0x21, 0x21,
];
