//! Reliability decorator for client transports (native-only).
//!
//! [`ReliableLink`] wraps an **unreliable** [`Transport`] (e.g. [`UdpClient`](super::udp::UdpClient))
//! and turns it into a *reliable, ordered* one by running the pure
//! [`ReliablePipe`] state machine below the event loop. The [`Client`](crate::Client)
//! then stays reliability-agnostic: it emits `Envelope`s carrying reliability
//! flags and receives delivered ones, never knowing whether reliability came
//! from TCP (bare WebSocket) or from this link (UDP).
//!
//! ```text
//! app_out → ReliabilityMode::from_flags → pipe.stamp_outgoing → inner socket
//! inner socket → pipe.handle_incoming (ACK→on_ack; deliver rest) → app_in
//! timer → pipe.tick → retransmits + ACKs → inner socket ; drops → MESSAGE_DROPPED → app_in
//! ```
//!
//! For a reliable transport you simply **do not wrap** — zero code, zero cost.
//!
//! Browsers cannot open UDP sockets, so this decorator is native-only; WASM
//! clients keep using `BrowserWebSocketClient` unwrapped.

use super::Transport;
use crate::compat::mpsc;
use async_trait::async_trait;
use mokosh_protocol::messages::routes;
use mokosh_protocol::{
    Ack, CodecType, Disconnect, DisconnectReason, Envelope, EnvelopeFlags, Inbound, MessageDropped,
    MonoMillisecond, ReliabilityConfig, ReliabilityMode, ReliablePipe, WindowFull,
    CURRENT_PROTOCOL_VERSION,
};
use std::time::{Duration, Instant};

/// Channel buffer between the decorator and the wrapped transport.
const LINK_BUFFER: usize = 256;

/// Wraps an unreliable [`Transport`] with the reliability layer (ACK +
/// retransmit + ordering + per-mode TTL), presenting a reliable `Transport`.
pub struct ReliableLink<T: Transport> {
    inner: T,
    cfg: ReliabilityConfig,
    control_codec: CodecType,
    retransmit_tick: Duration,
}

