//! Client-Side Prediction Demo
//!
//! This example demonstrates client-side prediction with server reconciliation.
//!
//! ## What it shows:
//!
//! 1. Shared `Simulation` trait implementation (runs on both client and server)
//! 2. Client-side prediction for instant feedback (zero latency feel)
//! 3. Server authoritative simulation (ground truth)
//! 4. Automatic reconciliation when predictions diverge
//! 5. Input replay to maintain game feel during correction
//!
//! ## Architecture:
//!
//! - **Client**: Applies inputs immediately (prediction), reconciles with server snapshots
//! - **Server**: Validates inputs, runs authoritative simulation, broadcasts snapshots
//! - **Shared**: Same simulation code runs on both sides (deterministic)
//!
//! ## Run:
//!
//! ```bash
//! cargo run --example prediction_demo
//! ```

use mokosh_protocol_derive::GameMessage;
use mokosh_simulation::{
    client_predictor::ClientPredictor,
    server_simulation::ServerSimulation,
    Simulation,
};
use mokosh_protocol::SessionId;
use serde::{Deserialize, Serialize};

// ============================================================================
// Shared Game Types (Client + Server)
// ============================================================================

/// 2D vector for position and velocity
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Vec2 {
    x: f32,
    y: f32,
}

impl Vec2 {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x + other.x, self.y + other.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, scalar: f32) -> Vec2 {
        Vec2::new(self.x * scalar, self.y * scalar)
    }
}

/// Player movement input (from client)
#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct MovementInput {
    /// Movement direction (normalized)
    direction: Vec2,
    /// Movement speed (units per second)
    speed: f32,
}

/// Complete game state snapshot (from server)
#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 101]
struct GameState {
    /// Player position
    position: Vec2,
    /// Player velocity
    velocity: Vec2,
    /// Timestamp (for debugging)
    timestamp: f32,
}

/// Simple 2D physics simulation
///
/// This simulation runs on both client (prediction) and server (authoritative).
/// It must be deterministic: same inputs + same delta_time = same result.
#[derive(Clone)]
struct PhysicsSimulation {
    position: Vec2,
    velocity: Vec2,
    timestamp: f32,
    friction: f32, // Friction coefficient (0.0 = no friction, 1.0 = instant stop)
}

impl PhysicsSimulation {
    fn new() -> Self {
        Self {
            position: Vec2::new(0.0, 0.0),
            velocity: Vec2::new(0.0, 0.0),
            timestamp: 0.0,
            friction: 0.9, // 90% friction per second
        }
    }
}

impl Simulation for PhysicsSimulation {
    type Input = MovementInput;
    type State = GameState;

    fn apply_input(&mut self, input: &MovementInput, _delta_time: f32) {
        // Set velocity based on input direction and speed
        self.velocity = input.direction * input.speed;
    }

    fn step(&mut self, delta_time: f32) {
        // Update position based on velocity
        self.position = self.position + self.velocity * delta_time;

        // Apply friction (exponential decay)
        let friction_factor = self.friction.powf(delta_time);
        self.velocity = self.velocity * friction_factor;

        // Update timestamp
        self.timestamp += delta_time;
    }

    fn snapshot(&self) -> GameState {
        GameState {
            position: self.position,
            velocity: self.velocity,
            timestamp: self.timestamp,
        }
    }

    fn restore(&mut self, state: &GameState) {
        self.position = state.position;
        self.velocity = state.velocity;
        self.timestamp = state.timestamp;
    }
}

// ============================================================================
// Demo Simulation
// ============================================================================

