use super::*;
use bytes::Bytes;

fn server(config: ServerConfig) -> (Server, mpsc::Receiver<SessionEnvelope>) {
    let (_, rx) = mpsc::channel(64);
    let (tx, out) = mpsc::channel(64);
    (
        Server::with_full_config(
            rx,
            tx,
            CodecType::from_id(1).unwrap(),
            CodecType::from_id(1).unwrap(),
            config,
            None,
            None,
            NoCompressor,
            NoEncryptor,
        ),
        out,
    )
}
fn config() -> ServerConfig {
    ServerConfig {
        guest_resume: Some(Default::default()),
        reliability: Some(Default::default()),
        max_concurrent_sessions: 1,
        ..Default::default()
    }
}
fn packet<T: serde::Serialize>(
    session: SessionId,
    route: u16,
    value: T,
    protected: bool,
) -> SessionEnvelope {
    SessionEnvelope {
        session_id: session,
        protected_udp: protected,
        envelope: Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            route,
            1,
            EnvelopeFlags::RELIABLE,
            CodecType::from_id(1).unwrap().encode(&value).unwrap(),
        ),
    }
}
async fn hello(server: &mut Server, session: SessionId, protected: bool) {
    server
        .handle_session_envelope(packet(
            session,
            routes::HELLO,
            Hello {
                guest_resume: true,
                protocol_version: CURRENT_PROTOCOL_VERSION,
                min_protocol_version: MIN_PROTOCOL_VERSION,
                codec_id: 1,
                schema_hash: 0,
                reliability: true,
            },
            protected,
        ))
        .await
        .unwrap();
}
async fn response<T: serde::de::DeserializeOwned>(
    out: &mut mpsc::Receiver<SessionEnvelope>,
    route: u16,
) -> T {
    loop {
        let p = out.recv().await.unwrap();
        if p.envelope.route_id == route {
            return CodecType::from_id(1)
                .unwrap()
                .decode(&p.envelope.payload)
                .unwrap();
        }
    }
}
fn initial(op: u8) -> ResumeRequest {
    ResumeRequest {
        player_id: None,
        token: None,
        operation: [op; 16],
    }
}
fn snapshot(id: u64) -> ResumeSnapshot {
    ResumeSnapshot {
        snapshot_id: id,
        route_id: 100,
        schema_hash: 0,
        codec_id: 1,
        payload: b"{}".to_vec(),
    }
}

#[tokio::test]
async fn snapshot_gate_blocks_game_and_late_owner_cleanup() {
    let mut cfg = config();
    cfg.guest_resume.as_mut().unwrap().max_pending = Some(2);
    let (mut server, mut out) = server(cfg);
    let old = SessionId::new_v4();
    hello(&mut server, old, true).await;
    server
        .handle_session_envelope(packet(old, routes::RESUME_REQUEST, initial(1), true))
        .await
        .unwrap();
    let accepted: ResumeAccepted = response(&mut out, routes::RESUME_ACCEPTED).await;
    while server.event_rx.try_recv().is_ok() {}
    server
        .handle_session_envelope(packet(old, 100, 1, true))
        .await
        .unwrap();
    assert!(server.event_rx.try_recv().is_err());
    server
        .supply_resume_snapshot(accepted.player_id, old, snapshot(10))
        .await
        .unwrap();
    server
        .handle_session_envelope(packet(
            old,
            routes::SNAPSHOT_APPLIED,
            SnapshotApplied { snapshot_id: 9 },
            true,
        ))
        .await
        .unwrap();
    assert!(!server.guests.ready(old));
    let new = SessionId::new_v4();
    hello(&mut server, new, true).await;
    server
        .handle_session_envelope(packet(
            new,
            routes::RESUME_REQUEST,
            ResumeRequest {
                player_id: Some(accepted.player_id),
                token: Some(accepted.token),
                operation: [2; 16],
            },
            true,
        ))
        .await
        .unwrap();
    assert!(server
        .supply_resume_snapshot(accepted.player_id, old, snapshot(11))
        .await
        .is_err());
    server
        .handle_session_envelope(packet(
            old,
            routes::DISCONNECT,
            Disconnect {
                reason: DisconnectReason::ClientRequested,
                message: String::new(),
            },
            true,
        ))
        .await
        .unwrap();
    assert_eq!(server.player_id(new), Some(accepted.player_id));
    server
        .handle_session_envelope(packet(old, 100, 2, true))
        .await
        .unwrap();
    assert!(!server.sessions.contains_key(&old));
    hello(&mut server, old, true).await;
    assert!(!server.sessions.contains_key(&old));
    server
        .supply_resume_snapshot(accepted.player_id, new, snapshot(12))
        .await
        .unwrap();
    server
        .handle_session_envelope(packet(
            new,
            routes::SNAPSHOT_APPLIED,
            SnapshotApplied { snapshot_id: 10 },
            true,
        ))
        .await
        .unwrap();
    assert!(!server.guests.ready(new));
    server
        .handle_session_envelope(packet(
            new,
            routes::SNAPSHOT_APPLIED,
            SnapshotApplied { snapshot_id: 12 },
            true,
        ))
        .await
        .unwrap();
    assert!(server.guests.ready(new));
    while server.event_rx.try_recv().is_ok() {}
    server
        .handle_session_envelope(packet(new, 100, 3, true))
        .await
        .unwrap();
    assert!(
        matches!(server.event_rx.try_recv(),Ok(GameEvent::GameMessage { session_id, .. }) if session_id == new)
    );
}

