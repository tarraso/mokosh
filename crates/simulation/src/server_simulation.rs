//! Server-side authoritative simulation
//!
//! This module provides `ServerSimulation` which wraps a `Simulation` and handles:
//! - Authoritative state management (ground truth)
//! - Client input validation and application
//! - State snapshot generation for broadcasting
//! - Per-client sequence tracking

use crate::Simulation;
use mokosh_protocol::SessionId;
use std::collections::HashMap;

/// Per-client session tracking for input validation
#[derive(Debug, Clone)]
struct ClientSession {
    /// Last processed input sequence from this client
    last_sequence: u32,
}

impl ClientSession {
    fn new() -> Self {
        Self { last_sequence: 0 }
    }
}

/// Server-side authoritative simulation
///
/// Wraps a `Simulation` implementation and provides:
/// - Authoritative state (ground truth)
/// - Per-client input validation
/// - Sequence number tracking to prevent replay attacks
/// - Snapshot generation for client synchronization
///
/// # Example
///
/// ```rust
/// # use mokosh_simulation::{Simulation, server_simulation::ServerSimulation};
/// # use mokosh_protocol::SessionId;
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
/// let mut server_sim = ServerSimulation::new(GameSim { x: 0.0 });
///
/// let session_id = SessionId::new_v4();
///
/// // Apply client input (with validation)
/// if let Ok(confirmed_seq) = server_sim.apply_client_input(
///     session_id,
///     1,              // sequence number
///     Input { dx: 1.0 },
///     0.016,
/// ) {
///     println!("Applied input, confirmed sequence: {}", confirmed_seq);
/// }
///
/// // Step simulation forward
/// server_sim.step(0.016);
///
/// // Get authoritative snapshot for broadcasting
/// let snapshot = server_sim.snapshot();
/// ```
pub struct ServerSimulation<S: Simulation> {
    /// Authoritative simulation state (ground truth)
    simulation: S,

    /// Per-client session tracking for input validation
    client_sessions: HashMap<SessionId, ClientSession>,

    /// Whether to enforce strict sequence ordering
    ///
    /// If true, inputs with out-of-order sequences are rejected.
    /// If false, allows some out-of-order inputs (for UDP-based protocols).
    strict_ordering: bool,
}

impl<S: Simulation> ServerSimulation<S> {
    /// Creates a new server simulation with the given initial state
    ///
    /// # Arguments
    ///
    /// - `simulation`: Initial simulation state
    pub fn new(simulation: S) -> Self {
        Self {
            simulation,
            client_sessions: HashMap::new(),
            strict_ordering: true,
        }
    }

    /// Creates a new server simulation with custom configuration
    ///
    /// # Arguments
    ///
    /// - `simulation`: Initial simulation state
    /// - `strict_ordering`: Whether to enforce strict sequence ordering
    pub fn with_config(simulation: S, strict_ordering: bool) -> Self {
        Self {
            simulation,
            client_sessions: HashMap::new(),
            strict_ordering,
        }
    }

