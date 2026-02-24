use bytes::Bytes;
use futures::StreamExt;
use mokosh_protocol::{Envelope, SessionEnvelope, SessionId};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
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
    session_id: SessionId,
    incoming_tx: mpsc::Sender<SessionEnvelope>,
    mut outgoing_rx: mpsc::Receiver<Envelope>,
    peer_addr: SocketAddr,
) -> Result<(), WebSocketServerError> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|e| WebSocketServerError::WebSocketError(e.to_string()))?;

    tracing::info!(
        peer = %peer_addr,
        session = %session_id,
        "WebSocket handshake completed"
    );

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    loop {
        tokio::select! {
            // Handle incoming messages from WebSocket client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match Envelope::from_bytes(Bytes::from(data)) {
                            Ok(envelope) => {
                                let session_envelope = SessionEnvelope::new(session_id, envelope);
                                if let Err(e) = incoming_tx.send(session_envelope).await {
                                    tracing::error!(error = %e, "Failed to send envelope to event loop");
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!(peer = %peer_addr, error = %e, "Failed to parse envelope");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!(peer = %peer_addr, session = %session_id, "Client disconnected");
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ignore text messages and other types
                    }
                    Some(Err(e)) => {
                        tracing::error!(peer = %peer_addr, error = %e, "WebSocket error");
                        break;
                    }
                    None => {
                        tracing::info!(peer = %peer_addr, session = %session_id, "Connection closed by client");
                        break;
                    }
                }
            }

            // Handle outgoing messages to WebSocket client
            Some(envelope) = outgoing_rx.recv() => {
                use futures::SinkExt;
                let bytes = envelope.to_bytes();
                if let Err(e) = ws_sender.send(Message::Binary(bytes.to_vec())).await {
                    tracing::error!(peer = %peer_addr, error = %e, "Failed to send to WebSocket client");
                    break;
                }
            }

            else => {
                tracing::debug!(peer = %peer_addr, session = %session_id, "Connection handler shutting down");
                break;
            }
        }
    }

    tracing::info!(peer = %peer_addr, session = %session_id, "Client connection closed");
    Ok(())
}

