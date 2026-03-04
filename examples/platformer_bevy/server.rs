//! 2D Platformer Server with Physics
//!
//! This example demonstrates a multiplayer 2D platformer with:
//! - Server-authoritative physics simulation
//! - Gravity, jumping, and movement
//! - Pushable boxes that can be stacked
//! - Players can jump on boxes
//! - Client-side prediction support
//!
//! Run:
//! ```bash
//! cargo run --example platformer_server
//! ```

mod simulation;

use mokosh_server::transport::websocket::WebSocketServer;
use mokosh_server::{GameEvent, Server};
use mokosh_simulation::Simulation;
use simulation::{PlatformerSimulation, PlayerInput};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;

// ============================================================================
// Server Main Loop
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🎮 Starting 2D Platformer Server on port 8080...");

    // Create channels for server communication
    let (incoming_tx, incoming_rx) = mpsc::channel(100);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Create ready signal for transport
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Start WebSocket transport
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let transport = WebSocketServer::new(addr);
    tokio::spawn(async move {
        if let Err(e) = transport
            .run(incoming_tx, outgoing_rx, Some(ready_tx))
            .await
        {
            eprintln!("❌ Transport error: {}", e);
        }
    });

    // Wait for transport to be ready
    ready_rx.await.expect("Transport failed to start");

    // Create server with new event-based API
    let mut server = Server::new(incoming_rx, outgoing_tx);

    // Create platformer simulation
    let mut platformer_sim = PlatformerSimulation::new();

    println!("✅ Server running on ws://127.0.0.1:8080");
    println!("📊 Initial boxes spawned: {}", platformer_sim.boxes.len());
    println!("🎮 Waiting for players to connect...\n");

    // Physics tick interval (~60 FPS)
    let mut physics_interval = tokio::time::interval(Duration::from_millis(16));

    // Main game loop
    loop {
        tokio::select! {
            // Physics simulation at 60 FPS
            _ = physics_interval.tick() => {
                // Step physics
                platformer_sim.step_physics(0.016);

                // Broadcast game state to all connected clients
                if server.client_count() > 0 {
                    let snapshot = platformer_sim.snapshot();
                    if let Err(e) = server.broadcast(snapshot).await {
                        eprintln!("Failed to broadcast snapshot: {}", e);
                    }
                }
            }

            // Process server events (connections, disconnections, game messages)
            result = server.tick() => {
                if let Some(event) = result? {
                    match event {
                        GameEvent::PlayerConnected(session_id) => {
                            platformer_sim.add_player(session_id);
                            println!("👤 Player {} joined (total: {})",
                                session_id, server.client_count());
                        }

                        GameEvent::PlayerDisconnected(session_id) => {
                            platformer_sim.remove_player(session_id);
                            println!("👋 Player {} left (total: {})",
                                session_id, server.client_count());
                        }

                        GameEvent::GameMessage { session_id, envelope } => {
                            // Check if this is a PlayerInput message (route_id = 100)
                            if envelope.route_id == 100 {
                                // Decode PlayerInput from JSON
                                if let Ok(input) = serde_json::from_slice::<PlayerInput>(&envelope.payload) {
                                    platformer_sim.apply_input_to_player(session_id, &input);
                                } else {
                                    eprintln!("Failed to decode PlayerInput from {}", session_id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
