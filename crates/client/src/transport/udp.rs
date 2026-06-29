//! UDP client transport for Mokosh (native-only)
//!
//! An alternative to the WebSocket transport for latency-sensitive games.
//! Browsers cannot open raw UDP sockets, so this transport is native-only;
//! WASM clients should continue to use `BrowserWebSocketClient`.
//!
//! The client binds an ephemeral local UDP socket and `connect`s it to the
//! server address so that `send`/`recv` talk only to that peer. Each envelope
//! is transmitted as a single datagram.
//!
//! # Caveats
//! - **No delivery guarantees.** UDP is unreliable and unordered; reliability
//!   and ordering are left to higher layers.
//! - **Datagram size.** Keep payloads below the path MTU (~1200 bytes is a safe
//!   practical limit); inbound datagrams are capped at 64 KiB.

use super::Transport;
use async_trait::async_trait;
use bytes::Bytes;
use crate::compat::mpsc;
use mokosh_protocol::Envelope;
use std::sync::Arc;
use tokio::net::UdpSocket;

/// Maximum size of a single inbound UDP datagram (64 KiB).
const MAX_DATAGRAM_SIZE: usize = 65_535;

/// UDP client that connects to a server and bridges envelope channels.
///
/// Mirrors the API of [`WebSocketClient`](super::websocket::WebSocketClient):
/// construct with the server address and drive it via [`Transport::run`].
pub struct UdpClient {
    /// Server address, e.g. `"127.0.0.1:8080"`.
    server_addr: String,
}

impl UdpClient {
    /// Creates a new UDP client targeting the given server address.
    ///
    /// The address must be a `host:port` string (not a `ws://` URL).
    pub fn new(server_addr: impl Into<String>) -> Self {
        Self {
            server_addr: server_addr.into(),
        }
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

        loop {
            tokio::select! {
                recv_result = socket.recv(&mut buf) => {
                    match recv_result {
                        Ok(len) => {
                            match Envelope::from_bytes(Bytes::copy_from_slice(&buf[..len])) {
                                Ok(envelope) => {
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
                    let bytes = envelope.to_bytes();
                    if let Err(e) = socket.send(&bytes).await {
                        tracing::error!(error = %e, "Failed to send datagram");
                        break;
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
            let env =
                tokio::time::timeout(tokio::time::Duration::from_secs(1), incoming_rx.recv())
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
}