    /// Applies client input to the authoritative simulation
    ///
    /// Validates input sequence number to prevent:
    /// - Replay attacks (duplicate sequence numbers)
    /// - Out-of-order inputs (if strict_ordering is enabled)
    ///
    /// # Arguments
    ///
    /// - `session_id`: Client session identifier
    /// - `sequence`: Input sequence number from client
    /// - `input`: Input message to apply
    /// - `delta_time`: Time delta for input application
    ///
    /// # Returns
    ///
    /// - `Ok(confirmed_sequence)`: Input was applied, returns last confirmed sequence
    /// - `Err(ServerSimulationError)`: Input was rejected (invalid sequence)
    ///
    /// # Example
    ///
    /// ```rust
    /// # use mokosh_simulation::{Simulation, server_simulation::ServerSimulation};
    /// # use mokosh_protocol::SessionId;
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
    /// # let mut server_sim = ServerSimulation::new(GameSim { x: 0.0 });
    /// # let session_id = SessionId::new_v4();
    /// #
    /// match server_sim.apply_client_input(session_id, 1, Input { dx: 1.0 }, 0.016) {
    ///     Ok(confirmed) => println!("Input applied, confirmed: {}", confirmed),
    ///     Err(e) => println!("Input rejected: {}", e),
    /// }
    /// ```
    pub fn apply_client_input(
        &mut self,
        session_id: SessionId,
        sequence: u32,
        input: S::Input,
        delta_time: f32,
    ) -> Result<u32, ServerSimulationError> {
        // Get or create client session
        let session = self
            .client_sessions
            .entry(session_id)
            .or_insert_with(ClientSession::new);

        // Validate sequence number
        if sequence <= session.last_sequence {
            // Duplicate or old input - reject
            return Err(ServerSimulationError::InvalidSequence {
                session_id,
                sequence,
                last_sequence: session.last_sequence,
            });
        }

        if self.strict_ordering && sequence != session.last_sequence + 1 {
            // Out-of-order input in strict mode - reject
            return Err(ServerSimulationError::OutOfOrder {
                session_id,
                sequence,
                expected: session.last_sequence + 1,
            });
        }

        // Apply input to authoritative simulation
        self.simulation.apply_input(&input, delta_time);

        // Update client's last sequence
        session.last_sequence = sequence;

        tracing::debug!(
            session = %session_id,
            sequence,
            "Applied client input to authoritative simulation"
        );

        Ok(sequence) // Return confirmed sequence
    }

    /// Steps the simulation forward by delta_time
    ///
    /// Call this every server tick to update the authoritative simulation state.
    ///
    /// # Arguments
    ///
    /// - `delta_time`: Time delta in seconds (e.g., 0.016 for 60 FPS)
    pub fn step(&mut self, delta_time: f32) {
        self.simulation.step(delta_time);
    }

    /// Returns an authoritative state snapshot for broadcasting to clients
    ///
    /// # Returns
    ///
    /// Complete game state snapshot that clients can use for reconciliation
    pub fn snapshot(&self) -> S::State {
        self.simulation.snapshot()
    }

    /// Returns a reference to the authoritative simulation
    pub fn simulation(&self) -> &S {
        &self.simulation
    }

    /// Returns a mutable reference to the simulation (for direct manipulation)
    pub fn simulation_mut(&mut self) -> &mut S {
        &mut self.simulation
    }

    /// Returns the last confirmed sequence for a given client
    ///
    /// Returns `None` if client has not sent any inputs yet.
    pub fn last_confirmed_sequence(&self, session_id: SessionId) -> Option<u32> {
        self.client_sessions
            .get(&session_id)
            .map(|s| s.last_sequence)
    }

    /// Removes a client session (call when client disconnects)
    pub fn remove_client(&mut self, session_id: SessionId) {
        self.client_sessions.remove(&session_id);
    }

    /// Returns the number of active client sessions
    pub fn client_count(&self) -> usize {
        self.client_sessions.len()
    }

    /// Returns all active session IDs
    pub fn active_sessions(&self) -> Vec<SessionId> {
        self.client_sessions.keys().copied().collect()
    }
}

/// Server simulation errors
#[derive(Debug, thiserror::Error)]
pub enum ServerSimulationError {
    /// Invalid sequence number (duplicate or too old)
    #[error("Invalid sequence from session {session_id}: got {sequence}, last was {last_sequence}")]
    InvalidSequence {
        session_id: SessionId,
        sequence: u32,
        last_sequence: u32,
    },

    /// Out-of-order sequence in strict mode
    #[error("Out-of-order sequence from session {session_id}: got {sequence}, expected {expected}")]
    OutOfOrder {
        session_id: SessionId,
        sequence: u32,
        expected: u32,
    },
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
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
    #[route_id = 201]
    struct TestInput {
        dx: f32,
    }

    #[derive(Clone)]
    struct TestSimulation {
        x: f32,
    }

    impl Simulation for TestSimulation {
        type Input = TestInput;
        type State = TestState;

