use bytes::Bytes;
use mokosh_client::transport::websocket::WebSocketClient;
use mokosh_protocol::{Envelope, EnvelopeFlags, Transport};
use mokosh_server::transport::websocket::WebSocketServer;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_client_connection_refused() {
    let (client_incoming_tx, _client_incoming_rx) = mpsc::channel(100);
    let (_client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new("ws://127.0.0.1:1");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        ws_client.run(client_incoming_tx, client_outgoing_rx),
    )
    .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[tokio::test]
async fn test_server_handles_malformed_websocket_data() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(100);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await;
    });

    sleep(Duration::from_millis(200)).await;

    let url = format!("ws://{}", bound_addr);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    ws_stream
        .send(Message::Binary(vec![0xFF, 0xFF, 0xFF]))
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    let valid_envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"valid"),
    );

    ws_stream
        .send(Message::Binary(valid_envelope.to_bytes().to_vec()))
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(1), server_incoming_rx.recv())
        .await
        .unwrap();

    assert!(received.is_some());
    assert_eq!(received.unwrap().envelope.route_id, 100);

    server_handle.abort();
}

#[tokio::test]
async fn test_empty_payload() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(100);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await;
    });

    sleep(Duration::from_millis(200)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

    let client_transport_handle = tokio::spawn(async move {
        let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let client_loop_handle = tokio::spawn(async move {
        use mokosh_client::Client;
        let client = Client::new(client_incoming_rx, client_outgoing_tx);
        client.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let empty_envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::new(),
    );

    let url = format!("ws://{}", bound_addr);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    use futures::SinkExt;
    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            empty_envelope.to_bytes().to_vec(),
        ))
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(1), server_incoming_rx.recv())
        .await
        .unwrap();

    assert!(received.is_some());
    let session_envelope = received.unwrap();
    let envelope = session_envelope.envelope;
    assert_eq!(envelope.payload_len, 0);
    assert_eq!(envelope.payload.len(), 0);

    server_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();
}

#[tokio::test]
async fn test_channel_overflow() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, _server_incoming_rx) = mpsc::channel(1);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await;
    });

    sleep(Duration::from_millis(200)).await;

    let url = format!("ws://{}", bound_addr);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    use futures::SinkExt;

    for i in 0..10 {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100,
            i,
            EnvelopeFlags::RELIABLE,
            Bytes::from(format!("msg {}", i)),
        );

        let _ = ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Binary(
                envelope.to_bytes().to_vec(),
            ))
            .await;
    }

    sleep(Duration::from_millis(500)).await;

    server_handle.abort();
}

#[tokio::test]
async fn test_invalid_envelope_header() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(100);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await;
    });

    sleep(Duration::from_millis(200)).await;

    let url = format!("ws://{}", bound_addr);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    use futures::SinkExt;

    let short_data = vec![1, 2, 3, 4, 5, 6, 7, 8];

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            short_data,
        ))
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    let valid_envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"recovery test"),
    );

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            valid_envelope.to_bytes().to_vec(),
        ))
        .await
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(1), server_incoming_rx.recv()).await;

    assert!(result.is_ok());
    let received = result.unwrap();
    assert!(received.is_some());
    assert_eq!(received.unwrap().envelope.route_id, 100);

    server_handle.abort();
}

#[tokio::test]
async fn test_websocket_text_message_ignored() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, mut server_incoming_rx) = mpsc::channel(100);
    let (_server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await;
    });

    sleep(Duration::from_millis(200)).await;

    let url = format!("ws://{}", bound_addr);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    use futures::SinkExt;

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "this should be ignored".to_string(),
        ))
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;

    let valid_envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"binary message"),
    );

    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            valid_envelope.to_bytes().to_vec(),
        ))
        .await
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(1), server_incoming_rx.recv()).await;

    assert!(result.is_ok());
    let received = result.unwrap();
    assert!(received.is_some());
    assert_eq!(received.unwrap().envelope.route_id, 100);

    server_handle.abort();
}