impl<T: Transport> ReliableLink<T> {
    /// Wraps `inner` with reliability configured by `cfg`. The retransmit/ACK
    /// timer defaults to 25ms and the control codec (used for ACK / DISCONNECT /
    /// MESSAGE_DROPPED framing) to JSON — match the peer's control codec.
    pub fn new(inner: T, cfg: ReliabilityConfig) -> Self {
        Self {
            inner,
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

    /// Overrides the control codec used to frame ACK / DISCONNECT / MESSAGE_DROPPED
    /// (must match the peer's control codec; default JSON).
    pub fn with_control_codec(mut self, codec: CodecType) -> Self {
        self.control_codec = codec;
        self
    }
}

#[inline]
fn now_ms(epoch: Instant) -> MonoMillisecond {
    MonoMillisecond::from_millis(epoch.elapsed().as_millis() as u64)
}

/// Builds a bare (unreliable, msg_id 0) control envelope with `payload`.
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

#[async_trait]
impl<T: Transport> Transport for ReliableLink<T> {
    type Error = T::Error;

    async fn run(
        self,
        app_in_tx: mpsc::Sender<Envelope>,
        mut app_out_rx: mpsc::Receiver<Envelope>,
    ) -> Result<(), Self::Error> {
        let ReliableLink {
            inner,
            cfg,
            control_codec,
            retransmit_tick,
        } = self;

        // Channels between this decorator and the wrapped transport.
        let (inner_in_tx, mut inner_in_rx) = mpsc::channel::<Envelope>(LINK_BUFFER);
        let (inner_out_tx, inner_out_rx) = mpsc::channel::<Envelope>(LINK_BUFFER);

        // Drive the wrapped transport as an independent task.
        tokio::spawn(async move {
            if let Err(e) = inner.run(inner_in_tx, inner_out_rx).await {
                tracing::error!(error = %e, "ReliableLink inner transport error");
            }
        });

        let mut pipe = ReliablePipe::new(cfg.clone());
        let default_ttl = cfg.default_ttl;
        let epoch = Instant::now();
        let mut timer = tokio::time::interval(retransmit_tick);

        loop {
            tokio::select! {
                // App → network: stamp (assign seq + track), then forward.
                maybe = app_out_rx.recv() => {
                    let Some(mut env) = maybe else { break; }; // event loop gone
                    let now = now_ms(epoch);
                    let mode = ReliabilityMode::from_flags(env.flags);
                    match pipe.stamp_outgoing(&mut env, mode, default_ttl, now) {
                        Ok(()) => {
                            let _ = inner_out_tx.send(env).await;
                        }
                        Err(WindowFull) => {
                            // Peer a whole window behind on ACKs ⇒ tell the app.
                            let d = disconnect_envelope(
                                control_codec,
                                DisconnectReason::Overloaded,
                                "send window exceeded: server not acknowledging",
                            );
                            let _ = app_in_tx.send(d).await;
                        }
                    }
                }

                // Network → app: consume ACKs; dedup/reorder + deliver the rest.
                maybe = inner_in_rx.recv() => {
                    let Some(env) = maybe else { break; }; // inner transport ended
                    let now = now_ms(epoch);
                    if env.route_id == routes::ACK {
                        if let Ok(ack) = control_codec.decode::<Ack>(&env.payload) {
                            pipe.on_ack(&ack, now);
                        }
                        continue;
                    }
                    match pipe.handle_incoming(env, now) {
                        Inbound::Deliver(envs) => {
                            for e in envs {
                                if app_in_tx.send(e).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        Inbound::Consumed => {}
                        Inbound::Overflow => {
                            let d = disconnect_envelope(
                                control_codec,
                                DisconnectReason::ProtocolViolation,
                                "ordering buffer overflow",
                            );
                            let _ = app_in_tx.send(d).await;
                        }
                    }
                }

                // Timer: retransmits + coalesced ACKs → network; drops → app.
                _ = timer.tick() => {
                    let now = now_ms(epoch);
                    let out = pipe.tick(now);
                    for env in out.retransmits {
                        let _ = inner_out_tx.send(env).await;
                    }
                    for ack in out.acks {
                        let _ = inner_out_tx.send(ack_envelope(control_codec, &ack)).await;
                    }
                    for ex in out.dropped {
                        let _ = app_in_tx
                            .send(dropped_envelope(control_codec, ex.seq, ex.route_id))
                            .await;
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use mokosh_protocol::ack_channel;
    use std::sync::{Arc, Mutex};
    use tokio::time::{sleep, Duration as TDuration};

    /// A mock inner transport: records everything the link sends to the "network"
    /// in `sent`, and lets the test push envelopes "from the network" via `inject_rx`.
    struct MockInner {
        sent: Arc<Mutex<Vec<Envelope>>>,
        inject_rx: mpsc::Receiver<Envelope>,
    }

    #[async_trait]
    impl Transport for MockInner {
        type Error = std::io::Error;

        async fn run(
            self,
            incoming_tx: mpsc::Sender<Envelope>,
            mut outgoing_rx: mpsc::Receiver<Envelope>,
        ) -> Result<(), Self::Error> {
            let MockInner {
                sent,
                mut inject_rx,
            } = self;
            loop {
                tokio::select! {
                    maybe = outgoing_rx.recv() => {
                        let Some(env) = maybe else { break };
                        sent.lock().unwrap().push(env);
                    }
                    maybe = inject_rx.recv() => {
                        if let Some(env) = maybe {
                            if incoming_tx.send(env).await.is_err() { break; }
                        }
                    }
                }
            }
            Ok(())
        }
    }

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

    fn game_env(mode: ReliabilityMode, seq: u64, payload: &'static [u8]) -> Envelope {
        Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            100,
            seq,
            mode.to_flags(),
            Bytes::from_static(payload),
        )
    }

    /// `(sent-on-wire, inject-from-network, app→link, link→app)`.
    type LinkHarness = (
        Arc<Mutex<Vec<Envelope>>>,
        mpsc::Sender<Envelope>,
        mpsc::Sender<Envelope>,
        mpsc::Receiver<Envelope>,
    );

    fn spawn_link() -> LinkHarness {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (inject_tx, inject_rx) = mpsc::channel(16);
        let inner = MockInner {
            sent: sent.clone(),
            inject_rx,
        };
        let link = ReliableLink::new(inner, fast_cfg()).with_tick(TDuration::from_millis(10));
        let (app_in_tx, app_in_rx) = mpsc::channel(16);
        let (app_out_tx, app_out_rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let _ = link.run(app_in_tx, app_out_rx).await;
        });
        (sent, inject_tx, app_out_tx, app_in_rx)
    }

    #[tokio::test]
    async fn stamps_and_retransmits_until_acked() {
        let (sent, inject_tx, app_out_tx, _app_in_rx) = spawn_link();

        // App sends a ReliableOrdered game message (unstamped: msg_id 0).
        app_out_tx
            .send(game_env(ReliabilityMode::ReliableOrdered, 0, b"hi"))
            .await
            .unwrap();

        // Unacked ⇒ retransmitted a few times.
        sleep(TDuration::from_millis(90)).await;
        {
            let s = sent.lock().unwrap();
            assert!(s.len() >= 2, "should retransmit (got {})", s.len());
            assert_eq!(s[0].msg_id, 1, "link assigns the reliable game sequence");
            assert_eq!(s[0].route_id, 100);
        }

        // ACK seq 1 on the game channel ⇒ retransmits stop.
        let ack = Ack {
            channel: ack_channel::GAME,
            cumulative_ack: 1,
            ack_bitmap: 0,
        };
        let codec = CodecType::from_id(1).unwrap();
        let ack_env = control_envelope(codec, routes::ACK, codec.encode(&ack).unwrap());
        inject_tx.send(ack_env).await.unwrap();

        sleep(TDuration::from_millis(30)).await;
        let count = sent.lock().unwrap().len();
        sleep(TDuration::from_millis(60)).await;
        assert_eq!(
            sent.lock().unwrap().len(),
            count,
            "no more retransmits after ACK"
        );
    }

    #[tokio::test]
    async fn delivers_inbound_and_emits_ack() {
        let (sent, inject_tx, _app_out_tx, mut app_in_rx) = spawn_link();

        // Network delivers a ReliableOrdered game message (seq 1).
        inject_tx
            .send(game_env(ReliabilityMode::ReliableOrdered, 1, b"payload"))
            .await
            .unwrap();

        // It is delivered to the app...
        let delivered = tokio::time::timeout(TDuration::from_secs(1), app_in_rx.recv())
            .await
            .expect("delivered in time")
            .expect("channel open");
        assert_eq!(delivered.route_id, 100);
        assert_eq!(delivered.payload, Bytes::from_static(b"payload"));

        // ...and an ACK is emitted back onto the wire.
        sleep(TDuration::from_millis(40)).await;
        let acked = sent
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.route_id == routes::ACK);
        assert!(acked, "an ACK should be sent for the reliable message");
    }
}
