pub mod memory;

#[cfg(feature = "native")]
pub mod websocket;

#[cfg(feature = "native")]
pub mod udp;

#[cfg(feature = "wasm")]
pub mod browser_websocket;

// Re-export the Transport trait from protocol
pub use mokosh_protocol::Transport;

/// Default transport type (platform-specific)
///
/// - Native platforms: WebSocket using tokio-tungstenite
/// - WASM/Browser: Browser WebSocket API
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub type DefaultTransport = websocket::WebSocketClient;

#[cfg(feature = "wasm")]
pub type DefaultTransport = browser_websocket::BrowserWebSocketClient;
