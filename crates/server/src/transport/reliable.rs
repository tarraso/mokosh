//! Server-side reliability decorator.
//!
//! [`ReliableServerLink`] is the server counterpart of the client's
//! `ReliableLink`. Because the server is multi-client and its transport speaks
//! [`SessionEnvelope`] (not the bare-`Envelope` [`Transport`](mokosh_protocol::Transport)
//! trait), it is not a `Transport` impl but a **middle task** the app drops
//! between the transport and the [`Server`](crate::Server):
//!
//! ```text
//! UdpServer ──SessionEnvelope──▶ [ ReliableServerLink ] ──SessionEnvelope──▶ Server
//!           ◀──────────────────                        ◀──────────────────
//! ```
//!
//! It holds one [`ReliablePipe`] per [`SessionId`] (created on the first packet
//! from a session, mirroring how the UDP transport synthesizes sessions), runs a
//! single retransmit/ACK timer over all sessions, and keeps the `Server` event
//! loop reliability-agnostic. App-facing signals (send-window overflow, ordering
//! overflow, TTL drops) are injected as control envelopes into the stream toward
//! the `Server` — no new trait method.
//!
//! # Deliverability
//! The `Server` only accepts game messages once a session's handshake has
//! completed. To avoid ACKing a game message the `Server` would then drop
//! (which would lose it — a retransmit can't recover an already-ACKed message),
//! the decorator withholds game-channel processing for a session until it has
//! forwarded that session's `HELLO` (`established`). Game messages arriving
//! before then are dropped **without ACK**, so the client retransmits. (With
//! `auth_required`, messages sent after HELLO but before AUTH are a documented
//! residual gap — no example combines auth + UDP + reliability.)

