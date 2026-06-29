//! 2D Platformer CLI client over **UDP** with the reliability layer enabled.
//!
//! Connects to `platformer_server_udp`, sends `PlayerInput` (unreliable, the
//! idiomatic choice for inputs) and prints the `GameState` snapshots the server
//! broadcasts (reliable-ordered over UDP). Runs for a few seconds, then exits.
//!
//! Run (two terminals):
//! ```bash
//! cargo run --example platformer_server_udp
//! cargo run --example platformer_client_udp
//! ```

use bytes::Bytes;
use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::Transport;
use mokosh_client::{Client, ClientConfig};
use mokosh_examples_shared::platformer::{GameState, PlayerInput};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::{CodecType, Envelope, EnvelopeFlags, GameMessage, ReliabilityConfig, CURRENT_PROTOCOL_VERSION};
use std::time::Duration;
use tokio::sync::mpsc;

const SERVER_ADDR: &str = "127.0.0.1:8080";
const DEMO_SECS: u64 = 5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    println!("🎮 Platformer client (UDP) connecting to {SERVER_ADDR}...");

    // Transport <-> client channels, plus a channel for delivered game messages.
    let (to_transport_tx, to_transport_rx) = mpsc::channel::<Envelope>(100);
    let (from_transport_tx, from_transport_rx) = mpsc::channel::<Envelope>(100);
    let (game_tx, mut game_rx) = mpsc::channel::<Envelope>(100);

    // Start the UDP transport.
    tokio::spawn(async move {
        let transport = UdpClient::new(SERVER_ADDR.to_string());
        if let Err(e) = transport.run(from_transport_tx, to_transport_rx).await {
            eprintln!("❌ Transport error: {}", e);
        }
    });

    // Client with reliability enabled (must match the server, or HELLO is rejected).
    let config = ClientConfig {
        reliability: Some(ReliabilityConfig::default()),
        ..Default::default()
    };
    // Keep a clone of the outgoing sender to push inputs directly (see note below).
    let input_tx = to_transport_tx.clone();
    let mut client = Client::with_full_config(
        from_transport_rx,
        to_transport_tx,
        CodecType::from_id(1).unwrap(), // JSON control codec
        CodecType::from_id(1).unwrap(), // JSON game codec
        config,
        None, // no message registry
        NoCompressor,
        NoEncryptor,
        Some(game_tx),
    );

    // Reliable, negotiated HELLO over UDP.
    client.connect().await?;
    // The event loop drives incoming messages, ACKs and retransmissions.
    tokio::spawn(async move { client.run().await });

    println!("✅ Connected; streaming inputs and snapshots for {DEMO_SECS}s...\n");

    // NOTE: inputs are sent by pushing raw envelopes onto the transport's outgoing
    // channel. `Client::run()` consumes the client, so app->server sends currently
    // go around it (see doc/TODO.md, "Route client outgoing through Client").
    // Unreliable (empty flags) is the idiomatic choice for per-frame inputs anyway.
    let mut input_interval = tokio::time::interval(Duration::from_millis(50));
    let deadline = tokio::time::sleep(Duration::from_secs(DEMO_SECS));
    tokio::pin!(deadline);

    let mut frame: u64 = 0;
    let mut snapshots: u64 = 0;

    loop {
        tokio::select! {
            _ = &mut deadline => break,

            _ = input_interval.tick() => {
                frame += 1;
                let input = PlayerInput {
                    move_x: (frame as f32 * 0.1).sin(),
                    jump: frame.is_multiple_of(20),
                };
                let payload = serde_json::to_vec(&input)?;
                let env = Envelope::new_simple(
                    CURRENT_PROTOCOL_VERSION,
                    1, // JSON
                    PlayerInput::SCHEMA_HASH,
                    PlayerInput::ROUTE_ID,
                    0, // unreliable: no sequence
                    EnvelopeFlags::empty(), // Unreliable mode
                    Bytes::from(payload),
                );
                let _ = input_tx.send(env).await;
            }

            Some(env) = game_rx.recv() => {
                if env.route_id == GameState::ROUTE_ID {
                    if let Ok(state) = serde_json::from_slice::<GameState>(&env.payload) {
                        snapshots += 1;
                        if snapshots.is_multiple_of(10) {
                            let pos = state.players.first().map(|p| (p.position.x, p.position.y));
                            println!("📥 snapshot #{snapshots}: {} player(s), first pos {:?}",
                                state.players.len(), pos);
                        }
                    }
                }
            }
        }
    }

    println!("\n✅ Demo finished: sent {frame} inputs, received {snapshots} snapshots over UDP.");
    Ok(())
}
