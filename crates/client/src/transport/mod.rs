pub mod memory;

#[cfg(feature = "native")]
pub mod websocket;

#[cfg(feature = "native")]
pub mod udp;

/// Reliability decorator (`ReliableLink<T>`) — native-only; browsers can't open UDP.
#[cfg(feature = "native")]
pub mod reliable;

#[cfg(feature = "wasm")]
pub mod browser_websocket;

// Re-export the Transport trait from protocol
pub use mokosh_protocol::Transport;

#[cfg(feature = "native")]
pub use reliable::ReliableLink;

/// Default transport type (platform-specific)
///
/// - Native platforms: WebSocket using tokio-tungstenite
/// - WASM/Browser: Browser WebSocket API
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub type DefaultTransport = websocket::WebSocketClient;

#[cfg(feature = "wasm")]
pub type DefaultTransport = browser_websocket::BrowserWebSocketClient;
