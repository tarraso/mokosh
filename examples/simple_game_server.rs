//! Simple multiplayer game server
//!
//! Runs a WebSocket server that:
//! - Accepts multiple clients
//! - Tracks player positions
//! - Broadcasts world state to all clients

use mokosh_server::{Server, ServerConfig};
use mokosh_server::transport::websocket_server::WebSocketServer;
use mokosh_protocol::{CodecType, SessionId};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlayerPosition {
    session_id: u64,
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorldState {
    players: Vec<PlayerPosition>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎮 Starting Simple Game Server on port 8080...");

    // Create channels for server communication
    let (incoming_tx, incoming_rx) = mpsc::channel(100);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Start WebSocket server
    let ws_server = WebSocketServer::new("127.0.0.1:8080").await?;
    let ws_handle = tokio::spawn(async move {
        ws_server.run(incoming_tx, outgoing_rx).await
    });

    // Create server
    let config = ServerConfig::default();
    let mut server = Server::new(
        incoming_rx,
        outgoing_tx,
        CodecType::from_id(1).unwrap(), // JSON for control
        CodecType::from_id(1).unwrap(), // JSON for game messages
        config,
    );

    // Track player positions
    let mut player_positions: HashMap<SessionId, (f32, f32)> = HashMap::new();

    println!("✅ Server running on ws://127.0.0.1:8080");
    println!("📊 Waiting for players to connect...\n");

    // Main game loop
    loop {
        tokio::time::sleep(Duration::from_millis(16)).await; // ~60 FPS

        // Process one server tick
        if let Err(e) = server.tick().await {
            eprintln!("Server tick error: {}", e);
            continue;
        }

        // Check for new clients
        let sessions = server.get_active_sessions();
        for session_id in &sessions {
            if !player_positions.contains_key(session_id) {
                player_positions.insert(*session_id, (400.0, 300.0));
                println!("👤 Player {} joined (total: {})", session_id, sessions.len());
            }
        }

        // Clean up disconnected players
        player_positions.retain(|id, _| sessions.contains(id));

        // Broadcast world state to all clients
        if !player_positions.is_empty() {
            let players: Vec<PlayerPosition> = player_positions
                .iter()
                .map(|(id, (x, y))| PlayerPosition {
                    session_id: *id,
                    x: *x,
                    y: *y,
                })
                .collect();

            let world_state = WorldState { players };
            let json = serde_json::to_string(&world_state).unwrap();

            // Broadcast to all connected clients
            for session_id in &sessions {
                // We'll send as raw text for now
                // In production, use proper message protocol
                let _ = server.send_raw(*session_id, json.clone().into_bytes());
            }
        }
    }

    ws_handle.await??;
    Ok(())
}
