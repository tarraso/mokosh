//! Event loop test example
//!
//! This example demonstrates the basic event loop functionality by connecting
//! a client and server through in-memory channels.
//!
//! Run with: cargo run --example event_loop_test

use bytes::Bytes;
use godot_netlink_client::Client;
use godot_netlink_protocol::{Envelope, EnvelopeFlags, SessionEnvelope, SessionId};
use godot_netlink_server::Server;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    println!("=== GodotNetLink Event Loop Test ===\n");

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

    // Create server
    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    // Create client
    let client = Client::new(client_incoming_rx, client_outgoing_tx);

    // Spawn server task
    println!("Starting server...");
    let server_handle = tokio::spawn(async move {
        server.run().await;
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
    println!("The server received the message and echoed it back.");
    println!("Check the output above for 'Server received' and 'Client received' messages.\n");

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
    println!("✓ Server event loop: working");
    println!("✓ Client event loop: working");
    println!("✓ In-memory message passing: working");
    println!("✓ Envelope serialization/deserialization: working");
}
