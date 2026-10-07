//! UDP client transport for Mokosh (native-only)
//!
//! An alternative to the WebSocket transport for latency-sensitive games.
//! Browsers cannot open raw UDP sockets, so this transport is native-only;
//! WASM clients should continue to use `BrowserWebSocketClient`.
//!
//! The client binds an ephemeral local UDP socket and `connect`s it to the
//! server address so that `send`/`recv` talk only to that peer. Each envelope
//! is transmitted as a single datagram. Address-validation challenges are
//! answered inside this transport, transparently to the client event loop and
//! reliability decorator.
//!
//! # Caveats
//! - **No delivery guarantees.** UDP is unreliable and unordered; reliability
//!   and ordering are left to higher layers.
//! - **Datagram size.** Keep payloads below the path MTU (~1200 bytes is a safe
//!   practical limit); inbound datagrams are capped at 64 KiB.

use super::Transport;
use crate::compat::mpsc;
use async_trait::async_trait;
use bytes::Bytes;
use mokosh_protocol::messages::routes;
use mokosh_protocol::udp_record::{
    bootstrap_keys, counter_nonce, nonce_counter, session_keys, Direction, ReplayWindow,
};
use mokosh_protocol::{
    Envelope, EnvelopeFlags, RecordKey, UdpAddressChallenge, UdpAddressResponse, EPOCH_BOOTSTRAP,
    EPOCH_SESSION, SESSION_RANDOM_SIZE,
};
use std::sync::Arc;
use tokio::net::UdpSocket;

/// Maximum size of a single inbound UDP datagram (64 KiB).
const MAX_DATAGRAM_SIZE: usize = 65_535;

/// A random 96-bit nonce for a bootstrap (epoch-0) record, where the shared key
/// means counters would collide across peers. Returns `None` if the OS CSPRNG
/// fails — the caller MUST drop the datagram rather than risk a repeated nonce.
fn random_bootstrap_nonce() -> Option<[u8; 12]> {
    let mut nonce = [0u8; 12];
    match getrandom::getrandom(&mut nonce) {
        Ok(()) => Some(nonce),
        Err(e) => {
            tracing::error!(error = %e, "CSPRNG failure generating UDP bootstrap nonce; dropping datagram");
            None
        }
    }
}

/// Per-session record-layer state derived after the handshake (epoch 1).
struct ClientSession {
    send_key: RecordKey,
    send_counter: u64,
    recv_key: RecordKey,
    recv_replay: ReplayWindow,
}

/// UDP client that connects to a server and bridges envelope channels.
///
/// Mirrors the API of [`WebSocketClient`](super::websocket::WebSocketClient):
/// construct with the server address and drive it via [`Transport::run`].
pub struct UdpClient {
    /// Server address, e.g. `"127.0.0.1:8080"`.
    server_addr: String,
    psk: Option<[u8; 32]>,
    require_encryption: bool,
}

impl UdpClient {
    /// Creates a new UDP client targeting the given server address.
    ///
    /// The address must be a `host:port` string (not a `ws://` URL).
    pub fn new(server_addr: impl Into<String>) -> Self {
        Self {
            server_addr: server_addr.into(),
            psk: None,
            require_encryption: false,
        }
    }

    /// Authenticates and encrypts every datagram using keys derived from the
    /// 32-byte pre-shared key, matching the server's
    /// [`with_datagram_encryption`]
    /// A short handshake derives per-session, per-direction keys; see
    /// [`mokosh_protocol::udp_record`] for the scheme and trust model.
    pub fn with_datagram_encryption(mut self, psk: [u8; 32]) -> Self {
        self.psk = Some(psk);
        self
    }

    /// Requires a pre-shared key to be configured. When set, [`Transport::run`]
    /// returns [`UdpClientError::EncryptionRequired`] if no key was provided via
    /// [`with_datagram_encryption`](Self::with_datagram_encryption).
    pub fn require_encryption(mut self, required: bool) -> Self {
        self.require_encryption = required;
        self
    }
}

#[async_trait]
impl Transport for UdpClient {
    type Error = UdpClientError;

