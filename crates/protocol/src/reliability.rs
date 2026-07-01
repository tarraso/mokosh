//! Transport-agnostic reliability layer (ACK + retransmission + ordering + TTL).
//!
//! This module is a **pure, async-free, deterministic state machine**. It does
//! no I/O and uses no tokio — all time is supplied by the caller as a [`MonoMillisecond`]
//! (monotonic milliseconds), so it compiles unchanged for WASM and is trivial to
//! unit-test. The native UDP client/server drive it from their event loops.
//!
//! # Concepts
//! - [`ReliabilityMode`] — per-message delivery guarantee, encoded in
//!   [`EnvelopeFlags`] so it travels on the wire.
//! - [`ReliabilitySender`] — tracks outstanding reliable messages, retransmits
//!   them on a timer with exponential backoff, and gives up after a per-message
//!   TTL (or a hard retry cap), reporting the drop.
//! - [`ReliabilityReceiver`] — deduplicates, optionally reorders, and produces
//!   [`Ack`]s (cumulative + selective bitmap).
//! - [`ReliabilityChannel`] — one sender + receiver over one sequence space.
//! - [`ReliabilityState`] — the two channels a peer needs: `control`
//!   (route_id < 100, handshake/auth) and `game` (route_id >= 100).
//!
//! Reuses the envelope `msg_id` as the sequence number. TTL is **never**
//! serialized; it lives in the local outstanding table.

use crate::messages::{ack_channel, Ack};
use crate::messages::GAME_MESSAGES_START;
use crate::{Envelope, EnvelopeFlags};
use std::collections::BTreeMap;
use std::time::Duration;

/// Width of the selective-ACK bitmap and therefore the maximum reliable
/// in-flight window per channel. The `Ack.ack_bitmap` (a `u64`) can represent
/// exactly this many sequence numbers above the cumulative point, so the sender
/// window must not exceed it — otherwise out-of-window receipts could not be
/// acknowledged. Throughput per channel is bounded by `ACK_WINDOW / RTT`.
pub const ACK_WINDOW: u64 = 64;

/// Monotonic timestamp in milliseconds, supplied by the caller.
///
/// Using an integer (rather than `std::time::Instant`) keeps this module
/// WASM-compatible and makes tests fully deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoMillisecond(pub u64);

impl MonoMillisecond {
    /// Creates a timestamp from a millisecond value.
    #[inline]
    pub fn from_millis(ms: u64) -> Self {
        MonoMillisecond(ms)
    }

    #[inline]
    fn add(self, d: Duration) -> Self {
        MonoMillisecond(self.0.saturating_add(d.as_millis() as u64))
    }

    /// Milliseconds elapsed since `earlier` (saturating).
    #[inline]
    fn millis_since(self, earlier: MonoMillisecond) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// Per-message delivery guarantee.
///
/// Encoded in [`EnvelopeFlags`] (bits `RELIABLE`, `SEQUENCED`, `ORDERED`) so the
/// receiver knows how to treat each message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReliabilityMode {
    /// Fire-and-forget. May be lost, duplicated, or reordered.
    Unreliable,
    /// Fire-and-forget, but the receiver drops anything older than the latest seen.
    UnreliableSequenced,
    /// Guaranteed delivery (ACK + retransmit); delivered as soon as received.
    Reliable,
    /// Guaranteed delivery AND strictly in-order delivery (receiver buffers gaps).
    ReliableOrdered,
}

impl ReliabilityMode {
    /// Decodes the reliability mode from envelope flags.
    ///
    /// A bare `RELIABLE` bit (the legacy default) decodes to [`Reliable`].
    pub fn from_flags(flags: EnvelopeFlags) -> Self {
        let reliable = flags.contains(EnvelopeFlags::RELIABLE);
        let ordered = flags.contains(EnvelopeFlags::ORDERED);
        let sequenced = flags.contains(EnvelopeFlags::SEQUENCED);
        match (reliable, ordered, sequenced) {
            (true, true, _) => ReliabilityMode::ReliableOrdered,
            (true, false, _) => ReliabilityMode::Reliable,
            (false, _, true) => ReliabilityMode::UnreliableSequenced,
            (false, _, false) => ReliabilityMode::Unreliable,
        }
    }

    /// Returns the reliability subset of envelope flags for this mode.
    ///
    /// Callers OR this with `ENCRYPTED`/`COMPRESSED` as needed.
    pub fn to_flags(self) -> EnvelopeFlags {
        match self {
            ReliabilityMode::Unreliable => EnvelopeFlags::empty(),
            ReliabilityMode::UnreliableSequenced => EnvelopeFlags::SEQUENCED,
            ReliabilityMode::Reliable => EnvelopeFlags::RELIABLE,
            ReliabilityMode::ReliableOrdered => {
                EnvelopeFlags::RELIABLE | EnvelopeFlags::SEQUENCED | EnvelopeFlags::ORDERED
            }
        }
    }

    /// Whether this mode requires acknowledgement + retransmission.
    #[inline]
    pub fn is_reliable(self) -> bool {
        matches!(self, ReliabilityMode::Reliable | ReliabilityMode::ReliableOrdered)
    }

    /// Whether this mode requires strict in-order delivery.
    #[inline]
    pub fn is_ordered(self) -> bool {
        matches!(self, ReliabilityMode::ReliableOrdered)
    }
}

/// Tuning parameters for the reliability layer.
#[derive(Debug, Clone)]
pub struct ReliabilityConfig {
    /// Initial retransmission timeout before any RTT samples exist.
    pub initial_rto: Duration,
    /// Lower clamp for the computed RTO.
    pub min_rto: Duration,
    /// Upper clamp for the computed RTO (and for backoff).
    pub max_rto: Duration,
    /// Multiplier applied to RTO on each retransmission (exponential backoff).
    pub backoff_factor: f32,
    /// Hard cap on retransmissions regardless of TTL.
    pub max_retries: u32,
    /// Default per-message TTL when the caller doesn't specify one.
    pub default_ttl: Duration,
    /// Max time to coalesce acknowledgements before flushing.
    pub ack_delay: Duration,
    /// Upper bound on out-of-order messages held for ordered delivery.
    pub ordering_buffer_limit: usize,
    /// Reliable in-flight window **per channel**: the maximum span of
    /// unacknowledged sequence numbers above the cumulative ACK point. Clamped to
    /// [`ACK_WINDOW`] (the selective-ACK bitmap width) — larger values have no
    /// effect since out-of-window receipts cannot be acknowledged. When the peer
    /// falls a full window behind on ACKs the connection is torn down. Bounds
    /// sender memory and caps throughput at `send_window / RTT`.
    pub send_window: u64,
    /// EWMA smoothing factor (alpha) for SRTT estimation.
    pub rtt_smoothing: f32,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            initial_rto: Duration::from_millis(100),
            min_rto: Duration::from_millis(50),
            max_rto: Duration::from_secs(2),
            backoff_factor: 2.0,
            max_retries: 10,
            default_ttl: Duration::from_secs(10),
            ack_delay: Duration::from_millis(20),
            ordering_buffer_limit: 256,
            send_window: ACK_WINDOW,
            rtt_smoothing: 0.125,
        }
    }
}

