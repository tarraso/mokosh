use bytes::Bytes;
use godot_netlink_client::{transport::websocket::WebSocketClient, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags, Transport};
use godot_netlink_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_full_client_server_communication() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_transport_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

    let client_transport_handle = tokio::spawn(async move {
        let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let test_envelope = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"integration test message"),
    );

    client_outgoing_tx
        .send(test_envelope.clone())
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();
}

#[tokio::test]
async fn test_multiple_messages_in_sequence() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_transport_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

    let client_transport_handle = tokio::spawn(async move {
        let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    for i in 1u64..=10 {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            (100 + i) as u16,
            i,
            EnvelopeFlags::RELIABLE,
            Bytes::from(format!("message {}", i)),
        );

        client_outgoing_tx.send(envelope).await.unwrap();
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(500)).await;

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();
}

#[tokio::test]
async fn test_large_payload() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_transport_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

    let client_transport_handle = tokio::spawn(async move {
        let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let large_payload = vec![0xAB; 64 * 1024];
    let envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        Bytes::from(large_payload),
    );

    client_outgoing_tx.send(envelope).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();
}

#[tokio::test]
async fn test_different_envelope_flags() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(bound_addr);

    let server_transport_handle = tokio::spawn(async move {
        let _ = ws_server.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", bound_addr));

    let client_transport_handle = tokio::spawn(async move {
        let _ = ws_client.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    let test_cases = vec![
        EnvelopeFlags::RELIABLE,
        EnvelopeFlags::ENCRYPTED,
        EnvelopeFlags::COMPRESSED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::ENCRYPTED | EnvelopeFlags::COMPRESSED,
    ];

    for (i, flags) in test_cases.iter().enumerate() {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            100 + i as u16,
            (i + 1) as u64,
            *flags,
            Bytes::from(format!("flags test {}", i)),
        );

        client_outgoing_tx.send(envelope).await.unwrap();
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(500)).await;

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();
}
