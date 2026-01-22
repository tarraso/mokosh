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

use godot_netlink_protocol::{
    codec_registry::CodecRegistry,
    messages::{routes, Disconnect, DisconnectReason, ErrorReason, Hello, HelloError, HelloOk, Ping, Pong, GAME_MESSAGES_START},
    negotiate_version, ConnectionState, Envelope, EnvelopeFlags, SessionEnvelope, SessionId,
    CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Timeout for HELLO handshake
    pub hello_timeout: Duration,

    /// Keepalive PING interval
    pub keepalive_interval: Duration,

    /// Connection timeout if no messages received
    pub connection_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(60),
        }
    }
}

/// Server event loop handler
///
/// The server receives envelopes from incoming channel, processes them,
/// and sends responses through the outgoing channel.
pub struct Server {
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<SessionEnvelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<SessionEnvelope>,

    /// Current connection state
    state: ConnectionState,

    /// Current session ID (set during HELLO handshake)
    current_session_id: Option<SessionId>,

    /// Codec registry for encoding/decoding messages
    codec_registry: CodecRegistry,

    /// Codec ID to use for control messages (default: 1 = JSON)
    control_codec_id: u8,

    /// Codec ID to use for game messages (set by client during HELLO)
    game_codec_id: u8,

    /// Last time a message was received (for connection timeout detection)
    last_received: Instant,

    /// Last time a PING was sent (for keepalive)
    last_ping_sent: Instant,

    /// Server configuration (timeouts, intervals)
    config: ServerConfig,
}

impl Server {
    /// Creates a new server with the given channels and default codec registry
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    pub fn new(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
    ) -> Self {
        Self::with_codecs(
            incoming_rx,
            outgoing_tx,
            CodecRegistry::default(),
            1, // JSON for control messages
            1, // JSON for game messages (default, updated by client HELLO)
        )
    }

    /// Creates a new server with custom codec registry and codec IDs
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive session envelopes from transport
    /// * `outgoing_tx` - Channel to send session envelopes to transport
    /// * `codec_registry` - Registry of available codecs
    /// * `control_codec_id` - Codec ID for control messages (typically 1=JSON)
    /// * `game_codec_id` - Default codec ID for game messages (updated by client HELLO)
    pub fn with_codecs(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
        codec_registry: CodecRegistry,
        control_codec_id: u8,
        game_codec_id: u8,
    ) -> Self {
        Self::with_config(
            incoming_rx,
            outgoing_tx,
            codec_registry,
            control_codec_id,
            game_codec_id,
            ServerConfig::default(),
        )
    }

