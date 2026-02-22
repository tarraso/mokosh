//! Integration test for HELLO protocol negotiation
//!
//! Tests the full HELLO handshake between client and server using memory transport.

use bytes::Bytes;
use mokosh_client::Client;
use mokosh_protocol::{messages::routes, Envelope, EnvelopeFlags, SessionEnvelope, SessionId, CURRENT_PROTOCOL_VERSION};
use mokosh_server::Server;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn test_successful_hello_handshake() {
    // Create channels for server (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(10);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel::<SessionEnvelope>(10);

    // Create channels for client (uses Envelope)
    let (client_incoming_tx, mut client_incoming_rx) = mpsc::channel(10);
    let (client_outgoing_tx, mut client_outgoing_rx) = mpsc::channel(10);

    // Create adapter tasks
    let session_id = SessionId::new_v4();

    // Adapter: Client -> Server
    let server_incoming_tx_clone = server_incoming_tx.clone();
    tokio::spawn(async move {
        while let Some(envelope) = client_outgoing_rx.recv().await {
            let session_envelope = SessionEnvelope::new(session_id, envelope);
            if server_incoming_tx_clone.send(session_envelope).await.is_err() {
                break;
            }
        }
    });

    // Adapter: Server -> Client
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
    let mut client = Client::new(mpsc::channel(1).1, client_outgoing_tx.clone());

    // Spawn server event loop
    tokio::spawn(async move {
        server.run().await;
    });

    // Client initiates HELLO
    client.connect().await.expect("Failed to send HELLO");

    // Wait for HELLO_OK response
    let response = timeout(Duration::from_millis(500), client_incoming_rx.recv())
        .await
        .expect("Timeout waiting for HELLO_OK")
        .expect("Channel closed");

    assert_eq!(response.route_id, routes::HELLO_OK);
    println!("✅ HELLO_OK received");
}

#[tokio::test]
async fn test_version_mismatch() {
    // Create channels for server (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(10);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel::<SessionEnvelope>(10);

    // Create channels for client (uses Envelope)
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(10);
    let (client_outgoing_tx, mut client_outgoing_rx) = mpsc::channel(10);

    // Create adapter tasks
    let session_id = SessionId::new_v4();

    // Adapter: Client -> Server
    let server_incoming_tx_clone = server_incoming_tx.clone();
    tokio::spawn(async move {
        while let Some(envelope) = client_outgoing_rx.recv().await {
            let session_envelope = SessionEnvelope::new(session_id, envelope);
            if server_incoming_tx_clone.send(session_envelope).await.is_err() {
                break;
            }
        }
    });

    // Adapter: Server -> Client
    tokio::spawn(async move {
        while let Some(session_envelope) = server_outgoing_rx.recv().await {
            if client_incoming_tx.send(session_envelope.envelope).await.is_err() {
                break;
            }
        }
    });

    // Create server (current version 1.0)
    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    // Spawn server
    tokio::spawn(async move {
        server.run().await;
    });

    // Manually send a HELLO with incompatible version (v2.0 min)
    let hello_json = serde_json::json!({
        "protocol_version": 0x0200, // v2.0
        "min_protocol_version": 0x0200, // v2.0 minimum (incompatible with server's v1.0)
        "codec_id": 1,
        "schema_hash": 0,
    });

    let payload = serde_json::to_vec(&hello_json).unwrap();

    let hello_envelope = Envelope::new_simple(
        0x0200, // Client claims to be v2.0
        1,
        0,
        routes::HELLO,
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from(payload),
    );

    client_outgoing_tx
        .send(hello_envelope)
        .await
        .expect("Failed to send HELLO");

    // Wait for HELLO_ERROR
    let mut client_rx = client_incoming_rx;

    let response = timeout(Duration::from_millis(500), client_rx.recv())
        .await
        .expect("Timeout waiting for response")
        .expect("No response received");

    assert_eq!(
        response.route_id,
        routes::HELLO_ERROR,
        "Expected HELLO_ERROR"
    );

    let hello_error: serde_json::Value =
        serde_json::from_slice(&response.payload).expect("Failed to parse HELLO_ERROR");

    assert_eq!(hello_error["reason"], "VersionMismatch");
    println!("✅ Version mismatch correctly detected");
}