    async fn run(
        self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        if self.require_encryption && self.psk.is_none() {
            return Err(UdpClientError::EncryptionRequired);
        }
        let psk = self.psk;
        // Bootstrap (epoch-0) keys for the handshake: (outbound c2s, inbound s2c).
        let bootstrap = psk.as_ref().map(|psk| {
            let (kb_c2s, kb_s2c) = bootstrap_keys(psk);
            (
                RecordKey::new(&kb_c2s, EPOCH_BOOTSTRAP, Direction::ClientToServer),
                RecordKey::new(&kb_s2c, EPOCH_BOOTSTRAP, Direction::ServerToClient),
            )
        });

        tracing::info!(server = %self.server_addr, "Connecting UDP client");

        // Bind an ephemeral local socket. Use the unspecified address matching
        // the server's family so connect() can pick the right route.
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| UdpClientError::BindError(e.to_string()))?;
        socket
            .connect(&self.server_addr)
            .await
            .map_err(|e| UdpClientError::ConnectionError(e.to_string()))?;

        tracing::info!(server = %self.server_addr, "UDP socket ready");

        let socket = Arc::new(socket);
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let mut pending_hello: Option<Envelope> = None;
        // Per-session keys, derived once the challenge arrives.
        let mut session: Option<ClientSession> = None;
        // The sealed address-response for the committed handshake. We commit to the
        // FIRST challenge (one `client_random`, one cookie) and resend this exact
        // response for any duplicate challenge, so both ends converge on one key
        // agreement instead of diverging on a freshly-generated `client_random`.
        let mut committed_response: Option<Bytes> = None;
        // Switch outbound to epoch 1 only after opening the first inbound epoch-1
        // datagram (proof the server has the session keys); tolerates handshake loss.
        let mut outbound_epoch1 = false;