    /// Creates a new server with full configuration
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive session envelopes from transport
    /// * `outgoing_tx` - Channel to send session envelopes to transport
    /// * `codec_registry` - Registry of available codecs
    /// * `control_codec_id` - Codec ID for control messages (typically 1=JSON)
    /// * `game_codec_id` - Default codec ID for game messages (updated by client HELLO)
    /// * `config` - Server configuration (timeouts, intervals)
    pub fn with_config(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
        codec_registry: CodecRegistry,
        control_codec_id: u8,
        game_codec_id: u8,
        config: ServerConfig,
    ) -> Self {
        let now = Instant::now();
        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Connecting,
            current_session_id: None,
            codec_registry,
            control_codec_id,
            game_codec_id,
            last_received: now,
            last_ping_sent: now,
            config,
        }
    }

    /// Gracefully disconnects a client
    pub async fn disconnect(&mut self, reason: DisconnectReason, message: String) -> Result<(), ServerError> {
        tracing::info!(reason = ?reason, message = %message, "Disconnecting client");

        let disconnect = Disconnect { reason, message };
        self.send_control_message(routes::DISCONNECT, &disconnect).await?;

        // Transition to Closed state
        self.state.transition_to(ConnectionState::Closed)
            .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Runs the main event loop
    ///
    /// This method will block until the incoming channel is closed.
    pub async fn run(mut self) {
        // Interval for periodic tasks (keepalive check, timeout check)
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                Some(session_envelope) = self.incoming_rx.recv() => {
                    // Update last received time
                    self.last_received = Instant::now();
                    if let Err(e) = self.handle_session_envelope(session_envelope).await {
                        tracing::error!(error = %e, "Error handling envelope");
                    }
                }

                _ = interval.tick() => {
                    // Periodic tasks: check timeouts and send keepalive
                    if let Err(e) = self.handle_periodic_tasks().await {
                        tracing::error!(error = %e, "Periodic task error");
                        break;
                    }
                }

                else => {
                    tracing::info!("Server shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles periodic tasks: timeouts and keepalive
    async fn handle_periodic_tasks(&mut self) -> Result<(), ServerError> {
        let now = Instant::now();

        // Check HELLO timeout (only when in Connecting state)
        if self.state == ConnectionState::Connecting {
            if now.duration_since(self.last_received) > self.config.hello_timeout {
                tracing::error!("HELLO handshake timeout");
                return Err(ServerError::HelloTimeout);
            }
        }

        // Check connection timeout (only when connected)
        if self.state.is_connected() {
            if now.duration_since(self.last_received) > self.config.connection_timeout {
                tracing::error!("Connection timeout - no messages received");
                return Err(ServerError::ConnectionTimeout);
            }

            // Send keepalive PING
            if now.duration_since(self.last_ping_sent) > self.config.keepalive_interval {
                self.send_ping().await?;
            }
        }

        Ok(())
    }

    /// Sends a PING message for keepalive
    async fn send_ping(&mut self) -> Result<(), ServerError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let ping = Ping { timestamp };
        self.send_control_message(routes::PING, &ping).await?;
        self.last_ping_sent = Instant::now();

        tracing::debug!(timestamp, "Sent PING");
        Ok(())
    }

    /// Handles a single session envelope
    async fn handle_session_envelope(&mut self, session_envelope: SessionEnvelope) -> Result<(), ServerError> {
        let session_id = session_envelope.session_id;
        let envelope = session_envelope.envelope;

        // Store the session ID if not already set
        if self.current_session_id.is_none() {
            self.current_session_id = Some(session_id);
        }

        tracing::debug!(
            session = %session_id,
            route_id = envelope.route_id,
            msg_id = envelope.msg_id,
            payload_len = envelope.payload_len,
            state = ?self.state,
            "Server received envelope"
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
            routes::DISCONNECT => self.handle_disconnect(envelope).await,
            routes::PING => self.handle_ping(envelope).await,
            routes::PONG => self.handle_pong(envelope).await,
            _ => {
                tracing::warn!(route_id = envelope.route_id, "Unknown control message");
                Ok(())
            }
        }
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        // Game messages are only allowed in Connected state
        if !self.state.is_connected() {
            tracing::debug!(
                route_id = envelope.route_id,
                state = ?self.state,
                "Rejecting game message in invalid state"
            );
            return Ok(());
        }

        // For now, echo back to the same session
        tracing::debug!(route_id = envelope.route_id, "Echoing game message");

        // Get the current session ID
        let session_id = self.current_session_id
            .ok_or_else(|| ServerError::InvalidMessage("No session ID set".to_string()))?;

        let session_envelope = SessionEnvelope::new(session_id, envelope);
        self.outgoing_tx
            .send(session_envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }

    /// Handles HELLO message from client
    async fn handle_hello(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        // Parse HELLO message using control codec
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ServerError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let hello: Hello = codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse HELLO: {}", e)))?;

        tracing::info!(
            version = format!("{:#06x}", hello.protocol_version),
            min_version = format!("{:#06x}", hello.min_protocol_version),
            codec_id = hello.codec_id,
            "HELLO received from client"
        );

        // Store the client's requested codec for game messages
        self.game_codec_id = hello.codec_id;

        // Negotiate version
        match negotiate_version(
            hello.protocol_version,
            hello.min_protocol_version,
            CURRENT_PROTOCOL_VERSION,
            MIN_PROTOCOL_VERSION,
        ) {
            Ok(negotiated_version) => {
                tracing::info!(
                    version = format!("{:#06x}", negotiated_version),
                    "Version negotiated"
                );

                // Get the session ID (should be set by now)
                let session_id = self.current_session_id
                    .ok_or_else(|| ServerError::InvalidMessage("No session ID set".to_string()))?;

                // Send HELLO_OK with the session ID
                let hello_ok = HelloOk {
                    server_version: negotiated_version,
                    session_id: session_id.to_string(),
                    auth_required: false,       // No auth for MVP
                    available_auth_methods: vec![],
                };

                self.send_control_message(routes::HELLO_OK, &hello_ok)
                    .await?;

                // Transition to Connected state
                self.state.transition_to(ConnectionState::Connected)
                    .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;
                tracing::info!(session = %session_id, "Connection established");
            }
            Err(err) => {
                tracing::error!(error = %err, "Version mismatch");

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

    /// Handles DISCONNECT message from client
    async fn handle_disconnect(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ServerError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let disconnect: Disconnect = codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse DISCONNECT: {}", e)))?;

        tracing::info!(
            reason = ?disconnect.reason,
            message = %disconnect.message,
            "DISCONNECT received from client"
        );

        // Transition to Closed state
        self.state.transition_to(ConnectionState::Closed)
            .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Handles PING message from client
    async fn handle_ping(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ServerError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let ping: Ping = codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse PING: {}", e)))?;

        tracing::debug!(timestamp = ping.timestamp, "PING received, sending PONG");

        // Send PONG with the same timestamp
        let pong = Pong { timestamp: ping.timestamp };
        self.send_control_message(routes::PONG, &pong).await?;

        Ok(())
    }

    /// Handles PONG message from client
    async fn handle_pong(&mut self, envelope: Envelope) -> Result<(), ServerError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ServerError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let pong: Pong = codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse PONG: {}", e)))?;

        // Calculate round-trip time
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let rtt = now.saturating_sub(pong.timestamp);

        tracing::debug!(timestamp = pong.timestamp, rtt_ms = rtt, "PONG received");

        Ok(())
    }

    /// Sends a control message to the client
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ServerError> {
        let codec = self.codec_registry.get(self.control_codec_id)
            .ok_or_else(|| ServerError::CodecError(format!("Control codec {} not found", self.control_codec_id)))?;

        let payload_bytes = codec.encode(message)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.control_codec_id,
            0, // schema_hash (not used yet)
            route_id,
            0, // msg_id (control messages don't need unique IDs)
            EnvelopeFlags::RELIABLE,
            payload_bytes,
        );

        // Get the current session ID
        let session_id = self.current_session_id
            .ok_or_else(|| ServerError::InvalidMessage("No session ID set".to_string()))?;

        let session_envelope = SessionEnvelope::new(session_id, envelope);
        self.outgoing_tx
            .send(session_envelope)
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

    #[error("Invalid state transition: {0}")]
    InvalidStateTransition(String),

    #[error("Codec error: {0}")]
    CodecError(String),

    #[error("HELLO handshake timeout")]
    HelloTimeout,

    #[error("Connection timeout - no messages received")]
    ConnectionTimeout,
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

        let session_id = SessionId::new_v4();
        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        let session_envelope = SessionEnvelope::new(session_id, test_envelope.clone());
        incoming_tx.send(session_envelope).await.unwrap();

        let received_session_envelope = outgoing_rx.recv().await.unwrap();
        assert_eq!(received_session_envelope.session_id, session_id);

        let received = received_session_envelope.envelope;
        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.msg_id, test_envelope.msg_id);
        assert_eq!(received.payload, test_envelope.payload);
    }
}
