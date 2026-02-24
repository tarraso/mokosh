//! Simple multiplayer game server
//!
//! Demonstrates the event-based Server API:
//! - Uses `tick()` to receive GameEvents (PlayerConnected, PlayerDisconnected, GameMessage)
//! - Tracks player positions
//! - Broadcasts world state to all clients at 20 Hz
//!
//! This is a minimal example showing the recommended pattern for game servers.
//! For a more complex example with physics, see `platformer_server.rs`.

use mokosh_server::{Server, GameEvent};
use mokosh_server::transport::websocket::WebSocketServer;
use mokosh_protocol::SessionId;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};
use mokosh_protocol_derive::GameMessage;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlayerPosition {
    session_id: String, // UUID
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 200]
struct WorldState {
    players: Vec<PlayerPosition>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🎮 Starting Simple Game Server on port 8080...");

    // Create channels for server communication
    let (incoming_tx, incoming_rx) = mpsc::channel(100);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Create ready signal for transport
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Start WebSocket server
    let addr = "127.0.0.1:8080".parse().unwrap();
    let ws_server = WebSocketServer::new(addr);
    tokio::spawn(async move {
        if let Err(e) = ws_server.run(incoming_tx, outgoing_rx, Some(ready_tx)).await {
            eprintln!("❌ WebSocket error: {}", e);
        }
    });

    // Wait for transport to be ready
    ready_rx.await.expect("Transport failed to start");

    // Create server with event-based API
    let mut server = Server::new(incoming_rx, outgoing_tx);

    // Track player positions
    let mut player_positions: HashMap<SessionId, (f32, f32)> = HashMap::new();

    println!("✅ Server running on ws://127.0.0.1:8080");
    println!("📊 Waiting for players to connect...\n");

    // Broadcast interval (20 Hz to save bandwidth)
    let mut broadcast_interval = tokio::time::interval(Duration::from_millis(50));

    // Main game loop
    loop {
        tokio::select! {
            // Broadcast world state at 20 Hz
            _ = broadcast_interval.tick() => {
                if !player_positions.is_empty() {
                    let players: Vec<PlayerPosition> = player_positions
                        .iter()
                        .map(|(id, (x, y))| PlayerPosition {
                            session_id: id.to_string(),
                            x: *x,
                            y: *y,
                        })
                        .collect();

                    let world_state = WorldState { players };

                    // Use broadcast() helper method
                    if let Err(e) = server.broadcast(world_state).await {
                        eprintln!("Failed to broadcast world state: {}", e);
                    }
                }
            }

            // Process server events (connections, disconnections, messages)
            result = server.tick() => {
                match result? {
                    Some(GameEvent::PlayerConnected(session_id)) => {
                        // Spawn player at default position
                        player_positions.insert(session_id, (400.0, 300.0));
                        println!("👤 Player {} joined (total: {})",
                            session_id, player_positions.len());
                    }

                    Some(GameEvent::PlayerDisconnected(session_id)) => {
                        // Remove player
                        player_positions.remove(&session_id);
                        println!("👋 Player {} left (total: {})",
                            session_id, player_positions.len());
                    }

                    Some(GameEvent::GameMessage { session_id, envelope }) => {
                        // Handle player position updates (route_id = 100)
                        if envelope.route_id == 100 {
                            // Decode JSON position message from client
                            if let Ok(json_str) = String::from_utf8(envelope.payload.to_vec()) {
                                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
                                    if let (Some(x), Some(y)) = (data.get("x").and_then(|v| v.as_f64()),
                                                                   data.get("y").and_then(|v| v.as_f64())) {
                                        // Update player position
                                        player_positions.insert(session_id, (x as f32, y as f32));
                                    }
                                }
                            }
                        }
                    }

                    None => {
                        println!("Server shutting down");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
