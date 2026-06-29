//! Bevy 2D Platformer Client
//!
//! Simplified implementation with manual handshake:
//! - Direct HELLO handshake (no Client::run() conflicts)
//! - Keyboard input → PlayerInput → Server
//! - GameState from Server → Rendering
//! - Multi-player support
//!
//! Run with:
//! ```bash
//! cargo run --example bevy_platformer_client
//! ```

use bevy::prelude::*;
use bytes::Bytes;
use mokosh_client::transport::websocket::WebSocketClient;
use mokosh_examples_shared::platformer::{GameState, PlayerInput, GROUND_Y};
use mokosh_protocol::{
    messages::{routes, Hello, HelloOk},
    Envelope, EnvelopeFlags, GameMessage, Transport, CURRENT_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

const WINDOW_WIDTH: u32 = 800;
const WINDOW_HEIGHT: u32 = 600;
const SERVER_URL: &str = "ws://127.0.0.1:8080";

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
    /// Channel to send outgoing envelopes (PlayerInput, etc.)
    outgoing_tx: mpsc::Sender<Envelope>,
    /// Channel to receive GameState updates (as envelopes)
    game_state_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Envelope>>>,
}

#[derive(Resource)]
#[derive(Default)]
struct GameEntities {
    players: HashMap<String, Entity>,
    boxes: HashMap<u32, Entity>,
}


#[derive(Resource)]
struct LocalSessionId(String);

#[derive(Resource)]
struct MessageCounter(u64);

// ============================================================================
// Main
// ============================================================================

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (outgoing_tx, game_state_rx, session_id) = rt.block_on(async {
        println!("🔌 Connecting to server at {}...", SERVER_URL);

        // Create channels for transport layer
        let (incoming_tx, mut incoming_rx) = mpsc::channel(100);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

        // Create channel for game state updates (forwarded to Bevy)
        let (game_state_tx, game_state_rx) = mpsc::channel(100);

        // Start WebSocket transport
        tokio::spawn(async move {
            let transport = WebSocketClient::new(SERVER_URL.to_string());
            if let Err(e) = transport.run(incoming_tx, outgoing_rx).await {
                eprintln!("❌ Transport error: {}", e);
            }
        });

        // Send HELLO message
        let hello = Hello {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            min_protocol_version: CURRENT_PROTOCOL_VERSION,
            codec_id: 1, // JSON
            schema_hash: 0, // Not used for this example
            reliability: false, // WebSocket transport, reliability layer off
        };

        let hello_payload = serde_json::to_vec(&hello).unwrap();
        let hello_envelope = Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1, // JSON codec
            0, // schema_hash
            routes::HELLO,
            0, // msg_id
            EnvelopeFlags::RELIABLE,
            Bytes::from(hello_payload),
        );

        outgoing_tx.send(hello_envelope).await.unwrap();
        println!("📤 Sent HELLO");

        // Wait for HELLO_OK
        let session_id = loop {
            if let Some(envelope) = incoming_rx.recv().await {
                if envelope.route_id == routes::HELLO_OK {
                    let hello_ok: HelloOk = serde_json::from_slice(&envelope.payload).unwrap();
                    println!("✅ HELLO_OK received! Session ID: {}", hello_ok.session_id);
                    break hello_ok.session_id;
                }
            }
        };

        // Clone outgoing_tx for Bevy
        let outgoing_tx_clone = outgoing_tx.clone();

        // Forward game messages (route_id >= 100) to Bevy
        tokio::spawn(async move {
            while let Some(envelope) = incoming_rx.recv().await {
                if envelope.route_id >= 100 {
                    let _ = game_state_tx.send(envelope).await;
                }
            }
        });

        (outgoing_tx_clone, game_state_rx, session_id)
    });

    // Start Bevy app
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Bevy Platformer Client".to_string(),
                resolution: (WINDOW_WIDTH, WINDOW_HEIGHT).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(NetworkClient {
            outgoing_tx,
            game_state_rx: Arc::new(tokio::sync::Mutex::new(game_state_rx)),
        })
        .insert_resource(GameEntities::default())
        .insert_resource(LocalSessionId(session_id))
        .insert_resource(MessageCounter(1))
        .add_systems(Startup, setup_system)
        .add_systems(
            Update,
            (input_system, network_receive_system).chain(),
        )
        .run();
}