#[tokio::test]
async fn test_game_message_rejected_before_hello() {
    // Create channels for server (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(10);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel::<SessionEnvelope>(10);

    // Create channels for client (uses Envelope)
    let (client_incoming_tx, client_incoming_rx) = mpsc::channel(10);
    let (client_outgoing_tx, mut client_outgoing_rx) = mpsc::channel(10);

    // Create adapter tasks
    let session_id = SessionId::new_v4();

    // Adapter: Client -> Server
    let server_incoming_tx_clone = server_incoming_tx.clone();
    tokio::spawn(async move {
        while let Some(envelope) = client_outgoing_rx.recv().await {
            let session_envelope = SessionEnvelope::new(session_id, envelope);
            if server_incoming_tx_clone.send(session_envelope).await.is_err() {
                break;
            }
        }
    });

    // Adapter: Server -> Client
    tokio::spawn(async move {
        while let Some(session_envelope) = server_outgoing_rx.recv().await {
            if client_incoming_tx.send(session_envelope.envelope).await.is_err() {
                break;
            }
        }
    });

    // Create server
    let server = Server::new(server_incoming_rx, server_outgoing_tx);

    // Spawn server
    tokio::spawn(async move {
        server.run().await;
    });

    // Try to send a game message (route_id=100) WITHOUT sending HELLO first
    let game_envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        100, // game message
        1,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"test payload"),
    );

    client_outgoing_tx
        .send(game_envelope)
        .await
        .expect("Failed to send game message");

    // Server should NOT echo it back (because client is not Connected)
    let mut client_rx = client_incoming_rx;
    let result = timeout(Duration::from_millis(200), client_rx.recv()).await;

    assert!(
        result.is_err(),
        "Server should not respond to game messages before HELLO"
    );

    println!("✅ Game message correctly rejected before HELLO");
}

#[tokio::test]
async fn test_game_message_allowed_after_hello() {
    // Create channels for server (uses SessionEnvelope)
    let (server_incoming_tx, server_incoming_rx) = mpsc::channel(10);
    let (server_outgoing_tx, mut server_outgoing_rx) = mpsc::channel::<SessionEnvelope>(10);

    // Create channels for client (uses Envelope)
    let (client_incoming_tx, mut client_incoming_rx) = mpsc::channel(10);
    let (client_outgoing_tx, mut client_outgoing_rx) = mpsc::channel(10);

    // Create adapter tasks
    let session_id = SessionId::new_v4();

    // Adapter: Client -> Server
    let server_incoming_tx_clone = server_incoming_tx.clone();
    let client_outgoing_tx_clone = client_outgoing_tx.clone();
    tokio::spawn(async move {
        while let Some(envelope) = client_outgoing_rx.recv().await {
            let session_envelope = SessionEnvelope::new(session_id, envelope);
            if server_incoming_tx_clone.send(session_envelope).await.is_err() {
                break;
            }
        }
    });

    // Adapter: Server -> Client
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
    let mut client = Client::new(mpsc::channel(1).1, client_outgoing_tx.clone());

    // Spawn server
    tokio::spawn(async move {
        server.run().await;
    });

    // Client sends HELLO
    client.connect().await.expect("Failed to send HELLO");

    // Wait for HELLO_OK
    let hello_ok = timeout(Duration::from_millis(200), client_incoming_rx.recv())
        .await
        .expect("Timeout waiting for HELLO_OK")
        .expect("Channel closed");

    assert_eq!(hello_ok.route_id, routes::HELLO_OK);

    // Now send a game message
    let game_envelope = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        100, // game message
        2,
        EnvelopeFlags::RELIABLE,
        Bytes::from_static(b"game data"),
    );

    client_outgoing_tx_clone
        .send(game_envelope.clone())
        .await
        .expect("Failed to send game message");

    // Server should echo it back (because client is Connected)
    let response = timeout(Duration::from_millis(200), client_incoming_rx.recv())
        .await
        .expect("Timeout waiting for echo")
        .expect("No response");

    assert_eq!(response.route_id, 100, "Expected echoed game message");
    assert_eq!(response.payload, game_envelope.payload);

    println!("✅ Game message correctly allowed after HELLO");
}
