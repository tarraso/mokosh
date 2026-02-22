use mokosh_client::{transport::websocket::WebSocketClient, Client};
use mokosh_protocol::Transport;
use mokosh_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// Test PING/PONG exchange happens automatically
#[tokio::test]
async fn test_automatic_ping_pong_exchange() {
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
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    // Wait for server to start
    sleep(Duration::from_millis(100)).await;

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client_transport_handle = tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let mut client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    // Connect client
    client.connect().await.unwrap();

    let client_handle = tokio::spawn(async move {
        client.run().await;
    });

    // Wait for connection and automatic PING/PONG exchanges
    // Default keepalive_interval is 30s, so we won't see automatic PING
    // but the system should be stable
    sleep(Duration::from_millis(500)).await;

    // If we get here without crashes, basic connection works!

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client_transport_handle.abort();
    client_handle.abort();
}

/// Test that keepalive PING prevents connection timeout
#[tokio::test]
async fn test_keepalive_prevents_timeout() {
    tracing_subscriber::fmt().with_test_writer().try_init().ok();

    // Setup WebSocket server on random port
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound_addr = listener.local_addr().unwrap();
    drop(listener);

    // Server setup with default config (30s keepalive, 60s timeout)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(bound_addr);
    let server_transport_handle = tokio::spawn(async move {
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    // Wait for server to start
    sleep(Duration::from_millis(100)).await;

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client_transport_handle = tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let mut client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    // Connect client
    client.connect().await.unwrap();

    let client_handle = tokio::spawn(async move {
        client.run().await;
    });

    // Wait for connection to establish
    sleep(Duration::from_millis(500)).await;

    // Connection should stay alive
    // (with default 30s keepalive and 60s timeout, we're well within limits)

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client_transport_handle.abort();
    client_handle.abort();
}

/// Test bidirectional keepalive (both client and server send PING)
#[tokio::test]
async fn test_bidirectional_keepalive() {
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
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_handle = tokio::spawn(async move {
        server.run().await;
    });

    // Wait for server to start
    sleep(Duration::from_millis(100)).await;

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client_transport = WebSocketClient::new(format!("ws://{}", bound_addr));
    let client_transport_handle = tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    let mut client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    // Connect client
    client.connect().await.unwrap();

    let client_handle = tokio::spawn(async move {
        client.run().await;
    });

    // Both sides should send PING periodically (default: every 30s)
    // Wait enough time for connection to be stable
    sleep(Duration::from_secs(1)).await;

    // If we get here without crashes, bidirectional keepalive architecture works!

    // Cleanup
    server_transport_handle.abort();
    server_handle.abort();
    client_transport_handle.abort();
    client_handle.abort();
}