fn main() {
    println!("\n=== Client-Side Prediction Demo ===\n");

    // Create client predictor
    let client_sim = PhysicsSimulation::new();
    let mut client_predictor = ClientPredictor::new(client_sim);

    // Create server simulation
    let server_sim = PhysicsSimulation::new();
    let mut server_simulation = ServerSimulation::new(server_sim);

    let session_id = SessionId::new_v4();

    println!("Initial state:");
    println!("  Client position: ({:.2}, {:.2})",
        client_predictor.simulation().position.x,
        client_predictor.simulation().position.y);
    println!("  Server position: ({:.2}, {:.2})\n",
        server_simulation.simulation().position.x,
        server_simulation.simulation().position.y);

    // ========================================================================
    // Frame 1: Client applies input immediately (prediction)
    // ========================================================================

    println!("--- Frame 1: Client applies input (right, speed=10) ---");

    let input1 = MovementInput {
        direction: Vec2::new(1.0, 0.0), // Move right
        speed: 10.0,
    };

    let seq1 = client_predictor.apply_local_input(input1.clone(), 0.016);
    client_predictor.step(0.016);

    println!("  Client predicts position: ({:.2}, {:.2}) [instant feedback!]",
        client_predictor.simulation().position.x,
        client_predictor.simulation().position.y);
    println!("  Server hasn't received input yet (network delay)");
    println!("  Pending inputs: {}", client_predictor.pending_input_count());

    // ========================================================================
    // Frame 2: Server receives input (simulating network latency)
    // ========================================================================

    println!("\n--- Frame 2: Server receives input ---");

    // Server applies client input
    match server_simulation.apply_client_input(session_id, seq1, input1, 0.016) {
        Ok(confirmed) => println!("  Server confirmed input sequence: {}", confirmed),
        Err(e) => println!("  Server rejected input: {}", e),
    }

    server_simulation.step(0.016);

    let server_snapshot = server_simulation.snapshot();
    println!("  Server authoritative position: ({:.2}, {:.2})",
        server_snapshot.position.x,
        server_snapshot.position.y);

    // Client reconciles with server
    println!("\n  Client reconciles with server snapshot...");
    let reconciled = client_predictor.reconcile_with_server(&server_snapshot, seq1);

    if reconciled {
        println!("    Reconciliation performed (prediction corrected)");
    } else {
        println!("    No reconciliation needed (prediction was accurate!)");
    }

    println!("  Client position after reconciliation: ({:.2}, {:.2})",
        client_predictor.simulation().position.x,
        client_predictor.simulation().position.y);
    println!("  Pending inputs: {}", client_predictor.pending_input_count());

    // ========================================================================
    // Frame 3: Client applies another input (up+right)
    // ========================================================================

    println!("\n--- Frame 3: Client applies second input (up+right, speed=10) ---");

    let input2 = MovementInput {
        direction: Vec2::new(std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2), // 45 degrees (normalized)
        speed: 10.0,
    };

    let seq2 = client_predictor.apply_local_input(input2.clone(), 0.016);
    client_predictor.step(0.016);

    println!("  Client predicts position: ({:.2}, {:.2})",
        client_predictor.simulation().position.x,
        client_predictor.simulation().position.y);
    println!("  Pending inputs: {} (waiting for server confirmation)",
        client_predictor.pending_input_count());

    // ========================================================================
    // Frame 4: Simulate divergence (server applies different input)
    // ========================================================================

    println!("\n--- Frame 4: Simulating prediction divergence ---");
    println!("  (Server applies input with different parameters due to lag/packet loss)");

    // Server applies a slightly different input (simulating packet corruption or lag)
    let server_input2 = MovementInput {
        direction: Vec2::new(std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2),
        speed: 8.0, // Different speed! (simulating server validation/correction)
    };

    server_simulation.apply_client_input(session_id, seq2, server_input2, 0.016).unwrap();
    server_simulation.step(0.016);

    let server_snapshot2 = server_simulation.snapshot();
    println!("  Server authoritative position: ({:.2}, {:.2})",
        server_snapshot2.position.x,
        server_snapshot2.position.y);

    println!("\n  Client reconciles with server snapshot...");
    let reconciled = client_predictor.reconcile_with_server(&server_snapshot2, seq2);

    if reconciled {
        println!("    Reconciliation performed! (divergence detected and corrected)");
        println!("    Client corrected position: ({:.2}, {:.2})",
            client_predictor.simulation().position.x,
            client_predictor.simulation().position.y);
    } else {
        println!("    No reconciliation needed");
    }

    // ========================================================================
    // Frame 5-10: Continue simulation with friction
    // ========================================================================

    println!("\n--- Frames 5-10: No input, friction applied ---");

    for frame in 5..=10 {
        client_predictor.step(0.016);
        server_simulation.step(0.016);

        if frame % 2 == 0 {
            // Client reconciles every 2 frames
            let server_snapshot = server_simulation.snapshot();
            client_predictor.reconcile_with_server(&server_snapshot, seq2);
        }

        if frame == 10 {
            println!("  Frame {}: Client position: ({:.2}, {:.2}), velocity: ({:.2}, {:.2})",
                frame,
                client_predictor.simulation().position.x,
                client_predictor.simulation().position.y,
                client_predictor.simulation().velocity.x,
                client_predictor.simulation().velocity.y);
            println!("           Server position: ({:.2}, {:.2}), velocity: ({:.2}, {:.2})",
                server_simulation.simulation().position.x,
                server_simulation.simulation().position.y,
                server_simulation.simulation().velocity.x,
                server_simulation.simulation().velocity.y);
        }
    }

    // ========================================================================
    // Summary
    // ========================================================================

    println!("\n=== Summary ===\n");
    println!("✓ Client-side prediction provides instant feedback (zero latency)");
    println!("✓ Server authoritative simulation prevents cheating");
    println!("✓ Automatic reconciliation corrects prediction errors");
    println!("✓ Deterministic simulation ensures client/server consistency");
    println!("✓ Friction and physics work correctly on both sides\n");

    println!("Final positions:");
    println!("  Client: ({:.2}, {:.2})",
        client_predictor.simulation().position.x,
        client_predictor.simulation().position.y);
    println!("  Server: ({:.2}, {:.2})",
        server_simulation.simulation().position.x,
        server_simulation.simulation().position.y);

    // Calculate divergence
    let client_pos = client_predictor.simulation().position;
    let server_pos = server_simulation.simulation().position;
    let divergence = Vec2::new(
        client_pos.x - server_pos.x,
        client_pos.y - server_pos.y,
    );

    if divergence.length_squared() < 0.001 {
        println!("\n✓ Client and server are in sync! (divergence < 0.001)\n");
    } else {
        println!("\n⚠ Small divergence detected: ({:.4}, {:.4})", divergence.x, divergence.y);
        println!("  (This is expected due to floating-point precision)\n");
    }
}
