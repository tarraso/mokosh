//! Platform compatibility layer for protocol crate
//!
//! Provides unified channel types across platforms

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
