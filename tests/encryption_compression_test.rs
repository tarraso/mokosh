//! Integration tests for encryption and compression features

use mokosh_client::Client;
use mokosh_protocol::{
    compression::{Lz4Compressor, NoCompressor, ZstdCompressor},
    encryption::{ChaCha20Poly1305Encryptor, NoEncryptor},
    EnvelopeFlags,
};
use mokosh_protocol_derive::GameMessage;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, GameMessage)]
#[route_id = 100]
struct TestMessage {
    data: String,
    value: u32,
}

#[tokio::test]
async fn test_compression_only() {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        ZstdCompressor::new(),
        NoEncryptor,
        None,
    );

    let message = TestMessage {
        data: "Hello, Godot!".repeat(100), // Make it compressible
        value: 42,
    };

    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();

    // Check that COMPRESSED flag is set
    assert!(envelope.flags.contains(EnvelopeFlags::COMPRESSED));
    assert!(!envelope.flags.contains(EnvelopeFlags::ENCRYPTED));
}

#[tokio::test]
async fn test_encryption_only() {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let encryption_key = [0x42; 32];
    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        NoCompressor,
        ChaCha20Poly1305Encryptor::new(&encryption_key),
        None,
    );

    let message = TestMessage {
        data: "Secret message".to_string(),
        value: 123,
    };

    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();

    // Check that ENCRYPTED flag is set
    assert!(!envelope.flags.contains(EnvelopeFlags::COMPRESSED));
    assert!(envelope.flags.contains(EnvelopeFlags::ENCRYPTED));
}

#[tokio::test]
async fn test_compression_and_encryption() {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let encryption_key = [0x99; 32];
    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        ZstdCompressor::new(),
        ChaCha20Poly1305Encryptor::new(&encryption_key),
        None,
    );

    let message = TestMessage {
        data: "Compressed and encrypted!".repeat(50),
        value: 999,
    };

    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();

    // Check that both COMPRESSED and ENCRYPTED flags are set
    assert!(envelope.flags.contains(EnvelopeFlags::COMPRESSED));
    assert!(envelope.flags.contains(EnvelopeFlags::ENCRYPTED));
}

#[tokio::test]
async fn test_lz4_compression() {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    let mut client = Client::with_compression_encryption(
        incoming_rx,
        outgoing_tx,
        1, // JSON
        1, // JSON
        Lz4Compressor::new(),
        NoEncryptor,
        None,
    );

    let message = TestMessage {
        data: "LZ4 is fast!".repeat(100),
        value: 777,
    };

    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();

    // Check that COMPRESSED flag is set
    assert!(envelope.flags.contains(EnvelopeFlags::COMPRESSED));
}

#[tokio::test]
async fn test_no_compression_no_encryption() {
    let (_incoming_tx, incoming_rx) = mpsc::channel(10);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);

    // Use default Client::new() - no compression, no encryption
    let mut client = Client::new(incoming_rx, outgoing_tx);

    let message = TestMessage {
        data: "Plain message".to_string(),
        value: 1,
    };

    client.send_message(message).await.unwrap();

    let envelope = outgoing_rx.recv().await.unwrap();

    // Check that neither flag is set (only RELIABLE)
    assert!(!envelope.flags.contains(EnvelopeFlags::COMPRESSED));
    assert!(!envelope.flags.contains(EnvelopeFlags::ENCRYPTED));
    assert!(envelope.flags.contains(EnvelopeFlags::RELIABLE));
}
