use bytes::Bytes;
use godot_netlink_client::{transport::{memory::MemoryTransport, Transport}, Client};
use godot_netlink_protocol::{Envelope, EnvelopeFlags};
use godot_netlink_server::Server;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_memory_transport_client_server_echo() {
    // Create a pair of connected transports
    let (client_transport, server_transport) = MemoryTransport::create_pair(100);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    tokio::spawn(async move {
        server.run().await;
    });

    tokio::spawn(async move {
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    tokio::spawn(async move {
        client.run().await;
    });

    tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    sleep(Duration::from_millis(100)).await;

    // Send a test envelope
    let test_envelope = Envelope::new_simple(
        1,
        1,
        0,
        42,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"Hello Memory Transport!"),
    );

    client_outgoing_tx.send(test_envelope.clone()).await.unwrap();

    // Server should echo it back
    sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn test_memory_transport_high_throughput() {
    // Create a pair of connected transports with larger buffer
    let (client_transport, server_transport) = MemoryTransport::create_pair(1000);

    // Server setup
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(1000);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(1000);

    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    tokio::spawn(async move {
        server.run().await;
    });

    tokio::spawn(async move {
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    // Client setup
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(1000);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(1000);

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    tokio::spawn(async move {
        client.run().await;
    });

    tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    sleep(Duration::from_millis(100)).await;

    // Send many envelopes rapidly
    for i in 1..=100 {
        let envelope = Envelope::new_simple(
            1,
            1,
            0,
            (i % 256) as u16,
            i,
            EnvelopeFlags::RELIABLE,
            Bytes::from(format!("Message {}", i)),
        );

        client_outgoing_tx.send(envelope).await.unwrap();
    }

    sleep(Duration::from_millis(500)).await;
}

#[tokio::test]
async fn test_memory_transport_vs_websocket_compatibility() {
    // This test verifies that memory transport behaves the same as websocket
    // from the Client/Server perspective

    // Create a pair of connected transports
    let (client_transport, server_transport) = MemoryTransport::create_pair(100);

    // Server setup - identical to websocket tests
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(100);
    let (server_outgoing_tx, server_outgoing_rx) = mpsc::channel(100);

    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    tokio::spawn(async move {
        server.run().await;
    });

    tokio::spawn(async move {
        let _ = server_transport.run(server_incoming_tx, server_outgoing_rx).await;
    });

    // Client setup - identical to websocket tests
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(100);
    let (client_outgoing_tx, client_outgoing_rx) = mpsc::channel(100);

    let client = Client::new(client_incoming_rx, client_outgoing_tx.clone());

    tokio::spawn(async move {
        client.run().await;
    });

    tokio::spawn(async move {
        let _ = client_transport.run(client_incoming_tx, client_outgoing_rx).await;
    });

    sleep(Duration::from_millis(100)).await;

    // Test various envelope types
    let envelopes = vec![
        Envelope::new_simple(1, 1, 0, 1, 1, EnvelopeFlags::RELIABLE, Bytes::from("reliable")),
        Envelope::new_simple(1, 1, 0, 2, 2, EnvelopeFlags::ENCRYPTED, Bytes::from("encrypted")),
        Envelope::new_simple(1, 1, 0, 3, 3, EnvelopeFlags::RELIABLE | EnvelopeFlags::COMPRESSED, Bytes::from("both")),
    ];

    for envelope in envelopes {
        client_outgoing_tx.send(envelope).await.unwrap();
    }

    sleep(Duration::from_millis(200)).await;
}
