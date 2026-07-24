//! UDP server transport for Mokosh
//!
//! An alternative to the WebSocket transport for latency-sensitive games.
//!
//! UDP is connectionless, so this transport synthesizes "sessions" from the
//! peer's [`SocketAddr`]: the first valid HELLO seen from a new address is
//! treated as a new connection and assigned a fresh [`SessionId`]. Subsequent
//! datagrams from the same address reuse that session id. Outgoing envelopes
//! are routed back to the peer via a reverse `SessionId -> SocketAddr` map.
//!
//! # Caveats
//! - **No delivery guarantees.** UDP is unreliable and unordered. Reliability,
//!   ordering and fragmentation are the responsibility of higher layers (the
//!   protocol's replay window / msg ids, or the game itself).
//! - **Datagram size.** Each envelope must fit in a single datagram. Keep
//!   payloads below the path MTU (~1200 bytes is a safe practical limit) to
//!   avoid IP fragmentation; the receive buffer caps an inbound datagram at
//!   64 KiB.
//! - **Session cleanup.** Since there is no connection-close event, the
//!   address/session mapping is removed when a DISCONNECT envelope flows in
//!   either direction. The server's keepalive/timeout logic emits an outbound
//!   DISCONNECT so dead peers are also reaped from these transport maps.

use bytes::Bytes;
use mokosh_protocol::messages::routes;
use mokosh_protocol::{Envelope, SessionEnvelope, SessionId};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Maximum size of a single inbound UDP datagram (64 KiB).
const MAX_DATAGRAM_SIZE: usize = 65_535;

/// How often the bounded source table is scanned for idle buckets.
const SOURCE_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// Per-source admission limit for new UDP sessions.
///
/// The limiter is enabled by default by [`UdpServer::new`]. Each accepted HELLO
/// from an address without an existing session consumes one token. Existing
/// sessions do not consume tokens.
#[derive(Debug, Clone)]
pub struct UdpSessionRateLimitConfig {
    /// Steady-state number of new sessions allowed per second per source.
    pub max_new_sessions_per_second: u32,

    /// Maximum accumulated tokens and initial allowance for a new source.
    pub burst: u32,

    /// Maximum number of source buckets retained at once.
    pub max_tracked_sources: usize,

    /// Remove a source bucket after this long without a new-session attempt.
    pub source_idle_timeout: Duration,
}

