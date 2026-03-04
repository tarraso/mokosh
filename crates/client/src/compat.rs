//! Platform compatibility layer
//!
//! This module provides a unified API for async primitives across different platforms:
//! - Native (desktop/server): Uses tokio runtime
//! - WASM (browser): Uses futures crate (no tokio runtime needed)

// ============================================================================
// MPSC Channels
// ============================================================================

// WASM has priority - if wasm feature is enabled, use futures mpsc
#[cfg(feature = "wasm")]
pub mod mpsc {
    pub use futures::channel::mpsc::*;
}

// Native uses tokio mpsc only if wasm is not enabled
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub mod mpsc {
    pub use tokio::sync::mpsc::*;
}

// Fallback for when neither native nor wasm is enabled (e.g., testing with --no-default-features)
#[cfg(not(any(feature = "native", feature = "wasm")))]
pub mod mpsc {
    pub use futures::channel::mpsc::*;
}

// ============================================================================
// Time utilities
// ============================================================================

// Time utilities only for native (not available in WASM)
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub mod time {
    pub use tokio::time::*;
}

// WASM doesn't have tokio::time, so we don't provide a time module for WASM
