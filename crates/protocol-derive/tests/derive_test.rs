//! Integration tests for GameMessage derive macro

use godot_netlink_protocol::message_registry::GameMessage;
use godot_netlink_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct TestMessage1 {
    x: f32,
    y: f32,
}

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 101]
struct TestMessage2 {
    x: f32,
    y: f32,
}

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 102]
struct DifferentMessage {
    a: i32,
    b: String,
}

#[test]
fn test_route_id_generation() {
    assert_eq!(TestMessage1::ROUTE_ID, 100);
    assert_eq!(TestMessage2::ROUTE_ID, 101);
    assert_eq!(DifferentMessage::ROUTE_ID, 102);
}

#[test]
fn test_schema_hash_stability() {
    // Same struct definition should produce same hash every time
    let hash1 = TestMessage1::SCHEMA_HASH;
    let hash2 = TestMessage1::SCHEMA_HASH;
    assert_eq!(hash1, hash2);
}

#[test]
fn test_schema_hash_includes_type_name() {
    // Different type names should have different schema hashes
    // even if fields are identical
    assert_ne!(TestMessage1::SCHEMA_HASH, TestMessage2::SCHEMA_HASH);
}

#[test]
fn test_schema_hash_different_structure() {
    // Different struct should have different schema hash
    assert_ne!(TestMessage1::SCHEMA_HASH, DifferentMessage::SCHEMA_HASH);
}

#[test]
fn test_schema_hash_non_zero() {
    // Schema hash should not be zero (unless extremely unlucky)
    assert_ne!(TestMessage1::SCHEMA_HASH, 0);
    assert_ne!(DifferentMessage::SCHEMA_HASH, 0);
}

// Test that field name changes produce different hash
#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 103]
struct MessageWithX {
    x: f32,
}

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 104]
struct MessageWithY {
    y: f32,
}

#[test]
fn test_field_name_affects_hash() {
    // Different field name should produce different hash
    assert_ne!(MessageWithX::SCHEMA_HASH, MessageWithY::SCHEMA_HASH);
}

// Test that field type changes produce different hash
#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 105]
struct MessageWithF32 {
    value: f32,
}

#[derive(Serialize, Deserialize, GameMessage)]
#[route_id = 106]
struct MessageWithI32 {
    value: i32,
}

#[test]
fn test_field_type_affects_hash() {
    // Different field type should produce different hash
    assert_ne!(MessageWithF32::SCHEMA_HASH, MessageWithI32::SCHEMA_HASH);
}
