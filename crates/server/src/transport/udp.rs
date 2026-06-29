//! UDP server transport for Mokosh
//!
//! An alternative to the WebSocket transport for latency-sensitive games.
//!
//! UDP is connectionless, so this transport synthesizes "sessions" from the
//! peer's [`SocketAddr`]: the first datagram seen from a new address is treated
//! as a new connection and assigned a fresh [`SessionId`]. Subsequent datagrams
//! from the same address reuse that session id. Outgoing envelopes are routed
//! back to the peer via a reverse `SessionId -> SocketAddr` map.
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
//!   either direction. Dead peers are otherwise reaped by the server's own
//!   keepalive/timeout logic at the protocol layer.

use bytes::Bytes;
use mokosh_protocol::messages::routes;
use mokosh_protocol::{Envelope, SessionEnvelope, SessionId};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Maximum size of a single inbound UDP datagram (64 KiB).
const MAX_DATAGRAM_SIZE: usize = 65_535;

/// UDP server that accepts datagrams and bridges them to envelope channels.
///
/// Mirrors the API of [`WebSocketServer`](super::websocket::WebSocketServer):
/// construct with a bind address and drive it with [`UdpServer::run`].
pub struct UdpServer {
    addr: SocketAddr,
}

impl UdpServer {
    /// Creates a new UDP server bound to the given address.
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
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

                            // Look up or create a session for this peer.
                            let session_id = *addr_to_session.entry(peer_addr).or_insert_with(|| {
                                let id = SessionId::new_v4();
                                tracing::info!(peer = %peer_addr, session = %id, "New UDP connection");
                                id
                            });
                            session_to_addr.insert(session_id, peer_addr);

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

    async fn spawn_server() -> (SocketAddr, mpsc::Receiver<SessionEnvelope>, mpsc::Sender<SessionEnvelope>) {
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Bind first to learn the assigned port, then hand the socket's address
        // to the server (it rebinds the same ephemeral-resolved address).
        let probe = UdpSocket::bind(bind).await.unwrap();
        let bound_addr = probe.local_addr().unwrap();
        drop(probe);

        let server = UdpServer::new(bound_addr);
        tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, Some(ready_tx)).await;
        });
        ready_rx.await.unwrap();

        (bound_addr, incoming_rx, outgoing_tx)
    }

    #[tokio::test]
    async fn test_server_receives_envelope() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        let env = test_envelope(100, 1, b"hello");
        client.send(&env.to_bytes()).await.unwrap();

        let session_envelope =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), incoming_rx.recv())
                .await
                .unwrap()
                .unwrap();

        assert!(!session_envelope.session_id.is_nil());
        assert_eq!(session_envelope.envelope.route_id, 100);
        assert_eq!(session_envelope.envelope.payload, Bytes::from_static(b"hello"));
    }

    #[tokio::test]
    async fn test_same_peer_reuses_session() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        client.send(&test_envelope(100, 1, b"a").to_bytes()).await.unwrap();
        client.send(&test_envelope(100, 2, b"b").to_bytes()).await.unwrap();

        let first = incoming_rx.recv().await.unwrap();
        let second = incoming_rx.recv().await.unwrap();
        assert_eq!(first.session_id, second.session_id);
    }

    #[tokio::test]
    async fn test_distinct_peers_get_distinct_sessions() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client1.connect(server_addr).await.unwrap();
        let client2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client2.connect(server_addr).await.unwrap();

        client1.send(&test_envelope(100, 1, b"c1").to_bytes()).await.unwrap();
        client2.send(&test_envelope(100, 1, b"c2").to_bytes()).await.unwrap();

        let a = incoming_rx.recv().await.unwrap();
        let b = incoming_rx.recv().await.unwrap();
        assert_ne!(a.session_id, b.session_id);
    }

    #[tokio::test]
    async fn test_outgoing_routed_to_peer() {
        let (server_addr, mut incoming_rx, outgoing_tx) = spawn_server().await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        client.send(&test_envelope(100, 1, b"ping").to_bytes()).await.unwrap();
        let session_id = incoming_rx.recv().await.unwrap().session_id;

        let response = test_envelope(200, 2, b"pong");
        outgoing_tx
            .send(SessionEnvelope::new(session_id, response))
            .await
            .unwrap();

        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let len = tokio::time::timeout(tokio::time::Duration::from_secs(1), client.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let received = Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])).unwrap();
        assert_eq!(received.route_id, 200);
        assert_eq!(received.payload, Bytes::from_static(b"pong"));
    }

    #[tokio::test]
    async fn test_invalid_datagram_is_ignored() {
        let (server_addr, mut incoming_rx, _outgoing_tx) = spawn_server().await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(server_addr).await.unwrap();

        // Too short to be an envelope header.
        client.send(&[1, 2, 3]).await.unwrap();
        // Followed by a valid one.
        client.send(&test_envelope(100, 1, b"ok").to_bytes()).await.unwrap();

        let session_envelope =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), incoming_rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(session_envelope.envelope.payload, Bytes::from_static(b"ok"));
    }
}
