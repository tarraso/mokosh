//! Integration tests for Message Registry
//!
//! Tests type-safe message sending/receiving and schema validation.

use godot_netlink_client::Client;
use godot_netlink_protocol::{
    calculate_global_schema_hash, GameMessage, MessageRegistry,
};
use godot_netlink_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ============================================================================
// Test Messages (Using Derive Macro)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 100]
struct TestMessage1 {
    value: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 101]
struct TestMessage2 {
    text: String,
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_type_safe_send() {
    // Arrange
    let (_, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);
    let mut client = Client::new(incoming_rx, outgoing_tx);

    let message = TestMessage1 { value: 42 };

    // Act
    client.send_message(message.clone()).await.unwrap();

    // Assert
    let envelope = outgoing_rx.recv().await.unwrap();
    assert_eq!(envelope.route_id, TestMessage1::ROUTE_ID);
    assert_eq!(envelope.schema_hash, TestMessage1::SCHEMA_HASH);
    assert_eq!(envelope.msg_id, 1); // First message
}

#[tokio::test]
async fn test_type_safe_send_multiple_messages() {
    // Arrange
    let (_, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);
    let mut client = Client::new(incoming_rx, outgoing_tx);

    // Act - send different message types
    client.send_message(TestMessage1 { value: 1 }).await.unwrap();
    client.send_message(TestMessage2 { text: "hello".to_string() }).await.unwrap();
    client.send_message(TestMessage1 { value: 2 }).await.unwrap();

    // Assert - correct route_id and schema_hash for each
    let env1 = outgoing_rx.recv().await.unwrap();
    assert_eq!(env1.route_id, 100);
    assert_eq!(env1.schema_hash, TestMessage1::SCHEMA_HASH); // Auto-generated
    assert_eq!(env1.msg_id, 1);

    let env2 = outgoing_rx.recv().await.unwrap();
    assert_eq!(env2.route_id, 101);
    assert_eq!(env2.schema_hash, TestMessage2::SCHEMA_HASH); // Auto-generated
    assert_eq!(env2.msg_id, 2);

    let env3 = outgoing_rx.recv().await.unwrap();
    assert_eq!(env3.route_id, 100);
    assert_eq!(env3.schema_hash, TestMessage1::SCHEMA_HASH); // Auto-generated
    assert_eq!(env3.msg_id, 3);
}

#[test]
fn test_message_registry() {
    // Arrange
    let mut registry = MessageRegistry::new();

    // Act
    registry.register::<TestMessage1>();
    registry.register::<TestMessage2>();

    // Assert
    assert_eq!(registry.len(), 2);
    assert!(registry.is_registered(100));
    assert!(registry.is_registered(101));
    assert!(!registry.is_registered(102));

    assert_eq!(registry.get_schema_hash(100), Some(TestMessage1::SCHEMA_HASH));
    assert_eq!(registry.get_schema_hash(101), Some(TestMessage2::SCHEMA_HASH));
    assert_eq!(registry.get_schema_hash(102), None);
}

#[test]
fn test_global_schema_hash() {
    // Arrange
    let hashes = vec![
        TestMessage1::SCHEMA_HASH,
        TestMessage2::SCHEMA_HASH,
    ];

    // Act
    let global = calculate_global_schema_hash(&hashes);

    // Assert - XOR of both hashes
    let expected = TestMessage1::SCHEMA_HASH ^ TestMessage2::SCHEMA_HASH;
    assert_eq!(global, expected);
}

#[test]
fn test_global_schema_hash_empty() {
    let hashes: Vec<u64> = vec![];
    let global = calculate_global_schema_hash(&hashes);
    assert_eq!(global, 0);
}

#[test]
fn test_global_schema_hash_single() {
    let hashes = vec![0xAAAA_BBBB_CCCC_DDDD];
    let global = calculate_global_schema_hash(&hashes);
    assert_eq!(global, 0xAAAA_BBBB_CCCC_DDDD);
}

#[test]
fn test_global_schema_hash_order_independent() {
    let hashes1 = vec![
        0x1111_1111_1111_1111,
        0x2222_2222_2222_2222,
        0x3333_3333_3333_3333,
    ];

    let hashes2 = vec![
        0x3333_3333_3333_3333,
        0x1111_1111_1111_1111,
        0x2222_2222_2222_2222,
    ];

    assert_eq!(
        calculate_global_schema_hash(&hashes1),
        calculate_global_schema_hash(&hashes2)
    );
}

#[test]
fn test_message_registry_global_hash() {
    let mut registry = MessageRegistry::new();
    registry.register::<TestMessage1>();
    registry.register::<TestMessage2>();

    let expected = TestMessage1::SCHEMA_HASH ^ TestMessage2::SCHEMA_HASH;
    assert_eq!(registry.global_schema_hash(), expected);
}

#[tokio::test]
async fn test_msg_id_auto_increment() {
    // Arrange
    let (_, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);
    let mut client = Client::new(incoming_rx, outgoing_tx);

    // Act - send 3 messages
    for _ in 0..3 {
        client.send_message(TestMessage1 { value: 1 }).await.unwrap();
    }

    // Assert - msg_id increments
    assert_eq!(outgoing_rx.recv().await.unwrap().msg_id, 1);
    assert_eq!(outgoing_rx.recv().await.unwrap().msg_id, 2);
    assert_eq!(outgoing_rx.recv().await.unwrap().msg_id, 3);
}
