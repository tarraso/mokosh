//! In-memory transport for testing and single-player games
//!
//! Re-exports the shared MemoryTransport from the client crate.

pub use mokosh_client::transport::memory::{MemoryTransport, MemoryTransportError};
