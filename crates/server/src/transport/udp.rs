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
    BootstrapRecords, Envelope, EnvelopeFlags, RecordKey, Role, SessionEnvelope, SessionId,
    SessionRecords, UdpAddressChallenge, UdpAddressResponse, EPOCH_BOOTSTRAP, EPOCH_SESSION,
    SESSION_RANDOM_SIZE, UDP_ADDRESS_COOKIE_SIZE,
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

/// Domain separation for the stateless server key-agreement random.
const SERVER_RANDOM_DOMAIN: &[u8] = b"mokosh-udp-server-random-v1";

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
        cookie[COOKIE_ISSUED_AT].copy_from_slice(&issued_at_ms.to_be_bytes());
        cookie[COOKIE_TAG].copy_from_slice(tag.as_bytes());
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

        let issued_at_ms = cookie_issued_at_ms(cookie);
        let now_ms = self.elapsed_millis(now);
        if issued_at_ms > now_ms
            || now_ms.saturating_sub(issued_at_ms) > duration_millis(self.lifetime)
        {
            return false;
        }

        self.tag(peer_addr, hello, issued_at_ms) == *cookie_tag(cookie)
    }

    /// Deterministically derives the server's key-agreement random for a
    /// handshake, bound to the same `(peer, hello, issued_at_ms)` as the cookie.
    /// Because it is recomputed from the cookie's embedded timestamp (echoed in
    /// the response), the server stays stateless until the session is created yet
    /// derives the same `server_random` it sent in the challenge.
    fn server_random(
        &self,
        peer_addr: SocketAddr,
        hello: &Envelope,
        issued_at_ms: u64,
    ) -> [u8; SESSION_RANDOM_SIZE] {
        *self
            .keyed_hash(SERVER_RANDOM_DOMAIN, peer_addr, hello, issued_at_ms)
            .as_bytes()
    }

    fn elapsed_millis(&self, now: Instant) -> u64 {
        duration_millis(now.saturating_duration_since(self.started_at))
    }

    fn tag(&self, peer_addr: SocketAddr, hello: &Envelope, issued_at_ms: u64) -> blake3::Hash {
        self.keyed_hash(ADDRESS_COOKIE_DOMAIN, peer_addr, hello, issued_at_ms)
    }

    /// Keyed BLAKE3 over `(domain, normalized endpoint, issued_at_ms, hello)`.
    /// The domain separates the cookie tag from the server random.
    fn keyed_hash(
        &self,
        domain: &[u8],
        peer_addr: SocketAddr,
        hello: &Envelope,
        issued_at_ms: u64,
    ) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new_keyed(&self.secret);
        hasher.update(domain);
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

/// Cookie layout: `[version: 1][issued_at_ms: 8 BE][tag: 32]`.
const COOKIE_ISSUED_AT: std::ops::Range<usize> = 1..9;
const COOKIE_TAG: std::ops::Range<usize> = 9..UDP_ADDRESS_COOKIE_SIZE;

/// Reads the issue timestamp embedded in an address cookie.
fn cookie_issued_at_ms(cookie: &[u8; UDP_ADDRESS_COOKIE_SIZE]) -> u64 {
    u64::from_be_bytes(cookie[COOKIE_ISSUED_AT].try_into().expect("fixed cookie"))
}

/// The 32-byte keyed tag of an address cookie, unique per handshake.
fn cookie_tag(cookie: &[u8; UDP_ADDRESS_COOKIE_SIZE]) -> &[u8; 32] {
    cookie[COOKIE_TAG].try_into().expect("fixed cookie")
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
    psk: Option<[u8; 32]>,
    require_encryption: bool,
}

impl UdpServer {
    /// Creates a new UDP server bound to the given address with the default
    /// per-source new-session rate limit.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            session_rate_limit: UdpSessionRateLimitConfig::default(),
            address_validation: UdpAddressValidationConfig::default(),
            psk: None,
            require_encryption: false,
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

    /// Authenticates and encrypts every datagram using keys derived from the
    /// given 32-byte pre-shared key. A short handshake (carried in the
    /// address-validation exchange) derives **per-session, per-direction** keys,
    /// and each datagram carries a monotonic counter checked against an anti-replay
    /// window. This binds every datagram to a session, direction, and sequence, so
    /// an attacker without the key cannot replay, redirect, or reflect captured
    /// ciphertext. The client must use the same key. See
    /// [`mokosh_protocol::udp_record`] for the full scheme and trust model.
    pub fn with_datagram_encryption(mut self, psk: [u8; 32]) -> Self {
        self.psk = Some(psk);
        self
    }

