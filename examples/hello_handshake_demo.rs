//! HELLO handshake demonstration
//!
//! This example demonstrates the HELLO protocol negotiation between client and server.
//!
//! Run with: cargo run --example hello_handshake_demo

use bytes::Bytes;
use godot_netlink_client::Client;
use godot_netlink_protocol::{Envelope, EnvelopeFlags, CURRENT_PROTOCOL_VERSION};
use godot_netlink_server::Server;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    println!("=== HELLO Handshake Demo ===\n");

    // Create communication channels
    let (client_to_server_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_to_client_tx, mut client_incoming_rx) = mpsc::channel(100);

    // Create server
    let server = Server::new(server_incoming_rx, server_to_client_tx);
    println!("Server created (version {:#06x})", CURRENT_PROTOCOL_VERSION);

    // Create client
    let mut client = Client::new(mpsc::channel(1).1, client_to_server_tx.clone());
    println!("Client created (version {:#06x})\n", CURRENT_PROTOCOL_VERSION);

    // Spawn server event loop
    tokio::spawn(async move {
        println!("[Server] Starting event loop...");
        server.run().await;
    });

    // Wait a bit for server to be ready
    sleep(Duration::from_millis(100)).await;

    // Client initiates HELLO
    println!("[Client] Initiating HELLO handshake...");
    client.connect().await.expect("Failed to send HELLO");

    // Wait for HELLO_OK
    println!("[Client] Waiting for HELLO_OK...\n");
    match client_incoming_rx.recv().await {
        Some(envelope) => {
            if envelope.route_id == 2 {
                // HELLO_OK
                println!("[Client] ✅ HELLO_OK received!");
                println!("[Client] Connection established\n");

                // Now we can send game messages
                println!("[Client] Connection ready for game messages");
                println!("[Client] Sending a test game message...");

                let game_envelope = Envelope::new_simple(
                    CURRENT_PROTOCOL_VERSION,
                    1,
                    0,
                    100, // game message route_id
                    2,
                    EnvelopeFlags::RELIABLE,
                    Bytes::from_static(b"Hello from game!"),
                );

                client_to_server_tx
                    .send(game_envelope)
                    .await
                    .expect("Failed to send game message");

                // Wait for echo
                if let Some(response) = client_incoming_rx.recv().await {
                    println!(
                        "[Client] ✅ Game message echoed back: {}",
                        String::from_utf8_lossy(&response.payload)
                    );
                }
            }
        }
        None => println!("[Client] ❌ Channel closed"),
    }

    println!("\n=== Demo Complete ===");
}
