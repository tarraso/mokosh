//! Generic reliability *link bridge* shared by the client and server transport
//! decorators (native-only).
//!
//! A reliability decorator sits between the application (event loop) and the
//! network (an unreliable transport) and runs the pure [`ReliablePipe`] state
//! machine below the loop:
//!
//! ```text
//! app_out → stamp_outgoing            → net_out   (or WindowFull → inject DISCONNECT)
//! net_in  → ACK→on_ack | handle_incoming → app_in (or overflow → inject DISCONNECT)
//! timer   → tick → retransmits + ACKs → net_out ; drops → MESSAGE_DROPPED → app_in
//! ```
//!
//! The client (single peer, bare `Envelope`) and the server (per-session,
//! `SessionEnvelope`, plus an "established" handshake gate) differ only in *how a
//! message maps to a peer's pipe*. [`PeerSet`] abstracts exactly that, so the one
//! [`Bridge`] loop drives both — see `ReliableLink` (client) and
//! `ReliableServerLink` (server).

use crate::compat::mpsc;
use crate::messages::routes;
use crate::{
    Ack, CodecType, Disconnect, DisconnectReason, Envelope, EnvelopeFlags, Inbound, MessageDropped,
    MonoMillisecond, ReliabilityConfig, ReliabilityMode, ReliablePipe, WindowFull,
    CURRENT_PROTOCOL_VERSION,
};
use bytes::Bytes;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

/// Abstracts the reliability state a [`Bridge`] manages: a **single peer**
/// (client) vs a **per-session map** (server). Implementors map the wire message
/// type ([`Msg`](PeerSet::Msg)) to a peer [`Key`](PeerSet::Key) + [`Envelope`],
/// resolve/drop per-peer [`ReliablePipe`]s, and (server only) gate inbound game
/// traffic on the handshake.
pub trait PeerSet: Send + 'static {
    /// Message carried on the app/network channels (`Envelope` or `SessionEnvelope`).
    type Msg: Send + 'static;
    /// Peer key (`()` for a single peer, `SessionId` for the server).
    type Key: Copy;

    /// Splits an incoming message into its peer key and envelope.
    fn split(msg: Self::Msg) -> (Self::Key, Envelope);
    /// Rebuilds an outgoing message from a peer key and envelope.
    fn join(key: Self::Key, env: Envelope) -> Self::Msg;

    /// Returns the pipe for `key`, creating it on demand.
    fn pipe_mut(&mut self, key: Self::Key) -> &mut ReliablePipe;
    /// Drops a peer's reliability state. No-op for a single peer.
    fn remove(&mut self, key: Self::Key);
    /// Invokes `f` for every live pipe (used by the retransmit/ACK tick).
    fn for_each_pipe(&mut self, f: impl FnMut(Self::Key, &mut ReliablePipe));

    /// Whether an inbound (non-ACK) envelope may be processed now. The server's
    /// "established" gate withholds game traffic until the handshake lands (drop
    /// without ACK ⇒ the peer retransmits). Default: always ready (client).
    fn inbound_ready(&mut self, _key: Self::Key, _env: &Envelope) -> bool {
        true
    }
    /// Hook after an envelope is delivered to the app (the server marks a session
    /// `established` on `HELLO`). Default: no-op (client).
    fn on_delivered(&mut self, _key: Self::Key, _env: &Envelope) {}
}

/// The reliability bridge loop, generic over [`PeerSet`]. Owns the peer state and
/// the send halves; the receive halves + timer are passed to [`run`](Bridge::run)
/// so the `select!` futures borrow locals (not `self`).
pub struct Bridge<P: PeerSet> {
    peers: P,
    /// → app (event loop).
    app_in_tx: mpsc::Sender<P::Msg>,
    /// → network (inner transport / transport).
    net_out_tx: mpsc::Sender<P::Msg>,
    control_codec: CodecType,
    default_ttl: Duration,
    retransmit_tick: Duration,
    epoch: Instant,
}

impl<P: PeerSet> Bridge<P> {
    /// Creates a bridge. `app_in_tx` delivers to the event loop; `net_out_tx`
    /// sends to the wrapped transport.
    pub fn new(
        peers: P,
        app_in_tx: mpsc::Sender<P::Msg>,
        net_out_tx: mpsc::Sender<P::Msg>,
        cfg: &ReliabilityConfig,
        control_codec: CodecType,
        retransmit_tick: Duration,
    ) -> Self {
        Self {
            peers,
            app_in_tx,
            net_out_tx,
            control_codec,
            default_ttl: cfg.default_ttl,
            retransmit_tick,
            epoch: Instant::now(),
        }
    }