impl Default for UdpSessionRateLimitConfig {
    fn default() -> Self {
        Self {
            max_new_sessions_per_second: 10,
            burst: 20,
            max_tracked_sources: 65_536,
            source_idle_timeout: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    last_seen: Instant,
}

impl TokenBucket {
    fn new(burst: u32, now: Instant) -> Self {
        Self {
            tokens: burst as f64,
            last_refill: now,
            last_seen: now,
        }
    }

    fn try_take(&mut self, config: &UdpSessionRateLimitConfig, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.tokens = (self.tokens
            + elapsed.as_secs_f64() * config.max_new_sessions_per_second as f64)
            .min(config.burst as f64);
        self.last_refill = now;
        self.last_seen = now;

        if self.tokens < 1.0 {
            return false;
        }

        self.tokens -= 1.0;
        true
    }
}

#[derive(Debug)]
struct SessionAdmissionLimiter {
    config: UdpSessionRateLimitConfig,
    buckets: HashMap<IpAddr, TokenBucket>,
    next_cleanup: Instant,
}

impl SessionAdmissionLimiter {
    fn new(config: UdpSessionRateLimitConfig, now: Instant) -> Self {
        Self {
            config,
            buckets: HashMap::new(),
            next_cleanup: now + SOURCE_CLEANUP_INTERVAL,
        }
    }

    fn try_acquire(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.cleanup_if_due(now);
        let source = source_network(ip);

        if let Some(bucket) = self.buckets.get_mut(&source) {
            return bucket.try_take(&self.config, now);
        }

        if self.buckets.len() >= self.config.max_tracked_sources {
            return false;
        }

        let mut bucket = TokenBucket::new(self.config.burst, now);
        let accepted = bucket.try_take(&self.config, now);
        self.buckets.insert(source, bucket);
        accepted
    }

    fn cleanup_if_due(&mut self, now: Instant) {
        if now < self.next_cleanup {
            return;
        }

        let idle_timeout = self.config.source_idle_timeout;
        self.buckets
            .retain(|_, bucket| now.saturating_duration_since(bucket.last_seen) < idle_timeout);
        self.next_cleanup = now + SOURCE_CLEANUP_INTERVAL;
    }
}

fn source_network(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(ipv4) => IpAddr::V4(ipv4),
        IpAddr::V6(ipv6) => {
            if let Some(ipv4) = ipv6.to_ipv4_mapped() {
                return IpAddr::V4(ipv4);
            }

            let mut octets = ipv6.octets();
            octets[8..].fill(0);
            IpAddr::V6(octets.into())
        }
    }
}

/// UDP server that accepts datagrams and bridges them to envelope channels.
///
/// Mirrors the API of [`WebSocketServer`](super::websocket::WebSocketServer):
/// construct with a bind address and drive it with [`UdpServer::run`].
pub struct UdpServer {
    addr: SocketAddr,
    session_rate_limit: UdpSessionRateLimitConfig,
}

impl UdpServer {
    /// Creates a new UDP server bound to the given address with the default
    /// per-source new-session rate limit.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            session_rate_limit: UdpSessionRateLimitConfig::default(),
        }
    }

    /// Overrides the per-source admission limit for new UDP sessions.
    pub fn with_session_rate_limit(mut self, config: UdpSessionRateLimitConfig) -> Self {
        self.session_rate_limit = config;
        self
    }

    /// Runs the UDP server with multi-client session routing.
    ///
    /// Each distinct peer address is mapped to a unique [`SessionId`]. Inbound
    /// datagrams are decoded into [`Envelope`]s, tagged with their session id
    /// and forwarded on `incoming_tx`. Outbound [`SessionEnvelope`]s received on
    /// `outgoing_rx` are encoded and sent to the peer that owns the session.
    ///
    /// # Arguments
    /// * `incoming_tx` - Channel to send received session envelopes to the event loop
    /// * `outgoing_rx` - Channel to receive session envelopes from the event loop
    /// * `ready_tx` - Optional one-shot fired once the socket is bound and ready
    pub async fn run(
        self,
        incoming_tx: mpsc::Sender<SessionEnvelope>,
        mut outgoing_rx: mpsc::Receiver<SessionEnvelope>,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), UdpServerError> {
        let socket = UdpSocket::bind(self.addr)
            .await
            .map_err(|e| UdpServerError::BindError(e.to_string()))?;
        let socket = Arc::new(socket);

        tracing::info!(addr = %self.addr, "UDP server listening");

        // Signal that the server is ready to receive datagrams.
        if let Some(tx) = ready_tx {
            let _ = tx.send(());
        }

        // Bidirectional mapping between peer addresses and synthesized sessions.
        let mut addr_to_session: HashMap<SocketAddr, SessionId> = HashMap::new();
        let mut session_to_addr: HashMap<SessionId, SocketAddr> = HashMap::new();
        let mut admission = SessionAdmissionLimiter::new(self.session_rate_limit, Instant::now());

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];

        loop {
            tokio::select! {
                // Inbound datagram from a peer.
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((len, peer_addr)) => {
                            let envelope = match Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])) {
                                Ok(envelope) => envelope,
                                Err(e) => {
                                    tracing::error!(peer = %peer_addr, error = %e, "Failed to parse envelope");
                                    continue;
                                }
                            };

                            // Existing peers bypass admission limiting. Unknown
                            // peers may create state only with an allowed HELLO.
                            let session_id = if let Some(&session_id) = addr_to_session.get(&peer_addr) {
                                session_id
                            } else {
                                if envelope.route_id != routes::HELLO {
                                    tracing::debug!(
                                        peer = %peer_addr,
                                        route_id = envelope.route_id,
                                        "Dropping non-HELLO from unknown UDP peer"
                                    );
                                    continue;
                                }

                                if !admission.try_acquire(peer_addr.ip(), Instant::now()) {
                                    tracing::debug!(
                                        peer = %peer_addr,
                                        "Dropping UDP HELLO: source admission limit exceeded"
                                    );
                                    continue;
                                }

                                let session_id = SessionId::new_v4();
                                addr_to_session.insert(peer_addr, session_id);
                                session_to_addr.insert(session_id, peer_addr);
                                tracing::info!(
                                    peer = %peer_addr,
                                    session = %session_id,
                                    "New UDP connection"
                                );
                                session_id
                            };

                            let is_disconnect = envelope.route_id == routes::DISCONNECT;

                            if let Err(e) = incoming_tx.send(SessionEnvelope::new(session_id, envelope)).await {
                                tracing::error!(error = %e, "Failed to send envelope to event loop");
                                break;
                            }

                            // Peer asked to disconnect: forget the mapping.
                            if is_disconnect {
                                addr_to_session.remove(&peer_addr);
                                session_to_addr.remove(&session_id);
                                tracing::debug!(session = %session_id, "Removed session on client DISCONNECT");
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to receive datagram");
                        }
                    }
                }

                // Outbound envelope from the event loop.
                Some(session_envelope) = outgoing_rx.recv() => {
                    let session_id = session_envelope.session_id;
                    let envelope = session_envelope.envelope;

                    let Some(&peer_addr) = session_to_addr.get(&session_id) else {
                        tracing::warn!(
                            session = %session_id,
                            route_id = envelope.route_id,
                            "Cannot route envelope: peer address unknown"
                        );
                        continue;
                    };

                    let is_disconnect = envelope.route_id == routes::DISCONNECT;
                    let bytes = envelope.to_bytes();
                    if let Err(e) = socket.send_to(&bytes, peer_addr).await {
                        tracing::error!(peer = %peer_addr, error = %e, "Failed to send to UDP peer");
                    }

                    // Server closed the session: forget the mapping.
                    if is_disconnect {
                        if let Some(addr) = session_to_addr.remove(&session_id) {
                            addr_to_session.remove(&addr);
                        }
                        tracing::debug!(session = %session_id, "Removed session on server DISCONNECT");
                    }
                }

                else => {
                    tracing::debug!("UDP server shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}

/// UDP server errors.
#[derive(Debug, thiserror::Error)]
pub enum UdpServerError {
    #[error("Failed to bind to address: {0}")]
    BindError(String),

    #[error("Socket error: {0}")]
    SocketError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mokosh_protocol::EnvelopeFlags;
    use tokio::time::timeout;

    fn test_envelope(route_id: u16, msg_id: u64, payload: &'static [u8]) -> Envelope {
        Envelope::new_simple(
            1,
            1,
            0,
            route_id,
            msg_id,
            EnvelopeFlags::empty(),
            Bytes::from_static(payload),
        )
    }

    fn hello_envelope() -> Envelope {
        test_envelope(routes::HELLO, 1, b"{}")
    }

    async fn spawn_server() -> (
        SocketAddr,
        mpsc::Receiver<SessionEnvelope>,
        mpsc::Sender<SessionEnvelope>,
    ) {
        spawn_server_with_config(UdpSessionRateLimitConfig::default()).await
    }

    async fn spawn_server_with_config(
        config: UdpSessionRateLimitConfig,
    ) -> (
        SocketAddr,
        mpsc::Receiver<SessionEnvelope>,
        mpsc::Sender<SessionEnvelope>,
    ) {
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Bind first to learn the assigned port, then hand the socket's address
        // to the server (it rebinds the same ephemeral-resolved address).
        let probe = UdpSocket::bind(bind).await.unwrap();
        let bound_addr = probe.local_addr().unwrap();
        drop(probe);

        let server = UdpServer::new(bound_addr).with_session_rate_limit(config);
        tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, Some(ready_tx)).await;
        });
        ready_rx.await.unwrap();

        (bound_addr, incoming_rx, outgoing_tx)
    }

    async fn connect_client(server_addr: SocketAddr) -> UdpSocket {
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();
        client
    }

    async fn establish(
        client: &UdpSocket,
        incoming_rx: &mut mpsc::Receiver<SessionEnvelope>,
    ) -> SessionEnvelope {
        client.send(&hello_envelope().to_bytes()).await.unwrap();
        timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn test_server_receives_envelope() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;
        let client = connect_client(server_addr).await;
        let session_envelope = establish(&client, &mut incoming_rx).await;

        assert!(!session_envelope.session_id.is_nil());
        assert_eq!(session_envelope.envelope.route_id, routes::HELLO);
    }

    #[tokio::test]
    async fn test_same_peer_reuses_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client = connect_client(server_addr).await;
        let hello = establish(&client, &mut incoming_rx).await;

        client
            .send(&test_envelope(100, 1, b"a").to_bytes())
            .await
            .unwrap();
        client
            .send(&test_envelope(100, 2, b"b").to_bytes())
            .await
            .unwrap();

        let first = incoming_rx.recv().await.unwrap();
        let second = incoming_rx.recv().await.unwrap();
        assert_eq!(hello.session_id, first.session_id);
        assert_eq!(first.session_id, second.session_id);
    }

    #[tokio::test]
    async fn test_distinct_peers_get_distinct_sessions() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client1 = connect_client(server_addr).await;
        let client2 = connect_client(server_addr).await;
        let a = establish(&client1, &mut incoming_rx).await;
        let b = establish(&client2, &mut incoming_rx).await;
        assert_ne!(a.session_id, b.session_id);
    }

    #[tokio::test]
    async fn test_outgoing_routed_to_peer() {
        let (server_addr, mut incoming_rx, outgoing_tx) = spawn_server().await;

        let client = connect_client(server_addr).await;
        let session_id = establish(&client, &mut incoming_rx).await.session_id;

        let response = test_envelope(200, 2, b"pong");
        outgoing_tx
            .send(SessionEnvelope::new(session_id, response))
            .await
            .unwrap();

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let len = timeout(Duration::from_secs(1), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let received = Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])).unwrap();
        assert_eq!(received.route_id, 200);
        assert_eq!(received.payload, Bytes::from_static(b"pong"));
    }

    #[tokio::test]
    async fn test_invalid_datagram_is_ignored() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 0,
            burst: 1,
            ..Default::default()
        };
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server_with_config(config).await;

        let client = connect_client(server_addr).await;

        // Too short to be an envelope header.
        client.send(&[1, 2, 3]).await.unwrap();
        // Followed by a valid HELLO.
        client.send(&hello_envelope().to_bytes()).await.unwrap();

        let session_envelope = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session_envelope.envelope.route_id, routes::HELLO);
    }

    #[test]
    fn session_rate_limit_defaults_are_bounded() {
        let config = UdpSessionRateLimitConfig::default();
        assert_eq!(config.max_new_sessions_per_second, 10);
        assert_eq!(config.burst, 20);
        assert_eq!(config.max_tracked_sources, 65_536);
        assert_eq!(config.source_idle_timeout, Duration::from_secs(600));
    }

    #[test]
    fn token_bucket_allows_burst_refills_and_caps_tokens() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 2,
            burst: 2,
            ..Default::default()
        };
        let start = Instant::now();
        let mut bucket = TokenBucket::new(config.burst, start);

        assert!(bucket.try_take(&config, start));
        assert!(bucket.try_take(&config, start));
        assert!(!bucket.try_take(&config, start));
        assert!(bucket.try_take(&config, start + Duration::from_millis(500)));
        assert!(!bucket.try_take(&config, start + Duration::from_millis(500)));
        assert!(bucket.try_take(&config, start + Duration::from_secs(10)));
        assert!(bucket.try_take(&config, start + Duration::from_secs(10)));
        assert!(!bucket.try_take(&config, start + Duration::from_secs(10)));
    }

    #[test]
    fn source_network_groups_ipv6_prefix_and_unwraps_mapped_ipv4() {
        let ipv4: IpAddr = "192.0.2.7".parse().unwrap();
        assert_eq!(source_network(ipv4), ipv4);

        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(source_network(mapped), ipv4);

        let first: IpAddr = "2001:db8:1234:5678::1".parse().unwrap();
        let same_prefix: IpAddr = "2001:db8:1234:5678:ffff::2".parse().unwrap();
        let other_prefix: IpAddr = "2001:db8:1234:5679::1".parse().unwrap();
        assert_eq!(source_network(first), source_network(same_prefix));
        assert_ne!(source_network(first), source_network(other_prefix));
    }

    #[test]
    fn source_table_is_bounded_and_reclaims_idle_bucket() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 0,
            burst: 1,
            max_tracked_sources: 1,
            source_idle_timeout: Duration::from_secs(10),
        };
        let start = Instant::now();
        let mut limiter = SessionAdmissionLimiter::new(config, start);
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "198.51.100.1".parse().unwrap();

        assert!(limiter.try_acquire(first, start));
        assert!(!limiter.try_acquire(second, start));
        assert!(!limiter.try_acquire(second, start + Duration::from_secs(30)));
        assert!(limiter.try_acquire(second, start + Duration::from_secs(61)));
        assert_eq!(limiter.buckets.len(), 1);
    }

    #[tokio::test]
    async fn new_sessions_from_same_ip_share_bucket() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 0,
            burst: 1,
            ..Default::default()
        };
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server_with_config(config).await;
        let first = connect_client(server_addr).await;
        let second = connect_client(server_addr).await;

        let established = establish(&first, &mut incoming_rx).await;
        second.send(&hello_envelope().to_bytes()).await.unwrap();
        assert!(timeout(Duration::from_millis(100), incoming_rx.recv())
            .await
            .is_err());

        first
            .send(&test_envelope(100, 2, b"existing").to_bytes())
            .await
            .unwrap();
        let delivered = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.session_id, established.session_id);
        assert_eq!(delivered.envelope.payload, Bytes::from_static(b"existing"));
    }

    #[tokio::test]
    async fn unknown_non_hello_does_not_consume_admission_token() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 0,
            burst: 1,
            ..Default::default()
        };
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server_with_config(config).await;
        let client = connect_client(server_addr).await;

        client
            .send(&test_envelope(100, 1, b"early").to_bytes())
            .await
            .unwrap();
        assert!(timeout(Duration::from_millis(100), incoming_rx.recv())
            .await
            .is_err());

        let established = establish(&client, &mut incoming_rx).await;
        assert_eq!(established.envelope.route_id, routes::HELLO);
    }

    #[tokio::test]
    async fn source_can_create_session_after_token_refill() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 10,
            burst: 1,
            ..Default::default()
        };
        let (server_addr, mut incoming_rx, outgoing_tx) = spawn_server_with_config(config).await;
        let first = connect_client(server_addr).await;
        let first_session = establish(&first, &mut incoming_rx).await.session_id;

        outgoing_tx
            .send(SessionEnvelope::new(
                first_session,
                test_envelope(routes::DISCONNECT, 0, b"{}"),
            ))
            .await
            .unwrap();
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        timeout(Duration::from_secs(1), first.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();

        let second = connect_client(server_addr).await;
        tokio::time::sleep(Duration::from_millis(120)).await;
        second.send(&hello_envelope().to_bytes()).await.unwrap();
        let reconnected = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(reconnected.session_id, first_session);
    }
}