/// A reliable message that was given up on (TTL or retry cap exhausted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredMessage {
    /// Sequence number (envelope `msg_id`) of the dropped message.
    pub seq: u64,
    /// Route of the dropped message (so the app can identify it).
    pub route_id: u16,
    /// Correlation id of the dropped message (0 if not an RPC).
    pub correlation_id: u64,
}

/// Outcome of feeding an inbound envelope to a [`ReliabilityReceiver`].
#[derive(Debug)]
pub enum ReceiveOutcome {
    /// Deliver this envelope to the application now.
    Deliver(Envelope),
    /// Deliver these envelopes, in order, now (ordered drain).
    DeliverMany(Vec<Envelope>),
    /// Nothing to deliver right now (e.g. buffered out-of-order, or sequenced-stale).
    Drop,
    /// A duplicate (already received); silently ignore — do NOT treat as an attack.
    DropDuplicate,
    /// The ordering buffer is full; the caller should disconnect the peer.
    BufferOverflow,
}

// ============================================================================
// Sender
// ============================================================================

#[derive(Debug)]
struct OutstandingMessage {
    envelope: Envelope,
    first_sent: MonoMillisecond,
    next_retransmit_at: MonoMillisecond,
    retries: u32,
    expires_at: MonoMillisecond,
    rto: Duration,
    retransmitted: bool,
    /// Whether TTL / retry-cap may drop this message. False for `ReliableOrdered`
    /// (dropping an ordered message would permanently stall the stream), so those
    /// retransmit until ACKed or the connection is torn down by keepalive timeout.
    droppable: bool,
}

/// Tracks outstanding reliable messages and drives retransmission/expiry.
///
/// Internal to the reliability layer — driven through [`ReliablePipe`] /
/// [`SessionPipe`].
#[derive(Debug)]
pub(crate) struct ReliabilitySender {
    cfg: ReliabilityConfig,
    outstanding: BTreeMap<u64, OutstandingMessage>,
    srtt: Option<f64>,
    rttvar: Option<f64>,
    /// Highest cumulative ACK received (window anchor).
    acked_cumulative: u64,
    /// Highest sequence number ever tracked (window head).
    highest_sent: u64,
}

impl ReliabilitySender {
    /// Creates a new sender with the given config.
    pub fn new(cfg: ReliabilityConfig) -> Self {
        Self {
            cfg,
            outstanding: BTreeMap::new(),
            srtt: None,
            rttvar: None,
            acked_cumulative: 0,
            highest_sent: 0,
        }
    }

    /// Effective window width (config clamped to the ACK bitmap width).
    #[inline]
    fn window(&self) -> u64 {
        self.cfg.send_window.min(ACK_WINDOW)
    }

    /// Records an outgoing message for retransmission (no-op for unreliable modes).
    ///
    /// `envelope.msg_id` is used as the sequence number; it must already carry
    /// the reliability flags for `mode`.
    ///
    /// `ttl` and the retry cap only apply to [`ReliabilityMode::Reliable`]
    /// (unordered). For [`ReliabilityMode::ReliableOrdered`] they are ignored:
    /// dropping an ordered message would leave a gap the receiver waits on
    /// forever, so ordered messages retransmit until ACKed (the connection-level
    /// keepalive timeout reaps a truly dead peer).
    pub fn on_send(&mut self, envelope: &Envelope, mode: ReliabilityMode, ttl: Duration, now: MonoMillisecond) {
        if !mode.is_reliable() {
            return;
        }
        // Ordered messages must never be dropped (would stall the stream).
        let droppable = !mode.is_ordered();
        let rto = self.base_rto();
        if envelope.msg_id > self.highest_sent {
            self.highest_sent = envelope.msg_id;
        }
        self.outstanding.insert(
            envelope.msg_id,
            OutstandingMessage {
                envelope: envelope.clone(),
                first_sent: now,
                next_retransmit_at: now.add(rto),
                retries: 0,
                expires_at: now.add(ttl),
                rto,
                retransmitted: false,
                droppable,
            },
        );
    }

    /// Processes an incoming ACK, clearing acknowledged messages and updating RTT.
    ///
    /// Returns the list of sequence numbers that were acknowledged.
    pub fn on_ack(&mut self, ack: &Ack, now: MonoMillisecond) -> Vec<u64> {
        let mut acked = Vec::new();
        let cum = ack.cumulative_ack;
        if cum > self.acked_cumulative {
            self.acked_cumulative = cum;
        }

        // Cumulative: everything up to and including `cum`.
        let keys: Vec<u64> = self.outstanding.range(..=cum).map(|(k, _)| *k).collect();
        for k in keys {
            if let Some(m) = self.outstanding.remove(&k) {
                if !m.retransmitted {
                    self.update_rtt(now.millis_since(m.first_sent) as f64);
                }
                acked.push(k);
            }
        }

        // Selective: the bitmap covers the sequence numbers above `cum`.
        for i in 0..ACK_WINDOW {
            if (ack.ack_bitmap >> i) & 1 == 1 {
                let seq = cum + 1 + i;
                if let Some(m) = self.outstanding.remove(&seq) {
                    if !m.retransmitted {
                        self.update_rtt(now.millis_since(m.first_sent) as f64);
                    }
                    acked.push(seq);
                }
            }
        }
        acked
    }

    /// Returns envelopes whose retransmit timer has fired (and reschedules them).
    ///
    /// Call [`poll_expired`](Self::poll_expired) first so expired messages are
    /// not retransmitted.
    pub fn poll_retransmits(&mut self, now: MonoMillisecond) -> Vec<Envelope> {
        let max_retries = self.cfg.max_retries;
        let backoff = self.cfg.backoff_factor as f64;
        let max_rto = self.cfg.max_rto.as_millis() as u64;
        let min_rto = self.cfg.min_rto.as_millis() as u64;

        let mut out = Vec::new();
        for msg in self.outstanding.values_mut() {
            // Droppable messages stop at the retry cap / TTL (reaped by poll_expired);
            // non-droppable (ordered) messages retransmit until ACKed.
            if msg.droppable && msg.retries >= max_retries {
                continue;
            }
            let within_ttl = !msg.droppable || now < msg.expires_at;
            if now >= msg.next_retransmit_at && within_ttl {
                msg.retries += 1;
                msg.retransmitted = true;
                let next = ((msg.rto.as_millis() as f64) * backoff) as u64;
                let next = next.clamp(min_rto, max_rto);
                msg.rto = Duration::from_millis(next);
                msg.next_retransmit_at = now.add(msg.rto);
                out.push(msg.envelope.clone());
            }
        }
        out
    }

