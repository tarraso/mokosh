use super::Transport;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use mokosh_protocol::Envelope;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// WebSocket client that connects to a server and bridges envelope channels
pub struct WebSocketClient {
    url: String,
}

impl WebSocketClient {
    /// Creates a new WebSocket client for the given URL
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl Transport for WebSocketClient {
    type Error = WebSocketClientError;

    async fn run(
        self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        tracing::info!(url = %self.url, "Connecting to WebSocket server");

        let (ws_stream, _) = connect_async(&self.url)
            .await
            .map_err(|e| WebSocketClientError::ConnectionError(e.to_string()))?;

        tracing::info!(url = %self.url, "WebSocket connection established");

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        loop {
            tokio::select! {
                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(Message::Binary(data))) => {
                            match Envelope::from_bytes(Bytes::from(data)) {
                                Ok(envelope) => {
                                    if let Err(e) = incoming_tx.send(envelope).await {
                                        tracing::error!(error = %e, "Failed to send envelope to event loop");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to parse envelope");
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            tracing::info!("Server closed connection");
                            break;
                        }
                        Some(Ok(_)) => {
                            // Ignore text messages and other types
                        }
                        Some(Err(e)) => {
                            tracing::error!(error = %e, "WebSocket error");
                            break;
                        }
                        None => {
                            tracing::info!("Connection closed");
                            break;
                        }
                    }
                }

                Some(envelope) = outgoing_rx.recv() => {
                    let bytes = envelope.to_bytes();
                    if let Err(e) = ws_sender.send(Message::Binary(bytes.to_vec())).await {
                        tracing::error!(error = %e, "Failed to send message");
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

/// WebSocket client errors
#[derive(Debug, thiserror::Error)]
pub enum WebSocketClientError {
    #[error("Failed to connect: {0}")]
    ConnectionError(String),

    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    #[error("Channel error: {0}")]
    ChannelError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use mokosh_protocol::EnvelopeFlags;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    async fn start_echo_server(addr: SocketAddr) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let listener = TcpListener::bind(addr).await.unwrap();

            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let ws_stream = accept_async(stream).await.unwrap();
                    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

                    use futures::{SinkExt, StreamExt};
                    while let Some(Ok(msg)) = ws_receiver.next().await {
                        if let tokio_tungstenite::tungstenite::Message::Binary(data) = msg {
                            let _ = ws_sender
                                .send(tokio_tungstenite::tungstenite::Message::Binary(data))
                                .await;
                        }
                    }
                });
            }
        })
    }

    #[tokio::test]
    async fn test_client_connects() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server_handle = start_echo_server(bound_addr).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let client_handle = tokio::spawn(async move {
            let _ = client.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        client_handle.abort();
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_client_sends_envelope() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server_handle = start_echo_server(bound_addr).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let client_handle = tokio::spawn(async move {
            let _ = client.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test from client"),
        );

        outgoing_tx.send(test_envelope.clone()).await.unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, test_envelope.route_id);
        assert_eq!(received.msg_id, test_envelope.msg_id);
        assert_eq!(received.payload, test_envelope.payload);

        client_handle.abort();
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_client_sends_multiple_envelopes() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server_handle = start_echo_server(bound_addr).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let client_handle = tokio::spawn(async move {
            let _ = client.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        for i in 1u64..=5 {
            let envelope = Envelope::new_simple(
                1,
                1,
                0,
                (100 + i) as u16,
                i,
                EnvelopeFlags::RELIABLE,
                Bytes::from(format!("message {}", i)),
            );

            outgoing_tx.send(envelope).await.unwrap();
        }

        for i in 1u64..=5 {
            let received = tokio::time::timeout(
                tokio::time::Duration::from_secs(1),
                incoming_rx.recv(),
            )
            .await
            .unwrap()
            .unwrap();

            assert_eq!(received.msg_id, i);
            assert_eq!(received.route_id, (100 + i) as u16);
        }

        client_handle.abort();
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_client_handles_connection_close() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let ws_stream = accept_async(stream).await.unwrap();
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                drop(ws_stream);
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let client = WebSocketClient::new(format!("ws://{}", bound_addr));

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            client.run(incoming_tx, outgoing_rx),
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_client_fails_to_connect_invalid_address() {
        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let client = WebSocketClient::new("ws://127.0.0.1:1");

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            client.run(incoming_tx, outgoing_rx),
        )
        .await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}
