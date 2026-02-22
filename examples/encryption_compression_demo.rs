//! Encryption and Compression demonstration
//!
//! This example demonstrates the encryption and compression features of GodotNetLink.
//! It shows how to configure clients with different combinations of compression and encryption,
//! and compares the resulting payload sizes.
//!
//! Run with:
//! ```sh
//! cargo run --example encryption_compression_demo
//! ```

use godot_netlink_client::Client;
use godot_netlink_protocol::{
    compression::{Lz4Compressor, NoCompressor, ZstdCompressor},
    encryption::{ChaCha20Poly1305Encryptor, NoEncryptor},
};
use godot_netlink_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct GameState {
    player_positions: Vec<(f32, f32, f32)>,
    enemy_positions: Vec<(f32, f32, f32)>,
    terrain_data: String,
    frame: u64,
}

impl GameState {
    fn new() -> Self {
        // Create a realistic game state with repeated data (compressible)
        let mut player_positions = Vec::new();
        for i in 0..10 {
            player_positions.push((i as f32, i as f32 * 2.0, i as f32 * 3.0));
        }

        let mut enemy_positions = Vec::new();
        for i in 0..50 {
            enemy_positions.push((i as f32 * 0.5, i as f32, i as f32 * 1.5));
        }

        Self {
            player_positions,
            enemy_positions,
            terrain_data: "GRASS|GRASS|WATER|GRASS|STONE|".repeat(20),
            frame: 12345,
        }
    }
}

#[tokio::main]
async fn main() {
    println!("=== GodotNetLink Encryption & Compression Demo ===\n");

    let game_state = GameState::new();
    println!("Game State:");
    println!("  - Player positions: {}", game_state.player_positions.len());
    println!("  - Enemy positions: {}", game_state.enemy_positions.len());
    println!("  - Terrain data length: {} chars", game_state.terrain_data.len());
    println!();

    // Test different configurations
    println!("Testing different configurations:\n");

    // 1. No compression, no encryption (baseline)
    let size_plain = test_plain(game_state.clone()).await;

    // 2. Zstd compression only
    let size_zstd = test_zstd(game_state.clone()).await;

    // 3. Lz4 compression only
    let size_lz4 = test_lz4(game_state.clone()).await;

    // 4. Encryption only
    let encryption_key = [0x42; 32];
    let size_encrypted = test_encrypted(encryption_key, game_state.clone()).await;

    // 5. Zstd + Encryption
    let size_zstd_encrypted = test_zstd_encrypted(encryption_key, game_state.clone()).await;

    // 6. Lz4 + Encryption
    let size_lz4_encrypted = test_lz4_encrypted(encryption_key, game_state.clone()).await;

    // Summary
    println!("\n=== Size Comparison ===");
    println!("Plain:                {:5} bytes (baseline)", size_plain);
    println!("Zstd:                 {:5} bytes ({:5.1}% of baseline)", size_zstd, percentage(size_zstd, size_plain));
    println!("Lz4:                  {:5} bytes ({:5.1}% of baseline)", size_lz4, percentage(size_lz4, size_plain));
    println!("Encrypted:            {:5} bytes ({:5.1}% of baseline)", size_encrypted, percentage(size_encrypted, size_plain));
    println!("Zstd + Encrypted:     {:5} bytes ({:5.1}% of baseline)", size_zstd_encrypted, percentage(size_zstd_encrypted, size_plain));
    println!("Lz4 + Encrypted:      {:5} bytes ({:5.1}% of baseline)", size_lz4_encrypted, percentage(size_lz4_encrypted, size_plain));

    println!("\n=== Key Takeaways ===");
    println!("• Compression reduces payload size for repeated/structured data");
    println!("• Zstd provides better compression ratio than Lz4");
    println!("• Lz4 is faster but with slightly lower compression");
    println!("• Encryption adds ~28 bytes overhead (12-byte nonce + 16-byte auth tag)");
    println!("• Compression + Encryption = best bandwidth efficiency + security");
    println!("\nRecommendation:");
    println!("  - Use Zstd + Encryption for best security and bandwidth efficiency");
    println!("  - Use Lz4 + Encryption if CPU is a constraint");
    println!("  - Use plain only for debugging or very small messages");
}

async fn test_plain(message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::new(incoming_rx, outgoing_tx);
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "Plain (no compression, no encryption)", payload_size);
    payload_size
}

async fn test_zstd(message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        ZstdCompressor::new(),
        NoEncryptor,
        None, // No game messages channel
    );
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "Zstd compression only", payload_size);
    payload_size
}

async fn test_lz4(message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        Lz4Compressor::new(),
        NoEncryptor,
        None, // No game messages channel
    );
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "Lz4 compression only", payload_size);
    payload_size
}

async fn test_encrypted(encryption_key: [u8; 32], message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        NoCompressor,
        ChaCha20Poly1305Encryptor::new(&encryption_key),
        None, // No game messages channel
    );
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "ChaCha20-Poly1305 encryption only", payload_size);
    payload_size
}

async fn test_zstd_encrypted(encryption_key: [u8; 32], message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        ZstdCompressor::new(),
        ChaCha20Poly1305Encryptor::new(&encryption_key),
        None, // No game messages channel
    );
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "Zstd compression + encryption", payload_size);
    payload_size
}

async fn test_lz4_encrypted(encryption_key: [u8; 32], message: GameState) -> usize {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        Lz4Compressor::new(),
        ChaCha20Poly1305Encryptor::new(&encryption_key),
        None, // No game messages channel
    );
    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();
    let payload_size = envelope.payload.len();

    println!("{:40} -> {:5} bytes", "Lz4 compression + encryption", payload_size);
    payload_size
}

fn percentage(size: usize, baseline: usize) -> f64 {
    (size as f64 / baseline as f64) * 100.0
}
