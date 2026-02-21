//! Integration tests for simulation layer
//!
//! Tests client-side prediction, server authoritative simulation,
//! and reconciliation algorithms.

use godot_netlink_protocol::SessionId;
use godot_netlink_protocol_derive::GameMessage;
use godot_netlink_simulation::{
    client_predictor::ClientPredictor,
    server_simulation::ServerSimulation,
    Simulation,
};
use serde::{Deserialize, Serialize};

// ============================================================================
// Test Simulation Implementation
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 300]
struct TestState {
    value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 301]
struct TestInput {
    delta: f32,
}

#[derive(Clone)]
struct TestSimulation {
    value: f32,
}

impl Simulation for TestSimulation {
    type Input = TestInput;
    type State = TestState;

    fn apply_input(&mut self, input: &TestInput, _delta_time: f32) {
        self.value += input.delta;
    }

    fn step(&mut self, delta_time: f32) {
        // Simple increment based on delta_time
        self.value += delta_time;
    }

    fn snapshot(&self) -> TestState {
        TestState { value: self.value }
    }

    fn restore(&mut self, state: &TestState) {
        self.value = state.value;
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_client_prediction_no_lag() {
    // Test that client prediction matches server when there's no lag

    let mut client_predictor = ClientPredictor::new(TestSimulation { value: 0.0 });
    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });
    let session_id = SessionId::new_v4();

    // Client applies input
    let input = TestInput { delta: 5.0 };
    let seq = client_predictor.apply_local_input(input.clone(), 0.016);
    client_predictor.step(0.016);

    // Server receives and applies the same input
    server_simulation
        .apply_client_input(session_id, seq, input, 0.016)
        .unwrap();
    server_simulation.step(0.016);

    // Client and server should match exactly
    let client_state = client_predictor.snapshot();
    let server_state = server_simulation.snapshot();

    assert_eq!(client_state.value, server_state.value);
    assert!((client_state.value - 5.016).abs() < 0.001); // 5.0 (input) + 0.016 (step)
}

#[test]
fn test_reconciliation_corrects_divergence() {
    // Test that reconciliation corrects client when prediction diverges from server

    let mut client_predictor = ClientPredictor::new(TestSimulation { value: 0.0 });
    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });
    let session_id = SessionId::new_v4();

    // Client predicts with one value
    let client_input = TestInput { delta: 10.0 };
    let seq = client_predictor.apply_local_input(client_input, 0.016);
    client_predictor.step(0.016);

    // Server receives different value (simulating lag or correction)
    let server_input = TestInput { delta: 8.0 }; // Different!
    server_simulation
        .apply_client_input(session_id, seq, server_input, 0.016)
        .unwrap();
    server_simulation.step(0.016);

    // Client should diverge from server at this point
    let client_state_before = client_predictor.snapshot();
    let server_state = server_simulation.snapshot();
    assert_ne!(client_state_before.value, server_state.value);

    // Reconcile - client should correct to match server
    let reconciled = client_predictor.reconcile_with_server(&server_state, seq);
    assert!(reconciled); // Divergence was detected

    let client_state_after = client_predictor.snapshot();
    assert_eq!(client_state_after.value, server_state.value);
}

#[test]
fn test_input_replay_during_reconciliation() {
    // Test that pending inputs are replayed correctly during reconciliation

    let mut client_predictor = ClientPredictor::new(TestSimulation { value: 0.0 });
    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });
    let session_id = SessionId::new_v4();

    // Client applies multiple inputs
    let input1 = TestInput { delta: 5.0 };
    let seq1 = client_predictor.apply_local_input(input1.clone(), 0.016);
    client_predictor.step(0.016);

    let input2 = TestInput { delta: 3.0 };
    let _seq2 = client_predictor.apply_local_input(input2.clone(), 0.016);
    client_predictor.step(0.016);

    // Server only receives first input
    server_simulation
        .apply_client_input(session_id, seq1, input1, 0.016)
        .unwrap();
    server_simulation.step(0.016);

    // Reconcile with server (which only has input1)
    let server_state = server_simulation.snapshot();
    client_predictor.reconcile_with_server(&server_state, seq1);

    // Client should have: server state (5.016) + replayed input2 (3.0) + step (0.016)
    let client_state = client_predictor.snapshot();
    let expected = 5.016 + 3.0;
    assert!((client_state.value - expected).abs() < 0.01);

    // Client should still have input2 pending
    assert_eq!(client_predictor.pending_input_count(), 1);
}