    /// Removes and returns messages that have exhausted their TTL or retry cap.
    ///
    /// Only `droppable` (non-ordered) messages are ever reaped; `ReliableOrdered`
    /// messages are never dropped here (see [`on_send`](Self::on_send)).
    pub fn poll_expired(&mut self, now: MonoMillisecond) -> Vec<ExpiredMessage> {
        let max_retries = self.cfg.max_retries;
        let keys: Vec<u64> = self
            .outstanding
            .iter()
            .filter(|(_, m)| m.droppable && (now >= m.expires_at || m.retries >= max_retries))
            .map(|(k, _)| *k)
            .collect();

        let mut expired = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(m) = self.outstanding.remove(&k) {
                expired.push(ExpiredMessage {
                    seq: k,
                    route_id: m.envelope.route_id,
                    correlation_id: m.envelope.correlation_id,
                });
            }
        }
        expired
    }

    /// Number of messages awaiting acknowledgement (test helper).
    #[cfg(test)]
    pub fn outstanding_len(&self) -> usize {
        self.outstanding.len()
    }

    /// Whether the in-flight window is full, i.e. the span of unacknowledged
    /// sequence numbers above the cumulative ACK point has reached `send_window`.
    ///
    /// Callers must check this before [`on_send`](Self::on_send) for reliable
    /// modes; a full window means the peer is a full window behind on ACKs and
    /// the connection should be torn down. Span-based (not a raw count) so it
    /// matches what the selective-ACK bitmap can represent.
    pub fn is_full(&self) -> bool {
        self.highest_sent.saturating_sub(self.acked_cumulative) >= self.window()
    }

    fn base_rto(&self) -> Duration {
        let ms = match (self.srtt, self.rttvar) {
            (Some(srtt), Some(rttvar)) => srtt + 4.0 * rttvar,
            _ => self.cfg.initial_rto.as_millis() as f64,
        };
        let lo = self.cfg.min_rto.as_millis() as f64;
        let hi = self.cfg.max_rto.as_millis() as f64;
        Duration::from_millis(ms.clamp(lo, hi) as u64)
    }

    fn update_rtt(&mut self, sample_ms: f64) {
        let alpha = self.cfg.rtt_smoothing as f64;
        let beta = (alpha * 2.0).min(0.5);
        match (self.srtt, self.rttvar) {
            (Some(srtt), Some(rttvar)) => {
                self.rttvar = Some((1.0 - beta) * rttvar + beta * (srtt - sample_ms).abs());
                self.srtt = Some((1.0 - alpha) * srtt + alpha * sample_ms);
            }
            _ => {
                self.srtt = Some(sample_ms);
                self.rttvar = Some(sample_ms / 2.0);
            }
        }
    }
}

// ============================================================================
// Receiver
// ============================================================================

/// Deduplicates, reorders, and acknowledges inbound reliable messages.
///
/// Internal to the reliability layer — driven through [`ReliablePipe`] /
/// [`SessionPipe`].
#[derive(Debug)]
pub(crate) struct ReliabilityReceiver {
    cfg: ReliabilityConfig,
    channel: u8,
    /// Highest reliable sequence seen (frontier for `drain_ordered`'s skip).
    highest_reliable_seen: u64,
    /// Highest `UnreliableSequenced` sequence seen (independent staleness check).
    highest_sequenced_seen: u64,
    /// Highest contiguous received sequence (cumulative ACK base).
    cumulative: u64,
    /// Selective bitmap: bit `i` => `cumulative + 1 + i` received (width [`ACK_WINDOW`]).
    above: u64,
    /// Next sequence number the ordered stream will deliver.
    deliver_next: u64,
    /// Out-of-order ordered messages awaiting their predecessors.
    ordering_buffer: BTreeMap<u64, Envelope>,
    pending_ack: bool,
    ack_due_at: Option<MonoMillisecond>,
}

impl ReliabilityReceiver {
    /// Creates a receiver for the given channel (`ack_channel::CONTROL`/`GAME`).
    pub fn new(cfg: ReliabilityConfig, channel: u8) -> Self {
        Self {
            cfg,
            channel,
            highest_reliable_seen: 0,
            highest_sequenced_seen: 0,
            cumulative: 0,
            above: 0,
            deliver_next: 1, // sequence numbers start at 1
            ordering_buffer: BTreeMap::new(),
            pending_ack: false,
            ack_due_at: None,
        }
    }

    /// Feeds an inbound envelope, returning what (if anything) to deliver.
    pub fn on_receive(&mut self, envelope: Envelope, now: MonoMillisecond) -> ReceiveOutcome {
        let mode = ReliabilityMode::from_flags(envelope.flags);
        let seq = envelope.msg_id;

        match mode {
            ReliabilityMode::Unreliable => ReceiveOutcome::Deliver(envelope),

            ReliabilityMode::UnreliableSequenced => {
                if seq <= self.highest_sequenced_seen {
                    ReceiveOutcome::Drop
                } else {
                    self.highest_sequenced_seen = seq;
                    ReceiveOutcome::Deliver(envelope)
                }
            }

            ReliabilityMode::Reliable | ReliabilityMode::ReliableOrdered => {
                // Always (re)schedule an ACK — even for duplicates, since a
                // previous ACK may have been lost.
                self.schedule_ack(now);

                if self.is_received(seq) {
                    return ReceiveOutcome::DropDuplicate;
                }
                self.mark_received(seq);
                if seq > self.highest_reliable_seen {
                    self.highest_reliable_seen = seq;
                }

                if mode.is_ordered() {
                    self.deliver_ordered(seq, envelope)
                } else {
                    ReceiveOutcome::Deliver(envelope)
                }
            }
        }
    }

    /// Produces a coalesced ACK if one is due, clearing the pending flag.
    pub fn build_ack(&mut self, now: MonoMillisecond) -> Option<Ack> {
        if self.pending_ack {
            if let Some(due) = self.ack_due_at {
                if now >= due {
                    self.pending_ack = false;
                    self.ack_due_at = None;
                    return Some(Ack {
                        channel: self.channel,
                        cumulative_ack: self.cumulative,
                        ack_bitmap: self.above,
                    });
                }
            }
        }
        None
    }

    fn deliver_ordered(&mut self, seq: u64, envelope: Envelope) -> ReceiveOutcome {
        use std::cmp::Ordering;
        match seq.cmp(&self.deliver_next) {
            Ordering::Equal => {
                let mut out = vec![envelope];
                self.deliver_next += 1;
                self.drain_ordered(&mut out);
                if out.len() == 1 {
                    ReceiveOutcome::Deliver(out.pop().unwrap())
                } else {
                    ReceiveOutcome::DeliverMany(out)
                }
            }
            Ordering::Greater => {
                if self.ordering_buffer.len() >= self.cfg.ordering_buffer_limit {
                    return ReceiveOutcome::BufferOverflow;
                }
                self.ordering_buffer.insert(seq, envelope);
                ReceiveOutcome::Drop // buffered; nothing to deliver yet
            }
            Ordering::Less => ReceiveOutcome::DropDuplicate,
        }
    }

