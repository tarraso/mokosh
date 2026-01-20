//! Memory transport example for single-player games
//!
//! This example demonstrates using in-memory transport for scenarios where
//! both client and server run in the same process (e.g., single-player games
//! where the server logic runs locally for better performance than GDScript).
//!
//! Run with: cargo run --example memory_transport_test

use bytes::Bytes;
use godot_netlink_client::{transport::{memory::MemoryTransport, Transport}, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags, SessionEnvelope, SessionId};
use godot_netlink_server::Server;
use tokio::time::{sleep, Duration};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    println!("=== Memory Transport Example (Single-Player Mode) ===\n");
    println!("This example shows client and server communicating in the same process");
    println!("Perfect for single-player games with local Rust server logic!\n");

    // Create a pair of connected transports - super easy!
    let (client_transport, server_transport) = MemoryTransport::create_pair(100);

    // Server setup (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel(100);

    let server = Server::new(server_incoming_rx, server_outgoing_tx.clone());

    println!("[Server] Starting local server event loop...");
    tokio::spawn(async move {
        server.run().await;
    });

    // Create adapter for server transport
    let session_id = SessionId::new_v4();
    println!("[Server] Starting memory transport (no network!)...");
    tokio::spawn(async move {
        let (transport_incoming_tx, mut transport_incoming_rx) = mpsc::channel(100);
        let (transport_outgoing_tx, transport_outgoing_rx) = mpsc::channel(100);

        // Task to convert incoming Envelope to SessionEnvelope
        let server_incoming_tx_clone = server_incoming_tx.clone();
        tokio::spawn(async move {
            while let Some(envelope) = transport_incoming_rx.recv().await {
                let session_envelope = SessionEnvelope::new(session_id, envelope);
                if server_incoming_tx_clone.send(session_envelope).await.is_err() {
                    break;
                }
            }
        });

        // Task to convert outgoing SessionEnvelope to Envelope
        tokio::spawn(async move {
            while let Some(session_envelope) = server_outgoing_rx.recv().await {
                if transport_outgoing_tx.send(session_envelope.envelope).await.is_err() {
                    break;
                }
            }
        });

        if let Err(e) = server_transport.run(transport_incoming_tx, transport_outgoing_rx).await {
            eprintln!("[Server] Transport error: {}", e);
        }
    });

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = tokio::sync::mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = tokio::sync::mpsc::channel(100);

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    println!("[Client] Starting client event loop...");
    tokio::spawn(async move {
        client.run().await;
    });

    println!("[Client] Starting memory transport...\n");
    tokio::spawn(async move {
        if let Err(e) = client_transport.run(client_incoming_tx, client_outgoing_rx).await {
            eprintln!("[Client] Transport error: {}", e);
        }
    });

    sleep(Duration::from_millis(100)).await;

    // Simulate game loop sending multiple commands
    println!("=== Simulating Game Loop ===\n");

    for i in 1..=5 {
        let command = match i {
            1 => "PlayerMove(x: 100, y: 200)",
            2 => "PlayerAttack(target_id: 42)",
            3 => "PickupItem(item_id: 123)",
            4 => "UseAbility(ability_id: 5)",
            5 => "PlayerChat(message: 'Hello!')",
            _ => unreachable!(),
        };

        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            (100 + i) as u16,
            i as u64,
            EnvelopeFlags::RELIABLE,
            Bytes::from(command),
        );

        println!("[Client] Sending: {} (route_id={}, msg_id={})",
            command, envelope.route_id, envelope.msg_id);

        if let Err(e) = client_outgoing_tx.send(envelope).await {
            eprintln!("Failed to send: {}", e);
        }

        sleep(Duration::from_millis(200)).await;
    }

    sleep(Duration::from_millis(500)).await;

    println!("\n=== Benefits of Memory Transport ===");
    println!("✓ Zero network overhead");
    println!("✓ Perfect for single-player games");
    println!("✓ Fast development and testing");
    println!("✓ Server logic in Rust (faster than GDScript)");
    println!("✓ Easy to switch to WebSocket for multiplayer");
    println!("\n=== Example completed ===");
}
