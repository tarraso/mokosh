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
    auth::AuthProvider,
    compression::{Compressor, CompressionType, NoCompressor},
    encryption::{Encryptor, EncryptionType, NoEncryptor},
    messages::{routes, AuthRequest, AuthResponse, Disconnect, DisconnectReason, ErrorReason, Hello, HelloError, HelloOk, Ping, Pong, GAME_MESSAGES_START},
    negotiate_version, CodecType, ConnectionState, Envelope, EnvelopeFlags, MessageRegistry, SessionEnvelope, SessionId,
    CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Per-client session state
///
/// Each connected client has its own SessionState which tracks:
/// - Connection state (Connecting, Connected, Authorized, etc.)
/// - Timing information (last activity, last PING, RTT)
/// - Client-specific configuration (codec preferences)
/// - Security state (replay protection, rate limiting)
#[derive(Debug)]
struct SessionState {
    /// Current connection state for this client
    state: ConnectionState,

    /// Last time a message was received from this client
    last_received: Instant,

    /// Last time a PING was sent to this client
    last_ping_sent: Instant,

    /// Last measured round-trip time (RTT) for this client
    last_rtt: Option<Duration>,

    /// Codec to use for game messages (set by client during HELLO)
    game_codec: CodecType,

    /// Message ID counter for game messages to this client
    msg_id_counter: u64,

    /// Last received msg_id (for replay protection and out-of-order detection)
    last_received_msg_id: u64,

    /// Recent msg_ids within tolerance window (for out-of-order delivery support)
    received_recent_ids: HashSet<u64>,

    /// Token bucket for rate limiting (current number of available tokens)
    rate_limit_tokens: f64,

    /// Last time tokens were refilled for rate limiting
    last_token_refill: Instant,
}

impl SessionState {
    /// Creates a new session state for a newly connected client
    fn new(game_codec: CodecType, rate_limit_burst: u32) -> Self {
        let now = Instant::now();
        Self {
            state: ConnectionState::Connecting,
            last_received: now,
            last_ping_sent: now,
            last_rtt: None,
            game_codec,
            msg_id_counter: 1,
            last_received_msg_id: 0,
            received_recent_ids: HashSet::new(),
            rate_limit_tokens: rate_limit_burst as f64,
            last_token_refill: now,
        }
    }
}

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Timeout for HELLO handshake
    pub hello_timeout: Duration,

    /// Keepalive PING interval
    pub keepalive_interval: Duration,

    /// Connection timeout if no messages received
    pub connection_timeout: Duration,

    /// Whether authentication is required before sending game messages
    pub auth_required: bool,

    /// Tolerance window for out-of-order messages (allow messages up to N behind last_received_msg_id)
    pub replay_tolerance_window: u64,

    /// Maximum age of messages before rejection (TTL)
    pub message_ttl: Duration,

    /// Maximum messages per second allowed per client
    pub max_messages_per_second: u32,

    /// Burst capacity for rate limiting (max accumulated tokens)
    pub rate_limit_burst: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(60),
            auth_required: false, // Backward compatible: auth is optional by default
            replay_tolerance_window: 10, // Allow up to 10 messages out-of-order
            message_ttl: Duration::from_secs(60), // Reject messages older than 60s
            max_messages_per_second: 100, // 100 msg/s steady state
            rate_limit_burst: 150, // Allow bursts up to 150 tokens
        }
    }
}