use mokosh_protocol::messages::{routes, GAME_MESSAGES_START};
use mokosh_protocol::{
    Ack, CodecType, Disconnect, DisconnectReason, Envelope, EnvelopeFlags, Inbound, MessageDropped,
    MonoMillisecond, ReliabilityConfig, ReliabilityMode, ReliablePipe, SessionEnvelope, SessionId,
    WindowFull, CURRENT_PROTOCOL_VERSION,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Channel buffer between the decorator and the `Server`.
const LINK_BUFFER: usize = 256;

/// Per-session reliability state held by the link.
struct PerSession {
    pipe: ReliablePipe,
    /// Set once this session's `HELLO` has been forwarded to the `Server`. Until
    /// then, game-channel messages are dropped without ACK (see module docs).
    established: bool,
}

impl PerSession {
    fn new(cfg: &ReliabilityConfig) -> Self {
        Self {
            pipe: ReliablePipe::new(cfg.clone()),
            established: false,
        }
    }
}

/// Reliability decorator inserted between a `SessionEnvelope` transport and the
/// [`Server`](crate::Server).
pub struct ReliableServerLink {
    cfg: ReliabilityConfig,
    control_codec: CodecType,
    retransmit_tick: Duration,
}

impl ReliableServerLink {
    /// Creates a link configured by `cfg`. Retransmit/ACK timer defaults to 25ms
    /// and the control codec (ACK / DISCONNECT / MESSAGE_DROPPED framing) to JSON.
    pub fn new(cfg: ReliabilityConfig) -> Self {
        Self {
            cfg,
            control_codec: CodecType::from_id(1).expect("JSON codec"),
            retransmit_tick: Duration::from_millis(25),
        }
    }

    /// Overrides the retransmission/ACK timer interval.
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.retransmit_tick = tick;
        self
    }

    /// Overrides the control codec (must match the peers' control codec; default JSON).
    pub fn with_control_codec(mut self, codec: CodecType) -> Self {
        self.control_codec = codec;
        self
    }

    /// Spawns the middle task and returns the channels to hand to `Server::new`
    /// (`server_in_rx`, `server_out_tx`).
    ///
    /// `transport_in_rx` / `transport_out_tx` are the transport's session-envelope
    /// channels (the same ones you would otherwise pass straight to `Server`).
    pub fn spawn(
        self,
        transport_in_rx: mpsc::Receiver<SessionEnvelope>,
        transport_out_tx: mpsc::Sender<SessionEnvelope>,
    ) -> (
        mpsc::Receiver<SessionEnvelope>,
        mpsc::Sender<SessionEnvelope>,
    ) {
        let (server_in_tx, server_in_rx) = mpsc::channel(LINK_BUFFER);
        let (server_out_tx, server_out_rx) = mpsc::channel(LINK_BUFFER);
        tokio::spawn(self.run(transport_in_rx, transport_out_tx, server_in_tx, server_out_rx));
        (server_in_rx, server_out_tx)
    }

    async fn run(
        self,
        mut transport_in_rx: mpsc::Receiver<SessionEnvelope>,
        transport_out_tx: mpsc::Sender<SessionEnvelope>,
        server_in_tx: mpsc::Sender<SessionEnvelope>,
        mut server_out_rx: mpsc::Receiver<SessionEnvelope>,
    ) {
        let ReliableServerLink {
            cfg,
            control_codec: codec,
            retransmit_tick,
        } = self;
        let ttl = cfg.default_ttl;
        let epoch = Instant::now();
        let mut timer = tokio::time::interval(retransmit_tick);
        let mut sessions: HashMap<SessionId, PerSession> = HashMap::new();

        loop {
            tokio::select! {
                // Network → server: consume ACKs; dedup/reorder + deliver the rest.
                maybe = transport_in_rx.recv() => {
                    let Some(se) = maybe else { break; };
                    let now = now_ms(epoch);
                    let sid = se.session_id;
                    let env = se.envelope;
                    let entry = sessions.entry(sid).or_insert_with(|| PerSession::new(&cfg));

                    if env.route_id == routes::ACK {
                        if let Ok(ack) = codec.decode::<Ack>(&env.payload) {
                            entry.pipe.on_ack(&ack, now);
                        }
                        continue;
                    }

                    // Withhold game traffic (drop, no ACK) until the handshake lands.
                    if env.route_id >= GAME_MESSAGES_START && !entry.established {
                        continue;
                    }

                    match entry.pipe.handle_incoming(env, now) {
                        Inbound::Deliver(envs) => {
                            for e in envs {
                                if e.route_id == routes::HELLO {
                                    entry.established = true;
                                }
                                let is_disconnect = e.route_id == routes::DISCONNECT;
                                if server_in_tx.send(SessionEnvelope::new(sid, e)).await.is_err() {
                                    return;
                                }
                                if is_disconnect {
                                    sessions.remove(&sid);
                                    break;
                                }
                            }
                        }
                        Inbound::Consumed => {}
                        Inbound::Overflow => {
                            let d = disconnect_envelope(codec, DisconnectReason::ProtocolViolation, "ordering buffer overflow");
                            let _ = server_in_tx.send(SessionEnvelope::new(sid, d)).await;
                            sessions.remove(&sid);
                        }
                    }
                }

                // Server → network: stamp (assign seq + track), then forward.
                maybe = server_out_rx.recv() => {
                    let Some(se) = maybe else { break; };
                    let now = now_ms(epoch);
                    let sid = se.session_id;
                    let mut env = se.envelope;
                    let is_disconnect = env.route_id == routes::DISCONNECT;
                    let entry = sessions.entry(sid).or_insert_with(|| PerSession::new(&cfg));
                    let mode = ReliabilityMode::from_flags(env.flags);
                    match entry.pipe.stamp_outgoing(&mut env, mode, ttl, now) {
                        Ok(()) => {
                            let _ = transport_out_tx.send(SessionEnvelope::new(sid, env)).await;
                            // Server closed this session: forget its reliability state.
                            if is_disconnect {
                                sessions.remove(&sid);
                            }
                        }
                        Err(WindowFull) => {
                            // Peer a whole window behind on ACKs ⇒ tear the session down.
                            let d = disconnect_envelope(codec, DisconnectReason::Overloaded, "send window exceeded: peer not acknowledging");
                            let _ = server_in_tx.send(SessionEnvelope::new(sid, d)).await;
                            sessions.remove(&sid);
                        }
                    }
                }

                // Timer: per-session retransmits + ACKs → network; drops → server.
                _ = timer.tick() => {
                    let now = now_ms(epoch);
                    let mut retransmits: Vec<(SessionId, Envelope)> = Vec::new();
                    let mut acks: Vec<(SessionId, Ack)> = Vec::new();
                    let mut dropped: Vec<(SessionId, u64, u16)> = Vec::new();
                    for (sid, entry) in sessions.iter_mut() {
                        let out = entry.pipe.tick(now);
                        for env in out.retransmits { retransmits.push((*sid, env)); }
                        for ack in out.acks { acks.push((*sid, ack)); }
                        for ex in out.dropped { dropped.push((*sid, ex.seq, ex.route_id)); }
                    }
                    for (sid, env) in retransmits {
                        let _ = transport_out_tx.send(SessionEnvelope::new(sid, env)).await;
                    }
                    for (sid, ack) in acks {
                        let _ = transport_out_tx.send(SessionEnvelope::new(sid, ack_envelope(codec, &ack))).await;
                    }
                    for (sid, seq, route_id) in dropped {
                        let _ = server_in_tx.send(SessionEnvelope::new(sid, dropped_envelope(codec, seq, route_id))).await;
                    }
                }
            }
        }
    }
}

#[inline]
fn now_ms(epoch: Instant) -> MonoMillisecond {
    MonoMillisecond::from_millis(epoch.elapsed().as_millis() as u64)
}

fn control_envelope(codec: CodecType, route_id: u16, payload: bytes::Bytes) -> Envelope {
    Envelope::new_simple(
        CURRENT_PROTOCOL_VERSION,
        codec.id(),
        0,
        route_id,
        0,
        EnvelopeFlags::empty(),
        payload,
    )
}