        fn apply_input(&mut self, input: &TestInput, _delta_time: f32) {
            self.x += input.dx;
        }

        fn step(&mut self, _delta_time: f32) {
            // No physics for this test
        }

        fn snapshot(&self) -> TestState {
            TestState { x: self.x }
        }

        fn restore(&mut self, state: &TestState) {
            self.x = state.x;
        }
    }

    #[test]
    fn test_apply_client_input() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::new(sim);
        let session_id = SessionId::new_v4();

        let result = server_sim.apply_client_input(
            session_id,
            1,
            TestInput { dx: 5.0 },
            0.016,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
        assert_eq!(server_sim.simulation().x, 5.0);
    }

    #[test]
    fn test_reject_duplicate_sequence() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::new(sim);
        let session_id = SessionId::new_v4();

        // First input OK
        server_sim
            .apply_client_input(session_id, 1, TestInput { dx: 5.0 }, 0.016)
            .unwrap();

        // Duplicate sequence rejected
        let result = server_sim.apply_client_input(
            session_id,
            1,
            TestInput { dx: 10.0 },
            0.016,
        );

        assert!(result.is_err());
        assert_eq!(server_sim.simulation().x, 5.0); // State unchanged
    }

    #[test]
    fn test_reject_old_sequence() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::with_config(sim, false); // Non-strict for this test
        let session_id = SessionId::new_v4();

        server_sim
            .apply_client_input(session_id, 5, TestInput { dx: 5.0 }, 0.016)
            .unwrap();

        // Old sequence rejected (3 < 5)
        let result = server_sim.apply_client_input(
            session_id,
            3,
            TestInput { dx: 10.0 },
            0.016,
        );

        assert!(result.is_err());
        assert_eq!(server_sim.simulation().x, 5.0); // State unchanged
    }

    #[test]
    fn test_strict_ordering() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::new(sim);
        let session_id = SessionId::new_v4();

        server_sim
            .apply_client_input(session_id, 1, TestInput { dx: 1.0 }, 0.016)
            .unwrap();

        // Skipped sequence (2) - should be rejected in strict mode
        let result = server_sim.apply_client_input(
            session_id,
            3,
            TestInput { dx: 1.0 },
            0.016,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_non_strict_ordering() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::with_config(sim, false); // Non-strict
        let session_id = SessionId::new_v4();

        server_sim
            .apply_client_input(session_id, 1, TestInput { dx: 1.0 }, 0.016)
            .unwrap();

        // Skipped sequence (2) - should be accepted in non-strict mode
        let result = server_sim.apply_client_input(
            session_id,
            3,
            TestInput { dx: 1.0 },
            0.016,
        );

        assert!(result.is_ok());
        assert_eq!(server_sim.simulation().x, 2.0);
    }

    #[test]
    fn test_multiple_clients() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::new(sim);

        let session1 = SessionId::new_v4();
        let session2 = SessionId::new_v4();

        server_sim
            .apply_client_input(session1, 1, TestInput { dx: 1.0 }, 0.016)
            .unwrap();

        server_sim
            .apply_client_input(session2, 1, TestInput { dx: 2.0 }, 0.016)
            .unwrap();

        assert_eq!(server_sim.simulation().x, 3.0); // Both inputs applied
        assert_eq!(server_sim.client_count(), 2);
        assert_eq!(server_sim.last_confirmed_sequence(session1), Some(1));
        assert_eq!(server_sim.last_confirmed_sequence(session2), Some(1));
    }

    #[test]
    fn test_remove_client() {
        let sim = TestSimulation { x: 0.0 };
        let mut server_sim = ServerSimulation::new(sim);
        let session_id = SessionId::new_v4();

        server_sim
            .apply_client_input(session_id, 1, TestInput { dx: 1.0 }, 0.016)
            .unwrap();

        assert_eq!(server_sim.client_count(), 1);

        server_sim.remove_client(session_id);

        assert_eq!(server_sim.client_count(), 0);
        assert_eq!(server_sim.last_confirmed_sequence(session_id), None);
    }
}
