//! Network module for Bevy WASM client
//!
//! Handles WebSocket connection, message serialization/deserialization,
//! and integration with Bevy ECS.

use bevy::prelude::*;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use mokosh_client::mpsc;
use mokosh_client::transport::browser_websocket::BrowserWebSocketClient;
use mokosh_examples_shared::platformer::{GameState, PlayerInput};
use mokosh_protocol::{
    messages::{routes, Hello, HelloOk},
    Envelope, EnvelopeFlags, GameMessage, Transport, CURRENT_PROTOCOL_VERSION,
};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use wasm_bindgen_futures;

use crate::{BoxEntity, GameEntities, LocalSessionId, PlayerEntity, WINDOW_HEIGHT};

const SERVER_URL: &str = "ws://127.0.0.1:8080";

// ============================================================================
// Resources
// ============================================================================

/// Network client resource - provides channels for sending/receiving messages
#[derive(Resource)]
pub struct NetworkClient {
    /// Channel to send outgoing envelopes (PlayerInput, etc.)
    pub outgoing_tx: mpsc::Sender<Envelope>,
    /// Channel to receive GameState updates (as envelopes)
    pub game_state_rx: Arc<futures::lock::Mutex<mpsc::Receiver<Envelope>>>,
}

/// Message counter for generating unique message IDs
#[derive(Resource)]
pub struct MessageCounter(pub u64);

// ============================================================================
// Connection state
// ============================================================================

/// Global connection ready channel
/// Used to pass connection info from async task to Bevy systems
static CONNECTION_READY: OnceLock<
    StdMutex<Option<(mpsc::Sender<Envelope>, Arc<futures::lock::Mutex<mpsc::Receiver<Envelope>>>, String)>>,
> = OnceLock::new();

// ============================================================================
// Connection management
// ============================================================================

/// Connect to server and perform HELLO handshake
async fn connect_to_server(
) -> Result<(mpsc::Sender<Envelope>, mpsc::Receiver<Envelope>, String), String> {
    log::info!("🔌 Connecting to server at {}...", SERVER_URL);

    // Create channels for transport layer
    let (incoming_tx, mut incoming_rx) = mpsc::channel(100);
    let (mut outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Create channel for game state updates (forwarded to Bevy)
    let (mut game_state_tx, game_state_rx) = mpsc::channel(100);

    // Start Browser WebSocket transport
    let outgoing_tx_clone = outgoing_tx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let transport = BrowserWebSocketClient::new(SERVER_URL.to_string());
        if let Err(e) = transport.run(incoming_tx, outgoing_rx).await {
            log::error!("❌ Transport error: {}", e);
        }
    });

    // Send HELLO message
    let hello = Hello {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        min_protocol_version: CURRENT_PROTOCOL_VERSION,
        codec_id: 1, // JSON
        schema_hash: 0,
        reliability: false, // WebSocket transport, reliability layer off
    };

    let hello_payload = serde_json::to_vec(&hello)
        .map_err(|e| format!("Failed to serialize HELLO: {}", e))?;

    let hello_envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1, // JSON codec
        0, // schema_hash
        routes::HELLO,
        0, // msg_id
        EnvelopeFlags::RELIABLE,
        Bytes::from(hello_payload),
    );

    outgoing_tx
        .send(hello_envelope)
        .await
        .map_err(|e| format!("Failed to send HELLO: {}", e))?;

    log::info!("📤 Sent HELLO");

    // Wait for HELLO_OK (no timeout in WASM for simplicity)
    let session_id = loop {
        if let Some(envelope) = incoming_rx.next().await {
            if envelope.route_id == routes::HELLO_OK {
                let hello_ok: HelloOk = serde_json::from_slice(&envelope.payload)
                    .map_err(|e| format!("Failed to parse HELLO_OK: {}", e))?;
                log::info!(
                    "✅ HELLO_OK received! Session ID: {}",
                    hello_ok.session_id
                );
                break hello_ok.session_id;
            }
        } else {
            return Err("Connection closed before HELLO_OK".to_string());
        }
    };

    // Forward game messages (route_id >= 100) to Bevy
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(envelope) = incoming_rx.next().await {
            if envelope.route_id >= 100 {
                let _ = game_state_tx.send(envelope).await;
            }
        }
    });

    Ok((outgoing_tx_clone, game_state_rx, session_id))
}

/// Initialize connection (called from Bevy startup system)
pub fn start_network_connection() {
    log::info!("🌐 Starting network connection...");

    wasm_bindgen_futures::spawn_local(async move {
        match connect_to_server().await {
            Ok((outgoing_tx, game_state_rx, session_id)) => {
                log::info!("✅ Network connection established! Session: {}", session_id);

                // Store connection in static OnceLock
                if let Some(cell) = CONNECTION_READY.get() {
                    if let Ok(mut guard) = cell.lock() {
                        *guard = Some((
                            outgoing_tx,
                            Arc::new(futures::lock::Mutex::new(game_state_rx)),
                            session_id,
                        ));
                    }
                }
            }
            Err(e) => {
                log::error!("❌ Failed to connect: {}", e);
            }
        }
    });
}

