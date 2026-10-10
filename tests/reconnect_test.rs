#![cfg(feature = "native")]
//! Reconnect through a real UDP proxy that changes its server-facing endpoint.
use bytes::Bytes;
use mokosh_client::reconnect::{
    ReconnectConfig, ReconnectMessage, ReconnectState, ReconnectingClient,
};
use mokosh_client::transport::udp::UdpClient;
use mokosh_client::transport::ReliableLink;
use mokosh_client::{Client, ClientConfig};
use mokosh_protocol::compression::NoCompressor;
use mokosh_protocol::encryption::NoEncryptor;
use mokosh_protocol::messages::routes;
use mokosh_protocol::udp_record::{bootstrap_keys, session_keys, Direction};
use mokosh_protocol::{
    CodecType, Envelope, RecordKey, ReliabilityConfig, ReliabilityMode, SessionId,
    UdpAddressChallenge, UdpAddressResponse, EPOCH_BOOTSTRAP, EPOCH_SESSION,
};
use mokosh_protocol_derive::GameMessage;
use mokosh_server::transport::udp::UdpServer;
use mokosh_server::transport::ReliableServerLink;
use mokosh_server::{GameEvent, Server, ServerConfig};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};

const PSK: [u8; 32] = [0x37; 32];

#[derive(Debug, Clone, Serialize, Deserialize, GameMessage)]
#[route_id = 300]
struct Echo {
    value: u32,
}

