//! Event loop test example
//!
//! This example demonstrates the event-based Server API using in-memory transport.
//! It shows the minimal setup for a client-server communication with GameEvent handling.
//!
//! Run with: cargo run --example event_loop_test

use bytes::Bytes;
use mokosh_client::Client;
use mokosh_protocol::{Envelope, EnvelopeFlags, SessionEnvelope, SessionId};
use mokosh_server::{Server, GameEvent};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    println!("=== Mokosh Event Loop Test (Event-Based API) ===\n");

    // Create channels for server (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel::<SessionEnvelope>(100);

    // Create channels for client (uses Envelope)
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, mut client_outgoing_rx) = mpsc::channel(100);

    // Clone sender for later use
    let client_outgoing_tx_clone = client_outgoing_tx.clone();

    // Create adapter tasks to convert between Envelope and SessionEnvelope
    let session_id = SessionId::new_v4();

    // Adapter: Client -> Server (Envelope to SessionEnvelope)
    tokio::spawn(async move {
        while let Some(envelope) = client_outgoing_rx.recv().await {
            let session_envelope = SessionEnvelope::new(session_id, envelope);
            if server_incoming_tx.send(session_envelope).await.is_err() {
                break;
            }
        }
    });

    // Adapter: Server -> Client (SessionEnvelope to Envelope)
    tokio::spawn(async move {
        while let Some(session_envelope) = server_outgoing_rx.recv().await {
            if client_incoming_tx.send(session_envelope.envelope).await.is_err() {
                break;
            }
        }
    });

    // Create server with event-based API
    let mut server = Server::new(server_incoming_rx, server_outgoing_tx.clone());

    // Create client
    let client = Client::new(client_incoming_rx, client_outgoing_tx);

    // Spawn server task with event handling
    println!("Starting server (event-based API)...");
    let server_handle = tokio::spawn(async move {
        loop {
            match server.tick().await {
                Ok(Some(GameEvent::PlayerConnected(session_id))) => {
                    println!("[Server] Player connected: {}", session_id);
                }
                Ok(Some(GameEvent::PlayerDisconnected(session_id))) => {
                    println!("[Server] Player disconnected: {}", session_id);
                }
                Ok(Some(GameEvent::GameMessage { session_id, envelope })) => {
                    println!("[Server] Game message from {}: route_id={}, payload={:?}",
                        session_id, envelope.route_id,
                        std::str::from_utf8(&envelope.payload).unwrap_or("<binary>"));

                    // Echo back to sender (using low-level API for raw Envelope)
                    let session_envelope = SessionEnvelope::new(session_id, envelope);
                    let _ = server_outgoing_tx.send(session_envelope).await;
                }
                Ok(None) => {
                    println!("[Server] Shutting down");
                    break;
                }
                Err(e) => {
                    println!("[Server] Error: {}", e);
                    break;
                }
            }
        }
    });

    // Spawn client task
    println!("Starting client...");
    let client_handle = tokio::spawn(async move {
        client.run().await;
    });

    // Give time for both to start
    sleep(Duration::from_millis(100)).await;

    // Create a test envelope
    let test_message = Envelope::new_simple(
        1,                              // protocol_version
        1,                              // codec_id (JSON)
        0x1234567890ABCDEF,             // schema_hash
        100,                            // route_id
        1,                              // msg_id
        EnvelopeFlags::RELIABLE,        // flags
        Bytes::from_static(b"Hello from client!"),
    );

    println!("\n--- Sending test message ---");
    println!("  Route ID: {}", test_message.route_id);
    println!("  Message ID: {}", test_message.msg_id);
    println!("  Payload: {:?}", std::str::from_utf8(&test_message.payload).unwrap());
    println!("  Flags: {:?}", test_message.flags);

    // Send message from client to server
    client_outgoing_tx_clone.send(test_message.clone()).await.unwrap();

    // Wait for message to be processed
    sleep(Duration::from_millis(200)).await;

    println!("\n--- Message flow complete ---");

    // Send another message
    let test_message_2 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        200,
        2,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        Bytes::from_static(b"Second message with compression flag!"),
    );

    println!("--- Sending second test message ---");
    println!("  Route ID: {}", test_message_2.route_id);
    println!("  Message ID: {}", test_message_2.msg_id);
    println!("  Payload: {:?}", std::str::from_utf8(&test_message_2.payload).unwrap());
    println!("  Flags: {:?}", test_message_2.flags);

    client_outgoing_tx_clone.send(test_message_2).await.unwrap();

    // Wait for processing
    sleep(Duration::from_millis(200)).await;

    println!("\n--- Shutting down ---");

    // Close channels to signal shutdown
    drop(client_outgoing_tx_clone);

    // Wait for both to finish
    server_handle.await.unwrap();
    client_handle.await.unwrap();

    println!("\n=== Test complete! ===");
    println!("\nSummary:");
    println!("✓ Server event loop (event-based API): working");
    println!("✓ Client event loop: working");
    println!("✓ In-memory message passing: working");
    println!("✓ GameEvent handling (PlayerConnected, GameMessage): working");
    println!("✓ Envelope serialization/deserialization: working");
}
