//! # Mokosh
//!
//! A networking library for Godot game engine with support for:
//! - Custom protocol with versioning and schema validation
//! - Multiple codec support (JSON, Postcard, raw bytes)
//! - Client-side prediction and server reconciliation
//! - Authentication and authorization
//!
//! ## Components
//!
//! - `mokosh-protocol`: Core protocol definitions and envelope format
//! - `mokosh-server`: Server-side event loop and connection handling
//! - `mokosh-client`: Client-side event loop and connection management
//!
//! ## Example
//!
//! See the `examples/` directory for usage examples.

pub use mokosh_client as client;
pub use mokosh_protocol as protocol;
pub use mokosh_server as server;