    /// Advances `deliver_next` over buffered messages and seqs already
    /// delivered out-of-band (unordered-reliable on the same channel).
    fn drain_ordered(&mut self, out: &mut Vec<Envelope>) {
        loop {
            let next = self.deliver_next;
            if let Some(env) = self.ordering_buffer.remove(&next) {
                out.push(env);
                self.deliver_next += 1;
            } else if next <= self.highest_reliable_seen && self.is_received(next) {
                // Delivered immediately as unordered-reliable; skip it.
                self.deliver_next += 1;
            } else {
                break;
            }
        }
    }

    fn schedule_ack(&mut self, now: MonoMillisecond) {
        self.pending_ack = true;
        if self.ack_due_at.is_none() {
            self.ack_due_at = Some(now.add(self.cfg.ack_delay));
        }
    }

    fn is_received(&self, seq: u64) -> bool {
        if seq == 0 {
            return false;
        }
        if seq <= self.cumulative {
            return true;
        }
        let i = seq - self.cumulative - 1;
        if i < ACK_WINDOW {
            (self.above >> i) & 1 == 1
        } else {
            false
        }
    }

    fn mark_received(&mut self, seq: u64) {
        if seq == 0 || seq <= self.cumulative {
            return;
        }
        let i = seq - self.cumulative - 1;
        if i < ACK_WINDOW {
            self.above |= 1u64 << i;
        }
        // Absorb contiguous run into the cumulative point.
        while self.above & 1 == 1 {
            self.cumulative += 1;
            self.above >>= 1;
        }
    }
}

// ============================================================================
// Channel + State
// ============================================================================

/// A sender + receiver pair over a single sequence space.
#[derive(Debug)]
pub(crate) struct ReliabilityChannel {
    /// Outgoing reliable-message tracker.
    sender: ReliabilitySender,
    /// Inbound dedup/ordering/ACK tracker.
    receiver: ReliabilityReceiver,
}

impl ReliabilityChannel {
    /// Creates a channel for the given channel id.
    fn new(cfg: ReliabilityConfig, channel: u8) -> Self {
        Self {
            sender: ReliabilitySender::new(cfg.clone()),
            receiver: ReliabilityReceiver::new(cfg, channel),
        }
    }
}

/// All reliability state for one peer: a control channel and a game channel.
///
/// The two channels keep independent sequence spaces so the handshake/auth
/// (control, route_id < 100) can be made reliable without colliding with game
/// traffic (route_id >= 100).
#[derive(Debug)]
pub(crate) struct ReliabilityState {
    /// Control channel (HELLO/AUTH handshake).
    control: ReliabilityChannel,
    /// Game channel (application messages).
    game: ReliabilityChannel,
}

impl ReliabilityState {
    /// Creates fresh reliability state from a config.
    fn new(cfg: ReliabilityConfig) -> Self {
        Self {
            control: ReliabilityChannel::new(cfg.clone(), ack_channel::CONTROL),
            game: ReliabilityChannel::new(cfg, ack_channel::GAME),
        }
    }

    /// Selects a channel by ACK channel id (`ack_channel::CONTROL`/`GAME`).
    #[inline]
    fn channel_by_id(&mut self, channel: u8) -> Option<&mut ReliabilityChannel> {
        match channel {
            ack_channel::CONTROL => Some(&mut self.control),
            ack_channel::GAME => Some(&mut self.game),
            _ => None,
        }
    }

    /// Collects retransmissions from both channels.
    pub fn poll_retransmits(&mut self, now: MonoMillisecond) -> Vec<Envelope> {
        let mut out = self.control.sender.poll_retransmits(now);
        out.extend(self.game.sender.poll_retransmits(now));
        out
    }

    /// Collects expired messages from both channels.
    pub fn poll_expired(&mut self, now: MonoMillisecond) -> Vec<ExpiredMessage> {
        let mut out = self.control.sender.poll_expired(now);
        out.extend(self.game.sender.poll_expired(now));
        out
    }

    /// Collects any ACKs that are due from both channels.
    pub fn build_acks(&mut self, now: MonoMillisecond) -> Vec<Ack> {
        let mut out = Vec::new();
        if let Some(a) = self.control.receiver.build_ack(now) {
            out.push(a);
        }
        if let Some(a) = self.game.receiver.build_ack(now) {
            out.push(a);
        }
        out
    }
}

// ============================================================================
// SessionPipe: the link-layer facade over reliability state
// ============================================================================

/// Returned by [`SessionPipe::stamp_outgoing`] when the reliable in-flight
/// window for the target channel is full (the peer is a whole window behind on
/// ACKs). No sequence number is consumed and nothing is tracked — the caller
/// should tear the connection down (server → `Overloaded`; client →
/// `SendWindowExceeded`). This is terminal, **not** retry-style backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFull;

/// Work produced by one [`SessionPipe::tick`]: messages to (re)send and reports
/// to surface. Empty for a pass-through pipe.
#[derive(Debug, Default)]
pub struct TickOutput {
    /// Reliable envelopes whose retransmit timer fired — resend them verbatim.
    pub retransmits: Vec<Envelope>,
    /// Coalesced ACKs to send back to the peer (encode + send on the ACK route).
    pub acks: Vec<Ack>,
    /// Reliable messages given up on (TTL / retry cap) — surface as drops.
    pub dropped: Vec<ExpiredMessage>,
}

/// One peer's reliability state plus its outgoing sequence counters.
///
/// This is the reusable unit the event loops drive through [`SessionPipe`] and
/// that the transport-layer reliability decorator (link layer) will hold one of
/// per peer. It owns sequence assignment so callers never touch raw sequence
/// numbers.
#[derive(Debug)]
pub struct ReliablePipe {
    state: ReliabilityState,
    /// Contiguous sequence for reliable game messages (gap-free ordered stream).
    reliable_game_seq: u64,
    /// Independent sequence for `UnreliableSequenced` game messages.
    sequenced_game_seq: u64,
    /// Contiguous sequence for reliable control messages (HELLO_OK / AUTH).
    control_seq: u64,
}

impl ReliablePipe {
    /// Creates a fresh reliable pipe from a config.
    pub fn new(cfg: ReliabilityConfig) -> Self {
        Self {
            state: ReliabilityState::new(cfg),
            reliable_game_seq: 1,
            sequenced_game_seq: 1,
            control_seq: 1,
        }
    }

    #[inline]
    fn is_control(route_id: u16) -> bool {
        route_id < GAME_MESSAGES_START
    }

