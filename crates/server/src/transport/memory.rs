//! In-memory transport for testing and single-player games
//!
//! Re-exports the shared MemoryTransport from the client crate.

pub use godot_netlink_client::transport::memory::{MemoryTransport, MemoryTransportError};