struct Tasks(Vec<tokio::task::JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

fn reliability() -> ReliabilityConfig {
    ReliabilityConfig {
        initial_rto: Duration::from_millis(30),
        min_rto: Duration::from_millis(20),
        max_rto: Duration::from_millis(100),
        max_retries: 100,
        ..Default::default()
    }
}

async fn server(tasks: &mut Tasks, encrypted: bool) -> (SocketAddr, mpsc::Receiver<GameEvent>) {
    server_mode(tasks, encrypted, false).await
}
async fn server_mode(
    tasks: &mut Tasks,
    encrypted: bool,
    resume: bool,
) -> (SocketAddr, mpsc::Receiver<GameEvent>) {
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let (in_tx, in_rx) = mpsc::channel(100);
    let (out_tx, out_rx) = mpsc::channel(100);
    let (ready_tx, ready_rx) = oneshot::channel();
    let transport = UdpServer::new(addr);
    let transport = if encrypted {
        transport
            .with_datagram_encryption(PSK)
            .require_encryption(true)
    } else {
        transport
    };
    tasks.0.push(tokio::spawn(async move {
        transport.run(in_tx, out_rx, Some(ready_tx)).await.unwrap();
    }));
    ready_rx.await.unwrap();
    let (in_rx, out_tx) = ReliableServerLink::new(reliability())
        .with_tick(Duration::from_millis(5))
        .spawn(in_rx, out_tx);
    let json = CodecType::from_id(1).unwrap();
    let mut server = Server::with_full_config(
        in_rx,
        out_tx,
        json,
        json,
        ServerConfig {
            guest_resume: resume.then(Default::default),
            reliability: Some(reliability()),
            connection_timeout: Duration::from_millis(600),
            keepalive_interval: Duration::from_millis(100),
            ..Default::default()
        },
        None,
        None,
        NoCompressor,
        NoEncryptor,
    );
    let (events_tx, events_rx) = mpsc::channel(100);
    let mut world = mokosh_examples_shared::platformer::PlatformerSimulation::new();
    let mut snapshot_id = 0;
    tasks.0.push(tokio::spawn(async move {
        loop {
            if let Some(event) = server.tick().await.unwrap() {
                use mokosh_examples_shared::platformer::{GameState, PlayerInput, Simulation};
                use mokosh_protocol::GameMessage;
                match &event {
                    GameEvent::GuestCreated(id) => world.add_player(*id),
                    GameEvent::GuestEnded(id) => world.remove_player(*id),
                    GameEvent::GuestSuspended(id) => world.apply_input_to_player(
                        *id,
                        &PlayerInput {
                            move_x: 0.0,
                            jump: false,
                        },
                    ),
                    GameEvent::SnapshotRequired {
                        player_id,
                        session_id,
                    } => {
                        snapshot_id += 1;
                        server
                            .supply_resume_snapshot(
                                *player_id,
                                *session_id,
                                mokosh_protocol::resume::ResumeSnapshot {
                                    snapshot_id,
                                    route_id: GameState::ROUTE_ID,
                                    schema_hash: GameState::SCHEMA_HASH,
                                    codec_id: 1,
                                    payload: serde_json::to_vec(&world.snapshot()).unwrap(),
                                },
                            )
                            .await
                            .unwrap();
                    }
                    _ => {}
                }
                if let GameEvent::GameMessage {
                    session_id,
                    envelope,
                } = &event
                {
                    let echo: Echo = json.decode(&envelope.payload).unwrap();
                    if resume {
                        let id = server.player_id(*session_id).unwrap();
                        world.players.get_mut(&id).unwrap().position.x = echo.value as f32;
                    }
                    server
                        .send_message_with(
                            *session_id,
                            echo,
                            ReliabilityMode::ReliableOrdered,
                            Duration::from_secs(1),
                        )
                        .await
                        .unwrap();
                }
                if events_tx.send(event).await.is_err() {
                    break;
                }
            }
        }
    }));
    (addr, events_rx)
}

/// Decode only for selecting loss/replay targets; forwarding always uses original bytes.
struct Decoder {
    encrypted: bool,
    boot_out: RecordKey,
    boot_in: RecordKey,
    session_out: Option<RecordKey>,
    session_in: Option<RecordKey>,
    challenges: std::collections::HashMap<Vec<u8>, [u8; 32]>,
}
impl Decoder {
    fn new(encrypted: bool) -> Self {
        let (out, incoming) = bootstrap_keys(&PSK);
        Self {
            encrypted,
            boot_out: RecordKey::new(&out, EPOCH_BOOTSTRAP, Direction::ClientToServer),
            boot_in: RecordKey::new(&incoming, EPOCH_BOOTSTRAP, Direction::ServerToClient),
            session_out: None,
            session_in: None,
            challenges: std::collections::HashMap::new(),
        }
    }
    fn decode(&mut self, bytes: &[u8], outgoing: bool) -> Option<Envelope> {
        let plain = if self.encrypted {
            let epoch = *bytes.first()?;
            let key = if epoch == EPOCH_BOOTSTRAP {
                if outgoing {
                    &self.boot_out
                } else {
                    &self.boot_in
                }
            } else if epoch == EPOCH_SESSION {
                if outgoing {
                    self.session_out.as_ref()?
                } else {
                    self.session_in.as_ref()?
                }
            } else {
                return None;
            };
            key.open(bytes).ok()?.1
        } else {
            Bytes::copy_from_slice(bytes)
        };
        let envelope = Envelope::from_bytes(plain).ok()?;
        if envelope.route_id == routes::UDP_ADDRESS_CHALLENGE {
            let challenge = UdpAddressChallenge::from_bytes(&envelope.payload).ok()?;
            self.challenges
                .insert(challenge.cookie.to_vec(), challenge.server_random);
        }
        if envelope.route_id == routes::UDP_ADDRESS_RESPONSE {
            let response = UdpAddressResponse::from_bytes(&envelope.payload).ok()?;
            let (out, incoming) = session_keys(
                &PSK,
                &response.client_random,
                self.challenges.get(response.cookie.as_slice())?,
            );
            self.session_out = Some(RecordKey::new(
                &out,
                EPOCH_SESSION,
                Direction::ClientToServer,
            ));
            self.session_in = Some(RecordKey::new(
                &incoming,
                EPOCH_SESSION,
                Direction::ServerToClient,
            ));
        }
        Some(envelope)
    }
}

enum ProxyCommand {
    Rebind(oneshot::Sender<(SocketAddr, SocketAddr)>),
    Outage(Duration, oneshot::Sender<()>),
    Replay(oneshot::Sender<()>),
}

async fn backend(addr: SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect(addr).await.unwrap();
    socket
}

async fn proxy(
    tasks: &mut Tasks,
    server: SocketAddr,
    encrypted: bool,
) -> (
    SocketAddr,
    mpsc::Sender<ProxyCommand>,
    watch::Receiver<(u32, u32)>,
) {
    proxy_mode(tasks, server, encrypted, false).await
}
async fn proxy_mode(
    tasks: &mut Tasks,
    server: SocketAddr,
    encrypted: bool,
    resume_loss: bool,
) -> (
    SocketAddr,
    mpsc::Sender<ProxyCommand>,
    watch::Receiver<(u32, u32)>,
) {
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = front.local_addr().unwrap();
    let mut back = backend(server).await;
    let (command_tx, mut command_rx) = mpsc::channel(10);
    let (loss_tx, loss_rx) = watch::channel((0, 0));
    tasks.0.push(tokio::spawn(async move {
        let mut peer = None;
        let mut decoder = Decoder::new(encrypted);
        let mut outgoing_buf = vec![0; 65535];
        let mut incoming_buf = vec![0; 65535];
        let mut drop_challenge = true;
        let mut drop_response = true;
        let mut last_game = None;
        let mut old_game = None;
        let mut paused_until = tokio::time::Instant::now();
        let mut lost_accept = false;
        let mut lost_snapshot = false;
        let mut lost_applied = false;
        let mut lost_ready = false;
        loop {
            tokio::select! {
                packet = front.recv_from(&mut outgoing_buf) => {
                    let (len, source) = packet.unwrap();
                    if resume_loss && peer.is_some_and(|old|old != source) {
                        back = backend(server).await;
                        decoder = Decoder::new(encrypted);
                    }
                    peer = Some(source);
                    if tokio::time::Instant::now() < paused_until { continue; }
                    let bytes = &outgoing_buf[..len];
                    if let Some(envelope) = decoder.decode(bytes, true) {
                        if envelope.route_id == routes::UDP_ADDRESS_RESPONSE && drop_response {
                            drop_response = false;
                            loss_tx.send_modify(|loss| loss.1 += 1);
                            continue;
                        }
                        if resume_loss && envelope.route_id == routes::SNAPSHOT_APPLIED && !lost_applied { lost_applied = true; continue; }
                        if envelope.route_id == 300 { last_game = Some(bytes.to_vec()); }
                    }
                    back.send(bytes).await.unwrap();
                }
                packet = back.recv(&mut incoming_buf) => {
                    let len = packet.unwrap();
                    let bytes = &incoming_buf[..len];
                    if tokio::time::Instant::now() < paused_until { continue; }
                    if let Some(envelope) = decoder.decode(bytes, false) {
                        if resume_loss && envelope.route_id == routes::RESUME_ACCEPTED && !lost_accept {
                            lost_accept = true;
                            // Lose the reply and the connection: retry must reuse the initial operation.
                            paused_until = tokio::time::Instant::now() + Duration::from_millis(300);
                            continue;
                        }
                        if resume_loss && envelope.route_id == routes::RESUME_SNAPSHOT && !lost_snapshot { lost_snapshot = true; continue; }
                        if resume_loss && envelope.route_id == routes::RESUME_READY && !lost_ready { lost_ready = true; continue; }
                        if envelope.route_id == routes::UDP_ADDRESS_CHALLENGE && drop_challenge {
                            drop_challenge = false;
                            loss_tx.send_modify(|loss| loss.0 += 1);
                            continue;
                        }
                    }
                    if let Some(peer) = peer { front.send_to(bytes, peer).await.unwrap(); }
                }
                Some(command) = command_rx.recv() => match command {
                    ProxyCommand::Outage(duration, reply) => {
                        paused_until = tokio::time::Instant::now() + duration;
                        let previous = back.local_addr().unwrap();
                        back = backend(server).await;
                        assert_ne!(previous,back.local_addr().unwrap());
                        decoder = Decoder::new(encrypted);
                        reply.send(()).unwrap();
                    }
                    ProxyCommand::Rebind(reply) => {
                        let previous = back.local_addr().unwrap();
                        let new = backend(server).await;
                        let next = new.local_addr().unwrap();
                        back = new;
                        old_game = last_game.take();
                        decoder = Decoder::new(encrypted);
                        reply.send((previous, next)).unwrap();
                    }
                    ProxyCommand::Replay(reply) => {
                        back.send(old_game.as_ref().expect("captured old game packet")).await.unwrap();
                        reply.send(()).unwrap();
                    }
                },
            }
        }
    }));
    (addr, command_tx, loss_rx)
}

async fn connected(status: &mut watch::Receiver<ReconnectState>, after: u64) -> (u64, String) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ReconnectState::Connected { generation, hello } = &*status.borrow_and_update() {
                if *generation > after {
                    return (*generation, hello.session_id.clone());
                }
            }
            if let ReconnectState::Failed(error) = &*status.borrow() {
                panic!("reconnect failed: {error}");
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("connected within budget")
}

async fn echo(messages: &mut mpsc::Receiver<ReconnectMessage>, generation: u64, value: u32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let message = messages.recv().await.unwrap();
            if message.generation == generation {
                let echo: Echo = CodecType::from_id(1)
                    .unwrap()
                    .decode(&message.envelope.payload)
                    .unwrap();
                assert_eq!(echo.value, value);
                return;
            }
        }
    })
    .await
    .expect("echo delivered")
}