    /// Requires a pre-shared key to be configured. When set, [`UdpServer::run`]
    /// returns [`UdpServerError::EncryptionRequired`] if no key was provided via
    /// [`with_datagram_encryption`](Self::with_datagram_encryption), rather than
    /// silently accepting plaintext UDP.
    pub fn require_encryption(mut self, required: bool) -> Self {
        self.require_encryption = required;
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
        if self.require_encryption && self.psk.is_none() {
            return Err(UdpServerError::EncryptionRequired);
        }
        let psk = self.psk;
        let bootstrap = psk
            .as_ref()
            .map(|psk| BootstrapRecords::derive(psk, Role::Server));
        // Per-session record-layer state, keyed by session id (epoch 1).
        let mut session_records: HashMap<SessionId, SessionRecords> = HashMap::new();

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
        // Single-use handshakes: a fingerprint (cookie tag ++ client_random) of every
        // address-response that created a session, with the instant consumed. A
        // verbatim replay (same fingerprint) is rejected so it cannot resurrect a
        // session with the same deterministic keys; a genuinely fresh handshake picks
        // a new client_random and is unaffected even if it reuses a cookie minted in
        // the same millisecond. Entries expire after the cookie lifetime, past which
        // the cookie no longer validates anyway.
        let mut consumed_handshakes: HashMap<[u8; 64], Instant> = HashMap::new();
        let cookie_lifetime = self.address_validation.cookie_lifetime;

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];

