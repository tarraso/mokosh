//! Simple test client for manual testing
//!
//! This client is designed for quick manual testing with game servers
//! that use the event-based API (`tick()` + `GameEvent`).
//!
//! Usage:
//! ```bash
//! # Terminal 1: Start a game server
//! cargo run --example simple_game_server
//!
//! # Terminal 2-N: Connect multiple test clients
//! cargo run --example test_client 1
//! cargo run --example test_client 2
//! cargo run --example test_client 3
//! ```
//!
//! The client will:
//! - Connect to ws://127.0.0.1:8080
//! - Send 5 test messages (player input)
//! - Display any messages received from the server
//! - Disconnect gracefully

use mokosh_client::{Client, transport::websocket::WebSocketClient};
use mokosh_protocol::Transport;
use tokio::sync::mpsc;
use std::time::Duration;
use serde_json::json;

#[tokio::main]
async fn main() {
    let client_id = std::env::args().nth(1).unwrap_or_else(|| "1".to_string());

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🎮 Test Client {} starting...", client_id);
    println!("📡 Connecting to ws://127.0.0.1:8080...\n");

    // Create channels for transport
    let (incoming_tx, incoming_rx) = mpsc::channel(100);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Create channel for game messages
    let (game_messages_tx, mut game_messages_rx) = mpsc::channel(100);

    // Create client with game messages channel
    let client = Client::with_game_messages(
        incoming_rx,
        outgoing_tx.clone(),
        Some(game_messages_tx),
    );

    // Spawn WebSocket transport
    tokio::spawn(async move {
        let transport = WebSocketClient::new("ws://127.0.0.1:8080");
        if let Err(e) = transport.run(incoming_tx, outgoing_rx).await {
            eprintln!("Transport error: {}", e);
        }
    });

    // Connect client
    let mut client = client;
    match client.connect().await {
        Ok(_) => println!("✅ Client {} connected successfully\n", client_id),
        Err(e) => {
            eprintln!("❌ Connection failed: {}", e);
            return;
        }
    }

    // Spawn client event loop
    tokio::spawn(async move {
        client.run().await;
    });

    // Spawn message receiver to display server responses
    let client_id_clone = client_id.clone();
    tokio::spawn(async move {
        while let Some(envelope) = game_messages_rx.recv().await {
            // Try to parse as JSON for better display
            if let Ok(json_str) = std::str::from_utf8(&envelope.payload) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                    println!("📨 Client {} received: {} (route_id={})",
                        client_id_clone, json, envelope.route_id);
                } else {
                    println!("📨 Client {} received: {} (route_id={})",
                        client_id_clone, json_str, envelope.route_id);
                }
            } else {
                println!("📨 Client {} received binary message (route_id={}, size={})",
                    client_id_clone, envelope.route_id, envelope.payload.len());
            }
        }
    });

    // Send periodic messages to test server
    println!("📤 Sending test messages...\n");
    let mut interval = tokio::time::interval(Duration::from_secs(2));

    for i in 1..=5 {
        interval.tick().await;

        let msg = json!({
            "type": "player_input",
            "client_id": client_id,
            "x": i * 10,
            "y": i * 20,
            "timestamp": i,
        });

        println!("📤 Client {} sending message #{}: x={}, y={}",
            client_id, i, i * 10, i * 20);

        // Send as game message (route_id = 100)
        let payload = bytes::Bytes::from(msg.to_string().as_bytes().to_vec());
        let envelope = mokosh_protocol::Envelope::new_simple(
            mokosh_protocol::CURRENT_PROTOCOL_VERSION,
            1, // JSON codec
            0, // schema_hash
            100, // route_id for game messages
            i, // msg_id
            mokosh_protocol::EnvelopeFlags::RELIABLE,
            payload,
        );

        if let Err(e) = outgoing_tx.send(envelope).await {
            eprintln!("Failed to send: {}", e);
            break;
        }
    }

    println!("\n✅ Client {} finished sending messages", client_id);
    println!("⏳ Waiting 3 seconds for any remaining server messages...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    println!("👋 Client {} disconnecting", client_id);
}