impl WebSocketServer {
    /// Runs the WebSocket server with multi-client session routing
    ///
    /// This method accepts multiple client connections and routes messages
    /// based on session IDs. Each client connection is assigned a unique
    /// session ID, and incoming/outgoing messages are tagged with the
    /// session ID for proper routing.
    pub async fn run(
        self,
        incoming_tx: mpsc::Sender<SessionEnvelope>,
        mut outgoing_rx: mpsc::Receiver<SessionEnvelope>,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), WebSocketServerError> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| WebSocketServerError::BindError(e.to_string()))?;

        tracing::info!(addr = %self.addr, "WebSocket server listening");

        // Signal that server is ready to accept connections
        if let Some(tx) = ready_tx {
            let _ = tx.send(());
        }

        // Map of session_id -> channel sender for routing outgoing messages
        let clients: Arc<RwLock<HashMap<SessionId, mpsc::Sender<Envelope>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, peer_addr)) => {
                            // Generate unique session ID for this client
                            let session_id = SessionId::new_v4();

                            tracing::info!(peer = %peer_addr, session = %session_id, "New connection");

                            // Create a channel for outgoing messages to this specific client
                            let (client_tx, client_rx) = mpsc::channel::<Envelope>(100);

                            // Store the client sender in the routing map
                            {
                                let mut clients_lock = clients.write().await;
                                clients_lock.insert(session_id, client_tx);
                            }

                            // Clone necessary data for the connection handler
                            let incoming_tx = incoming_tx.clone();
                            let clients_clone = clients.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(
                                    stream,
                                    session_id,
                                    incoming_tx,
                                    client_rx,
                                    peer_addr,
                                ).await {
                                    tracing::error!(peer = %peer_addr, session = %session_id, error = %e, "Connection error");
                                }

                                // Clean up the client from the routing map on disconnect
                                let mut clients_lock = clients_clone.write().await;
                                clients_lock.remove(&session_id);
                                tracing::debug!(session = %session_id, "Removed session from routing table");
                            });
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to accept connection");
                        }
                    }
                }

                Some(session_envelope) = outgoing_rx.recv() => {
                    // Route the envelope to the correct client based on session_id
                    let session_id = session_envelope.session_id;
                    let envelope = session_envelope.envelope;

                    let clients_lock = clients.read().await;
                    if let Some(client_tx) = clients_lock.get(&session_id) {
                        if let Err(e) = client_tx.send(envelope).await {
                            tracing::error!(session = %session_id, error = %e, "Failed to route envelope");
                        }
                    } else {
                        tracing::warn!(
                            session = %session_id,
                            route_id = envelope.route_id,
                            "Cannot route envelope: client not connected"
                        );
                    }
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
    use mokosh_protocol::EnvelopeFlags;
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
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
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
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
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

        let session_envelope = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        // Verify session ID is present
        assert!(!session_envelope.session_id.is_nil());

        // Verify envelope contents
        let received = session_envelope.envelope;
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
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
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
            let session_envelope = tokio::time::timeout(
                tokio::time::Duration::from_secs(1),
                incoming_rx.recv(),
            )
            .await
            .unwrap()
            .unwrap();

            let received = session_envelope.envelope;
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
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
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

        let session_envelope = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        let received = session_envelope.envelope;
        assert_eq!(received.route_id, valid_envelope.route_id);

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_multiple_clients_connect() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (_outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);

        // Connect two clients
        let (mut ws_stream1, _) = connect_async(&url).await.unwrap();
        let (mut ws_stream2, _) = connect_async(&url).await.unwrap();

        use futures::SinkExt;

        // Send messages from both clients
        let envelope1 = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"client1"),
        );

        let envelope2 = Envelope::new_simple(
            1,
            1,
            0,
            100,
            2,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"client2"),
        );

        ws_stream1
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope1.to_bytes().to_vec(),
            ))
            .await
            .unwrap();

        ws_stream2
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope2.to_bytes().to_vec(),
            ))
            .await
            .unwrap();

        // Receive both messages
        let session_envelope1 = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        let session_envelope2 = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        // Verify different session IDs
        assert_ne!(session_envelope1.session_id, session_envelope2.session_id);

        // Verify payloads
        let recv1 = session_envelope1.envelope;
        let recv2 = session_envelope2.envelope;

        assert!(
            (recv1.payload == Bytes::from_static(b"client1")
                && recv2.payload == Bytes::from_static(b"client2"))
                || (recv1.payload == Bytes::from_static(b"client2")
                    && recv2.payload == Bytes::from_static(b"client1"))
        );

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_message_routing_to_correct_client() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);

        // Connect two clients
        let (mut ws_stream1, _) = connect_async(&url).await.unwrap();
        let (_ws_stream2, _) = connect_async(&url).await.unwrap();

        use futures::SinkExt;

        // Send a message from client1 to get its session ID
        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );

        ws_stream1
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                test_envelope.to_bytes().to_vec(),
            ))
            .await
            .unwrap();

        // Get the session envelope
        let session_envelope = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            incoming_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        let client1_session = session_envelope.session_id;

        // Send a response specifically to client1
        let response_envelope = Envelope::new_simple(
            1,
            1,
            0,
            200,
            2,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"response"),
        );

        let response_session_envelope = SessionEnvelope::new(client1_session, response_envelope);
        outgoing_tx.send(response_session_envelope).await.unwrap();

        // Client1 should receive the response
        let received_msg = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            ws_stream1.next(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        if let Message::Binary(data) = received_msg {
            let received_envelope = Envelope::from_bytes(Bytes::from(data)).unwrap();
            assert_eq!(received_envelope.route_id, 200);
            assert_eq!(received_envelope.payload, Bytes::from_static(b"response"));
        } else {
            panic!("Expected binary message");
        }

        server_handle.abort();
    }

    #[tokio::test]
    async fn test_client_disconnect_cleanup() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (incoming_tx, _incoming_rx) = mpsc::channel(10);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(10);

        let listener = TcpListener::bind(addr).await.unwrap();
        let bound_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = WebSocketServer::new(bound_addr);

        let server_handle = tokio::spawn(async move {
            let _ = server.run(incoming_tx, outgoing_rx, None).await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", bound_addr);

        // Connect a client
        let (ws_stream, _) = connect_async(&url).await.unwrap();

        // Disconnect immediately
        drop(ws_stream);

        // Wait for cleanup
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Try to send a message to a non-existent session (should log error but not crash)
        let fake_session = SessionId::new_v4();
        let test_envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"test"),
        );
        let session_envelope = SessionEnvelope::new(fake_session, test_envelope);

        // This should not panic, just log an error
        let _ = outgoing_tx.send(session_envelope).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        server_handle.abort();
    }
}