// ============================================================================
// Systems
// ============================================================================

fn setup_system(mut commands: Commands) {
    // Spawn camera
    commands.spawn((
        Camera2d,
        Transform::from_xyz(WINDOW_WIDTH as f32 / 2.0, WINDOW_HEIGHT as f32 / 2.0, 0.0),
    ));

    // Spawn ground line (invert Y for rendering)
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

fn input_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    net: Res<NetworkClient>,
    mut msg_counter: ResMut<MessageCounter>,
) {
    // Horizontal movement
    let mut move_x = 0.0;
    if keyboard.pressed(KeyCode::KeyA) || keyboard.pressed(KeyCode::ArrowLeft) {
        move_x = -1.0;
    }
    if keyboard.pressed(KeyCode::KeyD) || keyboard.pressed(KeyCode::ArrowRight) {
        move_x = 1.0;
    }

    // Jump
    let jump = keyboard.just_pressed(KeyCode::Space);

    // Only send input if there's actual movement or jump
    if move_x == 0.0 && !jump {
        return;
    }

    println!("🎮 Sending input: move_x={}, jump={}", move_x, jump);

    // Create PlayerInput
    let input = PlayerInput { move_x, jump };

    // Serialize to JSON (matching server's codec)
    if let Ok(payload_vec) = serde_json::to_vec(&input) {
        let payload = Bytes::from(payload_vec);
        let payload_len = payload.len() as u32;

        // Create envelope
        let envelope = Envelope {
            protocol_version: 1,
            codec_id: 1, // JSON
            schema_hash: PlayerInput::SCHEMA_HASH,
            route_id: PlayerInput::ROUTE_ID, // 100
            msg_id: msg_counter.0,
            correlation_id: 0,
            flags: EnvelopeFlags::RELIABLE,
            payload_len,
            payload,
        };

        msg_counter.0 = msg_counter.0.wrapping_add(1);

        // Send envelope (non-blocking)
        let _ = net.outgoing_tx.try_send(envelope);
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
                            // Update existing player (invert Y for rendering)
                            // Physics position is top-left, Bevy Transform is center
                            if let Ok((_player, mut transform)) = player_query.get_mut(entity) {
                                transform.translation.x = player_state.position.x + 16.0; // center
                                transform.translation.y = WINDOW_HEIGHT as f32 - (player_state.position.y + 16.0);
                            }
                        } else {
                            // Spawn new player
                            // First player to spawn is local player
                            let is_local = game_entities.players.is_empty();
                            let color = if is_local {
                                Color::srgb(0.2, 0.6, 1.0) // Blue for local player
                            } else {
                                Color::srgb(0.8, 0.3, 0.3) // Red for remote players
                            };

                            let mut entity_cmd = commands.spawn((
                                Sprite {
                                    color,
                                    custom_size: Some(bevy::math::Vec2::new(32.0, 32.0)),
                                    ..default()
                                },
                                Transform::from_xyz(
                                    player_state.position.x + 16.0, // center
                                    WINDOW_HEIGHT as f32 - (player_state.position.y + 16.0),
                                    1.0,
                                ),
                                PlayerEntity {
                                    session_id: player_state.id.clone(),
                                },
                            ));

                            if is_local {
                                entity_cmd.insert(LocalPlayerMarker);
                                // Update LocalSessionId with actual session from server
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
                            // Update existing box (invert Y for rendering)
                            // Physics position is top-left, Bevy Transform is center
                            if let Ok((_box_entity, mut transform)) = box_query.get_mut(entity) {
                                transform.translation.x = box_state.position.x + 16.0; // center
                                transform.translation.y = WINDOW_HEIGHT as f32 - (box_state.position.y + 16.0);
                            }
                        } else {
                            // Spawn new box (invert Y for rendering)
                            let entity = commands
                                .spawn((
                                    Sprite {
                                        color: Color::srgb(0.6, 0.4, 0.2),
                                        custom_size: Some(bevy::math::Vec2::new(32.0, 32.0)),
                                        ..default()
                                    },
                                    Transform::from_xyz(
                                        box_state.position.x + 16.0, // center
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
