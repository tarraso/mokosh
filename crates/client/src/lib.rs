//! # Mokosh Client
//!
//! Client-side event loop for Mokosh protocol.
//!
//! ## Example
//!
//! ```no_run
//! use mokosh_client::Client;
//! use mokosh_client::mpsc;
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

mod compat;
pub mod transport;

// Re-export compat for public API
pub use compat::mpsc;

use mokosh_protocol::{
    compression::{Compressor, NoCompressor},
    encryption::{Encryptor, NoEncryptor},
    messages::{
        routes, Ack, AuthRequest, AuthResponse, Disconnect, DisconnectReason, Hello, HelloError,
        HelloOk, Ping, Pong, GAME_MESSAGES_START,
    },
    reliability::{ExpiredMessage, ReceiveOutcome, ReliabilityConfig, ReliabilityMode, ReliabilityState, MonoMillisecond},
    CodecType, ConnectionState, Envelope, EnvelopeFlags, MessageRegistry, CURRENT_PROTOCOL_VERSION,
    MIN_PROTOCOL_VERSION,
};
use instant::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

// Import SinkExt for futures::mpsc::Sender::send() when using futures (WASM)
#[cfg(feature = "wasm")]
use futures::SinkExt;

/// Client configuration
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Timeout for HELLO handshake
    pub hello_timeout: Duration,

    /// Keepalive PING interval
    pub keepalive_interval: Duration,

    /// Connection timeout if no messages received
    pub connection_timeout: Duration,

    /// Reliability layer configuration. `None` (default) disables it — used with
    /// reliable transports (WebSocket). Set to `Some` over UDP to enable ACK +
    /// retransmission. Native-only; the browser/WASM client ignores this.
    pub reliability: Option<ReliabilityConfig>,

    /// How often the retransmission/ACK timer fires when reliability is enabled.
    pub retransmit_tick: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(60),
            reliability: None,
            retransmit_tick: Duration::from_millis(25),
        }
    }
}

/// Client event loop handler
///
/// The client sends envelopes through the outgoing channel and receives
/// responses through the incoming channel.
///
/// Generic parameters:
/// - `C`: Compressor type (NoCompressor, ZstdCompressor, Lz4Compressor)
/// - `E`: Encryptor type (NoEncryptor, ChaCha20Poly1305Encryptor)
pub struct Client<C = NoCompressor, E = NoEncryptor>
where
    C: Compressor,
    E: Encryptor,
{
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

    /// Message ID counter for game messages (legacy path; reliability disabled)
    msg_id_counter: u64,

    /// Contiguous sequence for reliable game messages (reliability enabled)
    reliable_game_seq: u64,

    /// Sequence for `UnreliableSequenced` game messages (independent space)
    sequenced_game_seq: u64,

    /// Last time a message was received (for connection timeout detection)
    last_received: Instant,

    /// Last time a PING was sent (for keepalive)
    last_ping_sent: Instant,

    /// Last measured round-trip time (RTT) from PING/PONG
    last_rtt: Option<Duration>,

    /// Client configuration (timeouts, intervals)
    config: ClientConfig,

    /// Optional message registry for schema validation
    message_registry: Option<MessageRegistry>,

    /// Compressor for game messages (zero-cost via generics)
    compressor: C,

    /// Encryptor for game messages (zero-cost via generics)
    encryptor: E,

    /// Optional channel to forward received game messages to application
    game_messages_tx: Option<mpsc::Sender<Envelope>>,

    /// Per-connection reliability state (None unless reliability is enabled)
    reliability: Option<ReliabilityState>,

    /// Counter for reliable control messages (HELLO/AUTH) sent to the server
    control_msg_id_counter: u64,

    /// Optional channel notified when a reliable message is dropped (TTL/retry exhausted)
    dropped_tx: Option<mpsc::Sender<ExpiredMessage>>,

    /// Monotonic epoch for converting wall-clock into reliability [`MonoMillisecond`]s
    reliability_epoch: Instant,
}

