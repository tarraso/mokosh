//! Integration tests for the reliability layer over a lossy link.
//!
//! These drive the real `Server` and `Client` event loops with a deterministic
//! packet-dropping relay between them (the event loop is transport-agnostic, so
//! dropping envelopes on the channel hop faithfully simulates UDP loss).

use bytes::Bytes;
use mokosh_protocol::messages::{routes, Hello};
use mokosh_protocol::{
    CodecType, Envelope, ReliabilityConfig, ReliabilityMode, SessionEnvelope, SessionId,
    CURRENT_PROTOCOL_VERSION,
};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol_derive::GameMessage;
use mokosh_client::{Client, ClientConfig};
use mokosh_server::{GameEvent, Server, ServerConfig};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 300]
struct TestMsg {
    seq: u32,
    value: f32,
}

/// Fast reliability config so tests converge quickly.
fn fast_reliability() -> ReliabilityConfig {
    ReliabilityConfig {
        initial_rto: Duration::from_millis(30),
        min_rto: Duration::from_millis(20),
        max_rto: Duration::from_millis(200),
        backoff_factor: 2.0,
        max_retries: 100,
        default_ttl: Duration::from_secs(30),
        ack_delay: Duration::from_millis(5),
        ordering_buffer_limit: 1024,
        send_window: 64,
        rtt_smoothing: 0.125,
    }
}

fn json() -> CodecType {
    CodecType::from_id(1).unwrap()
}

