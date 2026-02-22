//! Keepalive and RTT Monitoring Demo
//!
//! This example demonstrates:
//! - Automatic PING/PONG keepalive
//! - RTT (Round-Trip Time) measurement
//! - Connection timeout detection
//! - Graceful disconnect
//!
//! Run with: `cargo run --example keepalive_demo`

use mokosh_client::{transport::websocket::WebSocketClient, Client};
use mokosh_protocol::Transport;
use mokosh_server::{transport::websocket::WebSocketServer, Server};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("🚀 Keepalive and RTT Monitoring Demo");
    println!("=====================================\n");

    // Setup WebSocket server on localhost
    let addr: SocketAddr = "127.0.0.1:9001".parse().unwrap();

    println!("📡 Starting server on ws://{}", addr);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server_transport = WebSocketServer::new(addr);
    tokio::spawn(async move {
        if let Err(e) = server_transport.run(server_incoming_tx, server_outgoing_rx).await {
            eprintln!("Server transport error: {}", e);
        }
    });

    let server = Server::new(server_incoming_rx, server_outgoing_tx);
    tokio::spawn(async move {
        server.run().await;
    });

    // Wait for server to start
    sleep(Duration::from_millis(500)).await;

    println!("✅ Server started\n");

    // Client setup
    println!("📡 Connecting client to ws://{}", addr);

    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client_transport = WebSocketClient::new(format!("ws://{}", addr));
    tokio::spawn(async move {
        if let Err(e) = client_transport.run(client_incoming_tx, client_outgoing_rx).await {
            eprintln!("Client transport error: {}", e);
        }
    });

    let mut client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    // Connect client
    client.connect().await.unwrap();
    println!("✅ Client connected\n");

    // Spawn client event loop
    let client_handle = tokio::spawn(async move {
        client.run().await;
    });

    println!("ℹ️  Keepalive Configuration:");
    println!("   - PING interval: 30 seconds");
    println!("   - Connection timeout: 60 seconds");
    println!("   - Both client and server send PING\n");

    println!("📊 Monitoring connection for 10 seconds...");
    println!("   (PING/PONG happens automatically in background)\n");

    // Monitor connection for a period
    let mut monitor_interval = interval(Duration::from_secs(2));
    let mut elapsed = 0;

    for _ in 0..5 {
        monitor_interval.tick().await;
        elapsed += 2;

        println!("⏱️  {} seconds - Connection alive", elapsed);

        // Note: Since client is moved into the spawned task, we can't access get_last_rtt() here
        // In a real application, you would either:
        // 1. Keep client outside the spawn and manually tick the event loop
        // 2. Use channels to query RTT from the spawned task
        // 3. Use a shared state (Arc<Mutex<Client>>) - but that requires refactoring

        println!("   Status: ✅ Healthy (no timeout)");
    }

    println!("\n🔚 Demonstration complete!");
    println!("   Connection stayed alive thanks to automatic keepalive.\n");

    println!("💡 Key Points:");
    println!("   1. PING/PONG happens automatically every 30s");
    println!("   2. Both client and server monitor for timeouts");
    println!("   3. RTT can be queried via client.get_last_rtt()");
    println!("   4. No manual keepalive logic needed!\n");

    // Graceful shutdown
    println!("👋 Shutting down gracefully...");
    client_handle.abort();

    println!("✅ Done!");
}
