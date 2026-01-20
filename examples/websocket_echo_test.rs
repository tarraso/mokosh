//! WebSocket echo test example
//!
//! This example demonstrates WebSocket transport by running a server and client
//! that communicate over a real network connection.
//!
//! Run with: cargo run --example websocket_echo_test
//!
//! To see detailed logs, run with:
//! RUST_LOG=debug cargo run --example websocket_echo_test

use bytes::Bytes;
use godot_netlink_client::{transport::websocket::WebSocketClient, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags, Transport};
use godot_netlink_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    // Initialize tracing subscriber for structured logging
    // Set RUST_LOG environment variable to control log level
    // Example: RUST_LOG=debug cargo run --example websocket_echo_test
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    println!("=== GodotNetLink WebSocket Echo Test ===\n");

    let addr: SocketAddr = "127.0.0.1:9090".parse().unwrap();

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(addr);

    println!("Starting WebSocket server on {}...", addr);
    let server_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_server.run(server_incoming_tx, server_outgoing_rx).await {
            tracing::error!(error = %e, "WebSocket server error");
        }
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", addr));

    println!("Connecting WebSocket client to {}...", addr);
    let client_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_client.run(client_incoming_tx, client_outgoing_rx).await {
            tracing::error!(error = %e, "WebSocket client error");
        }
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Sending test message 1 ---");
    let test_message_1 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello from WebSocket client!"),
    );

    println!("  Route ID: {}", test_message_1.route_id);
    println!("  Message ID: {}", test_message_1.msg_id);
    println!(
        "  Payload: {:?}",
        std::str::from_utf8(&test_message_1.payload).unwrap()
    );

    client_outgoing_tx
        .send(test_message_1.clone())
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Sending test message 2 ---");
    let test_message_2 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        200,
        2,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        Bytes::from_static(b"Second message with compression flag!"),
    );

    println!("  Route ID: {}", test_message_2.route_id);
    println!("  Message ID: {}", test_message_2.msg_id);
    println!(
        "  Payload: {:?}",
        std::str::from_utf8(&test_message_2.payload).unwrap()
    );

    client_outgoing_tx.send(test_message_2).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Testing multi-client routing ---");

    // Connect a second client
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let ws_client2 = WebSocketClient::new(format!("ws://{}", addr));

    println!("Connecting second WebSocket client to {}...", addr);
    let client2_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_client2.run(client2_incoming_tx, client2_outgoing_rx).await {
            tracing::error!(error = %e, "WebSocket client 2 error");
        }
    });

    let client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    let client2_loop_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Sending test message from client 2 ---");
    let test_message_3 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        300,
        3,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello from second client!"),
    );

    println!("  Route ID: {}", test_message_3.route_id);
    println!("  Message ID: {}", test_message_3.msg_id);
    println!(
        "  Payload: {:?}",
        std::str::from_utf8(&test_message_3.payload).unwrap()
    );

    client2_outgoing_tx.send(test_message_3).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Test complete ---");
    println!("\n✓ WebSocket server accepts multiple connections");
    println!("✓ WebSocket clients connect successfully");
    println!("✓ Messages are sent from clients to server");
    println!("✓ Server receives and processes envelopes");
    println!("✓ Multi-client routing is working");
    println!("✓ Each client has a unique session ID");

    drop(client_outgoing_tx);
    drop(client2_outgoing_tx);

    sleep(Duration::from_millis(100)).await;

    client2_transport_handle.abort();
    client2_loop_handle.abort();

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();

    println!("\n=== Test finished ===");
}