async fn network_change(encrypted: bool) {
    let mut tasks = Tasks(vec![]);
    let (server, mut events) = server(&mut tasks, encrypted).await;
    let (proxy, commands, loss) = proxy(&mut tasks, server, encrypted).await;
    let json = CodecType::from_id(1).unwrap();
    let mut coordinator = ReconnectingClient::new(
        ReconnectConfig {
            deadline: Duration::from_secs(5),
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(100),
            jitter: 0.0,
            ..Default::default()
        },
        move |incoming, outgoing| {
            let client = Client::with_full_config(
                incoming,
                outgoing,
                json,
                json,
                ClientConfig {
                    reliability: Some(reliability()),
                    hello_timeout: Duration::from_millis(600),
                    connection_timeout: Duration::from_millis(200),
                    keepalive_interval: Duration::from_millis(50),
                    ..Default::default()
                },
                None,
                NoCompressor,
                NoEncryptor,
                None,
            );
            let udp = UdpClient::new(proxy.to_string());
            let udp = if encrypted {
                udp.with_datagram_encryption(PSK).require_encryption(true)
            } else {
                udp
            };
            (
                client,
                ReliableLink::new(udp, reliability()).with_tick(Duration::from_millis(5)),
            )
        },
    )
    .unwrap();
    let handle = coordinator.handle();
    let mut status = handle.subscribe();
    let mut messages = coordinator.take_messages().unwrap();
    let (done_tx, done_rx) = oneshot::channel();
    tasks.0.push(tokio::spawn(async move {
        let _ = done_tx.send(coordinator.run().await);
    }));
    let (first_generation, first_session) = connected(&mut status, 0).await;
    assert_eq!(
        *loss.borrow(),
        (1, 1),
        "both address-validation packets were lost once"
    );
    handle
        .send_message(
            json,
            &Echo { value: 11 },
            ReliabilityMode::ReliableOrdered,
            None,
        )
        .unwrap();
    echo(&mut messages, first_generation, 11).await;
    let (reply, rx) = oneshot::channel();
    commands.send(ProxyCommand::Rebind(reply)).await.unwrap();
    let (old_endpoint, new_endpoint) = rx.await.unwrap();
    assert_ne!(old_endpoint, new_endpoint);
    let (second_generation, second_session) = connected(&mut status, first_generation).await;
    assert_ne!(first_session, second_session);
    handle
        .send_message(
            json,
            &Echo { value: 99 },
            ReliabilityMode::ReliableOrdered,
            None,
        )
        .unwrap();
    echo(&mut messages, second_generation, 99).await;
    let (reply, rx) = oneshot::channel();
    commands.send(ProxyCommand::Replay(reply)).await.unwrap();
    rx.await.unwrap();
    let old_id = SessionId::parse_str(&first_session).unwrap();
    let mut new_connected = false;
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(event) = events.recv().await {
            match event {
                GameEvent::PlayerConnected(id) if id.to_string() == second_session => {
                    new_connected = true
                }
                GameEvent::GameMessage {
                    session_id,
                    envelope,
                } if session_id.to_string() == second_session => {
                    let echo: Echo = json.decode(&envelope.payload).unwrap();
                    assert_eq!(echo.value, 99, "old packet must not enter the new session");
                }
                GameEvent::PlayerDisconnected(id) if id == old_id => break,
                _ => {}
            }
        }
    })
    .await
    .expect("old server session reclaimed by timeout");
    assert!(new_connected);
    // Give a replay a bounded observation interval after processing queued events.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), messages.recv())
            .await
            .is_err(),
        "old ciphertext must not generate another echo"
    );
    handle.disconnect();
    tokio::time::timeout(Duration::from_secs(1), done_rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(*status.borrow(), ReconnectState::Stopped));
}

