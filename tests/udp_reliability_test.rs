//! Real-socket end-to-end test: the UDP transport and the reliability layer
//! running together over loopback.
//!
//! Unlike `reliability_test.rs` (which drops envelopes on an in-memory channel),
//! this wires the real `UdpServer`/`UdpClient` transports to the real `Server`/
//! `Client` event loops with reliability enabled. It proves the two features
//! integrate over genuine UDP sockets: HELLO handshake, ACK/retransmit plumbing,
//! and ordered delivery all work end-to-end.

use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::{ReliableLink, Transport};
use mokosh_client::{Client, ClientConfig};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::messages::{
    routes, Disconnect, DisconnectReason, ErrorReason, Hello, HelloError,
};
use mokosh_protocol::{
    CodecType, Envelope, EnvelopeFlags, ReliabilityConfig, ReliabilityMode, SessionEnvelope,
    CURRENT_PROTOCOL_VERSION,
};
use mokosh_protocol_derive::GameMessage;
use mokosh_server::transport::udp::UdpServer;
use mokosh_server::transport::ReliableServerLink;
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

fn reliable_hello() -> Envelope {
    let hello = Hello {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        min_protocol_version: 1,
        codec_id: 1,
        schema_hash: 0,
        reliability: true,
    };
    Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        routes::HELLO,
        1,
        ReliabilityMode::ReliableOrdered.to_flags(),
        serde_json::to_vec(&hello).unwrap().into(),
    )
}

fn reliable_disconnect() -> Envelope {
    let disconnect = Disconnect {
        reason: DisconnectReason::ClientRequested,
        message: "test disconnect".to_string(),
    };
    Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        routes::DISCONNECT,
        2,
        ReliabilityMode::ReliableOrdered.to_flags(),
        serde_json::to_vec(&disconnect).unwrap().into(),
    )
}

fn probe_envelope(route_id: u16) -> Envelope {
    Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        1,
        0,
        route_id,
        0,
        EnvelopeFlags::empty(),
        Vec::new().into(),
    )
}

async fn recv_route(socket: &UdpSocket, route_id: u16, wait: Duration) -> Envelope {
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = vec![0u8; 65_535];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let len = tokio::time::timeout(remaining, socket.recv(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for route {route_id}"))
            .unwrap();
        let envelope = Envelope::from_bytes(bytes::Bytes::copy_from_slice(&buf[..len])).unwrap();
        if envelope.route_id == route_id {
            return envelope;
        }
    }
}

async fn assert_route_absent(socket: &UdpSocket, route_id: u16, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = vec![0u8; 65_535];
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, socket.recv(&mut buf)).await {
            Ok(Ok(len)) => {
                let envelope =
                    Envelope::from_bytes(bytes::Bytes::copy_from_slice(&buf[..len])).unwrap();
                assert_ne!(
                    envelope.route_id, route_id,
                    "stale session still routed envelope to its old UDP peer"
                );
            }
            Ok(Err(e)) => panic!("UDP receive failed: {e}"),
            Err(_) => return,
        }
    }
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
    let (t_in_tx, t_in_rx) = mpsc::channel(256);
    let (t_out_tx, t_out_rx) = mpsc::channel(256);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let server_transport = UdpServer::new(server_addr);
    let transport_task = tokio::spawn(async move {
        let _ = server_transport
            .run(t_in_tx, t_out_rx, Some(ready_tx))
            .await;
    });
    ready_rx.await.expect("UDP server failed to start");

    // Reliability decorator between the UDP transport and the Server.
    let (srv_in_rx, srv_out_tx) = ReliableServerLink::new(fast_reliability())
        .with_tick(Duration::from_millis(10))
        .spawn(t_in_rx, t_out_tx);

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
                                    TestMsg {
                                        seq: i,
                                        value: i as f32,
                                    },
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

    // Reliability now lives in the transport decorator: wrap the unreliable
    // UdpClient in ReliableLink (the Client event loop is reliability-agnostic).
    let client_transport =
        ReliableLink::new(UdpClient::new(server_addr.to_string()), fast_reliability())
            .with_tick(Duration::from_millis(10));
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

