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
    messages::{routes, AuthRequest, AuthResponse, Disconnect, DisconnectReason, Hello, HelloError, HelloOk, Ping, Pong, GAME_MESSAGES_START},
    CodecType, ConnectionState, Envelope, EnvelopeFlags, MessageRegistry, CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Client configuration
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Timeout for HELLO handshake
    pub hello_timeout: Duration,

    /// Keepalive PING interval
    pub keepalive_interval: Duration,

    /// Connection timeout if no messages received
    pub connection_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(60),
        }
    }
}

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

    /// Codec to use for control messages (default: JSON)
    control_codec: CodecType,

    /// Codec to use for game messages
    game_codec: CodecType,

    /// Message ID counter for game messages
    msg_id_counter: u64,

    /// Last time a message was received (for connection timeout detection)
    last_received: Instant,

    /// Last time a PING was sent (for keepalive)
    last_ping_sent: Instant,

    /// Client configuration (timeouts, intervals)
    config: ClientConfig,

    /// Optional message registry for schema validation
    message_registry: Option<MessageRegistry>,
}

impl Client {
    /// Creates a new client with the given channels and default codecs
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    pub fn new(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
    ) -> Self {
        Self::with_codec_ids(
            incoming_rx,
            outgoing_tx,
            1, // JSON for control messages
            1, // JSON for game messages
        )
    }

    /// Creates a new client with custom codec IDs
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive envelopes from transport
    /// * `outgoing_tx` - Channel to send envelopes to transport
    /// * `control_codec_id` - Codec ID for control messages (typically 1=JSON)
    /// * `game_codec_id` - Codec ID for game messages (1=JSON, 2=Postcard, 3=Raw)
    pub fn with_codec_ids(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        control_codec_id: u8,
        game_codec_id: u8,
    ) -> Self {
        let control_codec = CodecType::from_id(control_codec_id)
            .unwrap_or_else(|_| panic!("Invalid control codec ID: {}", control_codec_id));
        let game_codec = CodecType::from_id(game_codec_id)
            .unwrap_or_else(|_| panic!("Invalid game codec ID: {}", game_codec_id));

        Self::with_config(
            incoming_rx,
            outgoing_tx,
            control_codec,
            game_codec,
            ClientConfig::default(),
        )
    }

    /// Creates a new client with full configuration
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive envelopes from transport
    /// * `outgoing_tx` - Channel to send envelopes to transport
    /// * `control_codec` - Codec for control messages (typically JSON)
    /// * `game_codec` - Codec for game messages (JSON, Postcard, or Raw)
    /// * `config` - Client configuration (timeouts, intervals)
    pub fn with_config(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        control_codec: CodecType,
        game_codec: CodecType,
        config: ClientConfig,
    ) -> Self {
        Self::with_full_config(incoming_rx, outgoing_tx, control_codec, game_codec, config, None)
    }