/// Server event loop handler
///
/// The server receives envelopes from incoming channel, processes them,
/// and sends responses through the outgoing channel.
///
/// The server supports multiple concurrent client connections. Each client
/// is tracked independently in the `sessions` HashMap, with per-client state,
/// RTT measurements, and keepalive timers.
///
/// Generic parameters:
/// - `C`: Compressor type (NoCompressor, ZstdCompressor, Lz4Compressor)
/// - `E`: Encryptor type (NoEncryptor, ChaCha20Poly1305Encryptor)
pub struct Server<C = NoCompressor, E = NoEncryptor>
where
    C: Compressor,
    E: Encryptor,
{
    /// Channel to receive envelopes from transport layer
    incoming_rx: mpsc::Receiver<SessionEnvelope>,

    /// Channel to send envelopes to transport layer
    outgoing_tx: mpsc::Sender<SessionEnvelope>,

    /// Map of active client sessions
    /// Each SessionId maps to a SessionState containing per-client data
    sessions: HashMap<SessionId, SessionState>,

    /// Codec to use for control messages (default: JSON)
    control_codec: CodecType,

    /// Default codec for game messages (can be overridden per-client during HELLO)
    default_game_codec: CodecType,

    /// Server configuration (timeouts, intervals)
    config: ServerConfig,

    /// Optional message registry for schema validation
    message_registry: Option<MessageRegistry>,

    /// Optional authentication provider
    auth_provider: Option<Arc<dyn AuthProvider>>,

    /// Compressor for game messages (zero-cost via generics)
    compressor: C,

    /// Encryptor for game messages (zero-cost via generics)
    encryptor: E,
}

// Default implementation (no compression, no encryption)
impl Server<NoCompressor, NoEncryptor> {
    /// Creates a new server with the given channels and default codecs
    ///
    /// Uses JSON codec (ID=1) for both control and game messages.
    /// No compression or encryption.
    pub fn new(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
    ) -> Self {
        Self::with_compression_encryption(
            incoming_rx,
            outgoing_tx,
            1, // JSON for control messages
            1, // JSON for game messages (default, updated by client HELLO)
            NoCompressor,
            NoEncryptor,
        )
    }
}

