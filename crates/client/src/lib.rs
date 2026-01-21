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

use godot_netlink_protocol::{
    codec_registry::CodecRegistry,
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

    /// Codec registry for encoding/decoding messages
    codec_registry: CodecRegistry,

    /// Codec ID to use for control messages (default: 1 = JSON)
    control_codec_id: u8,

    /// Codec ID to use for game messages
    game_codec_id: u8,
}

impl Client {
    /// Creates a new client with the given channels and default codec registry
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    pub fn new(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
    ) -> Self {
        Self::with_codecs(
            incoming_rx,
            outgoing_tx,
            CodecRegistry::default(),
            1, // JSON for control messages
            1, // JSON for game messages
        )
    }

    /// Creates a new client with custom codec registry and codec IDs
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive envelopes from transport
    /// * `outgoing_tx` - Channel to send envelopes to transport
    /// * `codec_registry` - Registry of available codecs
    /// * `control_codec_id` - Codec ID for control messages (typically 1=JSON)
    /// * `game_codec_id` - Codec ID for game messages (1=JSON, 2=Postcard, 3=Raw)
    pub fn with_codecs(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        codec_registry: CodecRegistry,
        control_codec_id: u8,
        game_codec_id: u8,
    ) -> Self {
        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Closed,
            codec_registry,
            control_codec_id,
            game_codec_id,
        }
    }

    /// Initiates connection by sending HELLO message
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        tracing::info!("Sending HELLO");

        // Transition from Closed to Connecting
        self.state.transition_to(ConnectionState::Connecting)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            codec_id: self.game_codec_id,
            schema_hash: 0,
        };

        self.send_control_message(routes::HELLO, &hello).await?;

        // Transition from Connecting to HelloSent
        self.state.transition_to(ConnectionState::HelloSent)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

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
                    tracing::info!("Client shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles a single incoming envelope
    async fn handle_envelope(&mut self, envelope: Envelope) {
        tracing::debug!(
            route_id = envelope.route_id,
            msg_id = envelope.msg_id,
            payload_len = envelope.payload_len,
            state = ?self.state,
            "Client received envelope"
        );

        // Dispatch based on route_id: control messages (<100) vs game messages (>=100)
        if envelope.route_id < GAME_MESSAGES_START {
            if let Err(e) = self.handle_control_message(envelope).await {
                tracing::error!(error = %e, "Error handling control message");
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
                tracing::warn!(route_id = envelope.route_id, "Unknown control message");
                Ok(())
            }
        }
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, envelope: Envelope) {
        tracing::debug!(route_id = envelope.route_id, "Received game message");
        // Application will handle this
    }

    /// Handles HELLO_OK message from server
    async fn handle_hello_ok(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ClientError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let hello_ok: HelloOk = codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_OK: {}", e)))?;

        tracing::info!(
            version = format!("{:#06x}", hello_ok.server_version),
            "HELLO_OK received"
        );

        // Transition to Connected state
        self.state.transition_to(ConnectionState::Connected)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;
        tracing::info!("Connection established");

        Ok(())
    }

    /// Handles HELLO_ERROR message from server
    async fn handle_hello_error(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ClientError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let hello_error: HelloError = codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_ERROR: {}", e)))?;

        tracing::error!(
            reason = ?hello_error.reason,
            message = %hello_error.message,
            "HELLO_ERROR received"
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
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ClientError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let payload_bytes = codec.encode(message)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.control_codec_id,
            0, // schema_hash (not used yet)
            route_id,
            0, // msg_id (control messages don't need unique IDs)
            EnvelopeFlags::RELIABLE,
            payload_bytes,
        );

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

    #[error("Invalid state transition: {0}")]
    InvalidStateTransition(String),

    #[error("Codec error: {0}")]
    CodecError(String),
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
