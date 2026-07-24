//! UDP server transport for Mokosh
//!
//! An alternative to the WebSocket transport for latency-sensitive games.
//!
//! UDP is connectionless, so this transport synthesizes "sessions" from the
//! peer's [`SocketAddr`]. Before allocating a [`SessionId`], an unknown peer
//! must echo a short-lived stateless cookie bound to its IP, port, and original
//! HELLO. Subsequent datagrams from the validated address reuse that session
//! id. Outgoing envelopes are routed back to the peer via a reverse
//! `SessionId -> SocketAddr` map.
//!
//! # Caveats
//! - **No delivery guarantees.** UDP is unreliable and unordered. Reliability,
//!   ordering and fragmentation are the responsibility of higher layers (the
//!   protocol's replay window / msg ids, or the game itself).
//! - **Datagram size.** Each envelope must fit in a single datagram. Keep
//!   payloads below the path MTU (~1200 bytes is a safe practical limit) to
//!   avoid IP fragmentation; the receive buffer caps an inbound datagram at
//!   64 KiB.
//! - **Address validation.** The cookie exchange is mandatory and adds one RTT
//!   to a new UDP connection. Custom clients must implement the validation
//!   routes; Mokosh's `UdpClient` handles them automatically.
//! - **Session cleanup.** Since there is no connection-close event, the
//!   address/session mapping is removed when a DISCONNECT envelope flows in
//!   either direction. The server's keepalive/timeout logic emits an outbound
//!   DISCONNECT so dead peers are also reaped from these transport maps.

use bytes::Bytes;
use mokosh_protocol::messages::routes;
use mokosh_protocol::{
    Envelope, EnvelopeFlags, SessionEnvelope, SessionId, UdpAddressChallenge, UdpAddressResponse,
    UDP_ADDRESS_COOKIE_SIZE,
};
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

/// Maximum response/input byte ratio before a UDP address is validated.
const MAX_UNVALIDATED_AMPLIFICATION: usize = 3;

/// Current format of the opaque address-validation cookie.
const ADDRESS_COOKIE_VERSION: u8 = 1;

/// Domain separation for the keyed cookie authenticator.
const ADDRESS_COOKIE_DOMAIN: &[u8] = b"mokosh-udp-address-cookie-v1";

/// Configuration for the mandatory UDP return-path validation exchange.
#[derive(Debug, Clone)]
pub struct UdpAddressValidationConfig {
    pub cookie_lifetime: Duration,
}

impl Default for UdpAddressValidationConfig {
    fn default() -> Self {
        Self {
            cookie_lifetime: Duration::from_secs(10),
        }
    }
}

/// Per-source admission limit for new UDP sessions.
///
/// The limiter is enabled by default by [`UdpServer::new`]. Each valid
/// address-response from an endpoint without an existing session consumes one
/// token. Unvalidated HELLOs and existing sessions do not consume tokens.
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

struct AddressCookieSigner {
    secret: [u8; 32],
    lifetime: Duration,
    started_at: Instant,
}

impl AddressCookieSigner {
    fn new(secret: [u8; 32], lifetime: Duration, started_at: Instant) -> Self {
        Self {
            secret,
            lifetime,
            started_at,
        }
    }

    fn issue(
        &self,
        peer_addr: SocketAddr,
        hello: &Envelope,
        now: Instant,
    ) -> [u8; UDP_ADDRESS_COOKIE_SIZE] {
        let issued_at_ms = self.elapsed_millis(now);
        let tag = self.tag(peer_addr, hello, issued_at_ms);
        let mut cookie = [0u8; UDP_ADDRESS_COOKIE_SIZE];
        cookie[0] = ADDRESS_COOKIE_VERSION;
        cookie[1..9].copy_from_slice(&issued_at_ms.to_be_bytes());
        cookie[9..].copy_from_slice(tag.as_bytes());
        cookie
    }

    fn validate(
        &self,
        cookie: &[u8; UDP_ADDRESS_COOKIE_SIZE],
        peer_addr: SocketAddr,
        hello: &Envelope,
        now: Instant,
    ) -> bool {
        if cookie[0] != ADDRESS_COOKIE_VERSION {
            return false;
        }

        let issued_at_ms = u64::from_be_bytes(cookie[1..9].try_into().expect("fixed cookie"));
        let now_ms = self.elapsed_millis(now);
        if issued_at_ms > now_ms
            || now_ms.saturating_sub(issued_at_ms) > duration_millis(self.lifetime)
        {
            return false;
        }

        self.tag(peer_addr, hello, issued_at_ms) == cookie[9..]
    }

    fn elapsed_millis(&self, now: Instant) -> u64 {
        duration_millis(now.saturating_duration_since(self.started_at))
    }