fn ack_envelope(codec: CodecType, ack: &Ack) -> Envelope {
    let payload = codec.encode(ack).unwrap_or_default();
    control_envelope(codec, routes::ACK, payload)
}

fn dropped_envelope(codec: CodecType, seq: u64, route_id: u16) -> Envelope {
    let payload = codec
        .encode(&MessageDropped { seq, route_id })
        .unwrap_or_default();
    control_envelope(codec, routes::MESSAGE_DROPPED, payload)
}

fn disconnect_envelope(codec: CodecType, reason: DisconnectReason, message: &str) -> Envelope {
    let payload = codec
        .encode(&Disconnect {
            reason,
            message: message.to_string(),
        })
        .unwrap_or_default();
    control_envelope(codec, routes::DISCONNECT, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tokio::time::{sleep, timeout, Duration as TDuration};

    fn fast_cfg() -> ReliabilityConfig {
        ReliabilityConfig {
            initial_rto: TDuration::from_millis(20),
            min_rto: TDuration::from_millis(20),
            max_rto: TDuration::from_millis(40),
            ack_delay: TDuration::from_millis(5),
            default_ttl: TDuration::from_secs(10),
            max_retries: 100,
            ..ReliabilityConfig::default()
        }
    }

    fn hello_env() -> Envelope {
        // The decorator only inspects route/flags, not the HELLO body. msg_id 1 =
        // the first control sequence number (as the client's ReliableLink stamps it).
        Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            routes::HELLO,
            1,
            ReliabilityMode::ReliableOrdered.to_flags(),
            Bytes::from_static(b"{}"),
        )
    }

    fn game_env() -> Envelope {
        Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            300,
            0,
            ReliabilityMode::ReliableOrdered.to_flags(),
            Bytes::from_static(b"g"),
        )
    }

    #[tokio::test]
    async fn demuxes_two_sessions_independently() {
        let (t_in_tx, t_in_rx) = mpsc::channel(64);
        let (t_out_tx, mut t_out_rx) = mpsc::channel(64);
        let (mut server_in_rx, server_out_tx) = ReliableServerLink::new(fast_cfg())
            .with_tick(TDuration::from_millis(10))
            .spawn(t_in_rx, t_out_tx);

        let a = SessionId::new_v4();
        let b = SessionId::new_v4();

        // Both sessions handshake; both HELLOs reach the server, tagged by session.
        t_in_tx.send(SessionEnvelope::new(a, hello_env())).await.unwrap();
        t_in_tx.send(SessionEnvelope::new(b, hello_env())).await.unwrap();

        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let se = timeout(TDuration::from_secs(1), server_in_rx.recv())
                .await
                .expect("delivered")
                .expect("open");
            assert_eq!(se.envelope.route_id, routes::HELLO);
            seen.insert(se.session_id);
        }
        assert_eq!(seen, std::collections::HashSet::from([a, b]));

        // Server sends a reliable game message to A only → stamped and routed to A.
        server_out_tx.send(SessionEnvelope::new(a, game_env())).await.unwrap();
        sleep(TDuration::from_millis(30)).await;
        let mut game_to_a = 0;
        while let Ok(se) = t_out_rx.try_recv() {
            if se.envelope.route_id == 300 {
                assert_eq!(se.session_id, a, "game message must route only to A");
                assert!(se.envelope.msg_id >= 1, "link assigns a reliable sequence");
                game_to_a += 1;
            }
        }
        assert!(game_to_a >= 1, "A should have received the game message");
    }

    #[tokio::test]
    async fn send_window_overflow_injects_disconnect() {
        let cfg = ReliabilityConfig {
            send_window: 2,
            ..fast_cfg()
        };
        let (t_in_tx, t_in_rx) = mpsc::channel(64);
        let (t_out_tx, mut _t_out_rx) = mpsc::channel(64);
        let (mut server_in_rx, server_out_tx) = ReliableServerLink::new(cfg)
            .with_tick(TDuration::from_millis(10))
            .spawn(t_in_rx, t_out_tx);

        let sid = SessionId::new_v4();
        t_in_tx.send(SessionEnvelope::new(sid, hello_env())).await.unwrap();
        // Drain the delivered HELLO.
        let _ = timeout(TDuration::from_secs(1), server_in_rx.recv()).await.unwrap();

        // No ACKs ever arrive; the 3rd reliable send exceeds the window of 2.
        for _ in 0..3 {
            server_out_tx.send(SessionEnvelope::new(sid, game_env())).await.unwrap();
        }

        // The decorator injects a DISCONNECT toward the server for teardown.
        let mut saw_disconnect = false;
        for _ in 0..5 {
            match timeout(TDuration::from_secs(1), server_in_rx.recv()).await {
                Ok(Some(se)) if se.envelope.route_id == routes::DISCONNECT => {
                    saw_disconnect = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_disconnect, "window overflow should inject a DISCONNECT to the server");
    }
}