// Generic implementation for all compressor/encryptor combinations
impl<C, E> Server<C, E>
where
    C: Compressor,
    E: Encryptor,
{
    /// Creates a new server with custom compressor and encryptor
    pub fn with_compression_encryption(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
        control_codec_id: u8,
        game_codec_id: u8,
        compressor: C,
        encryptor: E,
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
            ServerConfig::default(),
            None,
            None,
            compressor,
            encryptor,
        )
    }

    /// Creates a new server with full configuration
    ///
    /// # Arguments
    ///
    /// * `incoming_rx` - Channel to receive session envelopes from transport
    /// * `outgoing_tx` - Channel to send session envelopes to transport
    /// * `control_codec` - Codec for control messages (typically JSON)
    /// * `game_codec` - Codec for game messages (JSON, Postcard, or Raw)
    /// * `config` - Server configuration (timeouts, intervals, auth_required)
    /// * `message_registry` - Optional message registry for schema validation
    /// * `auth_provider` - Optional authentication provider
    /// * `compressor` - Compressor instance (NoCompressor, ZstdCompressor, etc.)
    /// * `encryptor` - Encryptor instance (NoEncryptor, ChaCha20Poly1305Encryptor, etc.)
    pub fn with_full_config(
        incoming_rx: mpsc::Receiver<SessionEnvelope>,
        outgoing_tx: mpsc::Sender<SessionEnvelope>,
        control_codec: CodecType,
        game_codec: CodecType,
        config: ServerConfig,
        message_registry: Option<MessageRegistry>,
        auth_provider: Option<Arc<dyn AuthProvider>>,
        compressor: C,
        encryptor: E,
    ) -> Self {
        Self {
            incoming_rx,
            outgoing_tx,
            sessions: HashMap::new(), // Initially no connected clients
            control_codec,
            default_game_codec: game_codec, // Default, overridden by client HELLO
            config,
            message_registry,
            auth_provider,
            compressor,
            encryptor,
        }
    }

    /// Gracefully disconnects a specific client session
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session ID of the client to disconnect
    /// * `reason` - Reason for disconnection
    /// * `message` - Human-readable disconnect message
    pub async fn disconnect_session(
        &mut self,
        session_id: SessionId,
        reason: DisconnectReason,
        message: String,
    ) -> Result<(), ServerError> {
        tracing::info!(
            session = %session_id,
            reason = ?reason,
            message = %message,
            "Disconnecting client session"
        );

        let disconnect = Disconnect { reason, message: message.clone() };
        self.send_control_message(session_id, routes::DISCONNECT, &disconnect).await?;

        // Remove session from HashMap
        if let Some(session_state) = self.sessions.remove(&session_id) {
            tracing::debug!(
                session = %session_id,
                final_state = ?session_state.state,
                "Session removed from server"
            );
        }

        Ok(())
    }

    /// Returns the RTT for a specific client session
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session ID to query
    ///
    /// # Returns
    ///
    /// `Some(Duration)` if RTT has been measured for this session, `None` otherwise
    ///
    /// # Example
    ///
    /// ```no_run
    /// if let Some(rtt) = server.get_session_rtt(session_id) {
    ///     println!("Client RTT: {}ms", rtt.as_millis());
    /// }
    /// ```
    pub fn get_session_rtt(&self, session_id: SessionId) -> Option<Duration> {
        self.sessions.get(&session_id).and_then(|s| s.last_rtt)
    }

    /// Returns a list of all active session IDs
    ///
    /// # Returns
    ///
    /// A vector of SessionId for all currently connected clients
    pub fn get_active_sessions(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// Returns the number of currently connected clients
    ///
    /// # Returns
    ///
    /// The count of active sessions
    pub fn client_count(&self) -> usize {
        self.sessions.len()
    }

    /// Returns the connection state for a specific session
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session ID to query
    ///
    /// # Returns
    ///
    /// `Some(ConnectionState)` if session exists, `None` otherwise
    pub fn get_session_state(&self, session_id: SessionId) -> Option<ConnectionState> {
        self.sessions.get(&session_id).map(|s| s.state)
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
                    // Handle the envelope (which will update last_received for the session)
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

    /// Handles periodic tasks: timeouts and keepalive for all sessions
    async fn handle_periodic_tasks(&mut self) -> Result<(), ServerError> {
        let now = Instant::now();
        let mut sessions_to_remove = Vec::new();
        let mut sessions_to_ping = Vec::new();

        // Iterate over all sessions to check timeouts
        for (session_id, session_state) in &self.sessions {
            // Check HELLO timeout (only when in Connecting state)
            if session_state.state == ConnectionState::Connecting
                && now.duration_since(session_state.last_received) > self.config.hello_timeout {
                    tracing::error!(session = %session_id, "HELLO handshake timeout");
                    sessions_to_remove.push(*session_id);
                    continue;
                }

            // Check connection timeout (only when connected or authorized)
            if session_state.state.is_connected() || session_state.state.is_authorized() {
                if now.duration_since(session_state.last_received) > self.config.connection_timeout {
                    tracing::error!(session = %session_id, "Connection timeout - no messages received");
                    sessions_to_remove.push(*session_id);
                    continue;
                }

                // Check if we need to send keepalive PING
                if now.duration_since(session_state.last_ping_sent) > self.config.keepalive_interval {
                    sessions_to_ping.push(*session_id);
                }
            }
        }

        // Send PINGs to sessions that need it
        for session_id in sessions_to_ping {
            if let Err(e) = self.send_ping(session_id).await {
                tracing::error!(session = %session_id, error = %e, "Failed to send PING");
                sessions_to_remove.push(session_id);
            }
        }

        // Remove timed-out sessions
        for session_id in sessions_to_remove {
            self.sessions.remove(&session_id);
            tracing::info!(session = %session_id, "Session removed due to timeout");
        }

        Ok(())
    }

    /// Sends a PING message for keepalive to a specific client
    async fn send_ping(&mut self, session_id: SessionId) -> Result<(), ServerError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let ping = Ping { timestamp };
        self.send_control_message(session_id, routes::PING, &ping).await?;

        // Update last_ping_sent for this session
        if let Some(session_state) = self.sessions.get_mut(&session_id) {
            session_state.last_ping_sent = Instant::now();
        }

        tracing::debug!(session = %session_id, timestamp, "Sent PING");
        Ok(())
    }

    /// Handles a single session envelope
    async fn handle_session_envelope(&mut self, session_envelope: SessionEnvelope) -> Result<(), ServerError> {
        let session_id = session_envelope.session_id;
        let envelope = session_envelope.envelope;

        // Get or create session state for this client
        // New clients start in Connecting state
        if !self.sessions.contains_key(&session_id) {
            tracing::info!(session = %session_id, "New client session created");
            self.sessions.insert(
                session_id,
                SessionState::new(self.default_game_codec, self.config.rate_limit_burst),
            );
        }

        // Update last_received timestamp for this session
        if let Some(session_state) = self.sessions.get_mut(&session_id) {
            session_state.last_received = Instant::now();
        }

        // Get session state for logging
        let session_state = self.sessions.get(&session_id).unwrap();

        tracing::debug!(
            session = %session_id,
            route_id = envelope.route_id,
            msg_id = envelope.msg_id,
            payload_len = envelope.payload_len,
            state = ?session_state.state,
            "Server received envelope"
        );

        // Dispatch based on route_id: control messages (<100) vs game messages (>=100)
        if envelope.route_id < GAME_MESSAGES_START {
            self.handle_control_message(session_id, envelope).await
        } else {
            self.handle_game_message(session_id, envelope).await
        }
    }

    /// Handles control messages (route_id < 100)
    async fn handle_control_message(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        match envelope.route_id {
            routes::HELLO => self.handle_hello(session_id, envelope).await,
            routes::AUTH_REQUEST => self.handle_auth_request(session_id, envelope).await,
            routes::DISCONNECT => self.handle_disconnect(session_id, envelope).await,
            routes::PING => self.handle_ping(session_id, envelope).await,
            routes::PONG => self.handle_pong(session_id, envelope).await,
            _ => {
                tracing::warn!(session = %session_id, route_id = envelope.route_id, "Unknown control message");
                Ok(())
            }
        }
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        let session_state = self.sessions.get_mut(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // 1. TTL check: reject messages older than message_ttl
        let now = Instant::now();
        let age = now.duration_since(session_state.last_received);
        if age > self.config.message_ttl {
            tracing::warn!(
                session = %session_id,
                msg_id = envelope.msg_id,
                age_ms = age.as_millis(),
                ttl_ms = self.config.message_ttl.as_millis(),
                "Message too old (TTL expired)"
            );

            let disconnect = Disconnect {
                reason: DisconnectReason::MessageTooOld,
                message: format!("Message age {}ms exceeds TTL {}ms", age.as_millis(), self.config.message_ttl.as_millis()),
            };
            let disconnect_payload = self.control_codec.encode(&disconnect)
                .map_err(|e| ServerError::CodecError(e.to_string()))?;
            let disconnect_envelope = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::DISCONNECT,
                0,
                EnvelopeFlags::RELIABLE,
                disconnect_payload,
            );

            let _ = self.outgoing_tx.send(SessionEnvelope::new(session_id, disconnect_envelope)).await;
            self.sessions.remove(&session_id);
            return Ok(());
        }

        // 2. Replay protection with tolerance window
        let msg_id = envelope.msg_id;
        let last_received = session_state.last_received_msg_id;

        if msg_id > last_received {
            // New message (most common case) - accept immediately
            session_state.last_received_msg_id = msg_id;
            session_state.received_recent_ids.insert(msg_id);

        } else if msg_id >= last_received.saturating_sub(self.config.replay_tolerance_window) {
            // Within tolerance window - check for duplicates
            if session_state.received_recent_ids.contains(&msg_id) {
                tracing::warn!(
                    session = %session_id,
                    msg_id = envelope.msg_id,
                    last_received = last_received,
                    "Replay attack detected: duplicate msg_id within tolerance window"
                );

                let disconnect = Disconnect {
                    reason: DisconnectReason::ReplayAttack,
                    message: format!("Duplicate message ID: {}", envelope.msg_id),
                };
                let disconnect_payload = self.control_codec.encode(&disconnect)
                    .map_err(|e| ServerError::CodecError(e.to_string()))?;
                let disconnect_envelope = Envelope::new_simple(
                    CURRENT_PROTOCOL_VERSION,
                    self.control_codec.id(),
                    0,
                    routes::DISCONNECT,
                    0,
                    EnvelopeFlags::RELIABLE,
                    disconnect_payload,
                );

                let _ = self.outgoing_tx.send(SessionEnvelope::new(session_id, disconnect_envelope)).await;
                self.sessions.remove(&session_id);
                return Ok(());
            }

            // Out-of-order but not duplicate - accept
            session_state.received_recent_ids.insert(msg_id);

        } else {
            // msg_id too old (beyond tolerance window)
            tracing::warn!(
                session = %session_id,
                msg_id = envelope.msg_id,
                last_received = last_received,
                tolerance_window = self.config.replay_tolerance_window,
                "Message ID too old (beyond tolerance window)"
            );

            let disconnect = Disconnect {
                reason: DisconnectReason::ProtocolViolation,
                message: format!("Message ID {} too old (last_received: {})", msg_id, last_received),
            };
            let disconnect_payload = self.control_codec.encode(&disconnect)
                .map_err(|e| ServerError::CodecError(e.to_string()))?;
            let disconnect_envelope = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::DISCONNECT,
                0,
                EnvelopeFlags::RELIABLE,
                disconnect_payload,
            );

            let _ = self.outgoing_tx.send(SessionEnvelope::new(session_id, disconnect_envelope)).await;
            self.sessions.remove(&session_id);
            return Ok(());
        }

        // 3. Cleanup: remove msg_ids outside tolerance window
        let min_id = session_state.last_received_msg_id.saturating_sub(self.config.replay_tolerance_window);
        session_state.received_recent_ids.retain(|&id| id >= min_id);

        // Rate limiting: check if tokens are available
        let session_state = self.sessions.get_mut(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // Refill tokens based on time elapsed since last refill
        let now = Instant::now();
        let elapsed = now.duration_since(session_state.last_token_refill).as_secs_f64();
        let tokens_to_add = elapsed * (self.config.max_messages_per_second as f64);
        session_state.rate_limit_tokens = (session_state.rate_limit_tokens + tokens_to_add)
            .min(self.config.rate_limit_burst as f64);
        session_state.last_token_refill = now;

        // Check if we have at least 1 token available
        if session_state.rate_limit_tokens < 1.0 {
            tracing::warn!(
                session = %session_id,
                tokens = session_state.rate_limit_tokens,
                "Rate limit exceeded"
            );

            // Disconnect client for rate limit violation
            let disconnect = Disconnect {
                reason: DisconnectReason::RateLimitExceeded,
                message: "Too many messages per second".to_string(),
            };
            let disconnect_payload = self.control_codec.encode(&disconnect)
                .map_err(|e| ServerError::CodecError(e.to_string()))?;
            let disconnect_envelope = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::DISCONNECT,
                0,
                EnvelopeFlags::RELIABLE,
                disconnect_payload,
            );

            let _ = self.outgoing_tx.send(SessionEnvelope::new(session_id, disconnect_envelope)).await;
            self.sessions.remove(&session_id);

            return Ok(());
        }

        // Consume 1 token for this message
        session_state.rate_limit_tokens -= 1.0;

        // Get session state to check authorization (immutable borrow)
        let session_state = self.sessions.get(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // Game messages require authentication if auth_required is true
        if self.config.auth_required && !session_state.state.is_authorized() {
            tracing::debug!(
                session = %session_id,
                route_id = envelope.route_id,
                state = ?session_state.state,
                "Rejecting game message: authentication required"
            );
            return Ok(());
        }

        // Game messages are only allowed in Connected or Authorized state
        if !session_state.state.is_connected() && !session_state.state.is_authorized() {
            tracing::debug!(
                session = %session_id,
                route_id = envelope.route_id,
                state = ?session_state.state,
                "Rejecting game message in invalid state"
            );
            return Ok(());
        }

        // Echo the message back to this specific client
        tracing::debug!(session = %session_id, route_id = envelope.route_id, "Echoing game message");

        let session_envelope = SessionEnvelope::new(session_id, envelope);
        self.outgoing_tx
            .send(session_envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }

    /// Handles HELLO message from client
    async fn handle_hello(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        // Parse HELLO message using control codec
        let hello: Hello = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse HELLO: {}", e)))?;

        tracing::info!(
            session = %session_id,
            version = format!("{:#06x}", hello.protocol_version),
            min_version = format!("{:#06x}", hello.min_protocol_version),
            codec_id = hello.codec_id,
            "HELLO received from client"
        );

        // Get session state and update game codec
        let session_state = self.sessions.get_mut(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // Store the client's requested codec for game messages
        session_state.game_codec = CodecType::from_id(hello.codec_id)
            .map_err(|e| ServerError::CodecError(e.to_string()))?;

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

                // Schema validation (optional for MVP)
                // If both client and server provide non-zero schema_hash, validate they match
                // This allows incremental adoption - systems without message registry use 0
                let server_schema_hash = self.message_registry
                    .as_ref()
                    .map(|r| r.global_schema_hash())
                    .unwrap_or(0);

                if hello.schema_hash != 0 && server_schema_hash != 0 && hello.schema_hash != server_schema_hash {
                    tracing::error!(
                        client_hash = format!("{:#018x}", hello.schema_hash),
                        server_hash = format!("{:#018x}", server_schema_hash),
                        "Schema mismatch"
                    );

                    let hello_error = HelloError {
                        reason: ErrorReason::SchemaMismatch,
                        message: format!(
                            "Schema mismatch: client={:#018x}, server={:#018x}",
                            hello.schema_hash, server_schema_hash
                        ),
                        expected_schema_hash: server_schema_hash,
                    };

                    self.send_control_message(session_id, routes::HELLO_ERROR, &hello_error)
                        .await?;

                    return Ok(());
                }

                tracing::debug!(
                    session = %session_id,
                    client_schema_hash = format!("{:#018x}", hello.schema_hash),
                    server_schema_hash = format!("{:#018x}", server_schema_hash),
                    "Schema validation passed (or skipped)"
                );

                // Send HELLO_OK with the session ID
                let available_auth_methods = self.auth_provider
                    .as_ref()
                    .map(|provider| provider.supported_methods())
                    .unwrap_or_default();

                let hello_ok = HelloOk {
                    server_version: negotiated_version,
                    session_id: session_id.to_string(),
                    auth_required: self.config.auth_required,
                    available_auth_methods,
                };

                self.send_control_message(session_id, routes::HELLO_OK, &hello_ok)
                    .await?;

                // Transition to Connected state for this session
                let session_state = self.sessions.get_mut(&session_id)
                    .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

                session_state.state.transition_to(ConnectionState::Connected)
                    .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

                tracing::info!(session = %session_id, "Connection established");
            }
            Err(err) => {
                tracing::error!(session = %session_id, error = %err, "Version mismatch");

                // Send HELLO_ERROR
                let hello_error = HelloError {
                    reason: ErrorReason::VersionMismatch,
                    message: err.to_string(),
                    expected_schema_hash: 0, // Not relevant for version mismatch
                };

                self.send_control_message(session_id, routes::HELLO_ERROR, &hello_error)
                    .await?;

                // Stay in Connecting state (will disconnect)
            }
        }

        Ok(())
    }

    /// Handles AUTH_REQUEST message from client
    async fn handle_auth_request(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        // Get session state
        let session_state = self.sessions.get(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // Auth is only allowed in Connected state
        if !session_state.state.is_connected() {
            tracing::warn!(session = %session_id, state = ?session_state.state, "AUTH_REQUEST received in invalid state");
            return Ok(());
        }

        // Parse AUTH_REQUEST
        let auth_request: AuthRequest = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse AUTH_REQUEST: {}", e)))?;

        tracing::info!(session = %session_id, method = %auth_request.method, "AUTH_REQUEST received");

        // Check if auth provider is available
        let Some(ref auth_provider) = self.auth_provider else {
            tracing::warn!(session = %session_id, "AUTH_REQUEST received but no auth provider configured");
            let auth_response = AuthResponse {
                success: false,
                session_id: None,
                error_message: Some("Authentication not supported".to_string()),
            };
            self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response).await?;
            return Ok(());
        };

        // Transition to AuthPending state for this session
        let session_state = self.sessions.get_mut(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        session_state.state.transition_to(ConnectionState::AuthPending)
            .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

        // Call auth provider
        match auth_provider.authenticate(&auth_request.method, &auth_request.credentials).await {
            Ok(auth_result) => {
                use godot_netlink_protocol::auth::AuthResult;

                match auth_result {
                    AuthResult::Success { session_id: auth_session_id } => {
                        tracing::info!(session = %session_id, auth_session_id = %auth_session_id, "Authentication successful");

                        // Send success response
                        let auth_response = AuthResponse {
                            success: true,
                            session_id: Some(auth_session_id),
                            error_message: None,
                        };
                        self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response).await?;

                        // Transition to Authorized state for this session
                        let session_state = self.sessions.get_mut(&session_id)
                            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

                        session_state.state.transition_to(ConnectionState::Authorized)
                            .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;
                    }
                    AuthResult::Failure { error_message } => {
                        tracing::warn!(session = %session_id, error = %error_message, "Authentication failed");

                        // Send failure response
                        let auth_response = AuthResponse {
                            success: false,
                            session_id: None,
                            error_message: Some(error_message),
                        };
                        self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response).await?;

                        // Transition back to Closed (reject connection) and remove session
                        self.sessions.remove(&session_id);
                        tracing::info!(session = %session_id, "Session removed after auth failure");
                    }
                }
            }
            Err(e) => {
                tracing::error!(session = %session_id, error = %e, "Auth provider error");

                // Send failure response
                let auth_response = AuthResponse {
                    success: false,
                    session_id: None,
                    error_message: Some(format!("Authentication error: {}", e)),
                };
                self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response).await?;

                // Remove session after auth error
                self.sessions.remove(&session_id);
                tracing::info!(session = %session_id, "Session removed after auth error");
            }
        }

        Ok(())
    }

    /// Handles DISCONNECT message from client
    async fn handle_disconnect(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        let disconnect: Disconnect = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse DISCONNECT: {}", e)))?;

        tracing::info!(
            session = %session_id,
            reason = ?disconnect.reason,
            message = %disconnect.message,
            "DISCONNECT received from client"
        );

        // Remove session from HashMap
        if let Some(session_state) = self.sessions.remove(&session_id) {
            tracing::debug!(
                session = %session_id,
                final_state = ?session_state.state,
                "Session removed after client disconnect"
            );
        }

        Ok(())
    }

    /// Handles PING message from client
    async fn handle_ping(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        let ping: Ping = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse PING: {}", e)))?;

        tracing::debug!(session = %session_id, timestamp = ping.timestamp, "PING received, sending PONG");

        // Send PONG with the same timestamp
        let pong = Pong { timestamp: ping.timestamp };
        self.send_control_message(session_id, routes::PONG, &pong).await?;

        Ok(())
    }

    /// Handles PONG message from client
    async fn handle_pong(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        let pong: Pong = self.control_codec.decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse PONG: {}", e)))?;

        // Calculate round-trip time
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let rtt_ms = now.saturating_sub(pong.timestamp);

        // Store RTT for this session
        if let Some(session_state) = self.sessions.get_mut(&session_id) {
            session_state.last_rtt = Some(Duration::from_millis(rtt_ms));
        }

        tracing::debug!(session = %session_id, timestamp = pong.timestamp, rtt_ms, "PONG received");

        Ok(())
    }

    /// Sends a control message to a specific client
    async fn send_control_message<T: serde::Serialize>(
        &mut self,
        session_id: SessionId,
        route_id: u16,
        message: &T,
    ) -> Result<(), ServerError> {
        let payload_bytes = self.control_codec.encode(message)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            self.control_codec.id(),
            0,
            route_id,
            0, // msg_id (control messages don't need unique IDs)
            EnvelopeFlags::RELIABLE,
            payload_bytes,
        );

        let session_envelope = SessionEnvelope::new(session_id, envelope);
        self.outgoing_tx
            .send(session_envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        Ok(())
    }

    /// Sends a type-safe game message to a specific client
    ///
    /// This method provides a type-safe API for sending game messages with
    /// automatic route ID and schema hash handling.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use godot_netlink_server::Server;
    /// use godot_netlink_protocol::{GameMessage, SessionId};
    /// use serde::{Serialize, Deserialize};
    /// use tokio::sync::mpsc;
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct PlayerState {
    ///     x: f32,
    ///     y: f32,
    ///     health: u32,
    /// }
    ///
    /// impl GameMessage for PlayerState {
    ///     const ROUTE_ID: u16 = 101;
    ///     const SCHEMA_HASH: u64 = 0xFEDC_BA98_7654_3210;
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let (_, incoming_rx) = mpsc::channel(10);
    ///     let (outgoing_tx, _) = mpsc::channel(10);
    ///     let mut server = Server::new(incoming_rx, outgoing_tx);
    ///
    ///     let session_id = SessionId::new_v4();
    ///     // Type-safe send - route_id and schema_hash are automatic!
    ///     server.send_message(session_id, PlayerState { x: 10.0, y: 20.0, health: 100 })
    ///         .await
    ///         .unwrap();
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
        session_id: SessionId,
        message: T,
    ) -> Result<(), ServerError> {
        // Get session state for codec and msg_id
        let session_state = self.sessions.get_mut(&session_id)
            .ok_or_else(|| ServerError::InvalidMessage(format!("Session not found: {}", session_id)))?;

        // Serialize message using this session's game codec
        let payload_bytes = session_state.game_codec.encode(&message)
            .map_err(|e| ServerError::CodecError(format!("Failed to serialize game message: {}", e)))?;

        // Apply compression (zero-cost: NoCompressor inlines to no-op)
        let payload_bytes = self.compressor.compress(&payload_bytes)
            .map_err(|e| ServerError::CompressionError(format!("Failed to compress payload: {}", e)))?;

        // Apply encryption (zero-cost: NoEncryptor inlines to no-op)
        let payload_bytes = self.encryptor.encrypt(&payload_bytes)
            .map_err(|e| ServerError::EncryptionError(format!("Failed to encrypt payload: {}", e)))?;

        // Set flags based on actual types (const-folded by compiler)
        let mut flags = EnvelopeFlags::RELIABLE;
        if !matches!(self.compressor.compression_type(), CompressionType::None) {
            flags |= EnvelopeFlags::COMPRESSED;
        }
        if !matches!(self.encryptor.encryption_type(), EncryptionType::None) {
            flags |= EnvelopeFlags::ENCRYPTED;
        }

        // Get current msg_id and increment counter for this session
        let msg_id = session_state.msg_id_counter;
        let codec_id = session_state.game_codec.id();
        session_state.msg_id_counter = session_state.msg_id_counter.wrapping_add(1);

        // Create envelope with automatic route_id and schema_hash from trait
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            codec_id,
            T::SCHEMA_HASH,  // Automatic from GameMessage trait
            T::ROUTE_ID,     // Automatic from GameMessage trait
            msg_id,
            flags,           // Include COMPRESSED and ENCRYPTED flags if applied
            payload_bytes,
        );

        let session_envelope = SessionEnvelope::new(session_id, envelope);
        self.outgoing_tx
            .send(session_envelope)
            .await
            .map_err(|_| ServerError::ChannelSendError)?;

        tracing::debug!(
            route_id = T::ROUTE_ID,
            schema_hash = format!("{:#018x}", T::SCHEMA_HASH),
            msg_id,
            session = %session_id,
            compressed = flags.contains(EnvelopeFlags::COMPRESSED),
            encrypted = flags.contains(EnvelopeFlags::ENCRYPTED),
            "Sent game message"
        );

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