        loop {
            tokio::select! {
                // Inbound datagram from a peer.
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((len, peer_addr)) => {
                            // Authenticate + decrypt the whole datagram first. Anything
                            // that does not open (wrong key / wrong session / wrong
                            // direction / replayed) is dropped before it can reach envelope
                            // parsing, cookie validation, or session allocation — this is
                            // what blocks on-path / same-NAT replay, redirection, reflection.
                            // `from_bootstrap` records whether this datagram was opened with
                            // the shared bootstrap key (epoch 0). Such datagrams are the
                            // *handshake only* and must never carry established-session
                            // control/game routes (see the route guard after parsing).
                            let (datagram, from_bootstrap) = match bootstrap.as_ref() {
                                None => (Bytes::copy_from_slice(&buf[..len]), false),
                                Some(boot) => {
                                    let raw = &buf[..len];
                                    match RecordKey::peek_epoch(raw) {
                                        Some(EPOCH_SESSION) => {
                                            // Per-session key: requires an established session
                                            // bound to this exact address.
                                            let Some(&sid) = addr_to_session.get(&peer_addr) else {
                                                tracing::debug!(
                                                    peer = %peer_addr,
                                                    "Dropping session datagram from unknown UDP peer"
                                                );
                                                continue;
                                            };
                                            let Some(records) = session_records.get_mut(&sid) else {
                                                continue;
                                            };
                                            match records.open(raw) {
                                                Ok(plain) => (plain, false),
                                                Err(_) => {
                                                    tracing::debug!(
                                                        peer = %peer_addr,
                                                        "Dropping unauthenticated or replayed UDP datagram"
                                                    );
                                                    continue;
                                                }
                                            }
                                        }
                                        Some(EPOCH_BOOTSTRAP) => {
                                            // Handshake datagrams (HELLO / RESPONSE).
                                            match boot.open(raw) {
                                                Ok(plain) => (plain, true),
                                                Err(_) => {
                                                    tracing::debug!(
                                                        peer = %peer_addr,
                                                        "Dropping unauthenticated UDP handshake datagram"
                                                    );
                                                    continue;
                                                }
                                            }
                                        }
                                        _ => {
                                            tracing::debug!(
                                                peer = %peer_addr,
                                                "Dropping UDP datagram with unknown epoch"
                                            );
                                            continue;
                                        }
                                    }
                                }
                            };

                            let envelope = match Envelope::from_bytes(datagram) {
                                Ok(envelope) => envelope,
                                Err(e) => {
                                    tracing::error!(peer = %peer_addr, error = %e, "Failed to parse envelope");
                                    continue;
                                }
                            };

                            // The shared bootstrap key is for the handshake ONLY. Reject any
                            // other route sealed at epoch 0 so an established session's control
                            // or game traffic (e.g. DISCONNECT) cannot be carried on the
                            // session-agnostic bootstrap key instead of its per-session key.
                            if from_bootstrap
                                && !matches!(
                                    envelope.route_id,
                                    routes::HELLO | routes::UDP_ADDRESS_RESPONSE
                                )
                            {
                                tracing::debug!(
                                    peer = %peer_addr,
                                    route_id = envelope.route_id,
                                    "Dropping non-handshake route sealed with the bootstrap key"
                                );
                                continue;
                            }

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
                                    let issued_at_ms = cookie_issued_at_ms(&cookie);
                                    let server_random = cookie_signer.server_random(
                                        peer_addr,
                                        &envelope,
                                        issued_at_ms,
                                    );
                                    let challenge = UdpAddressChallenge {
                                        cookie,
                                        server_random,
                                    };
                                    let challenge_envelope = Envelope::new_simple(
                                        envelope.protocol_version,
                                        3,
                                        0,
                                        routes::UDP_ADDRESS_CHALLENGE,
                                        0,
                                        EnvelopeFlags::empty(),
                                        challenge.to_bytes(),
                                    );
                                    // Handshake replies are sealed at epoch 0 (bootstrap key,
                                    // random nonce) because the per-session key does not exist yet.
                                    let challenge_bytes = match bootstrap.as_ref() {
                                        None => challenge_envelope.to_bytes(),
                                        Some(boot) => match boot.seal(&challenge_envelope.to_bytes()) {
                                            Ok(sealed) => sealed,
                                            Err(e) => {
                                                // Includes CSPRNG failure: drop, never reuse a nonce.
                                                tracing::error!(error = %e, "Failed to seal UDP address challenge");
                                                continue;
                                            }
                                        },
                                    };
                                    // Compare sealed sizes: the inbound `len` is also sealed,
                                    // and AEAD adds a fixed overhead to both, so the ratio
                                    // budget still holds.
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

                                // Single-use handshake: an address-response that already created
                                // a session cannot create another. This stops a replayed response
                                // (e.g. after the session closes) from resurrecting a session with
                                // the same deterministic keys and reset counters. The fingerprint
                                // includes client_random so a fresh handshake (new random) is not
                                // blocked even if it reuses a same-millisecond cookie.
                                let now = Instant::now();
                                consumed_handshakes.retain(|_, consumed_at| {
                                    now.saturating_duration_since(*consumed_at) < cookie_lifetime
                                });
                                let mut handshake_id = [0u8; 64];
                                handshake_id[..32].copy_from_slice(cookie_tag(&response.cookie));
                                handshake_id[32..].copy_from_slice(&response.client_random);
                                if consumed_handshakes.contains_key(&handshake_id) {
                                    tracing::debug!(
                                        peer = %peer_addr,
                                        "Dropping replayed UDP address response (handshake already consumed)"
                                    );
                                    continue;
                                }
                                consumed_handshakes.insert(handshake_id, now);

                                let session_id = SessionId::new_v4();

                                // Derive per-session keys from the PSK + both randoms.
                                // `server_random` is recomputed statelessly from the cookie's
                                // timestamp, matching what was sent in the challenge.
                                if let Some(psk) = psk.as_ref() {
                                    let issued_at_ms = cookie_issued_at_ms(&response.cookie);
                                    let server_random = cookie_signer.server_random(
                                        peer_addr,
                                        &response.hello,
                                        issued_at_ms,
                                    );
                                    session_records.insert(
                                        session_id,
                                        SessionRecords::derive(
                                            psk,
                                            &response.client_random,
                                            &server_random,
                                            Role::Server,
                                        ),
                                    );
                                }

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

                            if let Err(e) = incoming_tx.send(SessionEnvelope { session_id, envelope, protected_udp: bootstrap.is_some() && !from_bootstrap }).await {
                                tracing::error!(error = %e, "Failed to send envelope to event loop");
                                break;
                            }

                            // Peer asked to disconnect: forget the mapping.
                            if is_disconnect {
                                addr_to_session.remove(&peer_addr);
                                session_to_addr.remove(&session_id);
                                session_records.remove(&session_id);
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
                    // Seal outbound with the per-session key (epoch 1). If encryption is
                    // on but the session has no record state, drop rather than leak plaintext.
                    let wire = match bootstrap.as_ref() {
                        None => Some(envelope.to_bytes()),
                        Some(_) => match session_records.get_mut(&session_id) {
                            Some(records) => match records.seal(&envelope.to_bytes()) {
                                Ok(sealed) => Some(sealed),
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to seal outbound UDP datagram");
                                    None
                                }
                            },
                            None => {
                                tracing::warn!(
                                    session = %session_id,
                                    "Dropping outbound: no session record state"
                                );
                                None
                            }
                        },
                    };
                    if let Some(bytes) = wire {
                        if let Err(e) = socket.send_to(&bytes, peer_addr).await {
                            tracing::error!(peer = %peer_addr, error = %e, "Failed to send to UDP peer");
                        }
                    }

                    // Server closed the session: forget the mapping.
                    if is_disconnect {
                        if let Some(addr) = session_to_addr.remove(&session_id) {
                            addr_to_session.remove(&addr);
                        }
                        session_records.remove(&session_id);
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

    #[error("UDP encryption is required but no authenticating datagram sealer was configured")]
    EncryptionRequired,
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
        let response = UdpAddressResponse {
            cookie,
            client_random: [0u8; SESSION_RANDOM_SIZE],
            hello,
        };
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

    // --- Per-session, per-direction datagram authentication (UDP security #3) ---

    const TEST_PSK: [u8; 32] = [0x11; 32];

    async fn spawn_sealed_server(
        psk: [u8; 32],
    ) -> (
        SocketAddr,
        mpsc::Receiver<SessionEnvelope>,
        mpsc::Sender<SessionEnvelope>,
    ) {
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        let probe = UdpSocket::bind(bind).await.unwrap();
        let bound_addr = probe.local_addr().unwrap();
        drop(probe);

        let server = UdpServer::new(bound_addr)
            .with_datagram_encryption(psk)
            .require_encryption(true);
        tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, Some(ready_tx)).await;
        });
        ready_rx.await.unwrap();

        (bound_addr, incoming_rx, outgoing_tx)
    }

    /// A minimal encrypted client built on the same record types as `UdpClient`,
    /// exposing the raw wire bytes so tests can capture and replay opaque ciphertext.
    struct SealedPeer {
        socket: UdpSocket,
        psk: [u8; 32],
        bootstrap: BootstrapRecords,
        /// Per-session records, set by `handshake`.
        session: Option<SessionRecords>,
        /// The last epoch-0 address-response wire bytes sent during `handshake`.
        last_response_wire: Option<Bytes>,
    }

    impl SealedPeer {
        async fn connect(server_addr: SocketAddr, psk: [u8; 32]) -> Self {
            Self {
                socket: connect_client(server_addr).await,
                psk,
                bootstrap: BootstrapRecords::derive(&psk, Role::Client),
                session: None,
                last_response_wire: None,
            }
        }

        fn session(&mut self) -> &mut SessionRecords {
            self.session.as_mut().expect("handshake first")
        }

        /// Runs the sealed HELLO → CHALLENGE → RESPONSE handshake, derives session
        /// keys, and returns the `SessionId` the server assigned.
        async fn handshake(
            &mut self,
            incoming_rx: &mut mpsc::Receiver<SessionEnvelope>,
        ) -> SessionId {
            let hello = hello_envelope();
            let hello_wire = self.bootstrap.seal(&hello.to_bytes()).unwrap();
            self.socket.send(&hello_wire).await.unwrap();

            // Challenge (epoch 0).
            let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
            let len = timeout(Duration::from_secs(1), self.socket.recv(&mut buf))
                .await
                .unwrap()
                .unwrap();
            let chal_env = Envelope::from_bytes(self.bootstrap.open(&buf[..len]).unwrap()).unwrap();
            assert_eq!(chal_env.route_id, routes::UDP_ADDRESS_CHALLENGE);
            let challenge = UdpAddressChallenge::from_bytes(&chal_env.payload).unwrap();

            // Derive per-session keys from both randoms.
            let mut client_random = [0u8; SESSION_RANDOM_SIZE];
            getrandom::getrandom(&mut client_random).unwrap();
            self.session = Some(SessionRecords::derive(
                &self.psk,
                &client_random,
                &challenge.server_random,
                Role::Client,
            ));

            // Response (epoch 0).
            let response = UdpAddressResponse {
                cookie: challenge.cookie,
                client_random,
                hello: hello.clone(),
            };
            let resp_env = Envelope::new_simple(
                hello.protocol_version,
                3,
                0,
                routes::UDP_ADDRESS_RESPONSE,
                0,
                EnvelopeFlags::empty(),
                response.to_bytes(),
            );
            let resp_wire = self.bootstrap.seal(&resp_env.to_bytes()).unwrap();
            self.last_response_wire = Some(resp_wire.clone());
            self.socket.send(&resp_wire).await.unwrap();

            timeout(Duration::from_secs(1), incoming_rx.recv())
                .await
                .unwrap()
                .unwrap()
                .session_id
        }

        /// Seals an epoch-1 game datagram, returning the opaque wire bytes.
        fn seal_game(&mut self, envelope: &Envelope) -> Bytes {
            self.session().seal(&envelope.to_bytes()).unwrap()
        }

        async fn send_game(&mut self, envelope: &Envelope) {
            let wire = self.seal_game(envelope);
            self.socket.send(&wire).await.unwrap();
        }
    }

    #[tokio::test]
    async fn sealed_session_establishes_and_routes_both_ways() {
        let (server_addr, mut incoming_rx, outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut peer = SealedPeer::connect(server_addr, TEST_PSK).await;

        let session_id = peer.handshake(&mut incoming_rx).await;

        // Client -> server game message round-trips through the record layer.
        peer.send_game(&test_envelope(100, 1, b"hi")).await;
        let delivered = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.session_id, session_id);
        assert_eq!(delivered.envelope.payload, Bytes::from_static(b"hi"));

        // Server -> client is sealed epoch-1 and opens with the per-session key.
        outgoing_tx
            .send(SessionEnvelope::new(
                session_id,
                test_envelope(200, 2, b"pong"),
            ))
            .await
            .unwrap();
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let len = timeout(Duration::from_secs(1), peer.socket.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let received = Envelope::from_bytes(peer.session().open(&buf[..len]).unwrap()).unwrap();
        assert_eq!(received.route_id, 200);
        assert_eq!(received.payload, Bytes::from_static(b"pong"));
    }

    #[tokio::test]
    async fn plaintext_hello_is_dropped_by_encrypted_server() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let client = connect_client(server_addr).await;

        // Unauthenticated (plaintext) HELLO: dropped before parsing, no challenge,
        // no session.
        client.send(&hello_envelope().to_bytes()).await.unwrap();

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        assert!(timeout(Duration::from_millis(150), client.recv(&mut buf))
            .await
            .is_err());
        assert!(timeout(Duration::from_millis(150), incoming_rx.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn wrong_key_handshake_is_dropped() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        // A peer with the wrong PSK: its bootstrap-sealed HELLO fails to open.
        let attacker = SealedPeer::connect(server_addr, [0x99; 32]).await;
        let hello = hello_envelope();
        let wire = attacker.bootstrap.seal(&hello.to_bytes()).unwrap();
        attacker.socket.send(&wire).await.unwrap();

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        assert!(
            timeout(Duration::from_millis(150), attacker.socket.recv(&mut buf))
                .await
                .is_err()
        );
        assert!(timeout(Duration::from_millis(150), incoming_rx.recv())
            .await
            .is_err());
    }

    /// UDP security #3 (a): a datagram captured from session A, resent verbatim
    /// from session B's address, must NOT be accepted as B's message. Per-session
    /// keys make B's opener reject A's ciphertext.
    #[tokio::test]
    async fn captured_ciphertext_cannot_be_redirected_to_another_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut a = SealedPeer::connect(server_addr, TEST_PSK).await;
        let mut b = SealedPeer::connect(server_addr, TEST_PSK).await;
        let sid_a = a.handshake(&mut incoming_rx).await;
        let sid_b = b.handshake(&mut incoming_rx).await;
        assert_ne!(sid_a, sid_b);

        // A sends a game packet; capture its opaque ciphertext.
        let captured = a.seal_game(&test_envelope(100, 1, b"from-A"));
        a.socket.send(&captured).await.unwrap();
        let delivered = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.session_id, sid_a);

        // Attacker replays A's exact bytes from B's socket. Must be dropped.
        b.socket.send(&captured).await.unwrap();
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());
    }

