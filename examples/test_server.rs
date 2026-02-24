//! Simple test server for Godot demo
//!
//! Now with automatic welcome messages and position broadcast!

use mokosh_server::{Server, transport::websocket::WebSocketServer};
use tokio::sync::mpsc;
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    println!("🚀 Game server starting on ws://{}", addr);
    println!("📨 Welcome messages enabled!");
    println!("📡 Position broadcast enabled!\n");

    // Create channels for server
    let (incoming_tx, incoming_rx) = mpsc::channel(100);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(100);

    // Create and spawn WebSocket transport
    let transport = WebSocketServer::new(addr);
    tokio::spawn(async move {
        if let Err(e) = transport.run(incoming_tx, outgoing_rx, None).await {
            eprintln!("❌ Transport error: {}", e);
        }
    });

    // Create and run server (now handles welcome and broadcast automatically)
    let server = Server::new(incoming_rx, outgoing_tx);
    println!("✅ Server ready, waiting for clients...\n");

    server.run().await;
}