#[tokio::test]
async fn udp_reconnect_after_endpoint_change_and_validation_loss() {
    network_change(false).await;
}

#[tokio::test]
async fn encrypted_udp_reconnect_rejects_previous_session_packets() {
    network_change(true).await;
}

async fn guest_ready(
    status: &mut watch::Receiver<ReconnectState>,
    handle: &mokosh_client::reconnect::ReconnectHandle,
    after: u64,
    expected_x: Option<f32>,
) -> (u64, mokosh_protocol::PlayerId, SessionId) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let state = status.borrow_and_update().clone();
            match state {
                ReconnectState::Synchronizing {
                    generation,
                    snapshot_id,
                    snapshot,
                    ..
                } => {
                    let world: mokosh_examples_shared::platformer::GameState =
                        serde_json::from_slice(&snapshot.payload).unwrap();
                    assert_eq!(world.players.len(), 1, "no duplicate character");
                    if let Some(x) = expected_x {
                        assert_eq!(
                            world.players[0].position.x, x,
                            "character state survives handover"
                        );
                    }
                    assert!(matches!(
                        handle.send_message(
                            CodecType::from_id(1).unwrap(),
                            &Echo { value: 999 },
                            ReliabilityMode::Unreliable,
                            None
                        ),
                        Err(mokosh_client::reconnect::ReconnectSendError::NotConnected)
                    ));
                    assert!(handle
                        .confirm_snapshot(generation.saturating_sub(1), snapshot_id)
                        .is_err());
                    handle.confirm_snapshot(generation, snapshot_id).unwrap();
                }
                ReconnectState::Ready {
                    generation,
                    player_id,
                    session_id,
                    ..
                } if generation > after => return (generation, player_id, session_id),
                ReconnectState::Failed(error) => panic!("resume failed: {error}"),
                _ => {}
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("guest ready within budget")
}