    fn tag(&self, peer_addr: SocketAddr, hello: &Envelope, issued_at_ms: u64) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.secret);
        hasher.update(ADDRESS_COOKIE_DOMAIN);
        match normalized_endpoint(peer_addr) {
            SocketAddr::V4(addr) => {
                hasher.update(&[4]);
                hasher.update(&addr.ip().octets());
                hasher.update(&addr.port().to_be_bytes());
            }
            SocketAddr::V6(addr) => {
                hasher.update(&[6]);
                hasher.update(&addr.ip().octets());
                hasher.update(&addr.port().to_be_bytes());
            }
        }
        hasher.update(&issued_at_ms.to_be_bytes());
        hasher.update(&hello.to_bytes());
        hasher.finalize()
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn normalized_endpoint(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(addr) => addr
            .ip()
            .to_ipv4_mapped()
            .map(|ip| SocketAddr::new(IpAddr::V4(ip), addr.port()))
            .unwrap_or(SocketAddr::V6(addr)),
        other => other,
    }
}

/// UDP server that accepts datagrams and bridges them to envelope channels.
///
/// Mirrors the API of [`WebSocketServer`](super::websocket::WebSocketServer):
/// construct with a bind address and drive it with [`UdpServer::run`].
pub struct UdpServer {
    addr: SocketAddr,
    session_rate_limit: UdpSessionRateLimitConfig,
    address_validation: UdpAddressValidationConfig,
}

