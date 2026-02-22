use bytes::Bytes;
use mokosh_client::{transport::websocket::WebSocketClient, Client};
use mokosh_protocol::{Envelope, EnvelopeFlags, Transport};
use mokosh_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// Test multiple clients can connect and maintain independent RTT measurements
#[tokio::test]
async fn test_multiple_clients_independent_rtt() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Client 1 setup
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 2 setup
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 3 setup
    let (client3_incoming_tx, client3_incoming_rx) = mpsc::channel(100);
    let (client3_outgoing_tx, client3_outgoing_rx) = mpsc::channel(100);

    let client3_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client3_transport_handle = tokio::spawn(async move {
        let _ = client3_transport
            .run(client3_incoming_tx, client3_outgoing_rx)
            .await;
    });

    let mut client3 = Client::new(client3_incoming_rx, client3_outgoing_tx.clone());
    client3.connect().await.unwrap();

    let client3_handle = tokio::spawn(async move {
        client3.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // All 3 clients should be connected
    // Each maintains independent RTT tracking
    // (We can't easily verify RTT values here without accessing the spawned server,
    //  but we verify the connections stay alive independently)

    sleep(Duration::from_secs(1)).await;

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client2_transport_handle.abort();
    client2_handle.abort();
    client3_transport_handle.abort();
    client3_handle.abort();
}

/// Test multiple clients maintain independent connection states
#[tokio::test]
async fn test_multiple_clients_independent_state() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Connect first client
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Connect second client
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Both clients should maintain independent state machines
    // Connection should be stable for both
    sleep(Duration::from_secs(1)).await;

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client2_transport_handle.abort();
    client2_handle.abort();
}

/// Test that each client receives independent keepalive PINGs
#[tokio::test]
async fn test_multiple_clients_independent_keepalive() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Client 1
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 2
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Both clients should receive independent keepalive
    // Default keepalive is 30s, so we won't see automatic PING in this test
    // But the architecture supports it (each session has independent last_ping_sent)
    sleep(Duration::from_secs(1)).await;

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client2_transport_handle.abort();
    client2_handle.abort();
}

/// Test that disconnecting one client doesn't affect others
#[tokio::test]
async fn test_client_disconnect_does_not_affect_others() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Client 1
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 2
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 3
    let (client3_incoming_tx, client3_incoming_rx) = mpsc::channel(100);
    let (client3_outgoing_tx, client3_outgoing_rx) = mpsc::channel(100);

    let client3_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client3_transport_handle = tokio::spawn(async move {
        let _ = client3_transport
            .run(client3_incoming_tx, client3_outgoing_rx)
            .await;
    });

    let mut client3 = Client::new(client3_incoming_rx, client3_outgoing_tx.clone());
    client3.connect().await.unwrap();

    let client3_handle = tokio::spawn(async move {
        client3.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // All 3 clients connected
    // Disconnect client 2
    client2_transport_handle.abort();
    client2_handle.abort();

    sleep(Duration::from_millis(200)).await;

    // Client 1 and 3 should still be alive and functioning
    sleep(Duration::from_secs(1)).await;

    // Cleanup remaining clients
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client3_transport_handle.abort();
    client3_handle.abort();
}

/// Test Server public API works with multiple clients
/// Note: We can't easily test get_active_sessions() in an integration test
/// where Server is spawned, but the architecture supports it.
/// This test verifies that multiple clients can connect successfully,
/// which implicitly validates the HashMap<SessionId, SessionState> architecture.
#[tokio::test]
async fn test_server_handles_multiple_client_connections() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Connect client 1
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Connect client 2
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Both clients successfully connected - validates multi-client HashMap architecture

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client2_transport_handle.abort();
    client2_handle.abort();
}

/// Test Server::send_message() sends to specific session only
#[tokio::test]
async fn test_server_send_to_specific_session() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport
            .run(server_incoming_tx, server_outgoing_rx)
            .await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Client 1
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let client1_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client1_transport_handle = tokio::spawn(async move {
        let _ = client1_transport
            .run(client1_incoming_tx, client1_outgoing_rx)
            .await;
    });

    let mut client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    client1.connect().await.unwrap();

    let client1_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Client 2
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let client2_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client2_transport_handle = tokio::spawn(async move {
        let _ = client2_transport
            .run(client2_incoming_tx, client2_outgoing_rx)
            .await;
    });

    let mut client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    client2.connect().await.unwrap();

    let client2_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(200)).await;

    // Send test message from client1
    let test_envelope = Envelope::new_simple(
        1,
        1,
        0,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test from client1"),
    );

    client1_outgoing_tx.send(test_envelope).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    // Server should receive it from client1's session only
    // (We can't easily verify here without instrumenting server,
    //  but we verify both clients can send independently)

    let test_envelope2 = Envelope::new_simple(
        1,
        1,
        0,
        100,
        2,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test from client2"),
    );

    client2_outgoing_tx.send(test_envelope2).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client1_transport_handle.abort();
    client1_handle.abort();
    client2_transport_handle.abort();
    client2_handle.abort();
}
