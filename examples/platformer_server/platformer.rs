//! 2D Platformer physics simulation
//!
//! Shared simulation for both client (prediction) and server (authoritative).
//! Implements deterministic 2D physics with gravity, jumping, collision detection.

use crate::Simulation;
use mokosh_protocol::SessionId;
use mokosh_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Shared Game Types (Network Messages)
// ============================================================================

/// 2D vector for position and velocity
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0 }
    }

    #[allow(dead_code)]
    pub fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x + other.x, self.y + other.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, other: Vec2) -> Vec2 {
        Vec2::new(self.x - other.x, self.y - other.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, scalar: f32) -> Vec2 {
        Vec2::new(self.x * scalar, self.y * scalar)
    }
}

/// Axis-Aligned Bounding Box for collision detection
#[derive(Debug, Clone, Copy)]
pub struct Aabb {
    pub min: Vec2,
    pub max: Vec2,
}

impl Aabb {
    pub fn new(position: Vec2, size: Vec2) -> Self {
        Self {
            min: position,
            max: position + size,
        }
    }

    pub fn from_center(center: Vec2, half_size: Vec2) -> Self {
        Self {
            min: center - half_size,
            max: center + half_size,
        }
    }

    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min.x < other.max.x
            && self.max.x > other.min.x
            && self.min.y < other.max.y
            && self.max.y > other.min.y
    }

    pub fn center(&self) -> Vec2 {
        Vec2::new(
            (self.min.x + self.max.x) * 0.5,
            (self.min.y + self.max.y) * 0.5,
        )
    }
}

/// Player movement input (from client)
#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 100]
pub struct PlayerInput {
    /// Horizontal movement (-1.0 = left, 1.0 = right)
    pub move_x: f32,
    /// Jump button pressed
    pub jump: bool,
}

/// Player state for network synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerState {
    /// Session ID (UUID as string for JSON compatibility)
    pub id: String,
    /// Position
    pub position: Vec2,
    /// Velocity
    pub velocity: Vec2,
    /// On ground flag
    pub on_ground: bool,
}

/// Physics box (dynamic object) state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxState {
    /// Box ID
    pub id: u32,
    /// Position
    pub position: Vec2,
    /// Velocity
    pub velocity: Vec2,
    /// On ground flag
    pub on_ground: bool,
}

/// Complete game state snapshot (from server)
#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 101]
pub struct GameState {
    /// All players
    pub players: Vec<PlayerState>,
    /// All boxes
    pub boxes: Vec<BoxState>,
}

// ============================================================================
// Internal Simulation State (Not Serialized)
// ============================================================================

#[derive(Clone)]
pub struct PlayerData {
    pub session_id: SessionId,
    pub position: Vec2,
    pub velocity: Vec2,
    pub on_ground: bool,
}

#[derive(Clone)]
pub struct PhysicsBox {
    pub id: u32,
    pub position: Vec2,
    pub velocity: Vec2,
    pub on_ground: bool,
    #[allow(dead_code)]
    pub mass: f32,
}

// ============================================================================
// Platformer Simulation
// ============================================================================

/// 2D Platformer simulation with gravity, jumping, and box pushing
#[derive(Clone)]
pub struct PlatformerSimulation {
    /// Player states by session ID
    pub players: HashMap<SessionId, PlayerData>,
    /// Dynamic boxes
    pub boxes: Vec<PhysicsBox>,
    /// Next box ID
    pub next_box_id: u32,
    /// Physics constants
    pub gravity: f32,
    pub jump_velocity: f32,
    pub move_speed: f32,
    pub player_size: Vec2,
    pub box_size: Vec2,
    pub ground_y: f32,
    pub friction: f32,
}

impl PlatformerSimulation {
    pub fn new() -> Self {
        let mut sim = Self {
            players: HashMap::new(),
            boxes: Vec::new(),
            next_box_id: 1,
            gravity: 980.0,         // pixels/s²
            jump_velocity: -400.0,  // pixels/s (negative = up)
            move_speed: 200.0,      // pixels/s
            player_size: Vec2::new(32.0, 32.0),
            box_size: Vec2::new(32.0, 32.0),
            ground_y: 500.0,        // Ground level
            friction: 0.95,
        };

        // Spawn some boxes for testing
        sim.spawn_box(Vec2::new(300.0, 400.0));
        sim.spawn_box(Vec2::new(400.0, 300.0));
        sim.spawn_box(Vec2::new(500.0, 200.0));

        sim
    }