impl UdpServer {
    /// Creates a new UDP server bound to the given address with the default
    /// per-source new-session rate limit.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            session_rate_limit: UdpSessionRateLimitConfig::default(),
            address_validation: UdpAddressValidationConfig::default(),
        }
    }

    /// Overrides the per-source admission limit for new UDP sessions.
    pub fn with_session_rate_limit(mut self, config: UdpSessionRateLimitConfig) -> Self {
        self.session_rate_limit = config;
        self
    }

    /// Overrides the mandatory UDP address-validation cookie lifetime.
    pub fn with_address_validation(mut self, config: UdpAddressValidationConfig) -> Self {
        self.address_validation = config;
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
        let mut cookie_secret = [0u8; 32];
        getrandom::getrandom(&mut cookie_secret)
            .map_err(|e| UdpServerError::RandomnessError(e.to_string()))?;
        let cookie_signer = AddressCookieSigner::new(
            cookie_secret,
            self.address_validation.cookie_lifetime,
            Instant::now(),
        );

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

                            // Transport-only validation messages never reach the
                            // protocol/reliability layers for an existing peer.
                            if addr_to_session.contains_key(&peer_addr)
                                && matches!(
                                    envelope.route_id,
                                    routes::UDP_ADDRESS_CHALLENGE | routes::UDP_ADDRESS_RESPONSE
                                )
                            {
                                continue;
                            }

                            // Existing peers bypass admission limiting. Unknown
                            // peers must echo a stateless cookie before any
                            // SessionId, routing map, limiter bucket, or
                            // reliability state is allocated.
                            let (session_id, envelope) = if let Some(&session_id) = addr_to_session.get(&peer_addr) {
                                (session_id, envelope)
                            } else {
                                if envelope.route_id == routes::HELLO {
                                    let cookie =
                                        cookie_signer.issue(peer_addr, &envelope, Instant::now());
                                    let challenge = UdpAddressChallenge { cookie };
                                    let challenge_envelope = Envelope::new_simple(
                                        envelope.protocol_version,
                                        3,
                                        0,
                                        routes::UDP_ADDRESS_CHALLENGE,
                                        0,
                                        EnvelopeFlags::empty(),
                                        challenge.to_bytes(),
                                    );
                                    let challenge_bytes = challenge_envelope.to_bytes();
                                    if challenge_bytes.len()
                                        <= len.saturating_mul(MAX_UNVALIDATED_AMPLIFICATION)
                                    {
                                        if let Err(e) = socket.send_to(&challenge_bytes, peer_addr).await {
                                            tracing::error!(
                                                peer = %peer_addr,
                                                error = %e,
                                                "Failed to send UDP address challenge"
                                            );
                                        }
                                    } else {
                                        tracing::warn!(
                                            peer = %peer_addr,
                                            received_bytes = len,
                                            challenge_bytes = challenge_bytes.len(),
                                            "Dropping UDP address challenge: amplification budget exceeded"
                                        );
                                    }
                                    continue;
                                }

                                if envelope.route_id != routes::UDP_ADDRESS_RESPONSE {
                                    tracing::debug!(
                                        peer = %peer_addr,
                                        route_id = envelope.route_id,
                                        "Dropping non-validation message from unknown UDP peer"
                                    );
                                    continue;
                                }

                                let response = match UdpAddressResponse::from_bytes(&envelope.payload) {
                                    Ok(response) if response.hello.route_id == routes::HELLO => response,
                                    Ok(_) => {
                                        tracing::debug!(
                                            peer = %peer_addr,
                                            "Dropping UDP address response without HELLO"
                                        );
                                        continue;
                                    }
                                    Err(e) => {
                                        tracing::debug!(
                                            peer = %peer_addr,
                                            error = %e,
                                            "Dropping malformed UDP address response"
                                        );
                                        continue;
                                    }
                                };

                                if !cookie_signer.validate(
                                    &response.cookie,
                                    peer_addr,
                                    &response.hello,
                                    Instant::now(),
                                ) {
                                    tracing::debug!(
                                        peer = %peer_addr,
                                        "Dropping invalid UDP address cookie"
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
                                (session_id, response.hello)
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

    #[error("Failed to generate UDP address-validation secret: {0}")]
    RandomnessError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mokosh_protocol::{EnvelopeFlags, UdpAddressResponse};
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
        let hello = hello_envelope();
        let challenge = request_challenge(client, &hello).await;
        send_address_response(client, challenge.cookie, hello).await;
        timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn request_challenge(client: &UdpSocket, hello: &Envelope) -> UdpAddressChallenge {
        client.send(&hello.to_bytes()).await.unwrap();
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let len = timeout(Duration::from_secs(1), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let envelope = Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])).unwrap();
        assert_eq!(envelope.route_id, routes::UDP_ADDRESS_CHALLENGE);
        UdpAddressChallenge::from_bytes(&envelope.payload).unwrap()
    }

    async fn send_address_response(
        client: &UdpSocket,
        cookie: [u8; UDP_ADDRESS_COOKIE_SIZE],
        hello: Envelope,
    ) {
        let protocol_version = hello.protocol_version;
        let response = UdpAddressResponse { cookie, hello };
        let envelope = Envelope::new_simple(
            protocol_version,
            3,
            0,
            routes::UDP_ADDRESS_RESPONSE,
            0,
            EnvelopeFlags::empty(),
            response.to_bytes(),
        );
        client.send(&envelope.to_bytes()).await.unwrap();
    }

    #[test]
    fn address_cookie_is_bound_to_endpoint_hello_and_lifetime() {
        let start = Instant::now();
        let signer = AddressCookieSigner::new([7; 32], Duration::from_secs(10), start);
        let addr: SocketAddr = "192.0.2.10:1234".parse().unwrap();
        let other_port: SocketAddr = "192.0.2.10:1235".parse().unwrap();
        let other_ip: SocketAddr = "192.0.2.11:1234".parse().unwrap();
        let hello = hello_envelope();
        let changed_hello = test_envelope(routes::HELLO, 2, b"{}");
        let cookie = signer.issue(addr, &hello, start + Duration::from_secs(1));

        assert!(signer.validate(&cookie, addr, &hello, start + Duration::from_secs(5)));
        assert!(!signer.validate(&cookie, other_port, &hello, start + Duration::from_secs(5)));
        assert!(!signer.validate(&cookie, other_ip, &hello, start + Duration::from_secs(5)));
        assert!(!signer.validate(
            &cookie,
            addr,
            &changed_hello,
            start + Duration::from_secs(5)
        ));
        assert!(!signer.validate(&cookie, addr, &hello, start + Duration::from_secs(12)));
        assert!(!signer.validate(&cookie, addr, &hello, start));

        let mut corrupted = cookie;
        corrupted[UDP_ADDRESS_COOKIE_SIZE - 1] ^= 1;
        assert!(!signer.validate(&corrupted, addr, &hello, start + Duration::from_secs(5)));
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
    async fn first_hello_only_receives_bounded_challenge() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;
        let client = connect_client(server_addr).await;
        let hello = hello_envelope();
        let hello_bytes = hello.to_bytes();

        client.send(&hello_bytes).await.unwrap();
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let len = timeout(Duration::from_secs(1), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let challenge = Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])).unwrap();

        assert_eq!(challenge.route_id, routes::UDP_ADDRESS_CHALLENGE);
        assert!(len <= hello_bytes.len() * MAX_UNVALIDATED_AMPLIFICATION);
        assert!(timeout(Duration::from_millis(100), incoming_rx.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cookie_captured_by_another_port_cannot_create_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;
        let first = connect_client(server_addr).await;
        let second = connect_client(server_addr).await;
        let hello = hello_envelope();
        let challenge = request_challenge(&first, &hello).await;

        send_address_response(&second, challenge.cookie, hello).await;

        assert!(timeout(Duration::from_millis(100), incoming_rx.recv())
            .await
            .is_err());
        let mut buf = [0u8; MAX_DATAGRAM_SIZE];
        assert!(timeout(Duration::from_millis(100), second.recv(&mut buf))
            .await
            .is_err());
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
        let session_envelope = establish(&client, &mut incoming_rx).await;
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
        let second_hello = hello_envelope();
        let challenge = request_challenge(&second, &second_hello).await;
        send_address_response(&second, challenge.cookie, second_hello).await;
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
    async fn unvalidated_challenges_do_not_consume_admission_tokens() {
        let config = UdpSessionRateLimitConfig {
            max_new_sessions_per_second: 0,
            burst: 1,
            ..Default::default()
        };
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server_with_config(config).await;

        for _ in 0..8 {
            let client = connect_client(server_addr).await;
            let _ = request_challenge(&client, &hello_envelope()).await;
        }

        let legitimate = connect_client(server_addr).await;
        let established = establish(&legitimate, &mut incoming_rx).await;
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
        let reconnected = establish(&second, &mut incoming_rx).await;
        assert_ne!(reconnected.session_id, first_session);
    }
}