    /// Assigns the sequence number into `env.msg_id` and tracks the envelope for
    /// retransmission (reliable modes only). The envelope's flags must already
    /// encode `mode`; the channel (control vs game) is chosen by `env.route_id`.
    ///
    /// Returns [`WindowFull`] *before* consuming a sequence number if the
    /// reliable window for the channel is full.
    pub fn stamp_outgoing(
        &mut self,
        env: &mut Envelope,
        mode: ReliabilityMode,
        ttl: Duration,
        now: MonoMillisecond,
    ) -> Result<(), WindowFull> {
        let is_control = Self::is_control(env.route_id);
        let channel = if is_control {
            &mut self.state.control
        } else {
            &mut self.state.game
        };

        if mode.is_reliable() && channel.sender.is_full() {
            return Err(WindowFull);
        }

        env.msg_id = match mode {
            ReliabilityMode::Reliable | ReliabilityMode::ReliableOrdered => {
                let counter = if is_control {
                    &mut self.control_seq
                } else {
                    &mut self.reliable_game_seq
                };
                let id = *counter;
                *counter = counter.wrapping_add(1);
                id
            }
            ReliabilityMode::UnreliableSequenced => {
                let id = self.sequenced_game_seq;
                self.sequenced_game_seq = self.sequenced_game_seq.wrapping_add(1);
                id
            }
            ReliabilityMode::Unreliable => 0,
        };

        if mode.is_reliable() {
            channel.sender.on_send(env, mode, ttl, now);
        }
        Ok(())
    }

    /// Feeds an inbound envelope through the appropriate channel's receiver
    /// (dedup / ordering). Channel is chosen by `env.route_id`.
    pub fn process_incoming(&mut self, env: Envelope, now: MonoMillisecond) -> ReceiveOutcome {
        let channel = if Self::is_control(env.route_id) {
            &mut self.state.control
        } else {
            &mut self.state.game
        };
        channel.receiver.on_receive(env, now)
    }

    /// Clears acknowledged outstanding messages for the ACK's channel.
    pub fn on_ack(&mut self, ack: &Ack, now: MonoMillisecond) {
        if let Some(channel) = self.state.channel_by_id(ack.channel) {
            channel.sender.on_ack(ack, now);
        }
    }

    /// Drives retransmission, expiry and ACK flushing for one tick.
    ///
    /// Expiry is collected before retransmits so a timed-out message is not
    /// resent on the same tick.
    pub fn tick(&mut self, now: MonoMillisecond) -> TickOutput {
        TickOutput {
            dropped: self.state.poll_expired(now),
            retransmits: self.state.poll_retransmits(now),
            acks: self.state.build_acks(now),
        }
    }

    /// Full inbound routing for a non-ACK envelope, encapsulating the rules the
    /// event loops used to inline:
    /// - **best-effort control** (`route_id < 100` and **not** `SEQUENCED`, i.e.
    ///   PING/PONG/DISCONNECT with a bare `RELIABLE` bit) bypasses the receiver
    ///   entirely — delivered as-is, never deduped or ACKed;
    /// - everything else (sequenced control HELLO/AUTH, and all game traffic)
    ///   goes through the channel receiver for dedup / ordering / ACK scheduling.
    ///
    /// ACK envelopes (`routes::ACK`) must be handled by the caller via
    /// [`on_ack`](Self::on_ack) *before* calling this — they are never delivered.
    pub fn handle_incoming(&mut self, env: Envelope, now: MonoMillisecond) -> Inbound {
        let is_control = Self::is_control(env.route_id);
        let sequenced = env.flags.contains(EnvelopeFlags::SEQUENCED);
        if is_control && !sequenced {
            return Inbound::Deliver(vec![env]);
        }
        match self.process_incoming(env, now) {
            ReceiveOutcome::Deliver(e) => Inbound::Deliver(vec![e]),
            ReceiveOutcome::DeliverMany(es) => Inbound::Deliver(es),
            ReceiveOutcome::Drop | ReceiveOutcome::DropDuplicate => Inbound::Consumed,
            ReceiveOutcome::BufferOverflow => Inbound::Overflow,
        }
    }
}

/// Result of [`ReliablePipe::handle_incoming`].
#[derive(Debug)]
pub enum Inbound {
    /// Hand these envelopes (in order) to the application/event loop.
    Deliver(Vec<Envelope>),
    /// Nothing to deliver (duplicate, buffered out-of-order, or sequenced-stale).
    Consumed,
    /// The ordering buffer overflowed; the caller should tear the connection down.
    Overflow,
}

/// The link-layer pipe an event loop drives, uniform across transports.
///
/// Reliable transports (WebSocket / reliability disabled) use
/// [`Passthrough`](SessionPipe::Passthrough): a bare monotonic `msg_id` and no
/// ACK/retransmit/ordering — zero cost. Unreliable transports (UDP) use
/// [`Reliable`](SessionPipe::Reliable), the full state machine. This collapses
/// the former `if reliability.is_some()` branching into one polymorphic surface
/// and is the unit the reliability decorator will later own per peer.
#[derive(Debug)]
pub enum SessionPipe {
    /// Pass-through: assign a legacy monotonic game `msg_id`; control carries 0.
    Passthrough {
        /// Monotonic counter for game-message `msg_id`s (replay-window input).
        game_seq: u64,
    },
    /// Full reliability state machine for an unreliable transport.
    Reliable(Box<ReliablePipe>),
}

impl SessionPipe {
    /// A pass-through pipe (reliable transport / reliability disabled).
    pub fn passthrough() -> Self {
        SessionPipe::Passthrough { game_seq: 1 }
    }

    /// A reliable pipe for an unreliable transport.
    pub fn reliable(cfg: ReliabilityConfig) -> Self {
        SessionPipe::Reliable(Box::new(ReliablePipe::new(cfg)))
    }

    /// Builds a pipe from an optional config: `Some` → reliable, `None` →
    /// pass-through.
    pub fn from_config(cfg: Option<&ReliabilityConfig>) -> Self {
        match cfg {
            Some(c) => Self::reliable(c.clone()),
            None => Self::passthrough(),
        }
    }

    /// Whether this pipe runs the reliability state machine (ACK/retransmit/
    /// ordering). Used to negotiate the handshake flag and to gate the legacy
    /// replay-protection path (bypassed when reliability owns dedup).
    #[inline]
    pub fn is_reliable(&self) -> bool {
        matches!(self, SessionPipe::Reliable(_))
    }

    /// Assigns the outgoing sequence number into `env.msg_id` (and tracks
    /// reliable envelopes). See [`ReliablePipe::stamp_outgoing`]. Pass-through
    /// assigns a monotonic game `msg_id` (control carries 0) and never fails.
    pub fn stamp_outgoing(
        &mut self,
        env: &mut Envelope,
        mode: ReliabilityMode,
        ttl: Duration,
        now: MonoMillisecond,
    ) -> Result<(), WindowFull> {
        match self {
            SessionPipe::Passthrough { game_seq } => {
                if env.route_id >= GAME_MESSAGES_START {
                    env.msg_id = *game_seq;
                    *game_seq = game_seq.wrapping_add(1);
                } else {
                    env.msg_id = 0;
                }
                Ok(())
            }
            SessionPipe::Reliable(p) => p.stamp_outgoing(env, mode, ttl, now),
        }
    }