/// Client sends reliable-ordered messages to the server **through `ClientHandle`**
/// (not the raw transport channel). Proves the handle routes sends via the running
/// `Client`, so the reliability layer engages: every message arrives at the server
/// exactly once and in order over real UDP.
#[tokio::test]
async fn udp_client_handle_reliable_to_server() {
    const N: u32 = 12;

    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe.local_addr().unwrap();
    drop(probe);

    // --- Server ---
    let (t_in_tx, t_in_rx) = mpsc::channel(256);
    let (t_out_tx, t_out_rx) = mpsc::channel(256);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let server_transport = UdpServer::new(server_addr);
    let transport_task = tokio::spawn(async move {
        let _ = server_transport
            .run(t_in_tx, t_out_rx, Some(ready_tx))
            .await;
    });
    ready_rx.await.expect("UDP server failed to start");

    // Reliability decorator between the UDP transport and the Server.
    let (srv_in_rx, srv_out_tx) = ReliableServerLink::new(fast_reliability())
        .with_tick(Duration::from_millis(10))
        .spawn(t_in_rx, t_out_tx);

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

    // Server forwards received game messages (route 300) to the test.
    let (recv_tx, mut recv_rx) = mpsc::channel(256);
    let server_task = tokio::spawn(async move {
        loop {
            match server.tick().await {
                Ok(Some(GameEvent::GameMessage { envelope, .. })) => {
                    if envelope.route_id == 300 {
                        let msg: TestMsg = serde_json::from_slice(&envelope.payload).unwrap();
                        let _ = recv_tx.send(msg.seq).await;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    // --- Client ---
    let (cli_in_tx, cli_in_rx) = mpsc::channel(256);
    let (cli_out_tx, cli_out_rx) = mpsc::channel(256);
    let (game_tx, _game_rx) = mpsc::channel(256);

    // Reliability now lives in the transport decorator: wrap the unreliable
    // UdpClient in ReliableLink (the Client event loop is reliability-agnostic).
    let client_transport =
        ReliableLink::new(UdpClient::new(server_addr.to_string()), fast_reliability())
            .with_tick(Duration::from_millis(10));
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
    // Grab the handle BEFORE run() consumes the client.
    let handle = client.handle();
    client.connect().await.unwrap();
    let client_task = tokio::spawn(async move { client.run().await });

    // Send through the handle (not the raw transport channel). Retransmission
    // covers any sent before the handshake completes.
    for i in 1..=N {
        handle
            .send_message(
                json(),
                &TestMsg {
                    seq: i,
                    value: i as f32,
                },
                ReliabilityMode::ReliableOrdered,
                None,
            )
            .expect("queue send via handle");
    }

    let mut received: Vec<u32> = Vec::new();
    while received.len() < N as usize {
        match tokio::time::timeout(Duration::from_secs(10), recv_rx.recv()).await {
            Ok(Some(seq)) => received.push(seq),
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
        "messages sent via ClientHandle must reach the server reliably, in order, exactly once"
    );
}

#[tokio::test]
async fn udp_connection_timeout_reclaims_transport_and_reliability_session() {
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe.local_addr().unwrap();
    drop(probe);

    let (t_in_tx, t_in_rx) = mpsc::channel(256);
    let (t_out_tx, t_out_rx) = mpsc::channel(256);
    let transport_probe_tx = t_out_tx.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let transport_task = tokio::spawn(async move {
        let _ = UdpServer::new(server_addr)
            .run(t_in_tx, t_out_rx, Some(ready_tx))
            .await;
    });
    ready_rx.await.expect("UDP server failed to start");

    // Keep HELLO_OK unacknowledged in a one-message window. Timeout teardown
    // must bypass that full reliability window.
    let reliability = ReliabilityConfig {
        send_window: 1,
        ..fast_reliability()
    };
    let (srv_in_rx, srv_out_tx) = ReliableServerLink::new(reliability.clone())
        .with_tick(Duration::from_millis(10))
        .spawn(t_in_rx, t_out_tx);
    let server_cfg = ServerConfig {
        reliability: Some(reliability),
        connection_timeout: Duration::from_millis(50),
        keepalive_interval: Duration::from_secs(30),
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

    let (event_tx, mut event_rx) = mpsc::channel(16);
    let server_task = tokio::spawn(async move {
        loop {
            match server.tick().await {
                Ok(Some(event)) => {
                    if event_tx.send(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    });

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect(server_addr).await.unwrap();
    client.send(&reliable_hello().to_bytes()).await.unwrap();

    let old_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("initial HELLO event timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerConnected(session_id) => break session_id,
            _ => continue,
        }
    };
    let _hello_ok = recv_route(&client, routes::HELLO_OK, Duration::from_secs(1)).await;

    let disconnected_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("connection timeout event timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerDisconnected(session_id) => break session_id,
            _ => continue,
        }
    };
    assert_eq!(disconnected_session, old_session);

    let disconnect = recv_route(&client, routes::DISCONNECT, Duration::from_secs(1)).await;
    let disconnect: Disconnect = serde_json::from_slice(&disconnect.payload).unwrap();
    assert_eq!(disconnect.reason, DisconnectReason::Timeout);

    // Probe the transport directly. If session_to_addr leaked, this route would
    // still be delivered to the old peer.
    const STALE_PROBE_ROUTE: u16 = 321;
    transport_probe_tx
        .send(SessionEnvelope::new(
            old_session,
            probe_envelope(STALE_PROBE_ROUTE),
        ))
        .await
        .unwrap();
    assert_route_absent(&client, STALE_PROBE_ROUTE, Duration::from_millis(150)).await;

    // The same SocketAddr must mint a new UDP SessionId. Reusing reliable
    // control sequence 1 must be accepted by a fresh reliability pipe.
    client.send(&reliable_hello().to_bytes()).await.unwrap();
    let new_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("reconnect HELLO event timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerConnected(session_id) => break session_id,
            _ => continue,
        }
    };
    assert_ne!(new_session, old_session);
    let _hello_ok = recv_route(&client, routes::HELLO_OK, Duration::from_secs(1)).await;

    server_task.abort();
    transport_task.abort();
}

#[tokio::test]
async fn udp_session_limit_rejects_and_releases_transport_state() {
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = probe.local_addr().unwrap();
    drop(probe);

    let (t_in_tx, t_in_rx) = mpsc::channel(256);
    let (t_out_tx, t_out_rx) = mpsc::channel(256);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let transport_task = tokio::spawn(async move {
        let _ = UdpServer::new(server_addr)
            .run(t_in_tx, t_out_rx, Some(ready_tx))
            .await;
    });
    ready_rx.await.expect("UDP server failed to start");

    let reliability = fast_reliability();
    let (srv_in_rx, srv_out_tx) = ReliableServerLink::new(reliability.clone())
        .with_tick(Duration::from_millis(10))
        .spawn(t_in_rx, t_out_tx);
    let server_cfg = ServerConfig {
        max_concurrent_sessions: 1,
        reliability: Some(reliability),
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

    let (event_tx, mut event_rx) = mpsc::channel(16);
    let server_task = tokio::spawn(async move {
        loop {
            match server.tick().await {
                Ok(Some(event)) => {
                    if event_tx.send(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
    });

    let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    first.connect(server_addr).await.unwrap();
    first.send(&reliable_hello().to_bytes()).await.unwrap();

    let first_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("first connection timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerConnected(session_id) => break session_id,
            _ => continue,
        }
    };
    let _hello_ok = recv_route(&first, routes::HELLO_OK, Duration::from_secs(1)).await;

    let rejected = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    rejected.connect(server_addr).await.unwrap();
    rejected.send(&reliable_hello().to_bytes()).await.unwrap();

    let hello_error = recv_route(&rejected, routes::HELLO_ERROR, Duration::from_secs(1)).await;
    let hello_error: HelloError = serde_json::from_slice(&hello_error.payload).unwrap();
    assert_eq!(hello_error.reason, ErrorReason::ServerFull);
    let disconnect = recv_route(&rejected, routes::DISCONNECT, Duration::from_secs(1)).await;
    let disconnect: Disconnect = serde_json::from_slice(&disconnect.payload).unwrap();
    assert_eq!(disconnect.reason, DisconnectReason::Overloaded);

    first.send(&reliable_disconnect().to_bytes()).await.unwrap();
    let disconnected_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("first disconnect timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerDisconnected(session_id) => break session_id,
            _ => continue,
        }
    };
    assert_eq!(disconnected_session, first_session);

    // The rejection DISCONNECT must have removed this peer from both the UDP
    // routing maps and the reliability peer set, so control sequence 1 is fresh.
    rejected.send(&reliable_hello().to_bytes()).await.unwrap();
    let admitted_session = loop {
        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("reconnect after capacity release timed out")
            .expect("server event channel closed")
        {
            GameEvent::PlayerConnected(session_id) => break session_id,
            _ => continue,
        }
    };
    assert_ne!(admitted_session, first_session);
    let _hello_ok = recv_route(&rejected, routes::HELLO_OK, Duration::from_secs(1)).await;

    server_task.abort();
    transport_task.abort();
}