    /// Creates a new client with message registry for schema validation
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive envelopes from transport
    /// * `outgoing_tx` - Channel to send envelopes to transport
    /// * `control_codec` - Codec for control messages (typically JSON)
    /// * `game_codec` - Codec for game messages (JSON, Postcard, or Raw)
    /// * `config` - Client configuration (timeouts, intervals)
    /// * `message_registry` - Optional message registry for schema validation
    pub fn with_full_config(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        control_codec: CodecType,
        game_codec: CodecType,
        config: ClientConfig,
        message_registry: Option<MessageRegistry>,
    ) -> Self {
        let now = Instant::now();
        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Closed,
            control_codec,
            game_codec,
            msg_id_counter: 1, // Start message IDs from 1
            last_received: now,
            last_ping_sent: now,
            config,
            message_registry,
        }
    }

    /// Initiates connection by sending HELLO message
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        tracing::info!("Sending HELLO");

        // Transition from Closed to Connecting
        self.state.transition_to(ConnectionState::Connecting)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        // Calculate schema hash from message registry (or 0 if not set)
        let schema_hash = self.message_registry
            .as_ref()
            .map(|r| r.global_schema_hash())
            .unwrap_or(0);

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            codec_id: self.game_codec.id(),
            schema_hash,
        };

        self.send_control_message(routes::HELLO, &hello).await?;

        // Transition from Connecting to HelloSent
        self.state.transition_to(ConnectionState::HelloSent)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Gracefully disconnects from the server
    pub async fn disconnect(&mut self, reason: DisconnectReason, message: String) -> Result<(), ClientError> {
        tracing::info!(reason = ?reason, message = %message, "Disconnecting");

        let disconnect = Disconnect { reason, message };
        self.send_control_message(routes::DISCONNECT, &disconnect).await?;

        // Transition to Closed state
        self.state.transition_to(ConnectionState::Closed)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Authenticate with the server
    ///
    /// # Arguments
    ///
    /// * `method` - Authentication method (e.g., "mock", "passcode", "steam")
    /// * `credentials` - Authentication credentials as bytes
    ///
    /// # Returns
    ///
    /// Ok(()) if authentication succeeds, Err otherwise
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Not in Connected state
    /// - Failed to send AUTH_REQUEST
    /// - Server rejected authentication
    pub async fn authenticate(&mut self, method: &str, credentials: &[u8]) -> Result<(), ClientError> {
        // Authentication only allowed in Connected state
        if !self.state.is_connected() {
            return Err(ClientError::InvalidStateTransition(
                format!("Cannot authenticate in state: {}", self.state)
            ));
        }

        tracing::info!(method = %method, "Sending AUTH_REQUEST");

        // Create and send AUTH_REQUEST
        let auth_request = AuthRequest {
            method: method.to_string(),
            credentials: credentials.to_vec(),
        };

        self.send_control_message(routes::AUTH_REQUEST, &auth_request).await?;

        // Transition to AuthPending state
        self.state.transition_to(ConnectionState::AuthPending)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        tracing::debug!("Waiting for AUTH_RESPONSE");

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
                Some(envelope) = self.incoming_rx.recv() => {
                    // Update last received time
                    self.last_received = Instant::now();
                    self.handle_envelope(envelope).await;
                }

                _ = interval.tick() => {
                    // Periodic tasks: check timeouts and send keepalive
                    if let Err(e) = self.handle_periodic_tasks().await {
                        tracing::error!(error = %e, "Periodic task error");
                        break;
                    }
                }

                else => {
                    tracing::info!("Client shutting down: incoming channel closed");
                    break;
                }
            }
        }
    }

    /// Handles periodic tasks: timeouts and keepalive
    async fn handle_periodic_tasks(&mut self) -> Result<(), ClientError> {
        let now = Instant::now();

        // Check HELLO timeout
        if self.state == ConnectionState::HelloSent
            && now.duration_since(self.last_received) > self.config.hello_timeout {
                tracing::error!("HELLO handshake timeout");
                return Err(ClientError::HelloTimeout);
            }

        // Check connection timeout (only when connected)
        if self.state.is_connected() {
            if now.duration_since(self.last_received) > self.config.connection_timeout {
                tracing::error!("Connection timeout - no messages received");
                return Err(ClientError::ConnectionTimeout);
            }

            // Send keepalive PING
            if now.duration_since(self.last_ping_sent) > self.config.keepalive_interval {
                self.send_ping().await?;
            }
        }

        Ok(())
    }

    /// Sends a PING message for keepalive
    async fn send_ping(&mut self) -> Result<(), ClientError> {
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
            routes::AUTH_RESPONSE => self.handle_auth_response(envelope).await,
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
    async fn handle_game_message(&mut self, envelope: Envelope) {
        tracing::debug!(route_id = envelope.route_id, "Received game message");
        // Application will handle this
    }

    /// Handles HELLO_OK message from server
    async fn handle_hello_ok(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let hello_ok: HelloOk = self.control_codec.decode(&envelope.payload)
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
        let hello_error: HelloError = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_ERROR: {}", e)))?;

        tracing::error!(
            reason = ?hello_error.reason,
            message = %hello_error.message,
            "HELLO_ERROR received"
        );

        // Stay in HelloSent (or could transition to Closed)
        Ok(())
    }

    /// Handles DISCONNECT message from server
    async fn handle_disconnect(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let disconnect: Disconnect = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse DISCONNECT: {}", e)))?;

        tracing::info!(
            reason = ?disconnect.reason,
            message = %disconnect.message,
            "DISCONNECT received from server"
        );

        // Transition to Closed state
        self.state.transition_to(ConnectionState::Closed)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Handles AUTH_RESPONSE message from server
    async fn handle_auth_response(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        // AUTH_RESPONSE only allowed in AuthPending state
        if !self.state.is_auth_pending() {
            tracing::warn!(state = ?self.state, "AUTH_RESPONSE received in invalid state");
            return Ok(());
        }

        // Parse AUTH_RESPONSE
        let auth_response: AuthResponse = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse AUTH_RESPONSE: {}", e)))?;

        if auth_response.success {
            tracing::info!(
                session_id = ?auth_response.session_id,
                "Authentication successful"
            );

            // Transition to Authorized state
            self.state.transition_to(ConnectionState::Authorized)
                .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;
        } else {
            tracing::warn!(
                error = ?auth_response.error_message,
                "Authentication failed"
            );

            // Transition to Closed state
            self.state.transition_to(ConnectionState::Closed)
                .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

            return Err(ClientError::AuthenticationFailed(
                auth_response.error_message.unwrap_or_else(|| "Unknown error".to_string())
            ));
        }

        Ok(())
    }

    /// Handles PING message from server
    async fn handle_ping(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let ping: Ping = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse PING: {}", e)))?;

        tracing::debug!(timestamp = ping.timestamp, "PING received, sending PONG");

        // Send PONG with the same timestamp
        let pong = Pong { timestamp: ping.timestamp };
        self.send_control_message(routes::PONG, &pong).await?;

        Ok(())
    }

    /// Handles PONG message from server
    async fn handle_pong(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let pong: Pong = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse PONG: {}", e)))?;

        // Calculate round-trip time
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let rtt = now.saturating_sub(pong.timestamp);

        tracing::debug!(timestamp = pong.timestamp, rtt_ms = rtt, "PONG received");

        Ok(())
    }

    /// Sends a control message to the server
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ClientError> {
        let payload_bytes = self.control_codec.encode(message)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.control_codec.id(),
            0,
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
    ///
    /// # Deprecation Notice
    ///
    /// **For game messages (route_id >= 100)**, prefer using the type-safe API:
    /// ```ignore
    /// client.send_message(my_message).await?;
    /// ```
    ///
    /// This low-level API should only be used for:
    /// - Control messages (route_id < 100)
    /// - Custom protocol extensions
    /// - Testing and debugging
    pub async fn send(&self, envelope: Envelope) -> Result<(), ClientError> {
        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)
    }

    /// Sends a type-safe game message to the server
    ///
    /// This method provides a type-safe API for sending game messages with
    /// automatic route ID and schema hash handling.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use godot_netlink_client::Client;
    /// use godot_netlink_protocol::GameMessage;
    /// use serde::{Serialize, Deserialize};
    /// use tokio::sync::mpsc;
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct PlayerInput {
    ///     x: f32,
    ///     y: f32,
    /// }
    ///
    /// impl GameMessage for PlayerInput {
    ///     const ROUTE_ID: u16 = 100;
    ///     const SCHEMA_HASH: u64 = 0x1234_5678_90AB_CDEF;
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let (_, incoming_rx) = mpsc::channel(10);
    ///     let (outgoing_tx, _) = mpsc::channel(10);
    ///     let mut client = Client::new(incoming_rx, outgoing_tx);
    ///
    ///     // Type-safe send - route_id and schema_hash are automatic!
    ///     client.send_message(PlayerInput { x: 10.0, y: 20.0 }).await.unwrap();
    /// }
    /// ```
    ///
    /// # Type Safety
    ///
    /// This method ensures compile-time correctness:
    /// - Route ID is automatically set from `T::ROUTE_ID`
    /// - Schema hash is automatically set from `T::SCHEMA_HASH`
    /// - Message is serialized with the correct game codec
    /// - Message ID is auto-incremented
    pub async fn send_message<T: godot_netlink_protocol::GameMessage>(
        &mut self,
        message: T,
    ) -> Result<(), ClientError> {
        // Serialize message using game codec
        let payload_bytes = self.game_codec.encode(&message)
            .map_err(|e| ClientError::CodecError(format!("Failed to serialize game message: {}", e)))?;

        // Get current msg_id and increment counter
        let msg_id = self.msg_id_counter;
        self.msg_id_counter = self.msg_id_counter.wrapping_add(1);

        // Create envelope with automatic route_id and schema_hash from trait
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.game_codec.id(),
            T::SCHEMA_HASH,  // Automatic from GameMessage trait
            T::ROUTE_ID,     // Automatic from GameMessage trait
            msg_id,
            EnvelopeFlags::RELIABLE,
            payload_bytes,
        );

        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)?;

        tracing::debug!(
            route_id = T::ROUTE_ID,
            schema_hash = format!("{:#018x}", T::SCHEMA_HASH),
            msg_id,
            "Sent game message"
        );

        Ok(())
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

    #[error("HELLO handshake timeout")]
    HelloTimeout,

    #[error("Connection timeout - no messages received")]
    ConnectionTimeout,

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),
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