    /// UDP security #3 (b): an old sealed DISCONNECT replayed after the peer
    /// reconnects must NOT tear down the new session (new per-session keys).
    #[tokio::test]
    async fn stale_disconnect_cannot_kill_reconnected_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut peer = SealedPeer::connect(server_addr, TEST_PSK).await;
        let sid1 = peer.handshake(&mut incoming_rx).await;

        // Capture a sealed DISCONNECT and deliver it (tears down session 1).
        let captured_disconnect = peer.seal_game(&test_envelope(routes::DISCONNECT, 0, b"{}"));
        peer.socket.send(&captured_disconnect).await.unwrap();
        let delivered = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.session_id, sid1);
        assert_eq!(delivered.envelope.route_id, routes::DISCONNECT);

        // Reconnect on the SAME socket → new session, new keys.
        let sid2 = peer.handshake(&mut incoming_rx).await;
        assert_ne!(sid2, sid1);

        // Replay the OLD DISCONNECT ciphertext: new session's key rejects it.
        peer.socket.send(&captured_disconnect).await.unwrap();
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());

        // A fresh (new-key) game message on the reconnected session still works.
        peer.send_game(&test_envelope(101, 1, b"after")).await;
        let after = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.session_id, sid2);
        assert_eq!(after.envelope.payload, Bytes::from_static(b"after"));
    }

    /// Replaying a peer's own captured epoch-1 datagram within the same session is
    /// rejected by the anti-replay window.
    #[tokio::test]
    async fn replayed_session_datagram_is_dropped() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut peer = SealedPeer::connect(server_addr, TEST_PSK).await;
        let sid = peer.handshake(&mut incoming_rx).await;

        let captured = peer.seal_game(&test_envelope(100, 1, b"once"));
        peer.socket.send(&captured).await.unwrap();
        let first = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.session_id, sid);

        // Exact same datagram again: dropped by the replay window.
        peer.socket.send(&captured).await.unwrap();
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());
    }

    /// Replaying a captured address-response after the session closes must NOT
    /// re-establish a session with the same (deterministic) keys — the cookie is
    /// single-use.
    #[tokio::test]
    async fn replayed_handshake_cannot_resurrect_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut peer = SealedPeer::connect(server_addr, TEST_PSK).await;
        let sid1 = peer.handshake(&mut incoming_rx).await;
        let captured_response = peer.last_response_wire.clone().unwrap();
        // An old game packet captured under session 1's keys.
        let old_game = peer.seal_game(&test_envelope(100, 1, b"old"));

        // Close the session.
        peer.send_game(&test_envelope(routes::DISCONNECT, 0, b"{}"))
            .await;
        let disc = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(disc.session_id, sid1);
        assert_eq!(disc.envelope.route_id, routes::DISCONNECT);

        // Replay the captured RESPONSE: the cookie is already consumed → no new session.
        peer.socket.send(&captured_response).await.unwrap();
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());

        // Even if it had, the old game packet must not be accepted.
        peer.socket.send(&old_game).await.unwrap();
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());
    }

    /// A control packet (e.g. DISCONNECT) sealed with the shared bootstrap key must
    /// NOT be accepted for an established session — epoch 0 is handshake-only.
    #[tokio::test]
    async fn bootstrap_key_cannot_carry_session_control() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_sealed_server(TEST_PSK).await;
        let mut peer = SealedPeer::connect(server_addr, TEST_PSK).await;
        let sid = peer.handshake(&mut incoming_rx).await;

        // DISCONNECT sealed with the bootstrap key (epoch 0) rather than the session key.
        let disc = test_envelope(routes::DISCONNECT, 0, b"{}");
        let boot_wire = peer.bootstrap.seal(&disc.to_bytes()).unwrap();
        peer.socket.send(&boot_wire).await.unwrap();

        // Dropped: not delivered as a DISCONNECT.
        assert!(timeout(Duration::from_millis(200), incoming_rx.recv())
            .await
            .is_err());

        // The session is still alive — a normal epoch-1 message is delivered.
        peer.send_game(&test_envelope(100, 1, b"alive")).await;
        let alive = timeout(Duration::from_secs(1), incoming_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alive.session_id, sid);
        assert_eq!(alive.envelope.payload, Bytes::from_static(b"alive"));
    }

    #[tokio::test]
    async fn require_encryption_without_psk_errors() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(1);

        let server = UdpServer::new(addr).require_encryption(true);
        let result = server.run(incoming_tx, outgoing_rx, None).await;
        assert!(matches!(result, Err(UdpServerError::EncryptionRequired)));
    }
}
