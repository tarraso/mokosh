//! Bevy 2D Platformer Client over **UDP** with the reliability layer.
//!
//! Same rendering/gameplay as `bevy_platformer_client`, but it connects over
//! `UdpClient` with reliability enabled. Because reliable messages must be
//! acknowledged, this client drives the `Client` event loop (which handles the
//! handshake, ACKs and retransmissions) and forwards game messages via a
//! channel; keyboard inputs are sent unreliable (the idiomatic choice).
//!
//! Run (paired with `bevy_platformer_server_udp`):
//! ```bash
//! cargo run --example bevy_platformer_server_udp
//! cargo run --example bevy_platformer_client_udp
//! ```

use bevy::prelude::*;
use bytes::Bytes;
use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::{ReliableLink, Transport};
use mokosh_client::{Client, ClientConfig, ClientHandle};
use mokosh_examples_shared::platformer::{GameState, PlayerInput, GROUND_Y};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::reliability::ReliabilityMode;
use mokosh_protocol::{CodecType, Envelope, GameMessage, ReliabilityConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

const WINDOW_WIDTH: u32 = 800;
const WINDOW_HEIGHT: u32 = 600;
const SERVER_ADDR: &str = "127.0.0.1:8080";

// ============================================================================
// Components
// ============================================================================

#[derive(Component)]
#[allow(dead_code)]
struct PlayerEntity {
    session_id: String,
}

#[derive(Component)]
struct LocalPlayerMarker;

#[derive(Component)]
#[allow(dead_code)]
struct BoxEntity {
    box_id: u32,
}

#[derive(Component)]
struct GroundLine;

// ============================================================================
// Resources
// ============================================================================

#[derive(Resource)]
struct NetworkClient {
    /// Handle for sending through the running client (engages reliability).
    handle: ClientHandle,
    /// Channel to receive GameState updates (as envelopes)
    game_state_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Envelope>>>,
}

#[derive(Resource, Default)]
struct GameEntities {
    players: HashMap<String, Entity>,
    boxes: HashMap<u32, Entity>,
}

#[derive(Resource)]
struct LocalSessionId(String);

// ============================================================================
// Main
// ============================================================================

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // The reliability layer needs the `Client` event loop running so it ACKs the
    // server's reliable HELLO_OK (otherwise the server drops us). We drive
    // `Client::run` and forward game messages via `game_messages_tx`.
    let (handle, game_state_rx) = rt.block_on(async {
        println!("🔌 Connecting (UDP + reliability) to {SERVER_ADDR}...");

        let (from_transport_tx, from_transport_rx) = mpsc::channel(100);
        let (to_transport_tx, to_transport_rx) = mpsc::channel(100);
        let (game_tx, game_rx) = mpsc::channel(100);

        // UDP transport, wrapped in the reliability decorator (the Client event
        // loop is reliability-agnostic; the link adds ACK/retransmit/ordering).
        tokio::spawn(async move {
            let transport =
                ReliableLink::new(UdpClient::new(SERVER_ADDR.to_string()), ReliabilityConfig::default());
            if let Err(e) = transport.run(from_transport_tx, to_transport_rx).await {
                eprintln!("❌ Transport error: {}", e);
            }
        });

        let config = ClientConfig {
            reliability: Some(ReliabilityConfig::default()),
            ..Default::default()
        };
        let mut client = Client::with_full_config(
            from_transport_rx,
            to_transport_tx,
            CodecType::from_id(1).unwrap(),
            CodecType::from_id(1).unwrap(),
            config,
            None,
            NoCompressor,
            NoEncryptor,
            Some(game_tx),
        );

        // Send inputs *through* the client (so reliability/sequencing engage)
        // rather than cloning the raw transport channel. `Client::run` consumes
        // the client, so grab the handle first.
        let handle = client.handle();
        client.connect().await.expect("Failed to send HELLO");
        println!("📤 Sent HELLO; running client event loop");
        tokio::spawn(async move { client.run().await });

        (handle, game_rx)
    });

    // `rt` stays in scope for the whole run, keeping the spawned tasks alive.
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Bevy Platformer Client (UDP)".to_string(),
                resolution: (WINDOW_WIDTH, WINDOW_HEIGHT).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(NetworkClient {
            handle,
            game_state_rx: Arc::new(tokio::sync::Mutex::new(game_state_rx)),
        })
        .insert_resource(GameEntities::default())
        .insert_resource(LocalSessionId(String::new()))
        .add_systems(Startup, setup_system)
        .add_systems(Update, (input_system, network_receive_system).chain())
        .run();
}

// ============================================================================
// Systems
// ============================================================================

fn setup_system(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Transform::from_xyz(WINDOW_WIDTH as f32 / 2.0, WINDOW_HEIGHT as f32 / 2.0, 0.0),
    ));

    commands.spawn((
        Sprite {
            color: Color::srgb(0.3, 0.3, 0.3),
            custom_size: Some(bevy::math::Vec2::new(WINDOW_WIDTH as f32, 4.0)),
            ..default()
        },
        Transform::from_xyz(WINDOW_WIDTH as f32 / 2.0, WINDOW_HEIGHT as f32 - GROUND_Y, 0.0),
        GroundLine,
    ));

    println!("🎮 Controls:");
    println!("   A/D or ←/→: Move");
    println!("   Space: Jump");
    println!("\n👤 Blue squares are players");
    println!("📦 Brown squares are physics boxes");
}

