//! Message Registry Demo
//!
//! This example demonstrates the type-safe message API with GameMessage trait.
//!
//! ## What it shows:
//!
//! 1. Defining game messages with `GameMessage` trait
//! 2. Type-safe `send_message()` API
//! 3. Schema hash generation and validation
//! 4. Comparison: Old way (manual Envelope) vs New way (type-safe)
//!
//! ## Run:
//!
//! ```bash
//! cargo run --example message_registry_demo
//! ```

use godot_netlink_client::Client;
use godot_netlink_protocol::{
    calculate_global_schema_hash, codec::Codec, Envelope, EnvelopeFlags, GameMessage, MessageRegistry,
    SessionId, CURRENT_PROTOCOL_VERSION,
};
use godot_netlink_protocol_derive::GameMessage;
use godot_netlink_server::Server;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ============================================================================
// Example Game Messages (Using Derive Macro)
// ============================================================================

/// Player input from client (movement, jump, etc.)
///
/// Now using #[derive(GameMessage)] with auto SCHEMA_HASH generation!
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 100]
struct PlayerInput {
    sequence: u32,
    x: f32,
    y: f32,
    jump: bool,
}

/// Player state from server (position, health, etc.)
///
/// SCHEMA_HASH is automatically generated from struct definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 101]
struct PlayerState {
    player_id: u32,
    x: f32,
    y: f32,
    health: u32,
}

/// Chat message (bidirectional)
///
/// No more manual SCHEMA_HASH - it's derived from fields!
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 102]
struct ChatMessage {
    sender: String,
    text: String,
}

// ============================================================================
// Demo Functions
// ============================================================================

/// Demonstrates the OLD way of sending messages (manual Envelope creation)
async fn demo_old_way() {
    println!("\n=== OLD WAY (Manual Envelope) ===\n");

    let (_, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let client = Client::new(incoming_rx, outgoing_tx);

    // Manual message creation - verbose and error-prone
    let player_input = PlayerInput {
        sequence: 42,
        x: 10.0,
        y: 20.0,
        jump: true,
    };

    // Need to know codec, serialize manually
    let codec = godot_netlink_protocol::codec::JsonCodec;
    let payload = codec.encode(&player_input).unwrap();

    // Manual Envelope construction - lots of parameters to remember!
    let envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,                              // codec_id - easy to get wrong
        0x1111_2222_3333_4444,          // schema_hash - manual, can forget to update
        100,                            // route_id - manual, typo-prone
        1,                              // msg_id - need to track manually
        EnvelopeFlags::RELIABLE,
        payload,
    );

    client.send(envelope).await.unwrap();

    let sent_envelope = outgoing_rx.recv().await.unwrap();
    println!("✅ Sent envelope (old way):");
    println!("   route_id: {}", sent_envelope.route_id);
    println!("   schema_hash: {:#018x}", sent_envelope.schema_hash);
    println!("   payload_len: {} bytes", sent_envelope.payload_len);
    println!("\n⚠️  Problems:");
    println!("   - 15+ lines of boilerplate");
    println!("   - Manual route_id (can forget or typo)");
    println!("   - Manual schema_hash (can forget to update)");
    println!("   - No compile-time type checking");
}

