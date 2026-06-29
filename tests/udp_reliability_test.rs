//! Real-socket end-to-end test: the UDP transport and the reliability layer
//! running together over loopback.
//!
//! Unlike `reliability_test.rs` (which drops envelopes on an in-memory channel),
//! this wires the real `UdpServer`/`UdpClient` transports to the real `Server`/
//! `Client` event loops with reliability enabled. It proves the two features
//! integrate over genuine UDP sockets: HELLO handshake, ACK/retransmit plumbing,
//! and ordered delivery all work end-to-end.

use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::Transport;
use mokosh_client::{Client, ClientConfig};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::{CodecType, ReliabilityConfig, ReliabilityMode};
use mokosh_protocol_derive::GameMessage;
use mokosh_server::transport::udp::UdpServer;
use mokosh_server::{GameEvent, Server, ServerConfig};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, GameMessage)]
#[route_id = 300]
struct TestMsg {
    seq: u32,
    value: f32,
}

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

/// Server reliably sends ordered messages to a client over real UDP loopback;
/// every message must arrive exactly once and in order.
#[tokio::test]
async fn udp_reliable_ordered_end_to_end() {
    const N: u32 = 12;

    // Pick a free UDP port via a probe socket, then hand the address to UdpServer.
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe.local_addr().unwrap();
    drop(probe);

    // --- Server: real UdpServer transport <-> Server event loop ---
    let (srv_in_tx, srv_in_rx) = mpsc::channel(256);
    let (srv_out_tx, srv_out_rx) = mpsc::channel(256);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let server_transport = UdpServer::new(server_addr);
    let transport_task = tokio::spawn(async move {
        let _ = server_transport
            .run(srv_in_tx, srv_out_rx, Some(ready_tx))
            .await;
    });
    ready_rx.await.expect("UDP server failed to start");

    let server_cfg = ServerConfig {
        reliability: Some(fast_reliability()),
        retransmit_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let mut server = Server::with_full_config(
        srv_in_rx,
        srv_out_tx,
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

    // --- Client: real UdpClient transport <-> Client event loop ---
    let (cli_in_tx, cli_in_rx) = mpsc::channel(256);
    let (cli_out_tx, cli_out_rx) = mpsc::channel(256);
    let (game_tx, mut game_rx) = mpsc::channel(256);

    let client_transport = UdpClient::new(server_addr.to_string());
    let client_transport_task = tokio::spawn(async move {
        let _ = client_transport.run(cli_in_tx, cli_out_rx).await;
    });

    let client_cfg = ClientConfig {
        reliability: Some(fast_reliability()),
        retransmit_tick: Duration::from_millis(10),
        ..Default::default()
    };
    let mut client = Client::with_full_config(
        cli_in_rx,
        cli_out_tx,
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
    transport_task.abort();
    client_transport_task.abort();

    assert_eq!(
        received,
        (1..=N).collect::<Vec<_>>(),
        "all reliable-ordered messages should arrive over real UDP, exactly once, in order"
    );
}