    /// Feeds an inbound reliable/sequenced envelope through the state machine.
    /// For a pass-through pipe there is no dedup/ordering, so the envelope is
    /// delivered as-is (callers gate this on [`is_reliable`](Self::is_reliable)
    /// and apply their own handling for the pass-through case).
    pub fn process_incoming(&mut self, env: Envelope, now: MonoMillisecond) -> ReceiveOutcome {
        match self {
            SessionPipe::Reliable(p) => p.process_incoming(env, now),
            SessionPipe::Passthrough { .. } => ReceiveOutcome::Deliver(env),
        }
    }

    /// Clears acknowledged outstanding messages (no-op for pass-through).
    pub fn on_ack(&mut self, ack: &Ack, now: MonoMillisecond) {
        if let SessionPipe::Reliable(p) = self {
            p.on_ack(ack, now);
        }
    }

    /// Drives retransmits / ACK flush / drop reporting for one tick (empty for
    /// pass-through).
    pub fn tick(&mut self, now: MonoMillisecond) -> TickOutput {
        match self {
            SessionPipe::Reliable(p) => p.tick(now),
            SessionPipe::Passthrough { .. } => TickOutput::default(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn env(seq: u64, route_id: u16, mode: ReliabilityMode) -> Envelope {
        Envelope::new_simple(1, 1, 0, route_id, seq, mode.to_flags(), Bytes::from_static(b"x"))
    }

    fn cfg() -> ReliabilityConfig {
        ReliabilityConfig::default()
    }

    fn t(ms: u64) -> MonoMillisecond {
        MonoMillisecond::from_millis(ms)
    }

    // ---- mode <-> flags ----

    #[test]
    fn mode_flag_roundtrip() {
        for mode in [
            ReliabilityMode::Unreliable,
            ReliabilityMode::UnreliableSequenced,
            ReliabilityMode::Reliable,
            ReliabilityMode::ReliableOrdered,
        ] {
            assert_eq!(ReliabilityMode::from_flags(mode.to_flags()), mode);
        }
    }

    #[test]
    fn legacy_reliable_bit_decodes_to_reliable() {
        assert_eq!(
            ReliabilityMode::from_flags(EnvelopeFlags::RELIABLE),
            ReliabilityMode::Reliable
        );
    }

    #[test]
    fn mode_flags_preserve_other_bits_via_or() {
        let flags = ReliabilityMode::ReliableOrdered.to_flags() | EnvelopeFlags::COMPRESSED;
        assert!(flags.contains(EnvelopeFlags::COMPRESSED));
        assert_eq!(
            ReliabilityMode::from_flags(flags),
            ReliabilityMode::ReliableOrdered
        );
    }

    // ---- sender: retransmit / backoff ----

    #[test]
    fn no_retransmit_before_rto() {
        let mut s = ReliabilitySender::new(cfg());
        s.on_send(&env(1, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_secs(10), t(0));
        assert!(s.poll_retransmits(t(50)).is_empty()); // initial_rto = 100ms
    }

    #[test]
    fn retransmit_at_rto_with_backoff() {
        let mut s = ReliabilitySender::new(cfg());
        s.on_send(&env(1, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_secs(10), t(0));
        // First retransmit at ~100ms.
        assert_eq!(s.poll_retransmits(t(100)).len(), 1);
        // Backoff to ~200ms: nothing at 250 relative? next at 100+200=300.
        assert!(s.poll_retransmits(t(250)).is_empty());
        assert_eq!(s.poll_retransmits(t(300)).len(), 1);
    }

    #[test]
    fn unreliable_is_not_tracked() {
        let mut s = ReliabilitySender::new(cfg());
        s.on_send(&env(1, 100, ReliabilityMode::Unreliable), ReliabilityMode::Unreliable, Duration::from_secs(10), t(0));
        assert_eq!(s.outstanding_len(), 0);
    }

    #[test]
    fn ack_clears_outstanding() {
        let mut s = ReliabilitySender::new(cfg());
        for seq in 1..=3 {
            s.on_send(&env(seq, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_secs(10), t(0));
        }
        let acked = s.on_ack(&Ack { channel: 1, cumulative_ack: 2, ack_bitmap: 0 }, t(10));
        assert_eq!(acked.len(), 2);
        assert_eq!(s.outstanding_len(), 1); // seq 3 remains
    }

    #[test]
    fn ack_bitmap_clears_selective() {
        let mut s = ReliabilitySender::new(cfg());
        for seq in 1..=4 {
            s.on_send(&env(seq, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_secs(10), t(0));
        }
        // cumulative 1, bitmap clears 3 (bit index 1 => cum+1+1 = 3) and 4 (bit 2).
        let acked = s.on_ack(&Ack { channel: 1, cumulative_ack: 1, ack_bitmap: 0b110 }, t(10));
        assert_eq!(acked.len(), 3); // 1, 3, 4
        assert_eq!(s.outstanding_len(), 1); // only seq 2 remains
    }

    #[test]
    fn ttl_expiry_reports_once() {
        let mut s = ReliabilitySender::new(cfg());
        s.on_send(&env(1, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_millis(500), t(0));
        assert!(s.poll_expired(t(400)).is_empty());
        let expired = s.poll_expired(t(600));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].seq, 1);
        assert_eq!(expired[0].route_id, 100);
        // Gone now — no second report and no retransmit.
        assert!(s.poll_expired(t(700)).is_empty());
        assert!(s.poll_retransmits(t(700)).is_empty());
    }

    #[test]
    fn ordered_ignores_ttl_and_retry_cap() {
        let mut c = cfg();
        c.max_retries = 2;
        c.initial_rto = Duration::from_millis(50);
        c.min_rto = Duration::from_millis(50);
        c.max_rto = Duration::from_millis(50);
        let mut s = ReliabilitySender::new(c);
        // Ordered message with a short TTL.
        s.on_send(
            &env(1, 100, ReliabilityMode::ReliableOrdered),
            ReliabilityMode::ReliableOrdered,
            Duration::from_millis(100),
            t(0),
        );
        // Far past TTL and the retry cap: still never expired...
        let mut now = 0u64;
        let mut sends = 0;
        for _ in 0..20 {
            now += 60;
            assert!(s.poll_expired(t(now)).is_empty(), "ordered must never expire");
            sends += s.poll_retransmits(t(now)).len();
        }
        // ...and it keeps being retransmitted (well beyond max_retries=2).
        assert!(sends > 2, "ordered should retransmit past the retry cap (got {sends})");
        assert_eq!(s.outstanding_len(), 1);
        // Only an ACK clears it.
        s.on_ack(&Ack { channel: 1, cumulative_ack: 1, ack_bitmap: 0 }, t(now));
        assert_eq!(s.outstanding_len(), 0);
    }

    #[test]
    fn window_fills_to_cap_and_frees_on_ack() {
        let mut c = cfg();
        c.send_window = 3;
        let mut s = ReliabilitySender::new(c);
        assert!(!s.is_full());
        for seq in 1..=3 {
            assert!(!s.is_full(), "should have room before reaching cap");
            s.on_send(
                &env(seq, 100, ReliabilityMode::ReliableOrdered),
                ReliabilityMode::ReliableOrdered,
                Duration::from_secs(10),
                t(0),
            );
        }
        // Cap reached.
        assert!(s.is_full());
        assert_eq!(s.outstanding_len(), 3);
        // An ACK frees room.
        s.on_ack(&Ack { channel: 1, cumulative_ack: 2, ack_bitmap: 0 }, t(1));
        assert!(!s.is_full());
        assert_eq!(s.outstanding_len(), 1);
    }

    // Regression for hole #1: with a gap, sequences beyond the old 32-bit window
    // (now within the 64-wide bitmap) are recorded, so retransmits are detected
    // as duplicates instead of being re-delivered (at-most-once preserved).
    #[test]
    fn reliable_unordered_dedups_beyond_32() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        // seq 1 is "lost"; receive 2..=40 (unordered Reliable), each delivered once.
        for seq in 2..=40 {
            assert!(matches!(
                r.on_receive(env(seq, 100, ReliabilityMode::Reliable), t(0)),
                ReceiveOutcome::Deliver(_)
            ));
        }
        // A retransmit of seq 37 (bit index 35 — past the old 32 limit) is a dup.
        assert!(matches!(
            r.on_receive(env(37, 100, ReliabilityMode::Reliable), t(0)),
            ReceiveOutcome::DropDuplicate
        ));
        // The ACK can represent all of them above the cumulative point.
        let ack = r.build_ack(t(100)).unwrap();
        assert_eq!(ack.cumulative_ack, 0);
        assert_eq!(ack.ack_bitmap & (1u64 << 35), 1u64 << 35); // seq 37 recorded
        // Gap fills: cumulative jumps past the whole contiguous run.
        let _ = r.on_receive(env(1, 100, ReliabilityMode::Reliable), t(0));
        let ack = r.build_ack(t(200)).unwrap();
        assert_eq!(ack.cumulative_ack, 40);
    }

    // Regression for hole #2: an interleaved unreliable message (separate sequence
    // space) does not stall an ordered stream.
    #[test]
    fn unreliable_does_not_stall_ordered() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        assert!(matches!(
            r.on_receive(env(1, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        // Unreliable arrives (independent space) — delivered, no effect on ordering.
        assert!(matches!(
            r.on_receive(env(7, 100, ReliabilityMode::Unreliable), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        // Ordered 2 and 3 still deliver in order (not waiting on the unreliable seq).
        assert!(matches!(
            r.on_receive(env(2, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        assert!(matches!(
            r.on_receive(env(3, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
    }

    #[test]
    fn max_retries_caps_retransmissions() {
        let mut c = cfg();
        c.max_retries = 3;
        c.max_rto = Duration::from_millis(50); // keep cadence tight
        c.min_rto = Duration::from_millis(50);
        c.initial_rto = Duration::from_millis(50);
        let mut s = ReliabilitySender::new(c);
        s.on_send(&env(1, 100, ReliabilityMode::Reliable), ReliabilityMode::Reliable, Duration::from_secs(60), t(0));
        let mut sends = 0;
        let mut now = 0u64;
        for _ in 0..20 {
            now += 60;
            sends += s.poll_retransmits(t(now)).len();
        }
        assert_eq!(sends, 3); // capped at max_retries
        assert_eq!(s.poll_expired(t(now)).len(), 1); // then expired
    }

    // ---- receiver: per-mode ----

    #[test]
    fn unreliable_always_delivers() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        assert!(matches!(
            r.on_receive(env(5, 100, ReliabilityMode::Unreliable), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        assert!(r.build_ack(t(100)).is_none()); // no ack for unreliable
    }

    #[test]
    fn unreliable_sequenced_drops_stale() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        assert!(matches!(
            r.on_receive(env(3, 100, ReliabilityMode::UnreliableSequenced), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        assert!(matches!(
            r.on_receive(env(2, 100, ReliabilityMode::UnreliableSequenced), t(0)),
            ReceiveOutcome::Drop
        ));
        assert!(matches!(
            r.on_receive(env(4, 100, ReliabilityMode::UnreliableSequenced), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
    }

    #[test]
    fn reliable_dedups_and_acks() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        assert!(matches!(
            r.on_receive(env(1, 100, ReliabilityMode::Reliable), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        assert!(matches!(
            r.on_receive(env(1, 100, ReliabilityMode::Reliable), t(0)),
            ReceiveOutcome::DropDuplicate
        ));
        // Ack due after ack_delay (20ms).
        assert!(r.build_ack(t(10)).is_none());
        let ack = r.build_ack(t(25)).unwrap();
        assert_eq!(ack.cumulative_ack, 1);
    }

    #[test]
    fn ordered_buffers_then_drains() {
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        // 1, 3, 2, 2, 5, 4
        assert!(matches!(
            r.on_receive(env(1, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Deliver(_)
        ));
        assert!(matches!(
            r.on_receive(env(3, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Drop // buffered
        ));
        // 2 arrives -> delivers 2 then drains 3.
        match r.on_receive(env(2, 100, ReliabilityMode::ReliableOrdered), t(0)) {
            ReceiveOutcome::DeliverMany(v) => {
                assert_eq!(v.iter().map(|e| e.msg_id).collect::<Vec<_>>(), vec![2, 3]);
            }
            other => panic!("expected DeliverMany, got {:?}", other),
        }
        // duplicate 2.
        assert!(matches!(
            r.on_receive(env(2, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::DropDuplicate
        ));
        // 5 buffered, 4 delivers 4 then 5.
        assert!(matches!(
            r.on_receive(env(5, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::Drop
        ));
        match r.on_receive(env(4, 100, ReliabilityMode::ReliableOrdered), t(0)) {
            ReceiveOutcome::DeliverMany(v) => {
                assert_eq!(v.iter().map(|e| e.msg_id).collect::<Vec<_>>(), vec![4, 5]);
            }
            other => panic!("expected DeliverMany, got {:?}", other),
        }
        let ack = r.build_ack(t(100)).unwrap();
        assert_eq!(ack.cumulative_ack, 5);
    }

    #[test]
    fn ordered_buffer_overflow_signals_disconnect() {
        let mut c = cfg();
        c.ordering_buffer_limit = 2;
        let mut r = ReliabilityReceiver::new(c, ack_channel::GAME);
        // Hold back seq 1; flood with future seqs.
        assert!(matches!(r.on_receive(env(3, 100, ReliabilityMode::ReliableOrdered), t(0)), ReceiveOutcome::Drop));
        assert!(matches!(r.on_receive(env(4, 100, ReliabilityMode::ReliableOrdered), t(0)), ReceiveOutcome::Drop));
        assert!(matches!(
            r.on_receive(env(5, 100, ReliabilityMode::ReliableOrdered), t(0)),
            ReceiveOutcome::BufferOverflow
        ));
    }

    #[test]
    fn roundtrip_sender_receiver_recovers_loss() {
        // Simulate: sender sends 1..=3 reliable-ordered; packet 2 lost on first
        // pass; retransmit recovers it.
        let mut s = ReliabilitySender::new(cfg());
        let mut r = ReliabilityReceiver::new(cfg(), ack_channel::GAME);
        let envs: Vec<Envelope> = (1..=3)
            .map(|seq| env(seq, 100, ReliabilityMode::ReliableOrdered))
            .collect();
        for e in &envs {
            s.on_send(e, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0));
        }
        // Deliver 1 and 3 (2 lost).
        let _ = r.on_receive(envs[0].clone(), t(0));
        let _ = r.on_receive(envs[2].clone(), t(0));
        // Ack reflects cumulative 1 with bit for 3.
        let ack = r.build_ack(t(25)).unwrap();
        assert_eq!(ack.cumulative_ack, 1);
        s.on_ack(&ack, t(25));
        assert_eq!(s.outstanding_len(), 1); // only seq 2 outstanding

        // Retransmit fires for seq 2.
        let rt = s.poll_retransmits(t(130));
        assert_eq!(rt.len(), 1);
        assert_eq!(rt[0].msg_id, 2);
        // Receiver gets 2 -> delivers 2 then 3.
        match r.on_receive(rt[0].clone(), t(130)) {
            ReceiveOutcome::DeliverMany(v) => {
                assert_eq!(v.iter().map(|e| e.msg_id).collect::<Vec<_>>(), vec![2, 3]);
            }
            other => panic!("expected DeliverMany, got {:?}", other),
        }
        let ack2 = r.build_ack(t(200)).unwrap();
        assert_eq!(ack2.cumulative_ack, 3);
        s.on_ack(&ack2, t(200));
        assert_eq!(s.outstanding_len(), 0);
    }

    #[test]
    fn state_routes_by_channel() {
        let mut st = ReliabilityState::new(cfg());
        assert_eq!(st.control.receiver.channel, ack_channel::CONTROL);
        assert_eq!(st.game.receiver.channel, ack_channel::GAME);
        assert!(st.channel_by_id(ack_channel::CONTROL).is_some());
        assert!(st.channel_by_id(ack_channel::GAME).is_some());
        assert!(st.channel_by_id(99).is_none());
    }

    // ---- SessionPipe ----

    fn blank_env(route_id: u16, mode: ReliabilityMode) -> Envelope {
        Envelope::new_simple(1, 1, 0, route_id, 0, mode.to_flags(), Bytes::from_static(b"x"))
    }

    #[test]
    fn passthrough_assigns_monotonic_game_id_and_zero_control() {
        let mut p = SessionPipe::passthrough();
        assert!(!p.is_reliable());
        let mut g1 = blank_env(100, ReliabilityMode::Reliable);
        let mut g2 = blank_env(100, ReliabilityMode::Reliable);
        p.stamp_outgoing(&mut g1, ReliabilityMode::Reliable, Duration::from_secs(10), t(0)).unwrap();
        p.stamp_outgoing(&mut g2, ReliabilityMode::Reliable, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!(g1.msg_id, 1);
        assert_eq!(g2.msg_id, 2);
        // Control carries no sequence in pass-through.
        let mut c = blank_env(5, ReliabilityMode::ReliableOrdered);
        p.stamp_outgoing(&mut c, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!(c.msg_id, 0);
        // Pass-through never tracks: tick is empty.
        let out = p.tick(t(100));
        assert!(out.retransmits.is_empty() && out.acks.is_empty() && out.dropped.is_empty());
    }

    #[test]
    fn reliable_pipe_assigns_contiguous_spaces_and_tracks() {
        let mut p = SessionPipe::reliable(cfg());
        assert!(p.is_reliable());
        // Reliable game messages get a contiguous space starting at 1.
        let mut g1 = blank_env(100, ReliabilityMode::ReliableOrdered);
        let mut g2 = blank_env(100, ReliabilityMode::ReliableOrdered);
        p.stamp_outgoing(&mut g1, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)).unwrap();
        p.stamp_outgoing(&mut g2, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!((g1.msg_id, g2.msg_id), (1, 2));
        // UnreliableSequenced has its own space (also starts at 1).
        let mut s = blank_env(100, ReliabilityMode::UnreliableSequenced);
        p.stamp_outgoing(&mut s, ReliabilityMode::UnreliableSequenced, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!(s.msg_id, 1);
        // Unreliable carries no sequence.
        let mut u = blank_env(100, ReliabilityMode::Unreliable);
        p.stamp_outgoing(&mut u, ReliabilityMode::Unreliable, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!(u.msg_id, 0);
        // Control reliable uses an independent contiguous space.
        let mut c = blank_env(5, ReliabilityMode::ReliableOrdered);
        p.stamp_outgoing(&mut c, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)).unwrap();
        assert_eq!(c.msg_id, 1);
        // All three tracked reliable messages (2 game + 1 control) retransmit
        // after the RTO; the unreliable/sequenced ones are not tracked.
        let out = p.tick(t(200));
        assert_eq!(out.retransmits.len(), 3);
    }

    #[test]
    fn reliable_pipe_window_full_is_terminal_and_consumes_no_seq() {
        let mut c = cfg();
        c.send_window = 2;
        let mut p = SessionPipe::reliable(c);
        for _ in 0..2 {
            let mut e = blank_env(100, ReliabilityMode::ReliableOrdered);
            p.stamp_outgoing(&mut e, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)).unwrap();
        }
        let mut e = blank_env(100, ReliabilityMode::ReliableOrdered);
        assert_eq!(
            p.stamp_outgoing(&mut e, ReliabilityMode::ReliableOrdered, Duration::from_secs(10), t(0)),
            Err(WindowFull)
        );
        // No sequence was consumed (msg_id untouched).
        assert_eq!(e.msg_id, 0);
    }

    #[test]
    fn reliable_pipe_roundtrip_ack_clears_and_drains() {
        let mut p = SessionPipe::reliable(cfg());
        // Receive an inbound ordered game message → deliver + schedule ACK.
        let inbound = env(1, 100, ReliabilityMode::ReliableOrdered);
        assert!(matches!(p.process_incoming(inbound, t(0)), ReceiveOutcome::Deliver(_)));
        let out = p.tick(t(50));
        assert_eq!(out.acks.len(), 1);
        assert_eq!(out.acks[0].cumulative_ack, 1);
    }
}
