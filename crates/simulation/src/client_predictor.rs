//! Client-side prediction with automatic reconciliation
//!
//! This module provides `ClientPredictor` which wraps a `Simulation` and handles:
//! - Immediate local input application (zero-latency feel)
//! - Input buffering for reconciliation
//! - Automatic reconciliation when server state diverges from prediction
//! - Input replay from divergence point

use crate::{InputBuffer, Simulation};

/// Client-side predictor with automatic reconciliation
///
/// Wraps a `Simulation` implementation and provides:
/// - Optimistic client-side prediction (instant feedback)
/// - Input buffering for reconciliation
/// - Automatic divergence detection and correction
///
/// # Example
///
/// ```rust
/// # use mokosh_simulation::{Simulation, client_predictor::ClientPredictor};
/// # use serde::{Serialize, Deserialize};
/// # use mokosh_protocol::GameMessage;
/// # use mokosh_protocol_derive::GameMessage;
/// #
/// # #[derive(Clone, Serialize, Deserialize, GameMessage)]
/// # #[route_id = 100]
/// # struct GameState { x: f32 }
/// # #[derive(Serialize, Deserialize, GameMessage)]
/// # #[route_id = 101]
/// # struct Input { dx: f32 }
/// # #[derive(Clone)]
/// # struct GameSim { x: f32 }
/// # impl Simulation for GameSim {
/// #     type Input = Input;
/// #     type State = GameState;
/// #     fn apply_input(&mut self, input: &Input, _dt: f32) { self.x += input.dx; }
/// #     fn step(&mut self, _dt: f32) {}
/// #     fn snapshot(&self) -> GameState { GameState { x: self.x } }
/// #     fn restore(&mut self, state: &GameState) { self.x = state.x; }
/// # }
/// #
/// let mut predictor = ClientPredictor::new(GameSim { x: 0.0 });
///
/// // Apply local input (instant feedback, no server round-trip)
/// predictor.apply_local_input(Input { dx: 1.0 }, 0.016);
///
/// // Step simulation forward
/// predictor.step(0.016);
///
/// // Reconcile with server snapshot (if diverged)
/// # let server_state = GameState { x: 1.0 };
/// if predictor.reconcile_with_server(&server_state, 1) {
///     println!("Corrected prediction divergence");
/// }
/// ```
pub struct ClientPredictor<S: Simulation> {
    /// Current predicted simulation state
    simulation: S,

    /// Buffer of inputs not yet confirmed by server
    pending_inputs: InputBuffer<S::Input>,

    /// Next input sequence number
    next_sequence: u32,

    /// Last confirmed sequence number from server
    last_confirmed_sequence: u32,

    /// Divergence threshold for reconciliation (squared distance)
    ///
    /// If predicted state diverges from server state by more than this threshold,
    /// reconciliation is triggered. Set to 0.0 to always reconcile, or increase
    /// for tolerance of minor floating-point drift.
    #[allow(dead_code)] // Reserved for future custom divergence detection
    divergence_threshold: f32,
}

impl<S: Simulation> ClientPredictor<S> {
    /// Creates a new client predictor with the given initial simulation state
    ///
    /// # Arguments
    ///
    /// - `simulation`: Initial simulation state
    pub fn new(simulation: S) -> Self {
        Self::with_config(simulation, 60, 0.01)
    }

    /// Creates a new client predictor with custom configuration
    ///
    /// # Arguments
    ///
    /// - `simulation`: Initial simulation state
    /// - `max_pending_inputs`: Maximum number of buffered inputs (default: 60)
    /// - `divergence_threshold`: Reconciliation threshold (default: 0.01)
    pub fn with_config(
        simulation: S,
        max_pending_inputs: usize,
        divergence_threshold: f32,
    ) -> Self {
        Self {
            simulation,
            pending_inputs: InputBuffer::new(max_pending_inputs),
            next_sequence: 1,
            last_confirmed_sequence: 0,
            divergence_threshold,
        }
    }

    /// Applies local input immediately (client-side prediction)
    ///
    /// This provides instant feedback without waiting for server round-trip.
    /// The input is buffered for potential reconciliation later.
    ///
    /// # Arguments
    ///
    /// - `input`: Input to apply
    /// - `delta_time`: Time delta for input application
    ///
    /// # Returns
    ///
    /// The sequence number assigned to this input (for server transmission)
    pub fn apply_local_input(&mut self, input: S::Input, delta_time: f32) -> u32
    where
        S::Input: Clone,
    {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);