// Default implementation (no compression, no encryption)
impl Client<NoCompressor, NoEncryptor> {
    /// Creates a new client with the given channels and default codecs
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    /// No compression or encryption.
    pub fn new(incoming_rx: mpsc::Receiver<Envelope>, outgoing_tx: mpsc::Sender<Envelope>) -> Self {
        Self::with_game_messages(incoming_rx, outgoing_tx, None)
    }

    /// Creates a new client with a channel for receiving game messages
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    /// No compression or encryption.
    ///
    /// # Arguments
    /// * `incoming_rx` - Channel to receive envelopes from transport
    /// * `outgoing_tx` - Channel to send envelopes to transport
    /// * `game_messages_tx` - Optional channel to receive game messages (route_id >= 100)
    pub fn with_game_messages(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        game_messages_tx: Option<mpsc::Sender<Envelope>>,
    ) -> Self {
        Self::with_compression_encryption(
            incoming_rx,
            outgoing_tx,
            1, // JSON for control messages
            1, // JSON for game messages
            NoCompressor,
            NoEncryptor,
            game_messages_tx,
        )
    }
}

// Generic implementation for all compressor/encryptor combinations
impl<C, E> Client<C, E>
where
    C: Compressor,
    E: Encryptor,
{
    /// Creates a new client with custom compressor and encryptor
    pub fn with_compression_encryption(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        control_codec_id: u8,
        game_codec_id: u8,
        compressor: C,
        encryptor: E,
        game_messages_tx: Option<mpsc::Sender<Envelope>>,
    ) -> Self {
        let control_codec = CodecType::from_id(control_codec_id)
            .unwrap_or_else(|_| panic!("Invalid control codec ID: {}", control_codec_id));
        let game_codec = CodecType::from_id(game_codec_id)
            .unwrap_or_else(|_| panic!("Invalid game codec ID: {}", game_codec_id));

        Self::with_full_config(
            incoming_rx,
            outgoing_tx,
            control_codec,
            game_codec,
            ClientConfig::default(),
            None,
            compressor,
            encryptor,
            game_messages_tx,
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
    /// * `message_registry` - Optional message registry for schema validation
    /// * `compressor` - Compressor instance (NoCompressor, ZstdCompressor, etc.)
    /// * `encryptor` - Encryptor instance (NoEncryptor, ChaCha20Poly1305Encryptor, etc.)
    /// * `game_messages_tx` - Optional channel to receive game messages (route_id >= 100)
    #[allow(clippy::too_many_arguments)]
    pub fn with_full_config(
        incoming_rx: mpsc::Receiver<Envelope>,
        outgoing_tx: mpsc::Sender<Envelope>,
        control_codec: CodecType,
        game_codec: CodecType,
        config: ClientConfig,
        message_registry: Option<MessageRegistry>,
        compressor: C,
        encryptor: E,
        game_messages_tx: Option<mpsc::Sender<Envelope>>,
    ) -> Self {
        let now = Instant::now();
        let reliability = config
            .reliability
            .as_ref()
            .map(|cfg| ReliabilityState::new(cfg.clone()));

        Self {
            incoming_rx,
            outgoing_tx,
            state: ConnectionState::Closed,
            control_codec,
            game_codec,
            msg_id_counter: 1, // Start message IDs from 1
            reliable_game_seq: 1,
            sequenced_game_seq: 1,
            last_received: now,
            last_ping_sent: now,
            last_rtt: None,
            config,
            message_registry,
            compressor,
            encryptor,
            game_messages_tx,
            reliability,
            control_msg_id_counter: 1,
            dropped_tx: None,
            reliability_epoch: now,
        }
    }

    /// Sets a channel that receives notifications about reliable messages that
    /// were dropped after exhausting their TTL / retry budget.
    pub fn set_dropped_channel(&mut self, tx: mpsc::Sender<ExpiredMessage>) {
        self.dropped_tx = Some(tx);
    }

    /// Initiates connection by sending HELLO message
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        tracing::info!("Sending HELLO");

        // Transition from Closed to Connecting
        self.state
            .transition_to(ConnectionState::Connecting)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        // Calculate schema hash from message registry (or 0 if not set)
        let schema_hash = self
            .message_registry
            .as_ref()
            .map(|r| r.global_schema_hash())
            .unwrap_or(0);

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            codec_id: self.game_codec.id(),
            schema_hash,
            reliability: self.reliability.is_some(),
        };

        self.send_reliable_control_message(routes::HELLO, &hello).await?;

        // Transition from Connecting to HelloSent
        self.state
            .transition_to(ConnectionState::HelloSent)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Gracefully disconnects from the server
    pub async fn disconnect(
        &mut self,
        reason: DisconnectReason,
        message: String,
    ) -> Result<(), ClientError> {
        tracing::info!(reason = ?reason, message = %message, "Disconnecting");

        let disconnect = Disconnect { reason, message };
        self.send_control_message(routes::DISCONNECT, &disconnect)
            .await?;

        // Transition to Closed state
        self.state
            .transition_to(ConnectionState::Closed)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        Ok(())
    }

    /// Returns the last measured round-trip time (RTT) from PING/PONG exchange
    ///
    /// Returns `None` if no PONG has been received yet.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use mokosh_client::{Client, mpsc};
    /// # #[tokio::main]
    /// # async fn main() {
    /// # let (_incoming_tx, incoming_rx) = mpsc::channel(100);
    /// # let (outgoing_tx, _outgoing_rx) = mpsc::channel(100);
    /// # let client = Client::new(incoming_rx, outgoing_tx);
    /// if let Some(rtt) = client.get_last_rtt() {
    ///     println!("Current RTT: {}ms", rtt.as_millis());
    /// }
    /// # }
    /// ```
    pub fn get_last_rtt(&self) -> Option<Duration> {
        self.last_rtt
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
    pub async fn authenticate(
        &mut self,
        method: &str,
        credentials: &[u8],
    ) -> Result<(), ClientError> {
        // Authentication only allowed in Connected state
        if !self.state.is_connected() {
            return Err(ClientError::InvalidStateTransition(format!(
                "Cannot authenticate in state: {}",
                self.state
            )));
        }

        tracing::info!(method = %method, "Sending AUTH_REQUEST");

        // Create and send AUTH_REQUEST
        let auth_request = AuthRequest {
            method: method.to_string(),
            credentials: credentials.to_vec(),
        };

        self.send_reliable_control_message(routes::AUTH_REQUEST, &auth_request)
            .await?;

        // Transition to AuthPending state
        self.state
            .transition_to(ConnectionState::AuthPending)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

        tracing::debug!("Waiting for AUTH_RESPONSE");

        Ok(())
    }

    /// Runs the main event loop
    ///
    /// This method will block until the incoming channel is closed.
    #[cfg(all(feature = "native", not(feature = "wasm")))]
    pub async fn run(mut self) {
        // Interval for periodic tasks (keepalive check, timeout check)
        let mut interval = crate::compat::time::interval(Duration::from_secs(1));

        // Retransmission/ACK timer (only when reliability is enabled).
        let mut retransmit_interval = self
            .reliability
            .is_some()
            .then(|| crate::compat::time::interval(self.config.retransmit_tick));

        loop {
            let retransmit_fut = futures::future::OptionFuture::from(
                retransmit_interval.as_mut().map(|iv| iv.tick()),
            );

            tokio::select! {
                result = self.incoming_rx.recv() => {
                    match result {
                        Some(envelope) => {
                            // Update last received time
                            self.last_received = Instant::now();
                            self.handle_envelope(envelope).await;
                        }
                        None => {
                            tracing::info!("Client shutting down: incoming channel closed");
                            break;
                        }
                    }
                }

                _ = interval.tick() => {
                    // Periodic tasks: check timeouts and send keepalive
                    if let Err(e) = self.handle_periodic_tasks().await {
                        tracing::error!(error = %e, "Periodic task error");
                        break;
                    }
                }

                Some(_) = retransmit_fut => {
                    if let Err(e) = self.handle_reliability_tick().await {
                        tracing::error!(error = %e, "Reliability tick error");
                        break;
                    }
                }
            }
        }
    }

    /// Runs the main event loop (WASM version)
    ///
    /// This method will block until the incoming channel is closed.
    /// For WASM, we use futures::select! and manual periodic task tracking.
    #[cfg(feature = "wasm")]
    pub async fn run(mut self) {
        use futures::StreamExt;

        // Track last periodic task time
        let mut last_periodic_check = Instant::now();
        let periodic_interval = Duration::from_secs(1);

        loop {
            futures::select! {
                envelope = self.incoming_rx.next() => {
                    match envelope {
                        Some(envelope) => {
                            // Update last received time
                            self.last_received = Instant::now();
                            self.handle_envelope(envelope).await;
                        }
                        None => {
                            tracing::info!("Client shutting down: incoming channel closed");
                            break;
                        }
                    }
                }

                complete => {
                    // Check if it's time for periodic tasks
                    let now = Instant::now();
                    if now.duration_since(last_periodic_check) >= periodic_interval {
                        last_periodic_check = now;
                        if let Err(e) = self.handle_periodic_tasks().await {
                            tracing::error!(error = %e, "Periodic task error");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Handles periodic tasks: timeouts and keepalive
    async fn handle_periodic_tasks(&mut self) -> Result<(), ClientError> {
        let now = Instant::now();

        // Check HELLO timeout
        if self.state == ConnectionState::HelloSent
            && now.duration_since(self.last_received) > self.config.hello_timeout
        {
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
        // ACKs are unreliable and handled directly (never acknowledged).
        if envelope.route_id == routes::ACK {
            return self.handle_ack(envelope);
        }

        // Reliable control messages go through the control receiver for dedup +
        // ACK scheduling before dispatch. Gate on SEQUENCED: only HELLO/AUTH are
        // sent ReliableOrdered; best-effort control (PING/PONG/DISCONNECT) carries
        // a bare RELIABLE bit and must NOT be acknowledged.
        let sequenced = envelope.flags.contains(EnvelopeFlags::SEQUENCED);
        if sequenced && self.reliability.is_some() {
            let now = self.now_mono();
            let outcome = self
                .reliability
                .as_mut()
                .unwrap()
                .control
                .receiver
                .on_receive(envelope, now);
            match outcome {
                ReceiveOutcome::Deliver(e) => self.dispatch_control(e).await,
                ReceiveOutcome::DeliverMany(es) => {
                    for e in es {
                        self.dispatch_control(e).await?;
                    }
                    Ok(())
                }
                ReceiveOutcome::Drop | ReceiveOutcome::DropDuplicate => Ok(()),
                ReceiveOutcome::BufferOverflow => {
                    tracing::error!("control ordering buffer overflow");
                    Err(ClientError::InvalidMessage(
                        "control ordering buffer overflow".to_string(),
                    ))
                }
            }
        } else {
            self.dispatch_control(envelope).await
        }
    }

    /// Dispatches a (deduplicated) control message to its handler.
    async fn dispatch_control(&mut self, envelope: Envelope) -> Result<(), ClientError> {
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

    /// Processes an incoming ACK, clearing acknowledged outstanding messages.
    fn handle_ack(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let ack: Ack = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse ACK: {}", e)))?;
        let now = self.now_mono();
        if let Some(rel) = self.reliability.as_mut() {
            if let Some(ch) = rel.channel_by_id(ack.channel) {
                ch.sender.on_ack(&ack, now);
            }
        }
        Ok(())
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, envelope: Envelope) {
        tracing::debug!(route_id = envelope.route_id, "Received game message");

        // When reliability is enabled, dedup/reorder via the game receiver.
        if self.reliability.is_some() {
            let now = self.now_mono();
            let outcome = self
                .reliability
                .as_mut()
                .unwrap()
                .game
                .receiver
                .on_receive(envelope, now);
            match outcome {
                ReceiveOutcome::Deliver(e) => self.forward_game_message(e).await,
                ReceiveOutcome::DeliverMany(es) => {
                    for e in es {
                        self.forward_game_message(e).await;
                    }
                }
                ReceiveOutcome::Drop | ReceiveOutcome::DropDuplicate => {}
                ReceiveOutcome::BufferOverflow => {
                    tracing::error!("game ordering buffer overflow");
                }
            }
        } else {
            self.forward_game_message(envelope).await;
        }
    }

    /// Forwards a delivered game message to the application channel.
    async fn forward_game_message(&mut self, envelope: Envelope) {
        if let Some(tx) = &mut self.game_messages_tx {
            if let Err(e) = tx.send(envelope).await {
                tracing::error!(error = %e, "Failed to forward game message to application");
            }
        }
    }

    /// Handles HELLO_OK message from server
    async fn handle_hello_ok(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let hello_ok: HelloOk = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse HELLO_OK: {}", e)))?;

        tracing::info!(
            version = format!("{:#06x}", hello_ok.server_version),
            "HELLO_OK received"
        );

        // Both ends must agree on the reliability layer.
        if hello_ok.reliability != self.reliability.is_some() {
            return Err(ClientError::InvalidMessage(format!(
                "reliability mismatch: server={}, client={}",
                hello_ok.reliability,
                self.reliability.is_some()
            )));
        }

        // Transition to Connected state
        self.state
            .transition_to(ConnectionState::Connected)
            .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;
        tracing::info!("Connection established");

        Ok(())
    }

    /// Handles HELLO_ERROR message from server
    async fn handle_hello_error(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let hello_error: HelloError =
            self.control_codec.decode(&envelope.payload).map_err(|e| {
                ClientError::InvalidMessage(format!("Failed to parse HELLO_ERROR: {}", e))
            })?;

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
        let disconnect: Disconnect = self.control_codec.decode(&envelope.payload).map_err(|e| {
            ClientError::InvalidMessage(format!("Failed to parse DISCONNECT: {}", e))
        })?;

        tracing::info!(
            reason = ?disconnect.reason,
            message = %disconnect.message,
            "DISCONNECT received from server"
        );

        // Transition to Closed state
        self.state
            .transition_to(ConnectionState::Closed)
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
        let auth_response: AuthResponse =
            self.control_codec.decode(&envelope.payload).map_err(|e| {
                ClientError::InvalidMessage(format!("Failed to parse AUTH_RESPONSE: {}", e))
            })?;

        if auth_response.success {
            tracing::info!(
                session_id = ?auth_response.session_id,
                "Authentication successful"
            );

            // Transition to Authorized state
            self.state
                .transition_to(ConnectionState::Authorized)
                .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;
        } else {
            tracing::warn!(
                error = ?auth_response.error_message,
                "Authentication failed"
            );

            // Transition to Closed state
            self.state
                .transition_to(ConnectionState::Closed)
                .map_err(|e| ClientError::InvalidStateTransition(e.to_string()))?;

            return Err(ClientError::AuthenticationFailed(
                auth_response
                    .error_message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        Ok(())
    }

    /// Handles PING message from server
    async fn handle_ping(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let ping: Ping = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse PING: {}", e)))?;

        tracing::debug!(timestamp = ping.timestamp, "PING received, sending PONG");

        // Send PONG with the same timestamp
        let pong = Pong {
            timestamp: ping.timestamp,
        };
        self.send_control_message(routes::PONG, &pong).await?;

        Ok(())
    }

    /// Handles PONG message from server
    async fn handle_pong(&mut self, envelope: Envelope) -> Result<(), ClientError> {
        let pong: Pong = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to parse PONG: {}", e)))?;

        // Calculate round-trip time
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let rtt_ms = now.saturating_sub(pong.timestamp);

        // Store RTT for public API access
        self.last_rtt = Some(Duration::from_millis(rtt_ms));

        tracing::debug!(timestamp = pong.timestamp, rtt_ms, "PONG received");

        Ok(())
    }

    /// Sends a control message to the server
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ClientError> {
        let payload_bytes = self
            .control_codec
            .encode(message)
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
    pub async fn send(&mut self, envelope: Envelope) -> Result<(), ClientError> {
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
    /// use mokosh_client::Client;
    /// use mokosh_protocol::GameMessage;
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
    pub async fn send_message<T: mokosh_protocol::GameMessage>(
        &mut self,
        message: T,
    ) -> Result<(), ClientError> {
        let mode = if self.reliability.is_some() {
            ReliabilityMode::ReliableOrdered
        } else {
            ReliabilityMode::Reliable
        };
        let ttl = self.default_reliable_ttl();
        self.send_message_with(message, mode, ttl).await
    }

    /// Sends a game message with an explicit reliability mode and TTL.
    ///
    /// For reliable modes (with reliability enabled), the message is tracked for
    /// retransmission until acknowledged or until `ttl` elapses.
    pub async fn send_message_with<T: mokosh_protocol::GameMessage>(
        &mut self,
        message: T,
        mode: ReliabilityMode,
        ttl: Duration,
    ) -> Result<(), ClientError> {
        let now = self.now_mono();

        // Reliable send window full ⇒ the server is too far behind on ACKs; the
        // connection is backed up. Signal the app (terminal — not retry backpressure).
        if mode.is_reliable() {
            if let Some(rel) = self.reliability.as_ref() {
                if rel.game.sender.is_full() {
                    return Err(ClientError::SendWindowExceeded);
                }
            }
        }

        // Serialize message using game codec
        let payload_bytes = self.game_codec.encode(&message).map_err(|e| {
            ClientError::CodecError(format!("Failed to serialize game message: {}", e))
        })?;

        // Apply compression (zero-cost: NoCompressor inlines to no-op)
        let payload_bytes = self.compressor.compress(&payload_bytes).map_err(|e| {
            ClientError::CompressionError(format!("Failed to compress payload: {}", e))
        })?;

        // Apply encryption (zero-cost: NoEncryptor inlines to no-op)
        let payload_bytes = self.encryptor.encrypt(&payload_bytes).map_err(|e| {
            ClientError::EncryptionError(format!("Failed to encrypt payload: {}", e))
        })?;

        // Reliability mode bits + compression/encryption bits.
        let mut flags = mode.to_flags();
        use mokosh_protocol::compression::CompressionType;
        use mokosh_protocol::encryption::EncryptionType;
        if !matches!(self.compressor.compression_type(), CompressionType::None) {
            flags |= EnvelopeFlags::COMPRESSED;
        }
        if !matches!(self.encryptor.encryption_type(), EncryptionType::None) {
            flags |= EnvelopeFlags::ENCRYPTED;
        }

        // Assign the sequence number. Reliable messages use a contiguous space
        // (so unreliable sends can't punch ordering gaps); UnreliableSequenced
        // uses its own space; Unreliable carries no sequence.
        let msg_id = if self.reliability.is_some() {
            match mode {
                ReliabilityMode::Reliable | ReliabilityMode::ReliableOrdered => {
                    let id = self.reliable_game_seq;
                    self.reliable_game_seq = self.reliable_game_seq.wrapping_add(1);
                    id
                }
                ReliabilityMode::UnreliableSequenced => {
                    let id = self.sequenced_game_seq;
                    self.sequenced_game_seq = self.sequenced_game_seq.wrapping_add(1);
                    id
                }
                ReliabilityMode::Unreliable => 0,
            }
        } else {
            let id = self.msg_id_counter;
            self.msg_id_counter = self.msg_id_counter.wrapping_add(1);
            id
        };

        // Create envelope with automatic route_id and schema_hash from trait
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.game_codec.id(),
            T::SCHEMA_HASH, // Automatic from GameMessage trait
            T::ROUTE_ID,    // Automatic from GameMessage trait
            msg_id,
            flags, // Include COMPRESSED and ENCRYPTED flags if applied
            payload_bytes,
        );

        // Track for retransmission (no-op for unreliable modes / disabled reliability).
        if mode.is_reliable() {
            if let Some(rel) = self.reliability.as_mut() {
                rel.game.sender.on_send(&envelope, mode, ttl, now);
            }
        }

        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)?;

        tracing::debug!(
            route_id = T::ROUTE_ID,
            schema_hash = format!("{:#018x}", T::SCHEMA_HASH),
            msg_id,
            mode = ?mode,
            compressed = flags.contains(EnvelopeFlags::COMPRESSED),
            encrypted = flags.contains(EnvelopeFlags::ENCRYPTED),
            "Sent game message"
        );

        Ok(())
    }

    /// Sends a game message with guaranteed, ordered delivery (default TTL).
    pub async fn send_reliable<T: mokosh_protocol::GameMessage>(
        &mut self,
        message: T,
    ) -> Result<(), ClientError> {
        let ttl = self.default_reliable_ttl();
        self.send_message_with(message, ReliabilityMode::ReliableOrdered, ttl)
            .await
    }

    /// Sends a game message fire-and-forget (no ACK, may be lost).
    pub async fn send_unreliable<T: mokosh_protocol::GameMessage>(
        &mut self,
        message: T,
    ) -> Result<(), ClientError> {
        let ttl = self.default_reliable_ttl();
        self.send_message_with(message, ReliabilityMode::Unreliable, ttl)
            .await
    }

    /// Default TTL for reliable sends, from config (fallback 10s).
    fn default_reliable_ttl(&self) -> Duration {
        self.config
            .reliability
            .as_ref()
            .map(|c| c.default_ttl)
            .unwrap_or_else(|| Duration::from_secs(10))
    }

    /// Current reliability timestamp (monotonic milliseconds since construction).
    #[inline]
    fn now_mono(&self) -> MonoMillisecond {
        MonoMillisecond::from_millis(self.reliability_epoch.elapsed().as_millis() as u64)
    }

    /// Sends a control message reliably (tracked on the control channel) when
    /// reliability is enabled; falls back to an unreliable send otherwise.
    async fn send_reliable_control_message<T: serde::Serialize>(
        &mut self,
        route_id: u16,
        message: &T,
    ) -> Result<(), ClientError> {
        let now = self.now_mono();

        // Control send window full ⇒ connection backed up.
        if let Some(rel) = self.reliability.as_ref() {
            if rel.control.sender.is_full() {
                return Err(ClientError::SendWindowExceeded);
            }
        }

        let payload_bytes = self
            .control_codec
            .encode(message)
            .map_err(|e| ClientError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let (msg_id, flags, track) = if self.reliability.is_some() {
            let id = self.control_msg_id_counter;
            self.control_msg_id_counter = self.control_msg_id_counter.wrapping_add(1);
            (id, ReliabilityMode::ReliableOrdered.to_flags(), true)
        } else {
            (0, EnvelopeFlags::RELIABLE, false)
        };

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.control_codec.id(),
            0,
            route_id,
            msg_id,
            flags,
            payload_bytes,
        );

        if track {
            let ttl = self.default_reliable_ttl();
            if let Some(rel) = self.reliability.as_mut() {
                rel.control
                    .sender
                    .on_send(&envelope, ReliabilityMode::ReliableOrdered, ttl, now);
            }
        }

        self.outgoing_tx
            .send(envelope)
            .await
            .map_err(|_| ClientError::ChannelSendError)?;

        Ok(())
    }

    /// Drives retransmission, ACK flushing and drop reporting (called by the
    /// native retransmit timer; no-op when reliability is disabled).
    #[cfg(all(feature = "native", not(feature = "wasm")))]
    async fn handle_reliability_tick(&mut self) -> Result<(), ClientError> {
        let now = self.now_mono();
        let (expired, retransmits, acks) = match self.reliability.as_mut() {
            Some(rel) => (
                rel.poll_expired(now),
                rel.poll_retransmits(now),
                rel.build_acks(now),
            ),
            None => return Ok(()),
        };

        for env in retransmits {
            let _ = self.outgoing_tx.send(env).await;
        }
        for ack in acks {
            let payload = self
                .control_codec
                .encode(&ack)
                .map_err(|e| ClientError::CodecError(e.to_string()))?;
            let env = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::ACK,
                0,
                EnvelopeFlags::empty(),
                payload,
            );
            let _ = self.outgoing_tx.send(env).await;
        }
        if let Some(tx) = &mut self.dropped_tx {
            for ex in expired {
                let _ = tx.send(ex).await;
            }
        }

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

    #[error("Send window exceeded: server not acknowledging (connection backed up)")]
    SendWindowExceeded,

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Compression error: {0}")]
    CompressionError(String),

    #[error("Encryption error: {0}")]
    EncryptionError(String),

    #[error("Decompression error: {0}")]
    DecompressionError(String),

    #[error("Decryption error: {0}")]
    DecryptionError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use mokosh_protocol::EnvelopeFlags;

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

        let mut client_handle = Client::new(mpsc::channel(1).1, outgoing_tx);
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