    /// Runs the bridge until either channel closes. `app_out_rx` carries app
    /// sends; `net_in_rx` carries envelopes received from the network.
    pub async fn run(
        mut self,
        mut app_out_rx: mpsc::Receiver<P::Msg>,
        mut net_in_rx: mpsc::Receiver<P::Msg>,
    ) {
        let mut timer = tokio::time::interval(self.retransmit_tick);
        loop {
            tokio::select! {
                maybe = app_out_rx.recv() => {
                    let Some(msg) = maybe else { break; };
                    self.on_app_outgoing(msg).await;
                }
                maybe = net_in_rx.recv() => {
                    let Some(msg) = maybe else { break; };
                    if self.on_inbound(msg).await.is_break() {
                        break;
                    }
                }
                _ = timer.tick() => {
                    self.on_tick().await;
                }
            }
        }
    }

    /// App → network: stamp (assign seq + track), forward; a full window tells the
    /// app to tear down (`Overloaded`).
    async fn on_app_outgoing(&mut self, msg: P::Msg) {
        let (key, mut env) = P::split(msg);
        let now = now_ms(self.epoch);
        let mode = ReliabilityMode::from_flags(env.flags);
        let is_disconnect = env.route_id == routes::DISCONNECT;
        match self
            .peers
            .pipe_mut(key)
            .stamp_outgoing(&mut env, mode, self.default_ttl, now)
        {
            Ok(()) => {
                let _ = self.net_out_tx.send(P::join(key, env)).await;
                if is_disconnect {
                    self.peers.remove(key);
                }
            }
            Err(WindowFull) => {
                let d = disconnect_envelope(
                    self.control_codec,
                    DisconnectReason::Overloaded,
                    "send window exceeded: peer not acknowledging",
                );
                let _ = self.app_in_tx.send(P::join(key, d)).await;
                self.peers.remove(key);
            }
        }
    }

    /// Network → app: consume ACKs; dedup/reorder + deliver the rest. Returns
    /// `Break` when the app channel has closed.
    async fn on_inbound(&mut self, msg: P::Msg) -> ControlFlow<()> {
        let (key, env) = P::split(msg);
        let now = now_ms(self.epoch);

        if env.route_id == routes::ACK {
            if let Ok(ack) = self.control_codec.decode::<Ack>(&env.payload) {
                self.peers.pipe_mut(key).on_ack(&ack, now);
            }
            return ControlFlow::Continue(());
        }

        // Server withholds game traffic before the handshake (drop, no ACK).
        if !self.peers.inbound_ready(key, &env) {
            return ControlFlow::Continue(());
        }

        match self.peers.pipe_mut(key).handle_incoming(env, now) {
            Inbound::Deliver(envs) => {
                for e in envs {
                    self.peers.on_delivered(key, &e);
                    let is_disconnect = e.route_id == routes::DISCONNECT;
                    if self.app_in_tx.send(P::join(key, e)).await.is_err() {
                        return ControlFlow::Break(());
                    }
                    if is_disconnect {
                        self.peers.remove(key);
                        break;
                    }
                }
            }
            Inbound::Consumed => {}
            Inbound::Overflow => {
                let d = disconnect_envelope(
                    self.control_codec,
                    DisconnectReason::ProtocolViolation,
                    "ordering buffer overflow",
                );
                let _ = self.app_in_tx.send(P::join(key, d)).await;
                self.peers.remove(key);
            }
        }
        ControlFlow::Continue(())
    }

    /// Timer: per-pipe retransmits + coalesced ACKs → network; TTL drops → app.
    async fn on_tick(&mut self) {
        let now = now_ms(self.epoch);
        let mut retransmits: Vec<(P::Key, Envelope)> = Vec::new();
        let mut acks: Vec<(P::Key, Ack)> = Vec::new();
        let mut dropped: Vec<(P::Key, u64, u16)> = Vec::new();

        self.peers.for_each_pipe(|key, pipe| {
            let out = pipe.tick(now);
            for env in out.retransmits {
                retransmits.push((key, env));
            }
            for ack in out.acks {
                acks.push((key, ack));
            }
            for ex in out.dropped {
                dropped.push((key, ex.seq, ex.route_id));
            }
        });

        for (key, env) in retransmits {
            let _ = self.net_out_tx.send(P::join(key, env)).await;
        }
        for (key, ack) in acks {
            let _ = self
                .net_out_tx
                .send(P::join(key, ack_envelope(self.control_codec, &ack)))
                .await;
        }
        for (key, seq, route_id) in dropped {
            let _ = self
                .app_in_tx
                .send(P::join(key, dropped_envelope(self.control_codec, seq, route_id)))
                .await;
        }
    }
}

// ---- shared envelope helpers ----

#[inline]
fn now_ms(epoch: Instant) -> MonoMillisecond {
    MonoMillisecond::from_millis(epoch.elapsed().as_millis() as u64)
}

/// A bare (unreliable, msg_id 0) control envelope carrying `payload`.
fn control_envelope(codec: CodecType, route_id: u16, payload: Bytes) -> Envelope {
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