#[test]
fn test_multiple_clients_independent_simulation() {
    // Test that multiple clients can have independent simulations on the server

    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });

    let session1 = SessionId::new_v4();
    let session2 = SessionId::new_v4();

    // Client 1 applies input
    let input1 = TestInput { delta: 10.0 };
    server_simulation
        .apply_client_input(session1, 1, input1, 0.016)
        .unwrap();

    // Client 2 applies input
    let input2 = TestInput { delta: 20.0 };
    server_simulation
        .apply_client_input(session2, 1, input2, 0.016)
        .unwrap();

    server_simulation.step(0.016);

    // Server should have applied both inputs (cumulative)
    let server_state = server_simulation.snapshot();
    assert!((server_state.value - 30.016).abs() < 0.001); // 10 + 20 + 0.016

    // Both clients should be tracked
    assert_eq!(server_simulation.client_count(), 2);
    assert_eq!(server_simulation.last_confirmed_sequence(session1), Some(1));
    assert_eq!(server_simulation.last_confirmed_sequence(session2), Some(1));
}

#[test]
fn test_deterministic_simulation() {
    // Test that simulation is deterministic (same inputs = same results)

    let sim1 = TestSimulation { value: 0.0 };
    let sim2 = TestSimulation { value: 0.0 };

    let mut client1 = ClientPredictor::new(sim1);
    let mut client2 = ClientPredictor::new(sim2);

    // Apply same inputs to both clients
    for i in 0..10 {
        let input = TestInput {
            delta: (i as f32) * 0.5,
        };
        client1.apply_local_input(input.clone(), 0.016);
        client2.apply_local_input(input, 0.016);

        client1.step(0.016);
        client2.step(0.016);
    }

    // Results should be identical
    let state1 = client1.snapshot();
    let state2 = client2.snapshot();

    assert_eq!(state1.value, state2.value);
}

#[test]
fn test_server_rejects_invalid_sequence() {
    // Test that server rejects inputs with invalid sequence numbers

    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });
    let session_id = SessionId::new_v4();

    // Send input with sequence 1
    let input = TestInput { delta: 5.0 };
    server_simulation
        .apply_client_input(session_id, 1, input.clone(), 0.016)
        .unwrap();

    // Try to send duplicate sequence (should be rejected)
    let result = server_simulation.apply_client_input(session_id, 1, input.clone(), 0.016);
    assert!(result.is_err());

    // Try to send old sequence (should be rejected)
    let result = server_simulation.apply_client_input(session_id, 0, input, 0.016);
    assert!(result.is_err());

    // Server state should be unchanged (only first input applied)
    server_simulation.step(0.016);
    let state = server_simulation.snapshot();
    assert!((state.value - 5.016).abs() < 0.001); // Only first input applied
}

#[test]
fn test_client_removes_confirmed_inputs() {
    // Test that client removes inputs from buffer after server confirmation

    let mut client_predictor = ClientPredictor::new(TestSimulation { value: 0.0 });
    let mut server_simulation = ServerSimulation::new(TestSimulation { value: 0.0 });
    let session_id = SessionId::new_v4();

    // Client applies 3 inputs
    let input1 = TestInput { delta: 1.0 };
    let input2 = TestInput { delta: 2.0 };
    let input3 = TestInput { delta: 3.0 };

    let seq1 = client_predictor.apply_local_input(input1.clone(), 0.016);
    let seq2 = client_predictor.apply_local_input(input2.clone(), 0.016);
    let _seq3 = client_predictor.apply_local_input(input3.clone(), 0.016);

    assert_eq!(client_predictor.pending_input_count(), 3);

    // Server confirms first 2 inputs
    server_simulation
        .apply_client_input(session_id, seq1, input1, 0.016)
        .unwrap();
    server_simulation
        .apply_client_input(session_id, seq2, input2, 0.016)
        .unwrap();
    server_simulation.step(0.016);

    // Client reconciles (should remove confirmed inputs 1 and 2)
    let server_state = server_simulation.snapshot();
    client_predictor.reconcile_with_server(&server_state, seq2);

    assert_eq!(client_predictor.pending_input_count(), 1); // Only input3 remains
}