    pub fn spawn_box(&mut self, position: Vec2) {
        let id = self.next_box_id;
        self.next_box_id += 1;

        self.boxes.push(PhysicsBox {
            id,
            position,
            velocity: Vec2::zero(),
            on_ground: false,
            mass: 1.0,
        });
    }

    pub fn add_player(&mut self, session_id: SessionId) {
        self.players.insert(
            session_id,
            PlayerData {
                session_id,
                position: Vec2::new(100.0, 300.0),
                velocity: Vec2::zero(),
                on_ground: false,
            },
        );
    }

    pub fn remove_player(&mut self, session_id: SessionId) {
        self.players.remove(&session_id);
    }

    pub fn apply_input_to_player(&mut self, session_id: SessionId, input: &PlayerInput) {
        if let Some(player) = self.players.get_mut(&session_id) {
            // Horizontal movement
            player.velocity.x = input.move_x * self.move_speed;

            // Jump (only if on ground)
            if input.jump && player.on_ground {
                player.velocity.y = self.jump_velocity;
                player.on_ground = false;
            }
        }
    }

    pub fn step_physics(&mut self, delta_time: f32) {
        // Step 1: Apply gravity to players
        for player in self.players.values_mut() {
            if !player.on_ground {
                player.velocity.y += self.gravity * delta_time;
            }
        }

        // Step 2: Apply gravity to boxes
        for b in &mut self.boxes {
            if !b.on_ground {
                b.velocity.y += self.gravity * delta_time;
            }
        }

        // Step 3: Update positions
        for player in self.players.values_mut() {
            player.position = player.position + player.velocity * delta_time;
        }

        for b in &mut self.boxes {
            b.position = b.position + b.velocity * delta_time;
        }

        // Step 4: Collision detection and resolution
        self.resolve_collisions();

        // Step 5: Apply friction to boxes
        for b in &mut self.boxes {
            if b.on_ground {
                b.velocity.x *= self.friction;
                if b.velocity.x.abs() < 1.0 {
                    b.velocity.x = 0.0;
                }
            }
        }

        // Step 6: Ground check
        self.check_ground();
    }

    fn resolve_collisions(&mut self) {
        // Player-Box collisions (push boxes)
        let player_ids: Vec<SessionId> = self.players.keys().copied().collect();

        for &player_id in &player_ids {
            if let Some(player) = self.players.get(&player_id) {
                let player_aabb = Aabb::from_center(
                    player.position + self.player_size * 0.5,
                    self.player_size * 0.5,
                );

                for b in &mut self.boxes {
                    let box_aabb = Aabb::from_center(
                        b.position + self.box_size * 0.5,
                        self.box_size * 0.5,
                    );

                    if player_aabb.intersects(&box_aabb) {
                        // Determine collision side
                        let player_center = player_aabb.center();
                        let box_center = box_aabb.center();
                        let delta = player_center - box_center;

                        // Horizontal collision (push)
                        if delta.y.abs() < delta.x.abs() {
                            let push_force = player.velocity.x * 0.5;
                            b.velocity.x += push_force;
                        }
                    }
                }
            }
        }

        // Box-Box collisions (simple separation)
        for i in 0..self.boxes.len() {
            for j in (i + 1)..self.boxes.len() {
                let box1_aabb = Aabb::from_center(
                    self.boxes[i].position + self.box_size * 0.5,
                    self.box_size * 0.5,
                );
                let box2_aabb = Aabb::from_center(
                    self.boxes[j].position + self.box_size * 0.5,
                    self.box_size * 0.5,
                );

                if box1_aabb.intersects(&box2_aabb) {
                    // Simple separation (push apart)
                    let center1 = box1_aabb.center();
                    let center2 = box2_aabb.center();
                    let delta = center1 - center2;

                    if delta.y.abs() < delta.x.abs() {
                        // Horizontal separation
                        let separation = (self.box_size.x - delta.x.abs()) * 0.5;
                        let direction = if delta.x > 0.0 { 1.0 } else { -1.0 };

                        self.boxes[i].position.x += separation * direction;
                        self.boxes[j].position.x -= separation * direction;
                    }
                }
            }
        }
    }