/// End-to-end: server reliably broadcasts ordered messages to a real client
/// over a link that drops ~25% of datagrams in both directions. Every message
/// must arrive exactly once and in order.
#[tokio::test]
async fn server_to_client_reliable_ordered_survives_loss() {
    const N: u32 = 12;
    let sid = SessionId::new_v4();

    // Client -> Server (Envelope) and Server -> Client (Envelope) wires, plus the
    // server's SessionEnvelope channels.
    let (c2s_tx, mut c2s_rx) = mpsc::channel::<Envelope>(256);
    let (s_in_tx, s_in_rx) = mpsc::channel::<SessionEnvelope>(256);
    let (s_out_tx, mut s_out_rx) = mpsc::channel::<SessionEnvelope>(256);
    let (s2c_tx, s2c_rx) = mpsc::channel::<Envelope>(256);
    let (game_tx, mut game_rx) = mpsc::channel::<Envelope>(256);

    // Deterministic loss: drop every 4th datagram per direction.
    tokio::spawn(async move {
        let mut n = 0usize;
        while let Some(env) = c2s_rx.recv().await {
            n += 1;
            if n.is_multiple_of(4) {
                continue; // dropped
            }
            if s_in_tx.send(SessionEnvelope::new(sid, env)).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut n = 0usize;
        while let Some(se) = s_out_rx.recv().await {
            n += 1;
            if n.is_multiple_of(4) {
                continue; // dropped
            }
            if s2c_tx.send(se.envelope).await.is_err() {
                break;
            }
        }
    });

    // Server with reliability enabled.
    let server_cfg = ServerConfig {
        reliability: Some(fast_reliability()),
        retransmit_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let mut server = Server::with_full_config(
        s_in_rx,
        s_out_tx,
        json(),
        json(),
        server_cfg,
        None,
        None,
        NoCompressor,
        NoEncryptor,
    );

    let server_task = tokio::spawn(async move {
        let mut sent = false;
        loop {
            match server.tick().await {
                Ok(Some(GameEvent::PlayerConnected(s))) => {
                    if !sent {
                        for i in 1..=N {
                            server
                                .send_message_with(
                                    s,
                                    TestMsg { seq: i, value: i as f32 },
                                    ReliabilityMode::ReliableOrdered,
                                    Duration::from_secs(30),
                                )
                                .await
                                .unwrap();
                        }
                        sent = true;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // Client with reliability enabled.
    let client_cfg = ClientConfig {
        reliability: Some(fast_reliability()),
        retransmit_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let mut client = Client::with_full_config(
        s2c_rx,
        c2s_tx,
        json(),
        json(),
        client_cfg,
        None,
        NoCompressor,
        NoEncryptor,
        Some(game_tx),
    );
    client.connect().await.unwrap();
    let client_task = tokio::spawn(async move { client.run().await });

    // Collect delivered game messages; expect 1..=N in order, exactly once.
    let mut received: Vec<u32> = Vec::new();
    while received.len() < N as usize {
        match tokio::time::timeout(Duration::from_secs(10), game_rx.recv()).await {
            Ok(Some(env)) => {
                let msg: TestMsg = serde_json::from_slice(&env.payload).unwrap();
                received.push(msg.seq);
            }
            _ => break,
        }
    }

    server_task.abort();
    client_task.abort();

    assert_eq!(
        received,
        (1..=N).collect::<Vec<_>>(),
        "all reliable-ordered messages should arrive exactly once, in order"
    );
}

/// A reliable message to a peer that never ACKs must be retransmitted several
/// times and then reported as dropped once its TTL elapses.
#[tokio::test]
async fn server_retransmits_then_drops_unacked_message() {
    let sid = SessionId::new_v4();

    let (s_in_tx, s_in_rx) = mpsc::channel::<SessionEnvelope>(256);
    let (s_out_tx, mut s_out_rx) = mpsc::channel::<SessionEnvelope>(256);

    // Count how many times the game message (route 300) hits the wire. The peer
    // never sends ACKs, so the server should keep retransmitting until TTL.
    let game_sends = Arc::new(AtomicUsize::new(0));
    let game_sends_drain = game_sends.clone();
    tokio::spawn(async move {
        while let Some(se) = s_out_rx.recv().await {
            if se.envelope.route_id == 300 {
                game_sends_drain.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let server_cfg = ServerConfig {
        reliability: Some(ReliabilityConfig {
            initial_rto: Duration::from_millis(30),
            min_rto: Duration::from_millis(20),
            max_rto: Duration::from_millis(60),
            default_ttl: Duration::from_millis(300),
            ack_delay: Duration::from_millis(5),
            ..fast_reliability()
        }),
        retransmit_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let mut server = Server::with_full_config(
        s_in_rx,
        s_out_tx,
        json(),
        json(),
        server_cfg,
        None,
        None,
        NoCompressor,
        NoEncryptor,
    );

    let (dropped_tx, mut dropped_rx) = mpsc::channel::<(u64, u16)>(8);

    let server_task = tokio::spawn(async move {
        let mut sent = false;
        loop {
            match server.tick().await {
                Ok(Some(GameEvent::PlayerConnected(s))) => {
                    if !sent {
                        // Unordered Reliable: a dropped message leaves no ordering
                        // gap, so TTL-based give-up applies (ReliableOrdered never
                        // expires — see fix #1).
                        server
                            .send_message_with(
                                s,
                                TestMsg { seq: 1, value: 1.0 },
                                ReliabilityMode::Reliable,
                                Duration::from_millis(300),
                            )
                            .await
                            .unwrap();
                        sent = true;
                    }
                }
                Ok(Some(GameEvent::MessageDropped { seq, route_id, .. })) => {
                    if route_id == 300 {
                        let _ = dropped_tx.send((seq, route_id)).await;
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // Connect via a HELLO (mirrors the real reliable client handshake).
    let hello = Hello {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        min_protocol_version: 1,
        codec_id: 1,
        schema_hash: 0,
        reliability: true, // server has reliability enabled — must match
    };
    let hello_env = Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        routes::HELLO,
        1,
        ReliabilityMode::ReliableOrdered.to_flags(),
        Bytes::from(serde_json::to_vec(&hello).unwrap()),
    );
    s_in_tx
        .send(SessionEnvelope::new(sid, hello_env))
        .await
        .unwrap();

    let dropped = tokio::time::timeout(Duration::from_secs(5), dropped_rx.recv())
        .await
        .expect("should report a dropped message before timeout")
        .expect("dropped channel closed");

    server_task.abort();

    assert_eq!(dropped, (1, 300), "the unacked message should be dropped");
    assert!(
        game_sends.load(Ordering::SeqCst) >= 2,
        "message should have been retransmitted at least once (got {})",
        game_sends.load(Ordering::SeqCst)
    );

    // Keep the wire alive long enough for the assertions; silence unused warning.
    drop(s_in_tx);
}