        loop {
            tokio::select! {
                recv_result = socket.recv(&mut buf) => {
                    match recv_result {
                        Ok(len) => {
                            // Open the datagram by epoch. Anything that fails to open
                            // (wrong key / wrong direction / replayed) is dropped.
                            let datagram = match bootstrap.as_ref() {
                                None => Bytes::copy_from_slice(&buf[..len]),
                                Some((_boot_out, boot_in)) => {
                                    let raw = &buf[..len];
                                    match RecordKey::peek_epoch(raw) {
                                        Some(EPOCH_BOOTSTRAP) => match boot_in.open(raw) {
                                            Ok((_nonce, plain)) => plain,
                                            Err(_) => {
                                                tracing::debug!(
                                                    "Dropping unauthenticated UDP handshake datagram"
                                                );
                                                continue;
                                            }
                                        },
                                        Some(EPOCH_SESSION) => {
                                            let Some(s) = session.as_mut() else {
                                                tracing::debug!(
                                                    "Dropping session datagram before key agreement"
                                                );
                                                continue;
                                            };
                                            match s.recv_key.open(raw) {
                                                Ok((nonce, plain)) => {
                                                    let counter = nonce_counter(&nonce);
                                                    if s.recv_replay.is_replay(counter) {
                                                        tracing::debug!("Dropping replayed UDP datagram");
                                                        continue;
                                                    }
                                                    s.recv_replay.record(counter);
                                                    // Server proved it has the session keys.
                                                    outbound_epoch1 = true;
                                                    plain
                                                }
                                                Err(_) => {
                                                    tracing::debug!("Dropping unauthenticated UDP datagram");
                                                    continue;
                                                }
                                            }
                                        }
                                        _ => {
                                            tracing::debug!("Dropping UDP datagram with unknown epoch");
                                            continue;
                                        }
                                    }
                                }
                            };

                            match Envelope::from_bytes(datagram) {
                                Ok(envelope) => {
                                    if envelope.route_id == routes::UDP_ADDRESS_CHALLENGE {
                                        // Duplicate challenge: resend the committed response so we
                                        // never renegotiate keys mid-handshake (which would diverge
                                        // from whichever response the server already accepted).
                                        if let Some(wire) = committed_response.clone() {
                                            if let Err(e) = socket.send(&wire).await {
                                                tracing::error!(
                                                    error = %e,
                                                    "Failed to resend UDP address response"
                                                );
                                                break;
                                            }
                                            continue;
                                        }

                                        let Some(hello) = pending_hello.clone() else {
                                            tracing::debug!(
                                                "Ignoring UDP address challenge without a pending HELLO"
                                            );
                                            continue;
                                        };
                                        let challenge =
                                            match UdpAddressChallenge::from_bytes(&envelope.payload) {
                                                Ok(challenge) => challenge,
                                                Err(e) => {
                                                    tracing::debug!(
                                                        error = %e,
                                                        "Ignoring malformed UDP address challenge"
                                                    );
                                                    continue;
                                                }
                                            };
                                        // First challenge: generate a fresh key-agreement random.
                                        // Always random (even in plaintext mode): the server also
                                        // uses it to fingerprint the single-use handshake, so a
                                        // zero value would make same-millisecond reconnects collide.
                                        let mut client_random = [0u8; SESSION_RANDOM_SIZE];
                                        if getrandom::getrandom(&mut client_random).is_err() {
                                            tracing::error!("Failed to generate client random");
                                            continue;
                                        }
                                        if let Some(psk) = psk.as_ref() {
                                            let (ks_c2s, ks_s2c) = session_keys(
                                                psk,
                                                &client_random,
                                                &challenge.server_random,
                                            );
                                            session = Some(ClientSession {
                                                send_key: RecordKey::new(
                                                    &ks_c2s,
                                                    EPOCH_SESSION,
                                                    Direction::ClientToServer,
                                                ),
                                                send_counter: 0,
                                                recv_key: RecordKey::new(
                                                    &ks_s2c,
                                                    EPOCH_SESSION,
                                                    Direction::ServerToClient,
                                                ),
                                                recv_replay: ReplayWindow::new(),
                                            });
                                            outbound_epoch1 = false;
                                        }

                                        let response = UdpAddressResponse {
                                            cookie: challenge.cookie,
                                            client_random,
                                            hello: hello.clone(),
                                        };
                                        let response_envelope = Envelope::new_simple(
                                            hello.protocol_version,
                                            3,
                                            0,
                                            routes::UDP_ADDRESS_RESPONSE,
                                            0,
                                            EnvelopeFlags::empty(),
                                            response.to_bytes(),
                                        );
                                        // Response is an epoch-0 (bootstrap) record.
                                        let wire = match bootstrap.as_ref() {
                                            None => response_envelope.to_bytes(),
                                            Some((boot_out, _)) => {
                                                let Some(nonce) = random_bootstrap_nonce() else {
                                                    continue;
                                                };
                                                match boot_out
                                                    .seal(&nonce, &response_envelope.to_bytes())
                                                {
                                                    Ok(w) => w,
                                                    Err(e) => {
                                                        tracing::error!(
                                                            error = %e,
                                                            "Failed to seal UDP address response"
                                                        );
                                                        continue;
                                                    }
                                                }
                                            }
                                        };
                                        // Commit so duplicate challenges resend this exact response.
                                        committed_response = Some(wire.clone());
                                        if let Err(e) = socket.send(&wire).await {
                                            tracing::error!(
                                                error = %e,
                                                "Failed to send UDP address response"
                                            );
                                            break;
                                        }
                                        continue;
                                    }

                                    if matches!(
                                        envelope.route_id,
                                        routes::HELLO_OK
                                            | routes::HELLO_ERROR
                                            | routes::DISCONNECT
                                    ) {
                                        pending_hello = None;
                                    }

                                    if incoming_tx.send(envelope).await.is_err() {
                                        tracing::error!("Failed to send envelope to event loop");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to parse envelope");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "UDP receive error");
                            break;
                        }
                    }
                }

                Some(envelope) = outgoing_rx.recv() => {
                    if envelope.route_id == routes::HELLO {
                        pending_hello = Some(envelope.clone());
                    } else if matches!(
                        envelope.route_id,
                        routes::UDP_ADDRESS_CHALLENGE | routes::UDP_ADDRESS_RESPONSE
                    ) {
                        tracing::debug!(
                            route_id = envelope.route_id,
                            "Dropping application-supplied UDP validation message"
                        );
                        continue;
                    }

                    // Seal with the per-session key (epoch 1) once the session is
                    // active; until then (HELLO / retransmits) use the bootstrap key.
                    let wire = match bootstrap.as_ref() {
                        None => Some(envelope.to_bytes()),
                        Some((boot_out, _)) => {
                            if outbound_epoch1 {
                                match session.as_mut() {
                                    Some(s) => {
                                        let nonce = counter_nonce(s.send_counter);
                                        s.send_counter = s.send_counter.wrapping_add(1);
                                        s.send_key.seal(&nonce, &envelope.to_bytes()).ok()
                                    }
                                    None => None,
                                }
                            } else {
                                random_bootstrap_nonce()
                                    .and_then(|nonce| boot_out.seal(&nonce, &envelope.to_bytes()).ok())
                            }
                        }
                    };
                    match wire {
                        Some(bytes) => {
                            if let Err(e) = socket.send(&bytes).await {
                                tracing::error!(error = %e, "Failed to send datagram");
                                break;
                            }
                        }
                        None => tracing::error!("Failed to seal outgoing UDP datagram"),
                    }
                }

                else => {
                    break;
                }
            }
        }

        Ok(())
    }
}

/// UDP client errors.
#[derive(Debug, thiserror::Error)]
pub enum UdpClientError {
    #[error("Failed to bind local socket: {0}")]
    BindError(String),

    #[error("Failed to connect: {0}")]
    ConnectionError(String),

    #[error("Socket error: {0}")]
    SocketError(String),

    #[error("UDP encryption is required but no authenticating datagram sealer was configured")]
    EncryptionRequired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mokosh_protocol::EnvelopeFlags;

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

    /// Spawns a trivial UDP echo server, returning its bound address.
    async fn spawn_echo_server() -> std::net::SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
            while let Ok((len, peer)) = socket.recv_from(&mut buf).await {
                let _ = socket.send_to(&buf[..len], peer).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn test_client_sends_and_receives() {
        let server_addr = spawn_echo_server().await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);

        let client = UdpClient::new(server_addr.to_string());
        let handle = tokio::spawn(async move {
            let _ = client.run(incoming_tx, outgoing_rx).await;
        });

        let env = test_envelope(100, 1, b"echo me");
        outgoing_tx.send(env.clone()).await.unwrap();

        let received =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), incoming_rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received.route_id, env.route_id);
        assert_eq!(received.msg_id, env.msg_id);
        assert_eq!(received.payload, env.payload);

        handle.abort();
    }