    fn check_ground(&mut self) {
        const GROUND_THRESHOLD: f32 = 5.0;

        // Check players
        for player in self.players.values_mut() {
            player.on_ground = false;

            // Check static ground
            if player.position.y + self.player_size.y >= self.ground_y - GROUND_THRESHOLD {
                player.position.y = self.ground_y - self.player_size.y;
                player.velocity.y = 0.0;
                player.on_ground = true;
            }

            // Check if standing on a box
            let player_aabb = Aabb::from_center(
                player.position + self.player_size * 0.5,
                self.player_size * 0.5,
            );

            for b in &self.boxes {
                let box_aabb = Aabb::from_center(
                    b.position + self.box_size * 0.5,
                    self.box_size * 0.5,
                );

                // Check if player is above box
                let player_bottom = player.position.y + self.player_size.y;
                let box_top = b.position.y;

                if player_bottom >= box_top - GROUND_THRESHOLD
                    && player_bottom <= box_top + GROUND_THRESHOLD
                    && player_aabb.min.x < box_aabb.max.x
                    && player_aabb.max.x > box_aabb.min.x
                {
                    player.position.y = box_top - self.player_size.y;
                    player.velocity.y = 0.0;
                    player.on_ground = true;
                }
            }
        }

        // Check boxes
        for b in &mut self.boxes {
            b.on_ground = false;

            // Check static ground
            if b.position.y + self.box_size.y >= self.ground_y - GROUND_THRESHOLD {
                b.position.y = self.ground_y - self.box_size.y;
                b.velocity.y = 0.0;
                b.on_ground = true;
            }
        }

        // Check if boxes are on top of other boxes
        for i in 0..self.boxes.len() {
            for j in 0..self.boxes.len() {
                if i == j {
                    continue;
                }

                let box_bottom = self.boxes[i].position.y + self.box_size.y;
                let other_top = self.boxes[j].position.y;

                if box_bottom >= other_top - GROUND_THRESHOLD
                    && box_bottom <= other_top + GROUND_THRESHOLD
                {
                    let box1_aabb = Aabb::new(self.boxes[i].position, self.box_size);
                    let box2_aabb = Aabb::new(self.boxes[j].position, self.box_size);

                    if box1_aabb.min.x < box2_aabb.max.x && box1_aabb.max.x > box2_aabb.min.x {
                        self.boxes[i].position.y = other_top - self.box_size.y;
                        self.boxes[i].velocity.y = 0.0;
                        self.boxes[i].on_ground = true;
                    }
                }
            }
        }
    }
}

impl Default for PlatformerSimulation {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Simulation Trait Implementation
// ============================================================================

impl Simulation for PlatformerSimulation {
    type Input = PlayerInput;
    type State = GameState;

    fn apply_input(&mut self, input: &PlayerInput, _delta_time: f32) {
        // Note: In a real implementation, you'd need to know which player this input is for
        // For now, we'll apply to the first player (single-player prediction)
        // Multi-player needs input tagging with player_id
        if let Some((&session_id, _)) = self.players.iter().next() {
            self.apply_input_to_player(session_id, input);
        }
    }

    fn step(&mut self, delta_time: f32) {
        self.step_physics(delta_time);
    }

    fn snapshot(&self) -> GameState {
        GameState {
            players: self
                .players
                .values()
                .map(|p| PlayerState {
                    id: p.session_id.to_string(),
                    position: p.position,
                    velocity: p.velocity,
                    on_ground: p.on_ground,
                })
                .collect(),
            boxes: self
                .boxes
                .iter()
                .map(|b| BoxState {
                    id: b.id,
                    position: b.position,
                    velocity: b.velocity,
                    on_ground: b.on_ground,
                })
                .collect(),
        }
    }

    fn restore(&mut self, state: &GameState) {
        // Restore players
        for player_state in &state.players {
            // Parse UUID string back to SessionId
            if let Ok(session_id) = player_state.id.parse::<SessionId>() {
                if let Some(player) = self.players.get_mut(&session_id) {
                    player.position = player_state.position;
                    player.velocity = player_state.velocity;
                    player.on_ground = player_state.on_ground;
                }
            }
        }

        // Restore boxes
        for (i, box_state) in state.boxes.iter().enumerate() {
            if i < self.boxes.len() {
                self.boxes[i].position = box_state.position;
                self.boxes[i].velocity = box_state.velocity;
                self.boxes[i].on_ground = box_state.on_ground;
            }
        }
    }
}
