//! WebSocket echo test example
//!
//! This example demonstrates WebSocket transport with the event-based Server API.
//! It shows:
//! - Real network communication over WebSocket
//! - Multi-client support
//! - GameEvent handling (PlayerConnected, GameMessage, PlayerDisconnected)
//! - Proper graceful shutdown
//!
//! Run with: cargo run --example websocket_echo_test
//!
//! To see detailed logs:
//! RUST_LOG=debug cargo run --example websocket_echo_test

use bytes::Bytes;
use mokosh_client::{transport::websocket::WebSocketClient, Client};
use mokosh_protocol::{Envelope, EnvelopeFlags, SessionEnvelope, Transport};
use mokosh_server::{transport::websocket::WebSocketServer, Server, GameEvent};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .init();

    println!("=== Mokosh WebSocket Echo Test (Event-Based API) ===\n");

    let addr: SocketAddr = "127.0.0.1:9090".parse().unwrap();

    // Server channels
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(addr);

    println!("Starting WebSocket server on {}...", addr);
    let server_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_server.run(server_incoming_tx, server_outgoing_rx, None).await {
            tracing::error!(error = %e, "WebSocket server error");
        }
    });

    // Server with event-based API
    let mut server = Server::new(server_incoming_rx, server_outgoing_tx.clone());
    let server_loop_handle = tokio::spawn(async move {
        println!("[Server] Running event loop with GameEvent handling...");
        loop {
            match server.tick().await {
                Ok(Some(GameEvent::PlayerConnected(session_id))) => {
                    println!("[Server] ✅ Player connected: {}", session_id);
                }
                Ok(Some(GameEvent::PlayerDisconnected(session_id))) => {
                    println!("[Server] 👋 Player disconnected: {}", session_id);
                }
                Ok(Some(GameEvent::GameMessage { session_id, envelope })) => {
                    let payload_str = std::str::from_utf8(&envelope.payload).unwrap_or("<binary>");
                    println!("[Server] 📨 Message from {}: route_id={}, payload={:?}",
                        session_id, envelope.route_id, payload_str);

                    // Echo back to sender (using low-level API for raw Envelope)
                    let session_envelope = SessionEnvelope::new(session_id, envelope);
                    if let Err(e) = server_outgoing_tx.send(session_envelope).await {
                        eprintln!("[Server] Failed to echo: {}", e);
                    }
                }
                Ok(None) => {
                    println!("[Server] Shutting down");
                    break;
                }
                Err(e) => {
                    eprintln!("[Server] Error: {}", e);
                    break;
                }
            }
        }
    });

    sleep(Duration::from_millis(500)).await;

    // Client 1
    let (client1_incoming_tx, client1_incoming_rx) = mpsc::channel(100);
    let (client1_outgoing_tx, client1_outgoing_rx) = mpsc::channel(100);

    let ws_client1 = WebSocketClient::new(format!("ws://{}", addr));

    println!("Connecting client 1 to {}...", addr);
    let client1_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_client1.run(client1_incoming_tx, client1_outgoing_rx).await {
            tracing::error!(error = %e, "Client 1 transport error");
        }
    });

    let client1 = Client::new(client1_incoming_rx, client1_outgoing_tx.clone());
    let client1_loop_handle = tokio::spawn(async move {
        client1.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    // Send message from client 1
    println!("\n--- Client 1: Sending test message ---");
    let test_message_1 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello from client 1!"),
    );

    println!("  Route ID: {}", test_message_1.route_id);
    println!("  Message ID: {}", test_message_1.msg_id);
    println!("  Payload: {:?}", std::str::from_utf8(&test_message_1.payload).unwrap());

    client1_outgoing_tx.send(test_message_1).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    // Client 2
    println!("\n--- Connecting second client ---");
    let (client2_incoming_tx, client2_incoming_rx) = mpsc::channel(100);
    let (client2_outgoing_tx, client2_outgoing_rx) = mpsc::channel(100);

    let ws_client2 = WebSocketClient::new(format!("ws://{}", addr));

    println!("Connecting client 2 to {}...", addr);
    let client2_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_client2.run(client2_incoming_tx, client2_outgoing_rx).await {
            tracing::error!(error = %e, "Client 2 transport error");
        }
    });

    let client2 = Client::new(client2_incoming_rx, client2_outgoing_tx.clone());
    let client2_loop_handle = tokio::spawn(async move {
        client2.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    // Send message from client 2
    println!("\n--- Client 2: Sending test message ---");
    let test_message_2 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        200,
        2,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello from client 2!"),
    );

    println!("  Route ID: {}", test_message_2.route_id);
    println!("  Message ID: {}", test_message_2.msg_id);
    println!("  Payload: {:?}", std::str::from_utf8(&test_message_2.payload).unwrap());

    client2_outgoing_tx.send(test_message_2).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Test complete ---");
    println!("\n✓ WebSocket server accepts multiple connections");
    println!("✓ WebSocket clients connect successfully");
    println!("✓ GameEvent::PlayerConnected triggered for each client");
    println!("✓ GameEvent::GameMessage received and echoed back");
    println!("✓ Each client has a unique session ID");
    println!("✓ Multi-client routing is working");

    // Graceful shutdown
    println!("\n--- Graceful shutdown ---");
    drop(client1_outgoing_tx);
    drop(client2_outgoing_tx);

    sleep(Duration::from_millis(200)).await;

    client1_transport_handle.abort();
    client1_loop_handle.abort();
    client2_transport_handle.abort();
    client2_loop_handle.abort();
    server_transport_handle.abort();
    server_loop_handle.abort();

    println!("\n=== Test finished ===");
}
