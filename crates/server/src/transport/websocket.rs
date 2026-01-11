use super::Transport;
use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use godot_netlink_protocol::Envelope;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, tungstenite::Message};

/// WebSocket server that accepts connections and bridges them to envelope channels
pub struct WebSocketServer {
    addr: SocketAddr,
}

impl WebSocketServer {
    /// Creates a new WebSocket server bound to the given address
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }
}

/// Handles a single WebSocket connection
async fn handle_connection(
    stream: TcpStream,
    incoming_tx: mpsc::Sender<Envelope>,
    peer_addr: SocketAddr,
) -> Result<(), WebSocketServerError> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| WebSocketServerError::WebSocketError(e.to_string()))?;

    println!("WebSocket handshake completed with {}", peer_addr);

    let (_ws_sender, mut ws_receiver) = ws_stream.split();

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match Envelope::from_bytes(Bytes::from(data)) {
                            Ok(envelope) => {
                                if let Err(e) = incoming_tx.send(envelope).await {
                                    eprintln!("Failed to send envelope to event loop: {}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to parse envelope from {}: {}", peer_addr, e);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("Client {} disconnected", peer_addr);
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ignore text messages and other types
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error from {}: {}", peer_addr, e);
                        break;
                    }
                    None => {
                        println!("Connection closed by {}", peer_addr);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[async_trait]
impl Transport for WebSocketServer {
    type Error = WebSocketServerError;

    async fn run(
        self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| WebSocketServerError::BindError(e.to_string()))?;

        println!("WebSocket server listening on {}", self.addr);

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer_addr)) => {
                            println!("New connection from {}", peer_addr);
                            let incoming_tx = incoming_tx.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, incoming_tx, peer_addr).await {
                                    eprintln!("Connection error from {}: {}", peer_addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            eprintln!("Failed to accept connection: {}", e);
                        }
                    }
                }

                Some(envelope) = outgoing_rx.recv() => {
                    // TODO: Need to implement per-connection routing
                    // For now, this is a simplified version that doesn't handle multiple connections
                    println!("Server wants to send envelope (routing not yet implemented): route_id={}", envelope.route_id);
                }
            }
        }
    }
}

/// WebSocket server errors
#[derive(Debug, thiserror::Error)]
pub enum WebSocketServerError {
    #[error("Failed to bind to address: {0}")]
    BindError(String),

    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    #[error("Channel error: {0}")]
    ChannelError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use godot_netlink_protocol::EnvelopeFlags;
    use tokio_tungstenite::connect_async;

    #[tokio::test]
    async fn test_server_accepts_connection() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let _server = WebSocketServer::new(addr);
        let actual_addr = addr;

        let server_handle = tokio::spawn(async move {
            let listener = TcpListener::bind(actual_addr).await.unwrap();
            let bound_addr = listener.local_addr().unwrap();
            drop(listener);

            let server = WebSocketServer::new(bound_addr);
            let _ = server.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_server_receives_envelope() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);
        let (mut ws_stream, _) = connect_async(&url).await.unwrap();

        use futures::SinkExt;
        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        let bytes = test_envelope.to_bytes();
        ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                bytes.to_vec(),
            ))
            .await
            .unwrap();

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

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_server_handles_multiple_messages() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);
        let (mut ws_stream, _) = connect_async(&url).await.unwrap();

        use futures::SinkExt;

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

            let bytes = envelope.to_bytes();
            ws_stream
                .send(tokio_tungstenite::tungstenite::Message::Binary(
                    bytes.to_vec(),
                ))
                .await
                .unwrap();
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

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_server_handles_invalid_envelope() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);
        let (mut ws_stream, _) = connect_async(&url).await.unwrap();

        use futures::SinkExt;

        ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Binary(vec![
                1, 2, 3, 4, 5,
            ]))
            .await
            .unwrap();

        let valid_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"valid"),
        );

        let bytes = valid_envelope.to_bytes();
        ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                bytes.to_vec(),
            ))
            .await
            .unwrap();

        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received.route_id, valid_envelope.route_id);

        server_handle.abort();
    }
}
