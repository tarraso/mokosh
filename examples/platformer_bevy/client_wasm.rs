//! Bevy 2D Platformer Client (WASM Version)
//!
//! Browser-compatible version using BrowserWebSocketClient
//!
//! Build with:
//! ```bash
//! cargo build --example bevy_platformer_client_wasm --target wasm32-unknown-unknown --release --no-default-features --features wasm
//! wasm-bindgen --out-dir examples/web/pkg --target web target/wasm32-unknown-unknown/release/examples/bevy_platformer_client_wasm.wasm
//! ```

#[path = "simulation.rs"]
mod simulation;

#[path = "network.rs"]
mod network;

use bevy::prelude::*;
use simulation::{PlayerInput, GROUND_Y};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

const WINDOW_WIDTH: u32 = 800;
const WINDOW_HEIGHT: u32 = 600;

// ============================================================================
// Components
// ============================================================================

#[derive(Component)]
pub struct PlayerEntity {
    pub session_id: String,
}

#[derive(Component)]
pub struct LocalPlayerMarker;

#[derive(Component)]
pub struct BoxEntity {
    pub box_id: u32,
}

#[derive(Component)]
struct GroundLine;

// ============================================================================
// Resources
// ============================================================================

#[derive(Resource)]
pub struct GameEntities {
    pub players: HashMap<String, Entity>,
    pub boxes: HashMap<u32, Entity>,
}

impl Default for GameEntities {
    fn default() -> Self {
        Self {
            players: HashMap::new(),
            boxes: HashMap::new(),
        }
    }
}

#[derive(Resource)]
pub struct LocalSessionId(pub String);

// ============================================================================
// Main (WASM Entry Point)
// ============================================================================

#[wasm_bindgen(start)]
pub fn main() {
    // Set panic hook for better error messages in browser console
    console_error_panic_hook::set_once();

    // Initialize tracing for browser console
    wasm_logger::init(wasm_logger::Config::new(log::Level::Info));

    log::info!("🚀 Starting Bevy WASM client...");

    // Initialize connection ready cell
    network::init_connection_cell();

    // Start Bevy app (non-blocking in WASM)
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Bevy Platformer Client (WASM)".to_string(),
                resolution: (WINDOW_WIDTH, WINDOW_HEIGHT).into(),
                canvas: Some("#bevy-canvas".to_string()),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(GameEntities::default())
        .insert_resource(LocalSessionId(String::new()))
        .insert_resource(network::MessageCounter(1))
        .add_systems(Startup, (setup_system, network::start_network_connection))
        .add_systems(
            Update,
            (
                network::poll_network_setup,
                input_system,
                network::network_receive_system,
            )
                .chain(),
        )
        .run();
}

// ============================================================================
// Game Systems
// ============================================================================

fn setup_system(mut commands: Commands) {
    log::info!("🎮 Setting up game scene...");

    // Spawn camera
    commands.spawn((
        Camera2d,
        Transform::from_xyz(
            WINDOW_WIDTH as f32 / 2.0,
            WINDOW_HEIGHT as f32 / 2.0,
            0.0,
        ),
    ));

    // Spawn ground line
    commands.spawn((
        Sprite {
            color: Color::srgb(0.3, 0.3, 0.3),
            custom_size: Some(bevy::math::Vec2::new(WINDOW_WIDTH as f32, 4.0)),
            ..default()
        },
        Transform::from_xyz(
            WINDOW_WIDTH as f32 / 2.0,
            WINDOW_HEIGHT as f32 - GROUND_Y,
            0.0,
        ),
        GroundLine,
    ));

    log::info!("🎮 Controls:");
    log::info!("   A/D or ←/→: Move");
    log::info!("   Space: Jump");
    log::info!("\n👤 Blue squares are players");
    log::info!("📦 Brown squares are physics boxes");
}

fn input_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut net: Option<ResMut<network::NetworkClient>>,
    mut msg_counter: ResMut<network::MessageCounter>,
) {
    // Skip if network not ready yet
    let Some(ref mut net) = net else { return };

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

    log::debug!("🎮 Sending input: move_x={}, jump={}", move_x, jump);

    // Send input via network module
    network::send_player_input(net, &mut msg_counter, PlayerInput { move_x, jump });
}