fn input_system(keyboard: Res<ButtonInput<KeyCode>>, net: Res<NetworkClient>) {
    let mut move_x = 0.0;
    if keyboard.pressed(KeyCode::KeyA) || keyboard.pressed(KeyCode::ArrowLeft) {
        move_x = -1.0;
    }
    if keyboard.pressed(KeyCode::KeyD) || keyboard.pressed(KeyCode::ArrowRight) {
        move_x = 1.0;
    }

    let jump = keyboard.just_pressed(KeyCode::Space);
    if move_x == 0.0 && !jump {
        return;
    }

    let input = PlayerInput { move_x, jump };

    // Inputs are sent unreliable (the idiomatic choice for per-frame input), but
    // *through* the client via its handle so sequencing/codec are applied
    // consistently instead of bypassing the event loop on the raw channel.
    if let Ok(payload_vec) = serde_json::to_vec(&input) {
        let _ = net.handle.send_encoded(
            PlayerInput::ROUTE_ID, // 100
            PlayerInput::SCHEMA_HASH,
            Bytes::from(payload_vec),
            ReliabilityMode::Unreliable,
            None,
        );
    }
}

fn network_receive_system(
    net: Res<NetworkClient>,
    mut game_entities: ResMut<GameEntities>,
    mut local_session: ResMut<LocalSessionId>,
    mut commands: Commands,
    mut player_query: Query<(&PlayerEntity, &mut Transform)>,
    mut box_query: Query<(&BoxEntity, &mut Transform), Without<PlayerEntity>>,
) {
    if let Ok(mut rx_guard) = net.game_state_rx.try_lock() {
        while let Ok(envelope) = rx_guard.try_recv() {
            // GameState messages have route_id=101
            if envelope.route_id == 101 {
                if let Ok(game_state) = serde_json::from_slice::<GameState>(&envelope.payload) {
                    // Update/spawn players
                    for player_state in &game_state.players {
                        if let Some(&entity) = game_entities.players.get(&player_state.id) {
                            if let Ok((_player, mut transform)) = player_query.get_mut(entity) {
                                transform.translation.x = player_state.position.x + 16.0;
                                transform.translation.y =
                                    WINDOW_HEIGHT as f32 - (player_state.position.y + 16.0);
                            }
                        } else {
                            let is_local = game_entities.players.is_empty();
                            let color = if is_local {
                                Color::srgb(0.2, 0.6, 1.0)
                            } else {
                                Color::srgb(0.8, 0.3, 0.3)
                            };

                            let mut entity_cmd = commands.spawn((
                                Sprite {
                                    color,
                                    custom_size: Some(bevy::math::Vec2::new(32.0, 32.0)),
                                    ..default()
                                },
                                Transform::from_xyz(
                                    player_state.position.x + 16.0,
                                    WINDOW_HEIGHT as f32 - (player_state.position.y + 16.0),
                                    1.0,
                                ),
                                PlayerEntity {
                                    session_id: player_state.id.clone(),
                                },
                            ));

                            if is_local {
                                entity_cmd.insert(LocalPlayerMarker);
                                local_session.0 = player_state.id.clone();
                            }

                            let entity = entity_cmd.id();
                            game_entities
                                .players
                                .insert(player_state.id.clone(), entity);

                            if is_local {
                                println!("👤 You joined the game! ID: {}", player_state.id);
                            } else {
                                println!("👥 Remote player joined: {}", player_state.id);
                            }
                        }
                    }

                    // Update/spawn boxes
                    for box_state in &game_state.boxes {
                        if let Some(&entity) = game_entities.boxes.get(&box_state.id) {
                            if let Ok((_box_entity, mut transform)) = box_query.get_mut(entity) {
                                transform.translation.x = box_state.position.x + 16.0;
                                transform.translation.y =
                                    WINDOW_HEIGHT as f32 - (box_state.position.y + 16.0);
                            }
                        } else {
                            let entity = commands
                                .spawn((
                                    Sprite {
                                        color: Color::srgb(0.6, 0.4, 0.2),
                                        custom_size: Some(bevy::math::Vec2::new(32.0, 32.0)),
                                        ..default()
                                    },
                                    Transform::from_xyz(
                                        box_state.position.x + 16.0,
                                        WINDOW_HEIGHT as f32 - (box_state.position.y + 16.0),
                                        1.0,
                                    ),
                                    BoxEntity {
                                        box_id: box_state.id,
                                    },
                                ))
                                .id();

                            game_entities.boxes.insert(box_state.id, entity);
                        }
                    }

                    // Remove disconnected players
                    let current_player_ids: Vec<String> =
                        game_state.players.iter().map(|p| p.id.clone()).collect();
                    game_entities.players.retain(|id, &mut entity| {
                        if current_player_ids.contains(id) {
                            true
                        } else {
                            commands.entity(entity).despawn();
                            println!("👋 Player left: {}", id);
                            false
                        }
                    });
                }
            }
        }
    }
}