    #[tokio::test]
    async fn test_client_sends_multiple() {
        let server_addr = spawn_echo_server().await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);

        let client = UdpClient::new(server_addr.to_string());
        let handle = tokio::spawn(async move {
            let _ = client.run(incoming_tx, outgoing_rx).await;
        });

        for i in 1u64..=5 {
            outgoing_tx
                .send(test_envelope((100 + i) as u16, i, b"msg"))
                .await
                .unwrap();
        }

        let mut seen = Vec::new();
        for _ in 0..5 {
            let env = tokio::time::timeout(tokio::time::Duration::from_secs(1), incoming_rx.recv())
                .await
                .unwrap()
                .unwrap();
            seen.push(env.msg_id);
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3, 4, 5]);

        handle.abort();
    }

    #[tokio::test]
    async fn test_client_invalid_address() {
        let (incoming_tx, _incoming_rx) = mpsc::channel(16);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(16);

        let client = UdpClient::new("not a valid addr");
        let result = client.run(incoming_tx, outgoing_rx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn require_encryption_without_key_errors() {
        let (incoming_tx, _incoming_rx) = mpsc::channel(16);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(16);

        let client = UdpClient::new("127.0.0.1:9").require_encryption(true);
        let result = client.run(incoming_tx, outgoing_rx).await;
        assert!(matches!(result, Err(UdpClientError::EncryptionRequired)));
    }

    /// UDP security #3 (c): an encrypted client's own datagram, reflected back to
    /// it, must be dropped. Per-direction keys mean a `c2s` record cannot open
    /// under the client's `s2c` key.
    #[tokio::test]
    async fn reflected_own_datagram_is_dropped() {
        // Raw server that simply echoes whatever the client sends back to it.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let (incoming_tx, mut incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);

        let handle = tokio::spawn(
            UdpClient::new(server_addr.to_string())
                .with_datagram_encryption([0x7; 32])
                .require_encryption(true)
                .run(incoming_tx, outgoing_rx),
        );

        // Client's first outbound (HELLO) is an encrypted c2s record.
        outgoing_tx
            .send(test_envelope(routes::HELLO, 1, b"{}"))
            .await
            .unwrap();

        // Reflect the client's own datagram straight back to it.
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let (len, client_addr) = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            server.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();
        server.send_to(&buf[..len], client_addr).await.unwrap();

        // The reflected c2s record must NOT be delivered as a server message.
        assert!(
            tokio::time::timeout(tokio::time::Duration::from_millis(200), incoming_rx.recv())
                .await
                .is_err()
        );

        handle.abort();
    }

    /// UDP security: a duplicate challenge must not make the client renegotiate
    /// keys. The client commits to the first agreement and resends the exact same
    /// response, so both ends converge on one key set.
    #[tokio::test]
    async fn duplicate_challenge_resends_committed_response() {
        const PSK: [u8; 32] = [0x3; 32];
        let (kb_c2s, kb_s2c) = bootstrap_keys(&PSK);
        let server_boot_in = RecordKey::new(&kb_c2s, EPOCH_BOOTSTRAP, Direction::ClientToServer);
        let server_boot_out = RecordKey::new(&kb_s2c, EPOCH_BOOTSTRAP, Direction::ServerToClient);

        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let (incoming_tx, _incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let handle = tokio::spawn(
            UdpClient::new(server_addr.to_string())
                .with_datagram_encryption(PSK)
                .require_encryption(true)
                .run(incoming_tx, outgoing_rx),
        );

        // Client sends HELLO.
        outgoing_tx
            .send(test_envelope(routes::HELLO, 1, b"{}"))
            .await
            .unwrap();

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let (len, client_addr) = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            server.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();
        let (_n, hello_pt) = server_boot_in.open(&buf[..len]).unwrap();
        assert_eq!(
            Envelope::from_bytes(hello_pt).unwrap().route_id,
            routes::HELLO
        );

        let make_challenge = |cookie: [u8; mokosh_protocol::UDP_ADDRESS_COOKIE_SIZE],
                              sr: [u8; SESSION_RANDOM_SIZE]| {
            let challenge = UdpAddressChallenge {
                cookie,
                server_random: sr,
            };
            let env = Envelope::new_simple(
                1,
                3,
                0,
                routes::UDP_ADDRESS_CHALLENGE,
                0,
                EnvelopeFlags::empty(),
                challenge.to_bytes(),
            );
            server_boot_out
                .seal(&random_bootstrap_nonce().unwrap(), &env.to_bytes())
                .unwrap()
        };

        // Challenge #1, capture the client's response.
        server
            .send_to(
                &make_challenge(
                    [1u8; mokosh_protocol::UDP_ADDRESS_COOKIE_SIZE],
                    [0xAA; SESSION_RANDOM_SIZE],
                ),
                client_addr,
            )
            .await
            .unwrap();
        let (l1, _) = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            server.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();
        let response1 = buf[..l1].to_vec();

        // Challenge #2 (different cookie + server_random).
        server
            .send_to(
                &make_challenge(
                    [2u8; mokosh_protocol::UDP_ADDRESS_COOKIE_SIZE],
                    [0xBB; SESSION_RANDOM_SIZE],
                ),
                client_addr,
            )
            .await
            .unwrap();
        let (l2, _) = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            server.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();
        let response2 = buf[..l2].to_vec();

        assert_eq!(
            response1, response2,
            "client must resend the committed response verbatim on a duplicate challenge"
        );

        handle.abort();
    }
}