        // Apply input immediately (prediction)
        self.simulation.apply_input(&input, delta_time);

        // Buffer input for potential reconciliation
        self.pending_inputs.push(sequence, input, delta_time);

        sequence
    }

    /// Steps the simulation forward by delta_time
    ///
    /// Call this every frame to update the predicted simulation state.
    ///
    /// # Arguments
    ///
    /// - `delta_time`: Time delta in seconds (e.g., 0.016 for 60 FPS)
    pub fn step(&mut self, delta_time: f32) {
        self.simulation.step(delta_time);
    }

    /// Reconciles with server's authoritative state snapshot
    ///
    /// Compares the current predicted state with the server's snapshot.
    /// If divergence is detected (beyond threshold), the client:
    /// 1. Resets to server's state
    /// 2. Replays all pending inputs from the acknowledged sequence
    ///
    /// # Arguments
    ///
    /// - `server_state`: Authoritative state from server
    /// - `confirmed_sequence`: Last input sequence confirmed by server
    ///
    /// # Returns
    ///
    /// `true` if reconciliation was performed (divergence detected), `false` otherwise
    ///
    /// # Example
    ///
    /// ```rust
    /// # use mokosh_simulation::{Simulation, client_predictor::ClientPredictor};
    /// # use serde::{Serialize, Deserialize};
    /// # use mokosh_protocol::GameMessage;
    /// # use mokosh_protocol_derive::GameMessage;
    /// #
    /// # #[derive(Clone, Serialize, Deserialize, GameMessage)]
    /// # #[route_id = 100]
    /// # struct GameState { x: f32 }
    /// # #[derive(Serialize, Deserialize, GameMessage)]
    /// # #[route_id = 101]
    /// # struct Input { dx: f32 }
    /// # #[derive(Clone)]
    /// # struct GameSim { x: f32 }
    /// # impl Simulation for GameSim {
    /// #     type Input = Input;
    /// #     type State = GameState;
    /// #     fn apply_input(&mut self, input: &Input, _dt: f32) { self.x += input.dx; }
    /// #     fn step(&mut self, _dt: f32) {}
    /// #     fn snapshot(&self) -> GameState { GameState { x: self.x } }
    /// #     fn restore(&mut self, state: &GameState) { self.x = state.x; }
    /// # }
    /// #
    /// # let mut predictor = ClientPredictor::new(GameSim { x: 0.0 });
    /// # let server_state = GameState { x: 5.0 };
    /// # let confirmed_sequence = 10;
    /// #
    /// if predictor.reconcile_with_server(&server_state, confirmed_sequence) {
    ///     println!("Client state corrected to match server");
    /// }
    /// ```
    pub fn reconcile_with_server(
        &mut self,
        server_state: &S::State,
        confirmed_sequence: u32,
    ) -> bool
    where
        S::Input: Clone,
    {
        // Acknowledge confirmed inputs
        self.pending_inputs.acknowledge(confirmed_sequence);
        self.last_confirmed_sequence = confirmed_sequence;

        // Check if we need to reconcile (compare states)
        let current_state = self.simulation.snapshot();
        if !self.should_reconcile(&current_state, server_state) {
            // No divergence - prediction was accurate
            return false;
        }

        tracing::debug!(
            confirmed_seq = confirmed_sequence,
            pending_inputs = self.pending_inputs.len(),
            "Reconciling client prediction with server state"
        );

        // Restore to server's authoritative state
        self.simulation.restore(server_state);

        // Replay all pending inputs from the confirmed point
        for pending in self.pending_inputs.pending() {
            self.simulation.apply_input(&pending.input, pending.delta_time);
        }

        true // Reconciliation performed
    }

    /// Checks if reconciliation is needed based on state divergence
    ///
    /// Override this method in subclasses for custom divergence detection logic.
    /// Default implementation compares state snapshots (requires `PartialEq`).
    ///
    /// # Arguments
    ///
    /// - `predicted`: Current predicted state
    /// - `authoritative`: Server's authoritative state
    ///
    /// # Returns
    ///
    /// `true` if reconciliation should be performed
    fn should_reconcile(&self, predicted: &S::State, authoritative: &S::State) -> bool
    where
        S::State: Clone,
    {
        // For now, use a simple approach: serialize and compare bytes
        // In a real implementation, you'd compare specific fields with thresholds
        //
        // Example for position-based games:
        // let pos_diff = (predicted.position - authoritative.position).length_squared();
        // pos_diff > self.divergence_threshold

        // Simple byte comparison for MVP
        use mokosh_protocol::CodecType;
        let codec = CodecType::from_id(2).unwrap(); // Postcard for compact comparison

        let predicted_bytes = codec.encode(predicted).unwrap();
        let authoritative_bytes = codec.encode(authoritative).unwrap();

        predicted_bytes != authoritative_bytes
    }

    /// Returns the current predicted simulation state
    pub fn simulation(&self) -> &S {
        &self.simulation
    }

    /// Returns a mutable reference to the simulation (for direct manipulation)
    pub fn simulation_mut(&mut self) -> &mut S {
        &mut self.simulation
    }

    /// Returns the number of pending inputs awaiting server confirmation
    pub fn pending_input_count(&self) -> usize {
        self.pending_inputs.len()
    }

    /// Returns the last confirmed sequence number
    pub fn last_confirmed_sequence(&self) -> u32 {
        self.last_confirmed_sequence
    }

    /// Returns the next sequence number that will be assigned
    pub fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    /// Returns a snapshot of the current predicted state
    pub fn snapshot(&self) -> S::State {
        self.simulation.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mokosh_protocol_derive::GameMessage;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
    #[route_id = 200]
    struct TestState {
        x: f32,
        y: f32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
    #[route_id = 201]
    struct TestInput {
        dx: f32,
        dy: f32,
    }

    #[derive(Clone)]
    struct TestSimulation {
        x: f32,
        y: f32,
    }

    impl Simulation for TestSimulation {
        type Input = TestInput;
        type State = TestState;

        fn apply_input(&mut self, input: &TestInput, _delta_time: f32) {
            self.x += input.dx;
            self.y += input.dy;
        }

        fn step(&mut self, _delta_time: f32) {
            // No physics for this test
        }

        fn snapshot(&self) -> TestState {
            TestState {
                x: self.x,
                y: self.y,
            }
        }

        fn restore(&mut self, state: &TestState) {
            self.x = state.x;
            self.y = state.y;
        }
    }

    #[test]
    fn test_apply_local_input() {
        let sim = TestSimulation { x: 0.0, y: 0.0 };
        let mut predictor = ClientPredictor::new(sim);

        let seq = predictor.apply_local_input(TestInput { dx: 1.0, dy: 2.0 }, 0.016);

        assert_eq!(seq, 1);
        assert_eq!(predictor.simulation().x, 1.0);
        assert_eq!(predictor.simulation().y, 2.0);
        assert_eq!(predictor.pending_input_count(), 1);
    }

    #[test]
    fn test_reconcile_no_divergence() {
        let sim = TestSimulation { x: 0.0, y: 0.0 };
        let mut predictor = ClientPredictor::new(sim);

        predictor.apply_local_input(TestInput { dx: 1.0, dy: 0.0 }, 0.016);

        let server_state = TestState { x: 1.0, y: 0.0 };
        let reconciled = predictor.reconcile_with_server(&server_state, 1);

        assert!(!reconciled); // No divergence - prediction was correct
        assert_eq!(predictor.pending_input_count(), 0); // Input acknowledged
    }

    #[test]
    fn test_reconcile_with_divergence() {
        let sim = TestSimulation { x: 0.0, y: 0.0 };
        let mut predictor = ClientPredictor::new(sim);

        // Client predicts x=5.0
        predictor.apply_local_input(TestInput { dx: 5.0, dy: 0.0 }, 0.016);
        predictor.apply_local_input(TestInput { dx: 1.0, dy: 0.0 }, 0.016);

        // Server says x=3.0 after first input
        let server_state = TestState { x: 3.0, y: 0.0 };
        let reconciled = predictor.reconcile_with_server(&server_state, 1);

        assert!(reconciled); // Divergence detected
        assert_eq!(predictor.pending_input_count(), 1); // One input still pending

        // After reconciliation: 3.0 (server) + 1.0 (replayed input) = 4.0
        assert_eq!(predictor.simulation().x, 4.0);
    }

    #[test]
    fn test_sequence_numbers() {
        let sim = TestSimulation { x: 0.0, y: 0.0 };
        let mut predictor = ClientPredictor::new(sim);

        let seq1 = predictor.apply_local_input(TestInput { dx: 1.0, dy: 0.0 }, 0.016);
        let seq2 = predictor.apply_local_input(TestInput { dx: 1.0, dy: 0.0 }, 0.016);
        let seq3 = predictor.apply_local_input(TestInput { dx: 1.0, dy: 0.0 }, 0.016);

        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(seq3, 3);
        assert_eq!(predictor.next_sequence(), 4);
    }
}
