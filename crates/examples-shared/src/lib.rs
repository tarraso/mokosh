//! Shared code for Mokosh examples
//!
//! This crate contains game-specific simulation logic that is shared between
//! different client and server implementations (Godot, Bevy, native).
//!
//! All code here must be WASM-compatible (no I/O, no platform-specific dependencies).

pub mod platformer;
