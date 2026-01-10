//! # GodotNetLink
//!
//! A networking library for Godot game engine with support for:
//! - Custom protocol with versioning and schema validation
//! - Multiple codec support (JSON, Postcard, raw bytes)
//! - Client-side prediction and server reconciliation
//! - Authentication and authorization
//!
//! ## Components
//!
//! - `godot-netlink-protocol`: Core protocol definitions and envelope format
//! - `godot-netlink-server`: Server-side event loop and connection handling
//! - `godot-netlink-client`: Client-side event loop and connection management
//!
//! ## Example
//!
//! See the `examples/` directory for usage examples.

pub use godot_netlink_client as client;
pub use godot_netlink_protocol as protocol;
pub use godot_netlink_server as server;