#[tokio::test]
async fn plaintext_and_disabled_resume_fail_closed_and_pending_is_bounded() {
    let (mut s, mut out) = server(config());
    let id = SessionId::new_v4();
    hello(&mut s, id, false).await;
    // Wire ENCRYPTED flags are no evidence of authenticated UDP.
    let mut request = packet(id, routes::RESUME_REQUEST, initial(1), false);
    request.envelope.flags |= EnvelopeFlags::ENCRYPTED;
    s.handle_session_envelope(request).await.unwrap();
    assert_eq!(
        response::<ResumeError>(&mut out, routes::RESUME_ERROR).await,
        ResumeError::Unsupported
    );
    let other = SessionId::new_v4();
    hello(&mut s, other, true).await;
    assert!(!s.sessions.contains_key(&other));
    assert_eq!(
        response::<HelloError>(&mut out, routes::HELLO_ERROR)
            .await
            .reason,
        ErrorReason::ServerFull
    );
    let (mut s, mut out) = server(ServerConfig {
        reliability: Some(Default::default()),
        ..Default::default()
    });
    hello(&mut s, SessionId::new_v4(), true).await;
    assert_eq!(
        response::<ResumeError>(&mut out, routes::RESUME_ERROR).await,
        ResumeError::Unsupported
    );
}

#[tokio::test]
async fn malformed_resume_and_oversized_snapshot_do_not_create_or_ready_guest() {
    let (mut s, mut out) = server(config());
    let id = SessionId::new_v4();
    hello(&mut s, id, true).await;
    let mut bad = packet(id, routes::RESUME_REQUEST, initial(1), true);
    bad.envelope.payload = Bytes::from_static(b"{");
    s.handle_session_envelope(bad).await.unwrap();
    assert_eq!(
        response::<ResumeError>(&mut out, routes::RESUME_ERROR).await,
        ResumeError::InvalidToken
    );
    assert_eq!(s.guests.len(), 0);
    s.handle_session_envelope(packet(id, routes::RESUME_REQUEST, initial(1), true))
        .await
        .unwrap();
    let accepted: ResumeAccepted = response(&mut out, routes::RESUME_ACCEPTED).await;
    let mut huge = snapshot(1);
    huge.payload = vec![255; MAX_RESUME_SNAPSHOT_BYTES + 1];
    assert!(s
        .supply_resume_snapshot(accepted.player_id, id, huge)
        .await
        .is_err());
    assert!(!s.guests.ready(id));
    let mut bad_ack = packet(
        id,
        routes::SNAPSHOT_APPLIED,
        SnapshotApplied { snapshot_id: 1 },
        true,
    );
    bad_ack.envelope.payload = Bytes::from_static(b"{");
    s.handle_session_envelope(bad_ack).await.unwrap();
    assert!(!s.guests.ready(id));
}

#[tokio::test]
async fn detached_guest_occupies_capacity_but_can_resume_at_full_capacity() {
    let mut cfg = config();
    cfg.guest_resume.as_mut().unwrap().max_pending = Some(2);
    let (mut s, mut out) = server(cfg);
    let id = SessionId::new_v4();
    hello(&mut s, id, true).await;
    s.handle_session_envelope(packet(id, routes::RESUME_REQUEST, initial(1), true))
        .await
        .unwrap();
    let accepted: ResumeAccepted = response(&mut out, routes::RESUME_ACCEPTED).await;
    s.retire_session(id, false);
    let other = SessionId::new_v4();
    hello(&mut s, other, true).await;
    s.handle_session_envelope(packet(other, routes::RESUME_REQUEST, initial(2), true))
        .await
        .unwrap();
    assert_eq!(
        response::<ResumeError>(&mut out, routes::RESUME_ERROR).await,
        ResumeError::Capacity
    );
    s.handle_session_envelope(packet(
        other,
        routes::RESUME_REQUEST,
        ResumeRequest {
            player_id: Some(accepted.player_id),
            token: Some(accepted.token),
            operation: [3; 16],
        },
        true,
    ))
    .await
    .unwrap();
    let restored: ResumeAccepted = response(&mut out, routes::RESUME_ACCEPTED).await;
    assert_eq!(restored.player_id, accepted.player_id);
}

#[tokio::test]
async fn protocol_one_is_rejected_instead_of_creating_a_player() {
    let (mut s, mut out) = server(config());
    let id = SessionId::new_v4();
    s.handle_session_envelope(packet(
        id,
        routes::HELLO,
        Hello {
            guest_resume: false,
            protocol_version: 0x0100,
            min_protocol_version: 0x0100,
            codec_id: 1,
            schema_hash: 0,
            reliability: true,
        },
        true,
    ))
    .await
    .unwrap();
    assert_eq!(
        response::<HelloError>(&mut out, routes::HELLO_ERROR)
            .await
            .reason,
        ErrorReason::VersionMismatch
    );
    assert!(s.event_rx.try_recv().is_err());
}
