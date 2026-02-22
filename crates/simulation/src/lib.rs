//! # Mokosh Simulation Layer
//!
//! Client-side prediction with server reconciliation for authoritative multiplayer games.
//!
//! This crate provides the simulation layer on top of the Mokosh protocol,
//! enabling client-side prediction, server-authoritative simulation, and automatic
//! reconciliation when predictions diverge from reality.
//!
//! ## Architecture
//!
//! - **Client**: Applies inputs immediately (prediction), reconciles with server snapshots
//! - **Server**: Runs authoritative simulation, validates inputs, broadcasts snapshots
//! - **Shared**: Same `Simulation` trait implementation runs on both sides
//!
//! ## Example
//!
//! ```rust
//! use mokosh_simulation::Simulation;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Clone, Serialize, Deserialize)]
//! struct GameState {
//!     position: (f32, f32),
//!     velocity: (f32, f32),
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! struct MovementInput {
//!     direction: (f32, f32),
//! }
//!
//! #[derive(Clone)]
//! struct GameSim {
//!     position: (f32, f32),
//!     velocity: (f32, f32),
//! }
//!
//! impl Simulation for GameSim {
//!     type Input = MovementInput;
//!     type State = GameState;
//!
//!     fn apply_input(&mut self, input: &MovementInput, delta_time: f32) {
//!         self.velocity = input.direction;
//!     }
//!
//!     fn step(&mut self, delta_time: f32) {
//!         self.position.0 += self.velocity.0 * delta_time;
//!         self.position.1 += self.velocity.1 * delta_time;
//!     }
//!
//!     fn snapshot(&self) -> GameState {
//!         GameState {
//!             position: self.position,
//!             velocity: self.velocity,
//!         }
//!     }
//!
//!     fn restore(&mut self, state: &GameState) {
//!         self.position = state.position;
//!         self.velocity = state.velocity;
//!     }
//! }
//! ```

pub mod client_predictor;
pub mod server_simulation;

/// Core simulation trait implemented by games for client-side prediction
///
/// This trait defines the interface for a deterministic game simulation that
/// can run on both client (for prediction) and server (for authority).
///
/// # Requirements
///
/// - **Deterministic**: Same inputs + same delta_time = same outputs
/// - **Cloneable**: Allows saving/restoring state for reconciliation
/// - **Serializable**: Input and State must be network-transmittable
///
/// # Type Parameters
///
/// - `Input`: Player input message type (e.g., movement, actions)
/// - `State`: Complete game state snapshot for reconciliation
///
/// # Example
///
/// ```rust
/// # use mokosh_simulation::Simulation;
/// # use serde::{Serialize, Deserialize};
/// #
/// # #[derive(Clone, Serialize, Deserialize)]
/// # struct GameState { x: f32 }
/// # #[derive(Serialize, Deserialize)]
/// # struct Input { dx: f32 }
/// #
/// #[derive(Clone)]
/// struct MyGame {
///     x: f32,
/// }
///
/// impl Simulation for MyGame {
///     type Input = Input;
///     type State = GameState;
///
///     fn apply_input(&mut self, input: &Input, _dt: f32) {
///         self.x += input.dx;
///     }
///
///     fn step(&mut self, dt: f32) {
///         // Physics update
///     }
///
///     fn snapshot(&self) -> GameState {
///         GameState { x: self.x }
///     }
///
///     fn restore(&mut self, state: &GameState) {
///         self.x = state.x;
///     }
/// }
/// ```
pub trait Simulation: Clone {
    /// Input message type (e.g., PlayerInput with movement/actions)
    ///
    /// Must implement `GameMessage` from mokosh-protocol for network transmission.
    type Input: mokosh_protocol::GameMessage;

    /// State snapshot type for reconciliation
    ///
    /// Must implement `GameMessage` for network transmission and `Clone` for state comparison.
    type State: mokosh_protocol::GameMessage + Clone;

    /// Apply player input to the simulation
    ///
    /// This method is called when:
    /// - Client receives local input (immediate application for prediction)
    /// - Server receives client input (validation + authoritative application)
    /// - Client replays inputs during reconciliation
    ///
    /// # Arguments
    ///
    /// - `input`: Player input to apply
    /// - `delta_time`: Time delta for input application (for time-based actions)
    ///
    /// # Determinism
    ///
    /// This method MUST be deterministic: same input + same delta_time = same result.
    /// Non-deterministic behavior (random numbers, time-based logic) will cause
    /// client-server divergence.
    fn apply_input(&mut self, input: &Self::Input, delta_time: f32);

    /// Step the simulation forward by delta_time
    ///
    /// This method updates the simulation state (physics, AI, timers, etc.).
    /// Called every frame on both client and server.
    ///
    /// # Arguments
    ///
    /// - `delta_time`: Time delta in seconds (e.g., 0.016 for 60 FPS)
    ///
    /// # Determinism
    ///
    /// Must be deterministic for client-side prediction to work correctly.
    fn step(&mut self, delta_time: f32);

    /// Create a state snapshot for network transmission or comparison
    ///
    /// Used by:
    /// - Server to broadcast authoritative state to clients
    /// - Client to save predicted state for comparison
    ///
    /// # Returns
    ///
    /// Complete game state that can be restored via `restore()`
    fn snapshot(&self) -> Self::State;

    /// Restore simulation state from a snapshot
    ///
    /// Used by:
    /// - Client during reconciliation (restore to server's authoritative state)
    /// - Server when loading saved game state
    ///
    /// # Arguments
    ///
    /// - `state`: State snapshot to restore
    fn restore(&mut self, state: &Self::State);
}

/// Pending input awaiting server confirmation
///
/// Clients buffer inputs that have been applied locally but not yet confirmed
/// by the server. If the server's authoritative state diverges from the client's
/// prediction, these inputs are replayed from the divergence point.
#[derive(Debug, Clone)]
pub struct PendingInput<I> {
    /// Sequence number for ordering and matching server confirmations
    pub sequence: u32,

    /// The input message
    pub input: I,

    /// Delta time when this input was originally applied
    ///
    /// Used during reconciliation to replay inputs with the same timing.
    pub delta_time: f32,
}

impl<I> PendingInput<I> {
    /// Creates a new pending input
    pub fn new(sequence: u32, input: I, delta_time: f32) -> Self {
        Self {
            sequence,
            input,
            delta_time,
        }
    }
}

/// Input buffer for client-side prediction
///
/// Stores inputs that have been applied locally but not yet confirmed by the server.
/// Maximum size prevents unbounded growth if server stops responding.
#[derive(Debug, Clone)]
pub struct InputBuffer<I> {
    /// Pending inputs (sequence-ordered)
    inputs: Vec<PendingInput<I>>,

    /// Maximum number of pending inputs to buffer
    max_size: usize,
}

impl<I> InputBuffer<I> {
    /// Creates a new input buffer with the given maximum size
    ///
    /// # Arguments
    ///
    /// - `max_size`: Maximum number of pending inputs (default: 60 for 1 second at 60 FPS)
    pub fn new(max_size: usize) -> Self {
        Self {
            inputs: Vec::new(),
            max_size,
        }
    }

    /// Adds a pending input to the buffer
    ///
    /// If buffer is full, oldest input is dropped (prevents unbounded growth).
    ///
    /// # Arguments
    ///
    /// - `sequence`: Sequence number for this input
    /// - `input`: Input message
    /// - `delta_time`: Delta time when input was applied
    pub fn push(&mut self, sequence: u32, input: I, delta_time: f32) {
        if self.inputs.len() >= self.max_size {
            self.inputs.remove(0); // Drop oldest
        }
        self.inputs.push(PendingInput::new(sequence, input, delta_time));
    }

    /// Removes all inputs up to and including the given sequence number
    ///
    /// Called when server confirms inputs up to a certain sequence.
    ///
    /// # Arguments
    ///
    /// - `sequence`: Last confirmed sequence number
    pub fn acknowledge(&mut self, sequence: u32) {
        self.inputs.retain(|pending| pending.sequence > sequence);
    }

    /// Returns all pending inputs
    pub fn pending(&self) -> &[PendingInput<I>] {
        &self.inputs
    }

    /// Returns the number of pending inputs
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Checks if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// Clears all pending inputs
    pub fn clear(&mut self) {
        self.inputs.clear();
    }
}

impl<I> Default for InputBuffer<I> {
    fn default() -> Self {
        Self::new(60) // Default: 60 inputs (1 second at 60 FPS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_buffer_push() {
        let mut buffer = InputBuffer::<u32>::new(5);

        buffer.push(1, 100, 0.016);
        buffer.push(2, 200, 0.016);

        assert_eq!(buffer.len(), 2);
        assert!(!buffer.is_empty());
    }

    #[test]
    fn test_input_buffer_acknowledge() {
        let mut buffer = InputBuffer::<u32>::new(5);

        buffer.push(1, 100, 0.016);
        buffer.push(2, 200, 0.016);
        buffer.push(3, 300, 0.016);

        buffer.acknowledge(2); // Acknowledge inputs 1 and 2

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.pending()[0].sequence, 3);
    }

    #[test]
    fn test_input_buffer_max_size() {
        let mut buffer = InputBuffer::<u32>::new(3);

        buffer.push(1, 100, 0.016);
        buffer.push(2, 200, 0.016);
        buffer.push(3, 300, 0.016);
        buffer.push(4, 400, 0.016); // Should drop oldest (seq=1)

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.pending()[0].sequence, 2); // Oldest is now seq=2
    }

    #[test]
    fn test_input_buffer_clear() {
        let mut buffer = InputBuffer::<u32>::new(5);

        buffer.push(1, 100, 0.016);
        buffer.push(2, 200, 0.016);

        buffer.clear();

        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
    }
}
