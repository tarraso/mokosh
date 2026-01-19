//! # GodotNetLink Server
//!
//! Server-side event loop for GodotNetLink protocol.
//!
//! ## Example
//!
//! ```no_run
//! use godot_netlink_server::Server;
//! use tokio::sync::mpsc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let (incoming_tx, incoming_rx) = mpsc::channel(100);
//!     let (outgoing_tx, outgoing_rx) = mpsc::channel(100);
//!
//!     let server = Server::new(incoming_rx, outgoing_tx);
//!     server.run().await;
//! }
//! ```

pub mod transport;

use bytes::Bytes;
use godot_netlink_protocol::{
    messages::{routes, ErrorReason, Hello, HelloError, HelloOk, GAME_MESSAGES_START},
    negotiate_version, ConnectionState, Envelope, EnvelopeFlags, CURRENT_PROTOCOL_VERSION,
    MIN_PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

/// Server event loop handler
///
/// The server receives envelopes from incoming channel, processes them,
/// and sends responses through the outgoing channel.
pub struct Server {
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<Envelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<Envelope>,

    /// Current connection state
    state: ConnectionState,

    /// Message ID counter for outgoing messages
    next_msg_id: u64,
}

impl Server {
    /// Creates a new server with the given channels
    pub fn new(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
    ) -> Self {
        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Connecting,
            next_msg_id: 1,
        }
    }

    /// Runs the main event loop
    ///
    /// This method will block until the incoming channel is closed.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(envelope) = self.incoming_rx.recv() => {
                    if let Err(e) = self.handle_envelope(envelope).await {
                        eprintln!("Error handling envelope: {}", e);
                    }
                }

                else => {
                    println!("Server shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles a single envelope
    async fn handle_envelope(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        println!(
            "Server received: route_id={}, msg_id={}, payload_len={}, state={:?}",
            envelope.route_id, envelope.msg_id, envelope.payload_len, self.state
        );

        // Dispatch based on route_id: control messages (<100) vs game messages (>=100)
        if envelope.route_id < GAME_MESSAGES_START {
            self.handle_control_message(envelope).await
        } else {
            self.handle_game_message(envelope).await
        }
    }

    /// Handles control messages (route_id < 100)
    async fn handle_control_message(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        match envelope.route_id {
            routes::HELLO => self.handle_hello(envelope).await,
            _ => {
                println!("Server: unknown control message route_id={}", envelope.route_id);
                Ok(())
            }
        }
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        // Game messages are only allowed in Connected state
        if !self.state.is_connected() {
            println!(
                "Server: rejecting game message (route_id={}) in state {:?}",
                envelope.route_id, self.state
            );
            return Ok(());
        }

        // For now, echo back (same as before)
        println!("Server: echoing game message route_id={}", envelope.route_id);
        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }

    /// Handles HELLO message from client
    async fn handle_hello(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        // Parse HELLO message
        let hello: Hello = serde_json::from_slice(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse HELLO: {}", e)))?;

        println!(
            "Server: HELLO from client (version={:#06x}, min={:#06x})",
            hello.protocol_version, hello.min_protocol_version
        );

        // Negotiate version
        match negotiate_version(
            hello.protocol_version,
            hello.min_protocol_version,
            CURRENT_PROTOCOL_VERSION,
            MIN_PROTOCOL_VERSION,
        ) {
            Ok(negotiated_version) => {
                println!("Server: Version negotiated: {:#06x}", negotiated_version);

                // Send HELLO_OK
                let hello_ok = HelloOk {
                    server_version: negotiated_version,
                    session_id: String::new(), // Empty until auth
                    auth_required: false,       // No auth for MVP
                    available_auth_methods: vec![],
                };

                self.send_control_message(routes::HELLO_OK, &hello_ok)
                    .await?;

                // Transition to Connected state
                self.state = ConnectionState::Connected;
                println!("Server: Connection established");
            }
            Err(err) => {
                println!("Server: Version mismatch: {}", err);

                // Send HELLO_ERROR
                let hello_error = HelloError {
                    reason: ErrorReason::VersionMismatch,
                    message: err.to_string(),
                };

                self.send_control_message(routes::HELLO_ERROR, &hello_error)
                    .await?;

                // Stay in Connecting state (will disconnect)
            }
        }

        Ok(())
    }

    /// Sends a control message to the client
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ServerError> {
        let payload_bytes = serde_json::to_vec(message)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1, // JSON codec
            0, // schema_hash (not used yet)
            route_id,
            self.next_msg_id,
            EnvelopeFlags::RELIABLE,
            Bytes::from(payload_bytes),
        );

        self.next_msg_id += 1;

        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }
}

/// Server errors
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("Failed to send envelope through channel")]
    ChannelSendError,

    #[error("Invalid message: {0}")]
    InvalidMessage(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use godot_netlink_protocol::EnvelopeFlags;

    #[tokio::test]
    async fn test_server_echo() {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

        let server = Server::new(incoming_rx, outgoing_tx);
        tokio::spawn(async move {
            server.run().await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        incoming_tx.send(test_envelope.clone()).await.unwrap();

        let received = outgoing_rx.recv().await.unwrap();
        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.msg_id, test_envelope.msg_id);
        assert_eq!(received.payload, test_envelope.payload);
    }
}
