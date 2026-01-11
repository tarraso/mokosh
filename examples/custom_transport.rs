//! Custom transport example
//!
//! This example shows how to implement a custom transport layer
//! (in-memory channels) for testing or special use cases.
//!
//! Run with: cargo run --example custom_transport

use async_trait::async_trait;
use bytes::Bytes;
use godot_netlink_client::{transport::Transport as ClientTransport, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags};
use godot_netlink_server::{transport::Transport as ServerTransport, Server};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

/// In-memory client transport for testing (no network required)
pub struct MemoryClientTransport {
    /// Channel to receive envelopes from the other side
    rx: mpsc::Receiver<Envelope>,
    /// Channel to send envelopes to the other side
    tx: mpsc::Sender<Envelope>,
}

impl MemoryClientTransport {
    pub fn new(tx: mpsc::Sender<Envelope>, rx: mpsc::Receiver<Envelope>) -> Self {
        Self { rx, tx }
    }
}

#[async_trait]
impl ClientTransport for MemoryClientTransport {
    type Error = MemoryTransportError;

    async fn run(
        mut self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                // Receive from other side, forward to event loop
                Some(envelope) = self.rx.recv() => {
                    if incoming_tx.send(envelope).await.is_err() {
                        break;
                    }
                }

                // Receive from event loop, send to other side
                Some(envelope) = outgoing_rx.recv() => {
                    if self.tx.send(envelope).await.is_err() {
                        break;
                    }
                }

                else => break,
            }
        }
        Ok(())
    }
}

/// In-memory server transport for testing (no network required)
pub struct MemoryServerTransport {
    /// Channel to receive envelopes from the other side
    rx: mpsc::Receiver<Envelope>,
    /// Channel to send envelopes to the other side
    tx: mpsc::Sender<Envelope>,
}

impl MemoryServerTransport {
    pub fn new(tx: mpsc::Sender<Envelope>, rx: mpsc::Receiver<Envelope>) -> Self {
        Self { rx, tx }
    }
}

#[async_trait]
impl ServerTransport for MemoryServerTransport {
    type Error = MemoryTransportError;

    async fn run(
        mut self,
        incoming_tx: mpsc::Sender<Envelope>,
        mut outgoing_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                // Receive from other side, forward to event loop
                Some(envelope) = self.rx.recv() => {
                    if incoming_tx.send(envelope).await.is_err() {
                        break;
                    }
                }

                // Receive from event loop, send to other side
                Some(envelope) = outgoing_rx.recv() => {
                    if self.tx.send(envelope).await.is_err() {
                        break;
                    }
                }

                else => break,
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryTransportError {
    #[error("Channel closed")]
    ChannelClosed,
}

#[tokio::main]
async fn main() {
    println!("=== Custom Transport Example ===\n");
    println!("Using in-memory transport (no network)\n");

    // Create bidirectional channels for the memory transport
    let (client_to_server_tx, client_to_server_rx) = mpsc::channel(100);
    let (server_to_client_tx, server_to_client_rx) = mpsc::channel(100);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server = Server::new(server_incoming_rx, server_outgoing_tx.clone());
    let server_transport = MemoryServerTransport::new(server_to_client_tx, client_to_server_rx);

    tokio::spawn(async move {
        println!("[Server] Starting event loop");
        server.run().await;
    });

    tokio::spawn(async move {
        println!("[Server] Starting memory transport");
        if let Err(e) = server_transport.run(server_incoming_tx, server_outgoing_rx).await {
            eprintln!("[Server] Transport error: {}", e);
        }
    });

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());
    let client_transport = MemoryClientTransport::new(client_to_server_tx, server_to_client_rx);

    tokio::spawn(async move {
        println!("[Client] Starting event loop");
        client.run().await;
    });

    tokio::spawn(async move {
        println!("[Client] Starting memory transport");
        if let Err(e) = client_transport.run(client_incoming_tx, client_outgoing_rx).await {
            eprintln!("[Client] Transport error: {}", e);
        }
    });

    sleep(Duration::from_millis(100)).await;

    // Send test message from client
    let test_message = Envelope::new_simple(
        1,
        1,
        0,
        42,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from("Hello from custom transport!"),
    );

    println!("\n[Client] Sending message: route_id={}, msg_id={}",
        test_message.route_id, test_message.msg_id);

    if let Err(e) = client_outgoing_tx.send(test_message).await {
        eprintln!("Failed to send: {}", e);
    }

    sleep(Duration::from_secs(1)).await;

    println!("\n=== Example completed ===");
}
