//! WebSocket echo test example
//!
//! This example demonstrates WebSocket transport by running a server and client
//! that communicate over a real network connection.
//!
//! Run with: cargo run --example websocket_echo_test

use bytes::Bytes;
use godot_netlink_client::{transport::websocket::WebSocketClient, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags, Transport};
use godot_netlink_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    println!("=== GodotNetLink WebSocket Echo Test ===\n");

    let addr: SocketAddr = "127.0.0.1:9090".parse().unwrap();

    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let ws_server = WebSocketServer::new(addr);

    println!("Starting WebSocket server on {}...", addr);
    let server_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_server.run(server_incoming_tx, server_outgoing_rx).await {
            eprintln!("WebSocket server error: {}", e);
        }
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    let server_loop_handle = tokio::spawn(async move {
        server.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let ws_client = WebSocketClient::new(format!("ws://{}", addr));

    println!("Connecting WebSocket client to {}...", addr);
    let client_transport_handle = tokio::spawn(async move {
        if let Err(e) = ws_client.run(client_incoming_tx, client_outgoing_rx).await {
            eprintln!("WebSocket client error: {}", e);
        }
    });

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_loop_handle = tokio::spawn(async move {
        client.run().await;
    });

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Sending test message 1 ---");
    let test_message_1 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        100,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello from WebSocket client!"),
    );

    println!("  Route ID: {}", test_message_1.route_id);
    println!("  Message ID: {}", test_message_1.msg_id);
    println!(
        "  Payload: {:?}",
        std::str::from_utf8(&test_message_1.payload).unwrap()
    );

    client_outgoing_tx
        .send(test_message_1.clone())
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Sending test message 2 ---");
    let test_message_2 = Envelope::new_simple(
        1,
        1,
        0x1234567890ABCDEF,
        200,
        2,
        EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED,
        Bytes::from_static(b"Second message with compression flag!"),
    );

    println!("  Route ID: {}", test_message_2.route_id);
    println!("  Message ID: {}", test_message_2.msg_id);
    println!(
        "  Payload: {:?}",
        std::str::from_utf8(&test_message_2.payload).unwrap()
    );

    client_outgoing_tx.send(test_message_2).await.unwrap();

    sleep(Duration::from_millis(500)).await;

    println!("\n--- Test complete ---");
    println!("\nNote: Full echo functionality requires per-connection routing,");
    println!("which will be implemented in the next iteration.");
    println!("\nFor now, verify that:");
    println!("✓ WebSocket server accepts connections");
    println!("✓ WebSocket client connects successfully");
    println!("✓ Messages are sent from client to server");
    println!("✓ Server receives and processes envelopes");

    drop(client_outgoing_tx);

    sleep(Duration::from_millis(100)).await;

    server_transport_handle.abort();
    server_loop_handle.abort();
    client_transport_handle.abort();
    client_loop_handle.abort();

    println!("\n=== Test finished ===");
}
