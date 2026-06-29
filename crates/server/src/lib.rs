//! # Mokosh Server
//!
//! Server-side event loop for Mokosh protocol with event-based API.
//!
//! Uses `tick()` method for non-blocking event processing.

pub mod transport;

use mokosh_protocol::{
    auth::AuthProvider,
    compression::{CompressionType, Compressor, NoCompressor},
    encryption::{EncryptionType, Encryptor, NoEncryptor},
    messages::{
        routes, Ack, AuthRequest, AuthResponse, Disconnect, DisconnectReason, ErrorReason, Hello,
        HelloError, HelloOk, Ping, Pong, GAME_MESSAGES_START,
    },
    negotiate_version,
    reliability::{ReceiveOutcome, ReliabilityConfig, ReliabilityMode, ReliabilityState, MonoMillisecond},
    CodecType, ConnectionState, Envelope, EnvelopeFlags, MessageRegistry, SessionEnvelope,
    SessionId, CURRENT_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

/// Game events that can be returned from `Server::tick()`
///
/// These events represent game-level actions (connections, disconnections,
/// game messages) that the game server should handle. Control messages
/// (HELLO, PING, AUTH, etc.) are handled automatically by the server.
#[derive(Debug)]
pub enum GameEvent {
    PlayerConnected(SessionId),

    PlayerDisconnected(SessionId),

    GameMessage {
        session_id: SessionId,
        envelope: Envelope,
    },

    /// A reliable message could not be delivered before its TTL / retry cap
    /// expired and was given up on. Only emitted when reliability is enabled.
    MessageDropped {
        session_id: SessionId,
        /// Sequence number (envelope `msg_id`) of the dropped message.
        seq: u64,
        /// Route of the dropped message.
        route_id: u16,
    },
}

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

    /// Message ID counter for game messages (legacy path; reliability disabled)
    msg_id_counter: u64,

    /// Contiguous sequence for reliable game messages (reliability enabled)
    reliable_game_seq: u64,

    /// Sequence for `UnreliableSequenced` game messages (independent space)
    sequenced_game_seq: u64,

    /// Message ID counter for reliable control messages (handshake/auth) to this client
    control_msg_id_counter: u64,

    /// Per-session reliability state (None unless reliability is enabled)
    reliability: Option<ReliabilityState>,

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
    fn new(
        game_codec: CodecType,
        rate_limit_burst: u32,
        reliability_cfg: Option<&ReliabilityConfig>,
    ) -> Self {
        let now = Instant::now();
        Self {
            state: ConnectionState::Connecting,
            last_received: now,
            last_ping_sent: now,
            last_rtt: None,
            game_codec,
            msg_id_counter: 1,
            reliable_game_seq: 1,
            sequenced_game_seq: 1,
            control_msg_id_counter: 1,
            reliability: reliability_cfg.map(|cfg| ReliabilityState::new(cfg.clone())),
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

    /// Reliability layer configuration. `None` (default) disables it entirely
    /// — used with reliable transports (WebSocket). Set to `Some` when running
    /// over an unreliable transport (UDP) to enable ACK + retransmission.
    pub reliability: Option<ReliabilityConfig>,

    /// How often the retransmission/ACK timer fires when reliability is enabled.
    pub retransmit_tick: Duration,
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
            reliability: None,    // Disabled by default (zero-cost)
            retransmit_tick: Duration::from_millis(25),
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

    /// Internal channel for game events (connections, disconnections, game messages)
    event_tx: mpsc::UnboundedSender<GameEvent>,
    event_rx: mpsc::UnboundedReceiver<GameEvent>,

    /// Interval for periodic tasks (keepalive, timeouts)
    periodic_interval: tokio::time::Interval,

    /// Interval for reliability retransmission/ACK flushing (Some when reliability enabled)
    retransmit_interval: Option<tokio::time::Interval>,

    /// Monotonic epoch for converting wall-clock into reliability [`MonoMillisecond`]s
    reliability_epoch: Instant,
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
    #[allow(clippy::too_many_arguments)]
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
        // Create internal event channel for game events
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Create interval for periodic tasks (keepalive, timeout checks)
        let periodic_interval = tokio::time::interval(Duration::from_secs(1));

        // Create the retransmission timer only when reliability is enabled (zero-cost otherwise)
        let retransmit_interval = config
            .reliability
            .as_ref()
            .map(|_| tokio::time::interval(config.retransmit_tick));

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
            event_tx,
            event_rx,
            periodic_interval,
            retransmit_interval,
            reliability_epoch: Instant::now(),
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

        let disconnect = Disconnect {
            reason,
            message: message.clone(),
        };
        self.send_control_message(session_id, routes::DISCONNECT, &disconnect)
            .await?;

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
    /// # use mokosh_server::Server;
    /// # use mokosh_protocol::SessionId;
    /// # use tokio::sync::mpsc;
    /// # #[tokio::main]
    /// # async fn main() {
    /// # let (_incoming_tx, incoming_rx) = mpsc::channel(100);
    /// # let (outgoing_tx, _outgoing_rx) = mpsc::channel(100);
    /// # let server = Server::new(incoming_rx, outgoing_tx);
    /// # let session_id = SessionId::new_v4();
    /// if let Some(rtt) = server.get_session_rtt(session_id) {
    ///     println!("Client RTT: {}ms", rtt.as_millis());
    /// }
    /// # }
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

    /// Processes a single event (non-blocking)
    ///
    /// This method processes ONE envelope from the transport layer or ONE periodic task,
    /// and returns game events (connections, disconnections, game messages).
    /// Control messages (HELLO, PING, AUTH, etc.) are handled automatically.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(GameEvent))` - A game event occurred (connection, message, etc.)
    /// * `Ok(None)` - No event available (would block)
    /// * `Err(ServerError)` - An error occurred
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mokosh_server::{Server, GameEvent};
    /// use tokio::sync::mpsc;
    /// use std::time::Duration;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let (incoming_tx, incoming_rx) = mpsc::channel(100);
    ///     let (outgoing_tx, outgoing_rx) = mpsc::channel(100);
    ///     let mut server = Server::new(incoming_rx, outgoing_tx);
    ///
    ///     let mut game_state = Vec::new();
    ///     let mut interval = tokio::time::interval(Duration::from_millis(16)); // 60 FPS
    ///
    ///     loop {
    ///         tokio::select! {
    ///             _ = interval.tick() => {
    ///                 // Update game state
    ///                 // Broadcast to all clients
    ///             }
    ///
    ///             result = server.tick() => {
    ///                 match result {
    ///                     Ok(Some(GameEvent::PlayerConnected(session_id))) => {
    ///                         game_state.push(session_id);
    ///                     }
    ///                     Ok(Some(GameEvent::GameMessage { session_id: _, envelope: _ })) => {
    ///                         // Handle game message
    ///                     }
    ///                     _ => {}
    ///                 }
    ///             }
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn tick(&mut self) -> Result<Option<GameEvent>, ServerError> {
        // Fires only when reliability is enabled; otherwise resolves to `None`
        // immediately and the branch is skipped (zero-cost when disabled).
        let retransmit_fut = futures::future::OptionFuture::from(
            self.retransmit_interval.as_mut().map(|iv| iv.tick()),
        );

        tokio::select! {
            // Priority 1: Check for pending game events
            Some(event) = self.event_rx.recv() => {
                Ok(Some(event))
            }

            // Priority 2: Process incoming envelope from transport
            Some(session_envelope) = self.incoming_rx.recv() => {
                self.handle_session_envelope(session_envelope).await?;
                // Check if an event was generated
                Ok(self.event_rx.try_recv().ok())
            }

            // Priority 3: Periodic tasks (keepalive, timeouts)
            _ = self.periodic_interval.tick() => {
                self.handle_periodic_tasks().await?;
                // Check if disconnect events were generated
                Ok(self.event_rx.try_recv().ok())
            }

            // Priority 4: Reliability retransmission / ACK flushing (if enabled)
            Some(_) = retransmit_fut => {
                self.handle_reliability_tick().await?;
                Ok(self.event_rx.try_recv().ok())
            }

            else => {
                // All channels closed
                tracing::info!("Server shutting down: all channels closed");
                Ok(None)
            }
        }
    }

    /// Current reliability timestamp (monotonic milliseconds since construction).
    #[inline]
    fn now_mono(&self) -> MonoMillisecond {
        MonoMillisecond::from_millis(self.reliability_epoch.elapsed().as_millis() as u64)
    }

    /// Drives retransmission, expiry reporting and ACK flushing for all sessions.
    async fn handle_reliability_tick(&mut self) -> Result<(), ServerError> {
        let now = self.now_mono();

        let mut retransmits: Vec<(SessionId, Envelope)> = Vec::new();
        let mut acks: Vec<(SessionId, Ack)> = Vec::new();
        let mut dropped: Vec<(SessionId, u64, u16)> = Vec::new();

        for (sid, st) in self.sessions.iter_mut() {
            if let Some(rel) = st.reliability.as_mut() {
                // Expire first so timed-out messages are not retransmitted.
                for ex in rel.poll_expired(now) {
                    dropped.push((*sid, ex.seq, ex.route_id));
                }
                for env in rel.poll_retransmits(now) {
                    retransmits.push((*sid, env));
                }
                for ack in rel.build_acks(now) {
                    acks.push((*sid, ack));
                }
            }
        }

        for (sid, env) in retransmits {
            let _ = self.outgoing_tx.send(SessionEnvelope::new(sid, env)).await;
        }
        for (sid, ack) in acks {
            let payload = self
                .control_codec
                .encode(&ack)
                .map_err(|e| ServerError::CodecError(e.to_string()))?;
            let env = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::ACK,
                0,
                EnvelopeFlags::empty(),
                payload,
            );
            let _ = self.outgoing_tx.send(SessionEnvelope::new(sid, env)).await;
        }
        for (session_id, seq, route_id) in dropped {
            let _ = self.event_tx.send(GameEvent::MessageDropped {
                session_id,
                seq,
                route_id,
            });
        }

        Ok(())
    }

    /// Broadcasts a message to all connected clients
    ///
    /// # Type Parameters
    ///
    /// * `T` - Must implement `GameMessage` trait (provides route_id and schema_hash)
    ///
    /// # Arguments
    ///
    /// * `message` - The message to broadcast
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mokosh_server::Server;
    /// use mokosh_protocol::GameMessage;
    /// use serde::{Serialize, Deserialize};
    /// use tokio::sync::mpsc;
    ///
    /// #[derive(Serialize, Deserialize, Clone)]
    /// struct GameState {
    ///     tick: u64,
    /// }
    ///
    /// impl GameMessage for GameState {
    ///     const ROUTE_ID: u16 = 101;
    ///     const SCHEMA_HASH: u64 = 0xABCD;
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let (_, incoming_rx) = mpsc::channel(10);
    ///     let (outgoing_tx, _) = mpsc::channel(10);
    ///     let mut server = Server::new(incoming_rx, outgoing_tx);
    ///
    ///     // Broadcast to all connected clients
    ///     server.broadcast(GameState { tick: 123 }).await.unwrap();
    /// }
    /// ```
    pub async fn broadcast<T: mokosh_protocol::GameMessage + Clone>(
        &mut self,
        message: T,
    ) -> Result<(), ServerError> {
        // Get all connected/authorized sessions
        let sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, state)| state.state.is_connected() || state.state.is_authorized())
            .map(|(id, _)| *id)
            .collect();

        // Send to each session
        for session_id in sessions {
            self.send_message(session_id, message.clone()).await?;
        }

        Ok(())
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
                && now.duration_since(session_state.last_received) > self.config.hello_timeout
            {
                tracing::error!(session = %session_id, "HELLO handshake timeout");
                sessions_to_remove.push(*session_id);
                continue;
            }

            // Check connection timeout (only when connected or authorized)
            if session_state.state.is_connected() || session_state.state.is_authorized() {
                if now.duration_since(session_state.last_received) > self.config.connection_timeout
                {
                    tracing::error!(session = %session_id, "Connection timeout - no messages received");
                    sessions_to_remove.push(*session_id);
                    continue;
                }

                // Check if we need to send keepalive PING
                if now.duration_since(session_state.last_ping_sent) > self.config.keepalive_interval
                {
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

        // Remove timed-out sessions and send disconnect events
        for session_id in sessions_to_remove {
            self.sessions.remove(&session_id);
            tracing::info!(session = %session_id, "Session removed due to timeout");

            // Notify application of player disconnection
            let _ = self
                .event_tx
                .send(GameEvent::PlayerDisconnected(session_id));
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
        self.send_control_message(session_id, routes::PING, &ping)
            .await?;

        // Update last_ping_sent for this session
        if let Some(session_state) = self.sessions.get_mut(&session_id) {
            session_state.last_ping_sent = Instant::now();
        }

        tracing::debug!(session = %session_id, timestamp, "Sent PING");
        Ok(())
    }

    /// Handles a single session envelope
    async fn handle_session_envelope(
        &mut self,
        session_envelope: SessionEnvelope,
    ) -> Result<(), ServerError> {
        let session_id = session_envelope.session_id;
        let envelope = session_envelope.envelope;

        // Get or create session state for this client
        // New clients start in Connecting state
        if !self.sessions.contains_key(&session_id) {
            tracing::info!(session = %session_id, "New client session created");
            self.sessions.insert(
                session_id,
                SessionState::new(
                    self.default_game_codec,
                    self.config.rate_limit_burst,
                    self.config.reliability.as_ref(),
                ),
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
    async fn handle_control_message(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        // ACKs are always unreliable and handled directly (never acknowledged).
        if envelope.route_id == routes::ACK {
            return self.handle_ack(session_id, envelope);
        }

        // Reliable control messages (HELLO/AUTH) go through the control-channel
        // receiver for dedup + ACK scheduling before dispatch. Gate on SEQUENCED:
        // only HELLO/AUTH are sent ReliableOrdered; best-effort control (PING/PONG/
        // DISCONNECT) carries a bare RELIABLE bit and must NOT be acknowledged.
        let sequenced = envelope.flags.contains(EnvelopeFlags::SEQUENCED);
        let has_reliability = self
            .sessions
            .get(&session_id)
            .map(|s| s.reliability.is_some())
            .unwrap_or(false);

        if sequenced && has_reliability {
            let now = self.now_mono();
            let outcome = {
                let st = self.sessions.get_mut(&session_id).unwrap();
                st.reliability
                    .as_mut()
                    .unwrap()
                    .control
                    .receiver
                    .on_receive(envelope, now)
            };
            match outcome {
                ReceiveOutcome::Deliver(e) => self.dispatch_control(session_id, e).await,
                ReceiveOutcome::DeliverMany(es) => {
                    for e in es {
                        self.dispatch_control(session_id, e).await?;
                    }
                    Ok(())
                }
                ReceiveOutcome::Drop | ReceiveOutcome::DropDuplicate => Ok(()),
                ReceiveOutcome::BufferOverflow => {
                    self.disconnect_protocol_violation(
                        session_id,
                        "control ordering buffer overflow",
                    )
                    .await
                }
            }
        } else {
            self.dispatch_control(session_id, envelope).await
        }
    }

    /// Dispatches a (deduplicated) control message to its handler.
    async fn dispatch_control(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
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

    /// Processes an incoming ACK, clearing acknowledged outstanding messages.
    fn handle_ack(&mut self, session_id: SessionId, envelope: Envelope) -> Result<(), ServerError> {
        let ack: Ack = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse ACK: {}", e)))?;
        let now = self.now_mono();
        if let Some(st) = self.sessions.get_mut(&session_id) {
            if let Some(rel) = st.reliability.as_mut() {
                if let Some(ch) = rel.channel_by_id(ack.channel) {
                    ch.sender.on_ack(&ack, now);
                }
            }
        }
        Ok(())
    }

    /// Disconnects a session for a protocol violation (best-effort notification).
    async fn disconnect_protocol_violation(
        &mut self,
        session_id: SessionId,
        message: &str,
    ) -> Result<(), ServerError> {
        let disconnect = Disconnect {
            reason: DisconnectReason::ProtocolViolation,
            message: message.to_string(),
        };
        if let Ok(payload) = self.control_codec.encode(&disconnect) {
            let env = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::DISCONNECT,
                0,
                EnvelopeFlags::RELIABLE,
                payload,
            );
            let _ = self
                .outgoing_tx
                .send(SessionEnvelope::new(session_id, env))
                .await;
        }
        self.sessions.remove(&session_id);
        Ok(())
    }

    /// Tears down a session that fell too far behind on ACKs (send window full).
    /// Notifies the peer, removes the session, and emits `PlayerDisconnected`.
    async fn disconnect_session_overloaded(
        &mut self,
        session_id: SessionId,
    ) -> Result<(), ServerError> {
        let disconnect = Disconnect {
            reason: DisconnectReason::Overloaded,
            message: "Send window exceeded: peer not acknowledging".to_string(),
        };
        if let Ok(payload) = self.control_codec.encode(&disconnect) {
            let env = Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                self.control_codec.id(),
                0,
                routes::DISCONNECT,
                0,
                EnvelopeFlags::RELIABLE,
                payload,
            );
            let _ = self
                .outgoing_tx
                .send(SessionEnvelope::new(session_id, env))
                .await;
        }
        if self.sessions.remove(&session_id).is_some() {
            let _ = self.event_tx.send(GameEvent::PlayerDisconnected(session_id));
        }
        Ok(())
    }

    /// Handles game messages (route_id >= 100)
    async fn handle_game_message(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        // When reliability is enabled, dedup/ordering is the receiver's job and
        // the security replay window is bypassed (retransmits are legitimate dups).
        let has_reliability = self
            .sessions
            .get(&session_id)
            .map(|s| s.reliability.is_some())
            .unwrap_or(false);

        if has_reliability {
            // Don't run undeliverable messages (e.g. arriving before the handshake
            // completes, via UDP reorder) through the receiver — that would ACK them
            // and they'd never be delivered. Drop without ACK so the sender
            // retransmits until the session is ready.
            if !self.game_message_deliverable(session_id) {
                return Ok(());
            }

            let now = self.now_mono();
            let outcome = {
                let st = self.sessions.get_mut(&session_id).ok_or_else(|| {
                    ServerError::InvalidMessage(format!("Session not found: {}", session_id))
                })?;
                st.reliability
                    .as_mut()
                    .unwrap()
                    .game
                    .receiver
                    .on_receive(envelope, now)
            };
            return match outcome {
                ReceiveOutcome::Deliver(e) => self.accept_game_message(session_id, e).await,
                ReceiveOutcome::DeliverMany(es) => {
                    for e in es {
                        self.accept_game_message(session_id, e).await?;
                    }
                    Ok(())
                }
                ReceiveOutcome::Drop | ReceiveOutcome::DropDuplicate => Ok(()),
                ReceiveOutcome::BufferOverflow => {
                    self.disconnect_protocol_violation(session_id, "game ordering buffer overflow")
                        .await
                }
            };
        }

        // Legacy path (no reliability): wall-clock TTL + replay protection.
        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

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
                message: format!(
                    "Message age {}ms exceeds TTL {}ms",
                    age.as_millis(),
                    self.config.message_ttl.as_millis()
                ),
            };
            let disconnect_payload = self
                .control_codec
                .encode(&disconnect)
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

            let _ = self
                .outgoing_tx
                .send(SessionEnvelope::new(session_id, disconnect_envelope))
                .await;
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
                let disconnect_payload = self
                    .control_codec
                    .encode(&disconnect)
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

                let _ = self
                    .outgoing_tx
                    .send(SessionEnvelope::new(session_id, disconnect_envelope))
                    .await;
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
                message: format!(
                    "Message ID {} too old (last_received: {})",
                    msg_id, last_received
                ),
            };
            let disconnect_payload = self
                .control_codec
                .encode(&disconnect)
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

            let _ = self
                .outgoing_tx
                .send(SessionEnvelope::new(session_id, disconnect_envelope))
                .await;
            self.sessions.remove(&session_id);
            return Ok(());
        }

        // 3. Cleanup: remove msg_ids outside tolerance window
        let min_id = session_state
            .last_received_msg_id
            .saturating_sub(self.config.replay_tolerance_window);
        session_state.received_recent_ids.retain(|&id| id >= min_id);

        self.accept_game_message(session_id, envelope).await
    }

    /// Whether a game message for this session may currently be delivered
    /// (connection/auth state). Used to avoid ACKing messages we'd then drop.
    fn game_message_deliverable(&self, session_id: SessionId) -> bool {
        match self.sessions.get(&session_id) {
            Some(st) => {
                if self.config.auth_required && !st.state.is_authorized() {
                    return false;
                }
                st.state.is_connected() || st.state.is_authorized()
            }
            None => false,
        }
    }

    /// Applies rate limiting + auth/state checks and queues a delivered game
    /// message as a [`GameEvent::GameMessage`].
    async fn accept_game_message(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        // Rate limiting: check if tokens are available
        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

        // Refill tokens based on time elapsed since last refill
        let now = Instant::now();
        let elapsed = now
            .duration_since(session_state.last_token_refill)
            .as_secs_f64();
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
            let disconnect_payload = self
                .control_codec
                .encode(&disconnect)
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

            let _ = self
                .outgoing_tx
                .send(SessionEnvelope::new(session_id, disconnect_envelope))
                .await;
            self.sessions.remove(&session_id);

            return Ok(());
        }

        // Consume 1 token for this message
        session_state.rate_limit_tokens -= 1.0;

        // Get session state to check authorization (immutable borrow)
        let session_state = self.sessions.get(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

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

        // Send game message event for application to handle
        tracing::debug!(session = %session_id, route_id = envelope.route_id, "Game message received");

        // Queue the event for tick() to return
        let _ = self.event_tx.send(GameEvent::GameMessage {
            session_id,
            envelope,
        });

        Ok(())
    }

    /// Handles HELLO message from client
    async fn handle_hello(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        // Parse HELLO message using control codec
        let hello: Hello = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse HELLO: {}", e)))?;

        tracing::info!(
            session = %session_id,
            version = format!("{:#06x}", hello.protocol_version),
            min_version = format!("{:#06x}", hello.min_protocol_version),
            codec_id = hello.codec_id,
            "HELLO received from client"
        );

        // Both ends must agree on the reliability layer — reject on mismatch.
        let server_reliability = self.config.reliability.is_some();
        if hello.reliability != server_reliability {
            tracing::error!(
                session = %session_id,
                client = hello.reliability,
                server = server_reliability,
                "Reliability mismatch"
            );
            let hello_error = HelloError {
                reason: ErrorReason::ReliabilityMismatch,
                message: format!(
                    "Reliability mismatch: client={}, server={}",
                    hello.reliability, server_reliability
                ),
                expected_schema_hash: 0,
            };
            self.send_control_message(session_id, routes::HELLO_ERROR, &hello_error)
                .await?;
            self.sessions.remove(&session_id);
            return Ok(());
        }

        // Get session state and update game codec
        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

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
                let server_schema_hash = self
                    .message_registry
                    .as_ref()
                    .map(|r| r.global_schema_hash())
                    .unwrap_or(0);

                if hello.schema_hash != 0
                    && server_schema_hash != 0
                    && hello.schema_hash != server_schema_hash
                {
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
                let available_auth_methods = self
                    .auth_provider
                    .as_ref()
                    .map(|provider| provider.supported_methods())
                    .unwrap_or_default();

                let hello_ok = HelloOk {
                    server_version: negotiated_version,
                    session_id: session_id.to_string(),
                    auth_required: self.config.auth_required,
                    available_auth_methods,
                    reliability: server_reliability,
                };

                self.send_reliable_control_message(session_id, routes::HELLO_OK, &hello_ok)
                    .await?;

                // Transition to Connected state for this session
                let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
                    ServerError::InvalidMessage(format!("Session not found: {}", session_id))
                })?;

                session_state
                    .state
                    .transition_to(ConnectionState::Connected)
                    .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

                tracing::info!(session = %session_id, "Connection established");

                // Notify application of new player connection
                let _ = self.event_tx.send(GameEvent::PlayerConnected(session_id));
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
    async fn handle_auth_request(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        // Get session state
        let session_state = self.sessions.get(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

        // Auth is only allowed in Connected state
        if !session_state.state.is_connected() {
            tracing::warn!(session = %session_id, state = ?session_state.state, "AUTH_REQUEST received in invalid state");
            return Ok(());
        }

        // Parse AUTH_REQUEST
        let auth_request: AuthRequest =
            self.control_codec.decode(&envelope.payload).map_err(|e| {
                ServerError::InvalidMessage(format!("Failed to parse AUTH_REQUEST: {}", e))
            })?;

        tracing::info!(session = %session_id, method = %auth_request.method, "AUTH_REQUEST received");

        // Check if auth provider is available
        let Some(ref auth_provider) = self.auth_provider else {
            tracing::warn!(session = %session_id, "AUTH_REQUEST received but no auth provider configured");
            let auth_response = AuthResponse {
                success: false,
                session_id: None,
                error_message: Some("Authentication not supported".to_string()),
            };
            self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response)
                .await?;
            return Ok(());
        };

        // Transition to AuthPending state for this session
        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

        session_state
            .state
            .transition_to(ConnectionState::AuthPending)
            .map_err(|e| ServerError::InvalidStateTransition(e.to_string()))?;

        // Call auth provider
        match auth_provider
            .authenticate(&auth_request.method, &auth_request.credentials)
            .await
        {
            Ok(auth_result) => {
                use mokosh_protocol::auth::AuthResult;

                match auth_result {
                    AuthResult::Success {
                        session_id: auth_session_id,
                    } => {
                        tracing::info!(session = %session_id, auth_session_id = %auth_session_id, "Authentication successful");

                        // Send success response
                        let auth_response = AuthResponse {
                            success: true,
                            session_id: Some(auth_session_id),
                            error_message: None,
                        };
                        self.send_reliable_control_message(
                            session_id,
                            routes::AUTH_RESPONSE,
                            &auth_response,
                        )
                        .await?;

                        // Transition to Authorized state for this session
                        let session_state =
                            self.sessions.get_mut(&session_id).ok_or_else(|| {
                                ServerError::InvalidMessage(format!(
                                    "Session not found: {}",
                                    session_id
                                ))
                            })?;

                        session_state
                            .state
                            .transition_to(ConnectionState::Authorized)
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
                        self.send_control_message(
                            session_id,
                            routes::AUTH_RESPONSE,
                            &auth_response,
                        )
                        .await?;

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
                self.send_control_message(session_id, routes::AUTH_RESPONSE, &auth_response)
                    .await?;

                // Remove session after auth error
                self.sessions.remove(&session_id);
                tracing::info!(session = %session_id, "Session removed after auth error");
            }
        }

        Ok(())
    }

    /// Handles DISCONNECT message from client
    async fn handle_disconnect(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        let disconnect: Disconnect = self.control_codec.decode(&envelope.payload).map_err(|e| {
            ServerError::InvalidMessage(format!("Failed to parse DISCONNECT: {}", e))
        })?;

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

            // Notify application of player disconnection
            let _ = self
                .event_tx
                .send(GameEvent::PlayerDisconnected(session_id));
        }

        Ok(())
    }

    /// Handles PING message from client
    async fn handle_ping(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        let ping: Ping = self
            .control_codec
            .decode(&envelope.payload)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to parse PING: {}", e)))?;

        tracing::debug!(session = %session_id, timestamp = ping.timestamp, "PING received, sending PONG");

        // Send PONG with the same timestamp
        let pong = Pong {
            timestamp: ping.timestamp,
        };
        self.send_control_message(session_id, routes::PONG, &pong)
            .await?;

        Ok(())
    }

    /// Handles PONG message from client
    async fn handle_pong(
        &mut self,
        session_id: SessionId,
        envelope: Envelope,
    ) -> Result<(), ServerError> {
        let pong: Pong = self
            .control_codec
            .decode(&envelope.payload)
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
        let payload_bytes = self
            .control_codec
            .encode(message)
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
    /// use mokosh_server::Server;
    /// use mokosh_protocol::{GameMessage, SessionId};
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
    pub async fn send_message<T: mokosh_protocol::GameMessage>(
        &mut self,
        session_id: SessionId,
        message: T,
    ) -> Result<(), ServerError> {
        // Default to ordered reliable delivery when reliability is enabled;
        // otherwise keep the legacy bare-RELIABLE flag behavior.
        let mode = if self.config.reliability.is_some() {
            ReliabilityMode::ReliableOrdered
        } else {
            ReliabilityMode::Reliable
        };
        let ttl = self.default_reliable_ttl();
        self.send_message_with(session_id, message, mode, ttl).await
    }

    /// Sends a game message with an explicit reliability mode and TTL.
    ///
    /// The mode is encoded into the envelope flags. For reliable modes, when the
    /// server has reliability enabled, the message is tracked for retransmission
    /// until acknowledged or until `ttl` elapses (then [`GameEvent::MessageDropped`]).
    pub async fn send_message_with<T: mokosh_protocol::GameMessage>(
        &mut self,
        session_id: SessionId,
        message: T,
        mode: ReliabilityMode,
        ttl: Duration,
    ) -> Result<(), ServerError> {
        let now = self.now_mono();

        // Reliable send window full ⇒ the peer is too far behind on ACKs; drop it.
        let window_full = mode.is_reliable()
            && self
                .sessions
                .get(&session_id)
                .and_then(|s| s.reliability.as_ref())
                .map(|r| r.game.sender.is_full())
                .unwrap_or(false);
        if window_full {
            tracing::warn!(session = %session_id, "Send window full — disconnecting (overloaded)");
            return self.disconnect_session_overloaded(session_id).await;
        }

        // Get session state for codec and msg_id
        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

        // Serialize message using this session's game codec
        let payload_bytes = session_state.game_codec.encode(&message).map_err(|e| {
            ServerError::CodecError(format!("Failed to serialize game message: {}", e))
        })?;

        // Apply compression (zero-cost: NoCompressor inlines to no-op)
        let payload_bytes = self.compressor.compress(&payload_bytes).map_err(|e| {
            ServerError::CompressionError(format!("Failed to compress payload: {}", e))
        })?;

        // Apply encryption (zero-cost: NoEncryptor inlines to no-op)
        let payload_bytes = self.encryptor.encrypt(&payload_bytes).map_err(|e| {
            ServerError::EncryptionError(format!("Failed to encrypt payload: {}", e))
        })?;

        // Reliability mode bits + compression/encryption bits.
        let mut flags = mode.to_flags();
        if !matches!(self.compressor.compression_type(), CompressionType::None) {
            flags |= EnvelopeFlags::COMPRESSED;
        }
        if !matches!(self.encryptor.encryption_type(), EncryptionType::None) {
            flags |= EnvelopeFlags::ENCRYPTED;
        }

        // Assign the sequence number. With reliability on, reliable messages get
        // a contiguous space (so unreliable sends can't punch ordering gaps);
        // UnreliableSequenced uses its own space; Unreliable carries no sequence.
        let codec_id = session_state.game_codec.id();
        let msg_id = if session_state.reliability.is_some() {
            match mode {
                ReliabilityMode::Reliable | ReliabilityMode::ReliableOrdered => {
                    let id = session_state.reliable_game_seq;
                    session_state.reliable_game_seq = session_state.reliable_game_seq.wrapping_add(1);
                    id
                }
                ReliabilityMode::UnreliableSequenced => {
                    let id = session_state.sequenced_game_seq;
                    session_state.sequenced_game_seq =
                        session_state.sequenced_game_seq.wrapping_add(1);
                    id
                }
                ReliabilityMode::Unreliable => 0,
            }
        } else {
            let id = session_state.msg_id_counter;
            session_state.msg_id_counter = session_state.msg_id_counter.wrapping_add(1);
            id
        };

        // Create envelope with automatic route_id and schema_hash from trait
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            codec_id,
            T::SCHEMA_HASH, // Automatic from GameMessage trait
            T::ROUTE_ID,    // Automatic from GameMessage trait
            msg_id,
            flags, // Include COMPRESSED and ENCRYPTED flags if applied
            payload_bytes,
        );

        // Track for retransmission (no-op for unreliable modes / disabled reliability).
        if mode.is_reliable() {
            if let Some(rel) = session_state.reliability.as_mut() {
                rel.game.sender.on_send(&envelope, mode, ttl, now);
            }
        }

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
        session_id: SessionId,
        message: T,
    ) -> Result<(), ServerError> {
        let ttl = self.default_reliable_ttl();
        self.send_message_with(session_id, message, ReliabilityMode::ReliableOrdered, ttl)
            .await
    }

    /// Sends a game message fire-and-forget (no ACK, may be lost).
    pub async fn send_unreliable<T: mokosh_protocol::GameMessage>(
        &mut self,
        session_id: SessionId,
        message: T,
    ) -> Result<(), ServerError> {
        let ttl = self.default_reliable_ttl();
        self.send_message_with(session_id, message, ReliabilityMode::Unreliable, ttl)
            .await
    }

    /// Broadcasts a message to all connected clients with an explicit mode + TTL.
    pub async fn broadcast_with<T: mokosh_protocol::GameMessage + Clone>(
        &mut self,
        message: T,
        mode: ReliabilityMode,
        ttl: Duration,
    ) -> Result<(), ServerError> {
        let sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, state)| state.state.is_connected() || state.state.is_authorized())
            .map(|(id, _)| *id)
            .collect();
        for session_id in sessions {
            self.send_message_with(session_id, message.clone(), mode, ttl)
                .await?;
        }
        Ok(())
    }

    /// Default TTL for reliable sends, from config (fallback 10s).
    fn default_reliable_ttl(&self) -> Duration {
        self.config
            .reliability
            .as_ref()
            .map(|c| c.default_ttl)
            .unwrap_or_else(|| Duration::from_secs(10))
    }

    /// Sends a control message reliably (tracked on the control channel) when
    /// reliability is enabled; falls back to an unreliable send otherwise.
    ///
    /// Used for the handshake/auth responses (HELLO_OK, AUTH_RESPONSE) so a lost
    /// datagram over UDP is retransmitted instead of stalling the connection.
    async fn send_reliable_control_message<T: serde::Serialize>(
        &mut self,
        session_id: SessionId,
        route_id: u16,
        message: &T,
    ) -> Result<(), ServerError> {
        let now = self.now_mono();

        // Control send window full ⇒ peer too far behind; drop it.
        let window_full = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.reliability.as_ref())
            .map(|r| r.control.sender.is_full())
            .unwrap_or(false);
        if window_full {
            tracing::warn!(session = %session_id, "Control send window full — disconnecting (overloaded)");
            return self.disconnect_session_overloaded(session_id).await;
        }

        let payload_bytes = self
            .control_codec
            .encode(message)
            .map_err(|e| ServerError::InvalidMessage(format!("Failed to serialize: {}", e)))?;

        let session_state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            ServerError::InvalidMessage(format!("Session not found: {}", session_id))
        })?;

        let reliable = session_state.reliability.is_some();
        let (msg_id, flags) = if reliable {
            let id = session_state.control_msg_id_counter;
            session_state.control_msg_id_counter =
                session_state.control_msg_id_counter.wrapping_add(1);
            (id, ReliabilityMode::ReliableOrdered.to_flags())
        } else {
            (0, EnvelopeFlags::RELIABLE)
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

        if reliable {
            if let Some(rel) = session_state.reliability.as_mut() {
                rel.control.sender.on_send(
                    &envelope,
                    ReliabilityMode::ReliableOrdered,
                    self.config
                        .reliability
                        .as_ref()
                        .map(|c| c.default_ttl)
                        .unwrap_or_else(|| Duration::from_secs(10)),
                    now,
                );
            }
        }

        self.outgoing_tx
            .send(SessionEnvelope::new(session_id, envelope))
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
    use mokosh_protocol::{messages::routes, ConnectionState, Envelope, EnvelopeFlags};
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// Helper: Creates a test server with default config
    fn create_test_server() -> (
        Server<NoCompressor, NoEncryptor>,
        mpsc::Sender<SessionEnvelope>,
        mpsc::Receiver<SessionEnvelope>,
    ) {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);
        let server = Server::new(incoming_rx, outgoing_tx);
        (server, incoming_tx, outgoing_rx)
    }

    /// Helper: Creates a reliability-enabled test server.
    fn create_reliable_test_server() -> (
        Server<NoCompressor, NoEncryptor>,
        mpsc::Sender<SessionEnvelope>,
        mpsc::Receiver<SessionEnvelope>,
    ) {
        let (incoming_tx, incoming_rx) = mpsc::channel(50);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(50);
        let config = ServerConfig {
            reliability: Some(ReliabilityConfig::default()),
            retransmit_tick: Duration::from_millis(10),
            ..Default::default()
        };
        let server = Server::with_full_config(
            incoming_rx,
            outgoing_tx,
            CodecType::from_id(1).unwrap(),
            CodecType::from_id(1).unwrap(),
            config,
            None,
            None,
            NoCompressor,
            NoEncryptor,
        );
        (server, incoming_tx, outgoing_rx)
    }

    /// Helper: Creates a reliable (sequenced) HELLO envelope with the given reliability flag.
    fn create_hello_envelope_reliable(session_id: SessionId, reliability: bool) -> SessionEnvelope {
        use mokosh_protocol::messages::Hello;
        use mokosh_protocol::CURRENT_PROTOCOL_VERSION;

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: 1,
            codec_id: 1,
            schema_hash: 0,
            reliability,
        };
        let payload = serde_json::to_vec(&hello).unwrap();
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            routes::HELLO,
            1, // control seq
            ReliabilityMode::ReliableOrdered.to_flags(),
            Bytes::from(payload),
        );
        SessionEnvelope::new(session_id, envelope)
    }

    /// Helper: Creates a HELLO envelope
    fn create_hello_envelope(session_id: SessionId) -> SessionEnvelope {
        use mokosh_protocol::messages::Hello;
        use mokosh_protocol::CURRENT_PROTOCOL_VERSION;

        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: 1,
            codec_id: 1, // JSON
            schema_hash: 0,
            reliability: false,
        };

        let payload = serde_json::to_vec(&hello).unwrap();
        let envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1, // JSON codec
            0,
            routes::HELLO,
            0,
            EnvelopeFlags::RELIABLE,
            Bytes::from(payload),
        );

        SessionEnvelope::new(session_id, envelope)
    }

    #[tokio::test]
    async fn test_server_new() {
        let (_incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, _outgoing_rx) = mpsc::channel(10);

        let server = Server::new(incoming_rx, outgoing_tx);

        // Server should start with no connected clients
        assert_eq!(server.client_count(), 0);
        assert!(server.get_active_sessions().is_empty());
    }

    #[tokio::test]
    async fn test_server_client_count() {
        let (mut server, incoming_tx, _outgoing_rx) = create_test_server();

        assert_eq!(server.client_count(), 0);

        // Simulate a client connecting via HELLO
        let session_id = SessionId::new_v4();
        let hello_envelope = create_hello_envelope(session_id);
        incoming_tx.send(hello_envelope).await.unwrap();

        // Process HELLO message
        let event = server.tick().await.unwrap();

        // Should emit PlayerConnected event
        assert!(matches!(event, Some(GameEvent::PlayerConnected(_))));
        assert_eq!(server.client_count(), 1);
    }

    #[tokio::test]
    async fn test_server_get_active_sessions() {
        let (mut server, incoming_tx, _outgoing_rx) = create_test_server();

        let session1 = SessionId::new_v4();
        let session2 = SessionId::new_v4();

        // Connect two clients
        incoming_tx
            .send(create_hello_envelope(session1))
            .await
            .unwrap();
        incoming_tx
            .send(create_hello_envelope(session2))
            .await
            .unwrap();

        // Process both HELLO messages
        server.tick().await.unwrap();
        server.tick().await.unwrap();

        let active_sessions = server.get_active_sessions();
        assert_eq!(active_sessions.len(), 2);
        assert!(active_sessions.contains(&session1));
        assert!(active_sessions.contains(&session2));
    }

    #[tokio::test]
    async fn test_server_session_state() {
        let (mut server, incoming_tx, _outgoing_rx) = create_test_server();

        let session_id = SessionId::new_v4();

        // Before HELLO, session doesn't exist
        assert!(server.get_session_state(session_id).is_none());

        // Send HELLO
        incoming_tx
            .send(create_hello_envelope(session_id))
            .await
            .unwrap();
        server.tick().await.unwrap();

        // After HELLO, session should be in Connected state
        let state = server.get_session_state(session_id);
        assert!(state.is_some());
        assert_eq!(state.unwrap(), ConnectionState::Connected);
    }

    #[tokio::test]
    async fn test_server_tick_no_events() {
        let (_incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, _outgoing_rx) = mpsc::channel(10);
        let mut server = Server::new(incoming_rx, outgoing_tx);

        // Close incoming channel to simulate shutdown
        drop(_incoming_tx);

        // tick() should return None when all channels are closed
        let result = tokio::time::timeout(Duration::from_millis(100), server.tick()).await;
        assert!(result.is_ok());
        let event = result.unwrap().unwrap();
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn test_server_hello_flow() {
        let (mut server, incoming_tx, mut outgoing_rx) = create_test_server();

        let session_id = SessionId::new_v4();

        // Send HELLO
        incoming_tx
            .send(create_hello_envelope(session_id))
            .await
            .unwrap();

        // Process HELLO - should return PlayerConnected event
        let event = server.tick().await.unwrap();
        assert!(matches!(
            event,
            Some(GameEvent::PlayerConnected(sid)) if sid == session_id
        ));

        // Server should send HELLO_OK back
        let response = outgoing_rx.try_recv();
        assert!(response.is_ok());
        let response_envelope = response.unwrap();
        assert_eq!(response_envelope.session_id, session_id);
        assert_eq!(response_envelope.envelope.route_id, routes::HELLO_OK);
    }

    #[tokio::test]
    async fn test_server_disconnect_session() {
        let (mut server, incoming_tx, mut outgoing_rx) = create_test_server();

        let session_id = SessionId::new_v4();

        // Connect client
        incoming_tx
            .send(create_hello_envelope(session_id))
            .await
            .unwrap();
        server.tick().await.unwrap();

        assert_eq!(server.client_count(), 1);

        // Drain HELLO_OK message from outgoing channel
        let _ = outgoing_rx.try_recv();

        // Disconnect the session
        server
            .disconnect_session(
                session_id,
                DisconnectReason::ServerShutdown,
                "Test disconnect".to_string(),
            )
            .await
            .unwrap();

        // Session should be removed
        assert_eq!(server.client_count(), 0);

        // Server should send DISCONNECT message
        let disconnect_msg = outgoing_rx.try_recv();
        assert!(disconnect_msg.is_ok());
        assert_eq!(disconnect_msg.unwrap().envelope.route_id, routes::DISCONNECT);
    }

    #[tokio::test]
    async fn test_server_game_message_event() {
        let (mut server, incoming_tx, _outgoing_rx) = create_test_server();

        let session_id = SessionId::new_v4();

        // Connect client first
        incoming_tx
            .send(create_hello_envelope(session_id))
            .await
            .unwrap();
        server.tick().await.unwrap();

        // Send a game message (route_id >= 100)
        let game_envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1, // JSON
            0,
            100, // Game message route
            1,   // msg_id
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test game message"),
        );

        incoming_tx
            .send(SessionEnvelope::new(session_id, game_envelope))
            .await
            .unwrap();

        // Process game message
        let event = server.tick().await.unwrap();

        // Should emit GameMessage event
        assert!(matches!(
            event,
            Some(GameEvent::GameMessage { session_id: sid, .. }) if sid == session_id
        ));
    }

    #[tokio::test]
    async fn test_server_hello_timeout() {
        let (_incoming_tx, incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, _outgoing_rx) = mpsc::channel(10);

        // Create config with very short HELLO timeout
        let config = ServerConfig {
            hello_timeout: Duration::from_millis(10),
            ..Default::default()
        };

        let mut server = Server::with_full_config(
            incoming_rx,
            outgoing_tx,
            CodecType::from_id(1).unwrap(),
            CodecType::from_id(1).unwrap(),
            config,
            None,
            None,
            NoCompressor,
            NoEncryptor,
        );

        // Manually add a session in Connecting state
        let session_id = SessionId::new_v4();
        server.sessions.insert(
            session_id,
            SessionState::new(CodecType::from_id(1).unwrap(), 150, None),
        );

        assert_eq!(server.client_count(), 1);

        // Wait for timeout + periodic check
        tokio::time::sleep(Duration::from_millis(1100)).await;

        // Process periodic tasks (should remove timed-out session)
        let _ = server.tick().await;

        // Session should be removed due to HELLO timeout
        assert_eq!(server.client_count(), 0);
    }

    #[tokio::test]
    async fn test_server_get_session_rtt() {
        let (mut server, incoming_tx, _outgoing_rx) = create_test_server();

        let session_id = SessionId::new_v4();

        // Connect client
        incoming_tx
            .send(create_hello_envelope(session_id))
            .await
            .unwrap();
        server.tick().await.unwrap();

        // Initially no RTT measured
        assert!(server.get_session_rtt(session_id).is_none());

        // Simulate PONG response (sets RTT)
        use mokosh_protocol::messages::Pong;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 50; // 50ms ago

        let pong = Pong { timestamp };
        let pong_payload = serde_json::to_vec(&pong).unwrap();
        let pong_envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            routes::PONG,
            0,
            EnvelopeFlags::RELIABLE,
            Bytes::from(pong_payload),
        );

        incoming_tx
            .send(SessionEnvelope::new(session_id, pong_envelope))
            .await
            .unwrap();
        server.tick().await.unwrap();

        // Now RTT should be measured
        let rtt = server.get_session_rtt(session_id);
        assert!(rtt.is_some());
        assert!(rtt.unwrap().as_millis() >= 45); // Approximately 50ms
    }

    // Fix #4: handshake must be rejected when reliability config disagrees.
    #[tokio::test]
    async fn hello_reliability_mismatch_is_rejected() {
        let (mut server, incoming_tx, mut outgoing_rx) = create_reliable_test_server();
        let session_id = SessionId::new_v4();

        // Client claims reliability=false, but the server has it enabled.
        incoming_tx
            .send(create_hello_envelope_reliable(session_id, false))
            .await
            .unwrap();
        server.tick().await.unwrap();

        // Server should reply HELLO_ERROR and drop the session.
        let se = tokio::time::timeout(Duration::from_secs(1), outgoing_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(se.envelope.route_id, routes::HELLO_ERROR);
        let err: HelloError = CodecType::from_id(1)
            .unwrap()
            .decode(&se.envelope.payload)
            .unwrap();
        assert_eq!(err.reason, ErrorReason::ReliabilityMismatch);
        assert_eq!(server.client_count(), 0);
    }

    // Fix #2: best-effort control (PING) must NOT be routed through the reliability
    // receiver, i.e. it must not generate an ACK.
    #[tokio::test]
    async fn ping_does_not_generate_ack() {
        use std::sync::Mutex;

        let (mut server, incoming_tx, mut outgoing_rx) = create_reliable_test_server();
        let session_id = SessionId::new_v4();

        // Complete a matching reliable handshake.
        incoming_tx
            .send(create_hello_envelope_reliable(session_id, true))
            .await
            .unwrap();

        let seen: Arc<Mutex<Vec<u16>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_drain = seen.clone();
        tokio::spawn(async move {
            while let Some(se) = outgoing_rx.recv().await {
                seen_drain.lock().unwrap().push(se.envelope.route_id);
            }
        });
        let server_task = tokio::spawn(async move {
            loop {
                if server.tick().await.is_err() {
                    break;
                }
            }
        });

        // Let the handshake + its (legitimate) HELLO ACK flush, then ignore them.
        tokio::time::sleep(Duration::from_millis(120)).await;
        seen.lock().unwrap().clear();

        // Send a best-effort PING (bare RELIABLE, msg_id 0, not SEQUENCED).
        let ping = Ping { timestamp: 1 };
        let ping_env = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            routes::PING,
            0,
            EnvelopeFlags::RELIABLE,
            Bytes::from(serde_json::to_vec(&ping).unwrap()),
        );
        incoming_tx
            .send(SessionEnvelope::new(session_id, ping_env))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        let routes_seen = seen.lock().unwrap().clone();
        server_task.abort();

        assert!(
            routes_seen.contains(&routes::PONG),
            "server should answer PING with PONG, saw {:?}",
            routes_seen
        );
        assert!(
            !routes_seen.contains(&routes::ACK),
            "best-effort PING must NOT generate an ACK, saw {:?}",
            routes_seen
        );
    }

    // Window cap: a peer that never ACKs and keeps receiving sends is dropped
    // once the outstanding window fills (bounded sender memory).
    #[tokio::test]
    async fn send_window_overflow_disconnects_session() {
        let (incoming_tx, incoming_rx) = mpsc::channel(50);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(50);
        let config = ServerConfig {
            reliability: Some(ReliabilityConfig {
                send_window: 3,
                ..ReliabilityConfig::default()
            }),
            retransmit_tick: Duration::from_millis(10),
            ..Default::default()
        };
        let mut server = Server::with_full_config(
            incoming_rx,
            outgoing_tx,
            CodecType::from_id(1).unwrap(),
            CodecType::from_id(1).unwrap(),
            config,
            None,
            None,
            NoCompressor,
            NoEncryptor,
        );

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Msg {
            n: u32,
        }
        impl mokosh_protocol::GameMessage for Msg {
            const ROUTE_ID: u16 = 300;
            const SCHEMA_HASH: u64 = 0;
        }

        let session_id = SessionId::new_v4();
        incoming_tx
            .send(create_hello_envelope_reliable(session_id, true))
            .await
            .unwrap();
        let ev = server.tick().await.unwrap();
        assert!(matches!(ev, Some(GameEvent::PlayerConnected(_))));
        assert_eq!(server.client_count(), 1);

        // No ACKs ever arrive. Sends 1..=3 fill the window; the 4th overflows.
        for n in 1..=3 {
            server
                .send_message_with(session_id, Msg { n }, ReliabilityMode::ReliableOrdered, Duration::from_secs(30))
                .await
                .unwrap();
        }
        assert_eq!(server.client_count(), 1, "window not yet full");

        server
            .send_message_with(session_id, Msg { n: 4 }, ReliabilityMode::ReliableOrdered, Duration::from_secs(30))
            .await
            .unwrap();

        // Session torn down (Overloaded), PlayerDisconnected emitted.
        assert_eq!(server.client_count(), 0, "overflow should drop the session");
        // Drain outgoing for a DISCONNECT envelope.
        let mut saw_disconnect = false;
        while let Ok(se) = outgoing_rx.try_recv() {
            if se.envelope.route_id == routes::DISCONNECT {
                saw_disconnect = true;
            }
        }
        assert!(saw_disconnect, "server should notify the peer with DISCONNECT");
    }
}
