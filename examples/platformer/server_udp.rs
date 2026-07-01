//! 2D Platformer Server over **UDP** with the reliability layer enabled.
//!
//! Same game logic as `platformer_server`, but the transport is `UdpServer`
//! (unreliable datagrams) with Mokosh's reliability layer turned on. The HELLO
//! handshake and reliable game messages survive packet loss; unreliable inputs
//! are delivered best-effort.
//!
//! Run (two terminals):
//! ```bash
//! cargo run --example platformer_server_udp
//! cargo run --example platformer_client_udp
//! ```

use mokosh_examples_shared::platformer::{PlatformerSimulation, PlayerInput, Simulation};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::{CodecType, ReliabilityConfig, ReliabilityMode};
use mokosh_server::transport::udp::UdpServer;
use mokosh_server::transport::ReliableServerLink;
use mokosh_server::{GameEvent, Server, ServerConfig};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🎮 Starting 2D Platformer Server (UDP) on port 8080...");

    // Channels between the UDP transport and the reliability decorator.
    let (t_in_tx, t_in_rx) = mpsc::channel(100);
    let (t_out_tx, t_out_rx) = mpsc::channel(100);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    // Start the UDP transport.
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let transport = UdpServer::new(addr);
    tokio::spawn(async move {
        if let Err(e) = transport.run(t_in_tx, t_out_rx, Some(ready_tx)).await {
            eprintln!("❌ Transport error: {}", e);
        }
    });
    ready_rx.await.expect("Transport failed to start");

    // Reliability decorator between the transport and the Server (the event loop
    // is reliability-agnostic; the link adds ACK/retransmit/ordering per session).
    let (incoming_rx, outgoing_tx) =
        ReliableServerLink::new(ReliabilityConfig::default()).spawn(t_in_rx, t_out_tx);

    // Server with the reliability layer enabled (required for UDP). The handshake
    // negotiates reliability, so the client must also enable it.
    let config = ServerConfig {
        reliability: Some(ReliabilityConfig::default()),
        ..Default::default()
    };
    let mut server = Server::with_full_config(
        incoming_rx,
        outgoing_tx,
        CodecType::from_id(1).unwrap(), // JSON control codec
        CodecType::from_id(1).unwrap(), // JSON game codec
        config,
        None, // no message registry
        None, // no auth provider
        NoCompressor,
        NoEncryptor,
    );

    let mut platformer_sim = PlatformerSimulation::new();

    println!("✅ Server running on udp://127.0.0.1:8080 (reliability ON)");
    println!("📊 Initial boxes spawned: {}", platformer_sim.boxes.len());
    println!("🎮 Waiting for players to connect...\n");

    // Physics tick (~60 FPS).
    let mut physics_interval = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
            _ = physics_interval.tick() => {
                platformer_sim.step_physics(0.016);

                // Snapshots are high-rate latest-wins state → send UNRELIABLE
                // (sequenced: stale ones dropped). Reliable-ordered at 60/s would
                // saturate the ACK window and drop clients (Overloaded). The
                // reliable path is still exercised by the HELLO/AUTH handshake.
                if server.client_count() > 0 {
                    let snapshot = platformer_sim.snapshot();
                    if let Err(e) = server
                        .broadcast_with(snapshot, ReliabilityMode::UnreliableSequenced, Duration::from_secs(1))
                        .await
                    {
                        eprintln!("Failed to broadcast snapshot: {}", e);
                    }
                }
            }

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
                            if envelope.route_id == 100 {
                                if let Ok(input) = serde_json::from_slice::<PlayerInput>(&envelope.payload) {
                                    platformer_sim.apply_input_to_player(session_id, &input);
                                } else {
                                    eprintln!("Failed to decode PlayerInput from {}", session_id);
                                }
                            }
                        }

                        GameEvent::MessageDropped { session_id, seq, route_id } => {
                            eprintln!("⚠️  Dropped reliable msg to {} (seq={}, route={})",
                                session_id, seq, route_id);
                        }
                    }
                }
            }
        }
    }
}
