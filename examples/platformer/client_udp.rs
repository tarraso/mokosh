//! 2D Platformer CLI client over **UDP** with the reliability layer enabled.
//!
//! Connects to `platformer_server_udp`, sends `PlayerInput` (unreliable, the
//! idiomatic choice for inputs) and prints the `GameState` snapshots the server
//! broadcasts. Runs for two minutes and preserves its guest session across network changes.
//!
//! Run (two terminals):
//! ```bash
//! cargo run --example platformer_server_udp
//! cargo run --example platformer_client_udp
//! ```

use mokosh_client::reconnect::{
    ReconnectConfig, ReconnectSendError, ReconnectState, ReconnectingClient,
};
use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::ReliableLink;
use mokosh_client::{Client, ClientConfig};
use mokosh_examples_shared::platformer::{
    GameState, PlatformerSimulation, PlayerInput, Simulation,
};
use mokosh_examples_shared::DEMO_UDP_PSK;
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::{CodecType, GameMessage, ReliabilityConfig, ReliabilityMode};
use std::time::Duration;

const SERVER_ADDR: &str = "127.0.0.1:8080";
const DEMO_SECS: u64 = 120;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .init();

    println!("🎮 Platformer client (UDP) connecting to {SERVER_ADDR}...");

    let mut coordinator = ReconnectingClient::new(
        ReconnectConfig {
            guest_resume: true,
            ..Default::default()
        },
        |incoming_rx, outgoing_tx| {
            let reliability = ReliabilityConfig::default();
            let config = ClientConfig {
                reliability: Some(reliability.clone()),
                keepalive_interval: Duration::from_secs(1),
                connection_timeout: Duration::from_secs(3),
                ..Default::default()
            };
            let client = Client::with_full_config(
                incoming_rx,
                outgoing_tx,
                CodecType::from_id(1).unwrap(),
                CodecType::from_id(1).unwrap(),
                config,
                None,
                NoCompressor,
                NoEncryptor,
                None,
            );
            let transport = ReliableLink::new(
                UdpClient::new(SERVER_ADDR)
                    .with_datagram_encryption(DEMO_UDP_PSK)
                    .require_encryption(true),
                reliability,
            );
            (client, transport)
        },
    )?;
    let handle = coordinator.handle();
    let mut status = handle.subscribe();
    let mut game_rx = coordinator.take_messages().unwrap();
    let task = tokio::spawn(coordinator.run());

    println!(
        "Streaming for {DEMO_SECS}s; network changes preserve the player while the server lives."
    );
    println!("A server restart or expired session produces an explicit failure.");

    let mut input_interval = tokio::time::interval(Duration::from_millis(50));
    let deadline = tokio::time::sleep(Duration::from_secs(DEMO_SECS));
    tokio::pin!(deadline);

    let mut frame: u64 = 0;
    let mut snapshots: u64 = 0;
    let mut world = PlatformerSimulation::new();

    loop {
        tokio::select! {
            _ = &mut deadline => break,

            _ = input_interval.tick() => {
                frame += 1;
                let input = PlayerInput {
                    move_x: (frame as f32 * 0.1).sin(),
                    jump: frame.is_multiple_of(20),
                };
                match handle.send_message(CodecType::from_id(1).unwrap(), &input, ReliabilityMode::Unreliable, None) {
                    Ok(()) | Err(ReconnectSendError::NotConnected) => {}
                    Err(error) => eprintln!("Input send failed: {error}"),
                }
            }

            changed = status.changed() => {
                if changed.is_err() { break; }
                match &*status.borrow_and_update() {
                    ReconnectState::Resuming { generation } => println!("Resuming: generation={generation}"),
                    ReconnectState::Synchronizing { generation, snapshot_id, snapshot, .. } => {
                        let state: GameState = CodecType::from_id(snapshot.codec_id)?.decode(&bytes::Bytes::copy_from_slice(&snapshot.payload))?;
                        frame = 0; world.restore(&state);
                        handle.confirm_snapshot(*generation, *snapshot_id)?;
                    }
                    ReconnectState::Ready { generation, player_id, session_id, resumed } => println!("Ready: generation={generation}, player={player_id}, transport={session_id}, resumed={resumed}"),
                    ReconnectState::Connected { generation, hello } => println!("Connected: generation={generation}, session={}", hello.session_id),
                    ReconnectState::Connecting { attempt, .. } => println!("Connecting: attempt={attempt}"),
                    ReconnectState::Reconnecting { attempt, last, .. } => println!("Reconnecting: attempt={attempt}, reason={last:?}"),
                    ReconnectState::Failed(error) => { eprintln!("Connection failed: {error}"); break; }
                    ReconnectState::Stopped => break,
                }
            }
            Some(message) = game_rx.recv() => {
                let current = match &*status.borrow() {
                    ReconnectState::Connected { generation, .. } | ReconnectState::Ready { generation, .. } => Some(*generation),
                    _ => None,
                };
                if current != Some(message.generation) { continue; }
                let env = message.envelope;
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

    handle.disconnect();
    task.await??;
    println!("\n✅ Demo finished: sent {frame} inputs, received {snapshots} snapshots over UDP.");
    Ok(())
}
