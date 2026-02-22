//! Simple test client using mokosh Client

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

    println!("🎮 Client {} starting...", client_id);

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
        Ok(_) => println!("✅ Client {} connected", client_id),
        Err(e) => {
            eprintln!("❌ Connection failed: {}", e);
            return;
        }
    }

    // Spawn client event loop
    tokio::spawn(async move {
        client.run().await;
    });

    // Spawn message receiver
    let client_id_clone = client_id.clone();
    tokio::spawn(async move {
        while let Some(envelope) = game_messages_rx.recv().await {
            println!("📨 Client {} received message (route={}, size={})",
                client_id_clone, envelope.route_id, envelope.payload.len());
        }
    });

    // Send periodic messages
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

        println!("📤 Client {} sending message #{}", client_id, i);
        
        // Send raw JSON as bytes
        let payload = bytes::Bytes::from(msg.to_string().as_bytes().to_vec());
        let envelope = mokosh_protocol::Envelope::new_simple(
            mokosh_protocol::CURRENT_PROTOCOL_VERSION,
            1, // JSON codec
            0, // schema_hash
            100, // route_id
            i, // msg_id
            mokosh_protocol::EnvelopeFlags::RELIABLE,
            payload,
        );

        if let Err(e) = outgoing_tx.send(envelope).await {
            eprintln!("Failed to send: {}", e);
            break;
        }
    }

    println!("✅ Client {} finished sending, waiting 2s...", client_id);
    tokio::time::sleep(Duration::from_secs(2)).await;
    println!("👋 Client {} exiting", client_id);
}
