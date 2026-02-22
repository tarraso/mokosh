pub mod memory;
pub mod websocket;

// Re-export the Transport trait from protocol
pub use mokosh_protocol::Transport;

/// Default transport type (WebSocket)
///
/// This is the recommended transport for most use cases.
pub type DefaultTransport = websocket::WebSocketServer;
