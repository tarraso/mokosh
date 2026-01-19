//! # GodotNetLink Client
//!
//! Client-side event loop for GodotNetLink protocol.
//!
//! ## Example
//!
//! ```no_run
//! use godot_netlink_client::Client;
//! use tokio::sync::mpsc;
//!
//! #[tokio::main]
//! async fn main() {
//!     let (incoming_tx, incoming_rx) = mpsc::channel(100);
//!     let (outgoing_tx, outgoing_rx) = mpsc::channel(100);
//!
//!     let client = Client::new(incoming_rx, outgoing_tx);
//!     client.run().await;
//! }
//! ```

pub mod transport;

use bytes::Bytes;
use godot_netlink_protocol::{
    messages::{routes, Hello, HelloError, HelloOk, GAME_MESSAGES_START},
    ConnectionState, Envelope, EnvelopeFlags, CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

/// Client event loop handler
///
/// The client sends envelopes through the outgoing channel and receives
/// responses through the incoming channel.
pub struct Client {
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<Envelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<Envelope>,

    /// Current connection state
    state: ConnectionState,

    /// Message ID counter for outgoing messages
    next_msg_id: u64,
}

impl Client {
    /// Creates a new client with the given channels
    pub fn new(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
    ) -> Self {
        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Closed,
            next_msg_id: 1,
        }
    }

    /// Initiates connection by sending HELLO message
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        println!("Client: Sending HELLO...");

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            codec_id: 1, // JSON
            schema_hash: 0,
        };

        self.send_control_message(routes::HELLO, &hello).await?;

        // Transition to HelloSent state
        self.state = ConnectionState::HelloSent;

        Ok(())
    }

    /// Runs the main event loop
    ///
    /// This method will block until the incoming channel is closed.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                Some(envelope) = self.incoming_rx.recv() => {
                    self.handle_envelope(envelope).await;
                }

                else => {
                    println!("Client shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles a single incoming envelope
    async fn handle_envelope(&mut self, envelope: Envelope) {
        println!(
            "Client received: route_id={}, msg_id={}, payload_len={}, state={:?}",
            envelope.route_id, envelope.msg_id, envelope.payload_len, self.state
        );

        // Dispatch based on route_id: control messages (<100) vs game messages (>=100)
        if envelope.route_id < GAME_MESSAGES_START {
            if let Err(e) = self.handle_control_message(envelope).await {
                eprintln!("Client: Error handling control message: {}", e);
            }
        } else {
            self.handle_game_message(envelope).await;
        }
    }

    /// Handles control messages (route_id < 100)
    async fn handle_control_message(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        match envelope.route_id {
            routes::HELLO_OK => self.handle_hello_ok(envelope).await,
            routes::HELLO_ERROR => self.handle_hello_error(envelope).await,
            _ => {
                println!("Client: unknown control message route_id={}", envelope.route_id);
                Ok(())
            }
        }
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, envelope: Envelope) {
        println!("Client: received game message route_id={}", envelope.route_id);
        // Application will handle this
    }

    /// Handles HELLO_OK message from server
    async fn handle_hello_ok(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let hello_ok: HelloOk = serde_json::from_slice(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_OK: {}", e)))?;

        println!(
            "Client: HELLO_OK received (version={:#06x})",
            hello_ok.server_version
        );

        // Transition to Connected state
        self.state = ConnectionState::Connected;
        println!("Client: Connection established");

        Ok(())
    }

    /// Handles HELLO_ERROR message from server
    async fn handle_hello_error(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let hello_error: HelloError = serde_json::from_slice(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_ERROR: {}", e)))?;

        eprintln!(
            "Client: HELLO_ERROR received - {:?}: {}",
            hello_error.reason, hello_error.message
        );

        // Stay in HelloSent (or could transition to Closed)
        Ok(())
    }

    /// Sends a control message to the server
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ClientError> {
        let payload_bytes = serde_json::to_vec(message)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

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
            .map_err(|_| ClientError::ChannelSendError)?;

        Ok(())
    }

    /// Sends an envelope to the server
    pub async fn send(&self, envelope: Envelope) -> Result<(), ClientError> {
        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)
    }
}

/// Client errors
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
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
    async fn test_client_send() {
        let (_incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

        let client = Client::new(incoming_rx, outgoing_tx.clone());

        tokio::spawn(async move {
            client.run().await;
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

        let client_handle = Client::new(mpsc::channel(1).1, outgoing_tx);
        client_handle.send(test_envelope.clone()).await.unwrap();

        let received = outgoing_rx.recv().await.unwrap();
        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.payload, test_envelope.payload);
    }

    #[tokio::test]
    async fn test_client_receive() {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, _outgoing_rx) = mpsc::channel(10);

        let client = Client::new(incoming_rx, outgoing_tx);

        let handle = tokio::spawn(async move {
            client.run().await;
        });

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"response"),
        );

        incoming_tx.send(test_envelope).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        drop(incoming_tx);

        handle.await.unwrap();
    }
}