/// Demonstrates the NEW way of sending messages (type-safe API)
async fn demo_new_way() {
    println!("\n=== NEW WAY (Type-Safe API) ===\n");

    let (_, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::new(incoming_rx, outgoing_tx);

    // Type-safe message creation - just the data!
    let player_input = PlayerInput {
        sequence: 42,
        x: 10.0,
        y: 20.0,
        jump: true,
    };

    // ONE LINE - route_id and schema_hash automatic!
    client.send_message(player_input).await.unwrap();

    let sent_envelope = outgoing_rx.recv().await.unwrap();
    println!("✅ Sent envelope (new way):");
    println!("   route_id: {} (automatic from PlayerInput::ROUTE_ID)", sent_envelope.route_id);
    println!("   schema_hash: {:#018x} (automatic from PlayerInput::SCHEMA_HASH)", sent_envelope.schema_hash);
    println!("   payload_len: {} bytes", sent_envelope.payload_len);
    println!("\n✨ Benefits:");
    println!("   - 1 line instead of 15");
    println!("   - Compile-time type safety");
    println!("   - Automatic route_id and schema_hash");
    println!("   - Can't forget to update schema_hash");
}

/// Demonstrates MessageRegistry and global schema hash
fn demo_message_registry() {
    println!("\n=== Message Registry ===\n");

    let mut registry = MessageRegistry::new();

    // Register all game messages
    registry.register::<PlayerInput>();
    registry.register::<PlayerState>();
    registry.register::<ChatMessage>();

    println!("✅ Registered {} message types:", registry.len());
    println!("   - PlayerInput  (route_id={}, schema_hash={:#018x})",
        PlayerInput::ROUTE_ID, PlayerInput::SCHEMA_HASH);
    println!("   - PlayerState  (route_id={}, schema_hash={:#018x})",
        PlayerState::ROUTE_ID, PlayerState::SCHEMA_HASH);
    println!("   - ChatMessage  (route_id={}, schema_hash={:#018x})",
        ChatMessage::ROUTE_ID, ChatMessage::SCHEMA_HASH);

    let global_hash = registry.global_schema_hash();
    println!("\n📊 Global schema hash: {:#018x}", global_hash);
    println!("   (XOR of all individual schema hashes)");

    // Schema validation simulation
    println!("\n🔒 Schema Validation:");
    println!("   Client sends: global_hash={:#018x}", global_hash);
    println!("   Server expects: global_hash={:#018x}", global_hash);
    println!("   ✅ Match! Connection allowed.");

    // Simulate version mismatch
    let wrong_hash = 0xDEAD_BEEF_CAFE_BABEu64;
    println!("\n   Client sends: global_hash={:#018x}", wrong_hash);
    println!("   Server expects: global_hash={:#018x}", global_hash);
    println!("   ❌ Mismatch! Connection rejected with HELLO_ERROR.");
}

/// Demonstrates calculation of global schema hash from multiple messages
fn demo_global_schema_hash() {
    println!("\n=== Global Schema Hash Calculation ===\n");

    // Collect all schema hashes
    let hashes = vec![
        PlayerInput::SCHEMA_HASH,
        PlayerState::SCHEMA_HASH,
        ChatMessage::SCHEMA_HASH,
    ];

    println!("Individual message schema hashes:");
    for (i, hash) in hashes.iter().enumerate() {
        println!("   Message {}: {:#018x}", i + 1, hash);
    }

    // Calculate global hash (XOR of all hashes)
    let global_hash = calculate_global_schema_hash(&hashes);

    println!("\nGlobal schema hash (XOR):");
    println!("   {:#018x} ^ {:#018x} ^ {:#018x}", hashes[0], hashes[1], hashes[2]);
    println!("   = {:#018x}", global_hash);

    println!("\n💡 Why XOR?");
    println!("   - Commutative: order doesn't matter");
    println!("   - Fast: O(n) single pass");
    println!("   - Changes if ANY message schema changes");
}

/// Demonstrates server-side type-safe messaging
async fn demo_server_type_safe() {
    println!("\n=== Server Type-Safe Messaging ===\n");

    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut server = Server::new(incoming_rx, outgoing_tx);

    let session_id = SessionId::new_v4();

    // Type-safe server send
    let player_state = PlayerState {
        player_id: 1,
        x: 100.0,
        y: 200.0,
        health: 100,
    };

    server.send_message(session_id, player_state).await.unwrap();

    let sent = outgoing_rx.recv().await.unwrap();
    println!("✅ Server sent PlayerState:");
    println!("   route_id: {} (automatic)", sent.envelope.route_id);
    println!("   schema_hash: {:#018x} (automatic)", sent.envelope.schema_hash);
    println!("   session_id: {}", sent.session_id);
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║         GodotNetLink Message Registry Demo                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    demo_old_way().await;
    demo_new_way().await;
    demo_message_registry();
    demo_global_schema_hash();
    demo_server_type_safe().await;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║                     Demo Complete!                          ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}
