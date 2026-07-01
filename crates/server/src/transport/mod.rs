pub mod memory;
pub mod reliable;
pub mod udp;
pub mod websocket;

// Re-export the Transport trait from protocol
pub use mokosh_protocol::Transport;

pub use reliable::ReliableServerLink;

/// Default transport type (WebSocket)
///
/// This is the recommended transport for most use cases.
pub type DefaultTransport = websocket::WebSocketServer;