/// Initialize CONNECTION_READY cell (call before starting connection)
pub fn init_connection_cell() {
    CONNECTION_READY.get_or_init(|| StdMutex::new(None));
}

// ============================================================================
// Bevy Systems
// ============================================================================

/// Poll network setup state and insert resources when ready
pub fn poll_network_setup(
    mut commands: Commands,
    net_client: Option<Res<NetworkClient>>,
    mut local_session: ResMut<LocalSessionId>,
) {
    // Skip if already connected
    if net_client.is_some() {
        return;
    }

    // Check if connection is ready
    if let Some(cell) = CONNECTION_READY.get() {
        if let Ok(mut guard) = cell.lock() {
            if let Some((outgoing_tx, game_state_rx, session_id)) = guard.take() {
                log::info!("✅ Network connected! Game systems activated!");

                commands.insert_resource(NetworkClient {
                    outgoing_tx,
                    game_state_rx,
                });
                local_session.0 = session_id;
            }
        }
    }
}

/// Send player input to server
pub fn send_player_input(
    net: &mut NetworkClient,
    msg_counter: &mut MessageCounter,
    input: PlayerInput,
) {
    // Serialize to JSON
    if let Ok(payload_vec) = serde_json::to_vec(&input) {
        let payload = Bytes::from(payload_vec);
        let payload_len = payload.len() as u32;

        let envelope = Envelope {
            protocol_version: 1,
            codec_id: 1, // JSON
            schema_hash: PlayerInput::SCHEMA_HASH,
            route_id: PlayerInput::ROUTE_ID,
            msg_id: msg_counter.0,
            correlation_id: 0,
            flags: EnvelopeFlags::RELIABLE,
            payload_len,
            payload,
        };

        msg_counter.0 = msg_counter.0.wrapping_add(1);

        let _ = net.outgoing_tx.try_send(envelope);
    }
}

/// Receive game state updates from server and update entities
pub fn network_receive_system(
    net: Option<Res<NetworkClient>>,
    mut game_entities: ResMut<GameEntities>,
    mut local_session: ResMut<LocalSessionId>,
    mut commands: Commands,
    mut player_query: Query<(&PlayerEntity, &mut Transform)>,
    mut box_query: Query<(&BoxEntity, &mut Transform), Without<PlayerEntity>>,
) {
    // Skip if network not ready yet
    if let Some(net) = net {
        if let Some(mut rx_guard) = net.game_state_rx.try_lock() {
            // Process all available messages (non-blocking)
            loop {
                match rx_guard.try_next() {
                    Ok(Some(envelope)) => {
                        if envelope.route_id == 101 {
                            if let Ok(game_state) =
                                serde_json::from_slice::<GameState>(&envelope.payload)
                            {
                                // Update/spawn players
                                for player_state in &game_state.players {
                                    if let Some(&entity) = game_entities.players.get(&player_state.id) {
                                        if let Ok((_player, mut transform)) =
                                            player_query.get_mut(entity)
                                        {
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
                                                WINDOW_HEIGHT as f32
                                                    - (player_state.position.y + 16.0),
                                                1.0,
                                            ),
                                            PlayerEntity {
                                                session_id: player_state.id.clone(),
                                            },
                                        ));

                                        if is_local {
                                            entity_cmd.insert(crate::LocalPlayerMarker);
                                            local_session.0 = player_state.id.clone();
                                        }

                                        let entity = entity_cmd.id();
                                        game_entities
                                            .players
                                            .insert(player_state.id.clone(), entity);

                                        if is_local {
                                            log::info!("👤 You joined the game! ID: {}", player_state.id);
                                        } else {
                                            log::info!("👥 Remote player joined: {}", player_state.id);
                                        }
                                    }
                                }

                                // Update/spawn boxes
                                for box_state in &game_state.boxes {
                                    if let Some(&entity) = game_entities.boxes.get(&box_state.id) {
                                        if let Ok((_box_entity, mut transform)) =
                                            box_query.get_mut(entity)
                                        {
                                            transform.translation.x = box_state.position.x + 16.0;
                                            transform.translation.y =
                                                WINDOW_HEIGHT as f32 - (box_state.position.y + 16.0);
                                        }
                                    } else {
                                        let entity = commands
                                            .spawn((
                                                Sprite {
                                                    color: Color::srgb(0.6, 0.4, 0.2),
                                                    custom_size: Some(bevy::math::Vec2::new(
                                                        32.0, 32.0,
                                                    )),
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
                                        log::info!("👋 Player left: {}", id);
                                        false
                                    }
                                });
                            }
                        }
                    }
                    Ok(None) => {
                        // Channel closed
                        break;
                    }
                    Err(_) => {
                        // Channel empty - no more messages available now
                        break;
                    }
                }
            }
        }
    }
}