async fn guest_network_change(outage: bool) {
    let mut tasks = Tasks(vec![]);
    let (addr, mut events) = server_mode(&mut tasks, true, true).await;
    let (proxy, commands, loss) = proxy_mode(&mut tasks, addr, true, true).await;
    let json = CodecType::from_id(1).unwrap();
    let mut coordinator = ReconnectingClient::new(
        ReconnectConfig {
            guest_resume: true,
            deadline: Duration::from_secs(8),
            max_attempts: 30,
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(100),
            jitter: 0.0,
            ..Default::default()
        },
        move |incoming, outgoing| {
            let client = Client::with_full_config(
                incoming,
                outgoing,
                json,
                json,
                ClientConfig {
                    reliability: Some(reliability()),
                    hello_timeout: Duration::from_millis(500),
                    connection_timeout: Duration::from_millis(200),
                    keepalive_interval: Duration::from_millis(50),
                    ..Default::default()
                },
                None,
                NoCompressor,
                NoEncryptor,
                None,
            );
            (
                client,
                ReliableLink::new(
                    UdpClient::new(proxy.to_string())
                        .with_datagram_encryption(PSK)
                        .require_encryption(true),
                    reliability(),
                )
                .with_tick(Duration::from_millis(5)),
            )
        },
    )
    .unwrap();
    let handle = coordinator.handle();
    let mut status = handle.subscribe();
    let mut messages = coordinator.take_messages().unwrap();
    let (done_tx, done_rx) = oneshot::channel();
    tasks.0.push(tokio::spawn(async move {
        let _ = done_tx.send(coordinator.run().await);
    }));
    let (first_generation, player, first_transport) =
        guest_ready(&mut status, &handle, 0, None).await;
    assert_eq!(*loss.borrow(), (1, 1));
    handle
        .send_message(
            json,
            &Echo { value: 73 },
            ReliabilityMode::ReliableOrdered,
            None,
        )
        .unwrap();
    echo(&mut messages, first_generation, 73).await;
    if outage {
        let (tx, rx) = oneshot::channel();
        commands
            .send(ProxyCommand::Outage(Duration::from_millis(1500), tx))
            .await
            .unwrap();
        rx.await.unwrap();
    } else {
        let (tx, rx) = oneshot::channel();
        commands.send(ProxyCommand::Rebind(tx)).await.unwrap();
        rx.await.unwrap();
    }
    let (generation, restored, new_transport) =
        guest_ready(&mut status, &handle, first_generation, Some(73.0)).await;
    assert_eq!(player, restored);
    assert_ne!(first_transport, new_transport);
    handle
        .send_message(
            json,
            &Echo { value: 91 },
            ReliabilityMode::ReliableOrdered,
            None,
        )
        .unwrap();
    echo(&mut messages, generation, 91).await;
    if !outage {
        let (tx, rx) = oneshot::channel();
        commands.send(ProxyCommand::Replay(tx)).await.unwrap();
        rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), messages.recv())
                .await
                .is_err()
        );
    }
    handle.disconnect();
    tokio::time::timeout(Duration::from_secs(1), done_rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let mut created = 0;
    let mut suspended = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = events.recv().await {
            match event {
                GameEvent::GuestCreated(id) => {
                    assert_eq!(id, player);
                    created += 1;
                }
                GameEvent::GuestSuspended(id) if id == player => suspended = true,
                GameEvent::GuestEnded(id) if id == player => break,
                GameEvent::PlayerConnected(_) | GameEvent::PlayerDisconnected(_) => {
                    panic!("transport handover leaked as player lifecycle")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(created, 1);
    if outage {
        assert!(suspended, "old transport timed out before resume");
    }
}

#[tokio::test]
async fn protected_guest_resume_survives_live_handover_and_lost_control_packets() {
    guest_network_change(false).await;
}
#[tokio::test]
async fn protected_guest_resume_survives_detach_without_recreating_character() {
    guest_network_change(true).await;
}

#[tokio::test]
async fn cancelling_snapshot_synchronization_ends_guest_and_never_sends_input() {
    let mut tasks = Tasks(vec![]);
    let (addr, mut events) = server_mode(&mut tasks, true, true).await;
    let json = CodecType::from_id(1).unwrap();
    let coordinator = ReconnectingClient::new(
        ReconnectConfig {
            guest_resume: true,
            ..Default::default()
        },
        move |incoming, outgoing| {
            (
                Client::with_full_config(
                    incoming,
                    outgoing,
                    json,
                    json,
                    ClientConfig {
                        reliability: Some(reliability()),
                        ..Default::default()
                    },
                    None,
                    NoCompressor,
                    NoEncryptor,
                    None,
                ),
                ReliableLink::new(
                    UdpClient::new(addr.to_string()).with_datagram_encryption(PSK),
                    reliability(),
                ),
            )
        },
    )
    .unwrap();
    let handle = coordinator.handle();
    let mut status = handle.subscribe();
    let task = tokio::spawn(coordinator.run());
    let (generation, snapshot_id) = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ReconnectState::Synchronizing {
                generation,
                snapshot_id,
                ..
            } = *status.borrow_and_update()
            {
                return (generation, snapshot_id);
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    handle.disconnect();
    assert!(handle.confirm_snapshot(generation, snapshot_id).is_err());
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.recv().await {
            match event {
                GameEvent::GuestEnded(_) => return,
                GameEvent::GameMessage { .. } | GameEvent::GuestResumed { .. } => {
                    panic!("game opened before confirmation")
                }
                _ => {}
            }
        }
        panic!("no final guest cleanup");
    })
    .await
    .unwrap();
}
