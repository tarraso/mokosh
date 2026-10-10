//! Native reconnect coordinator. Each attempt owns a fresh client and transport.
//!
//! Optional guest resume preserves player identity after a protected UDP handshake. The factory must create a new transport and a closed client
//! using the supplied channels. The coordinator owns the client's game channel.

use crate::transport::udp::UdpClientError;
use crate::transport::Transport;
use crate::{Client, ClientError, ClientExit, ClientHandle};
use bytes::Bytes;
use mokosh_protocol::compression::Compressor;
use mokosh_protocol::encryption::Encryptor;
use mokosh_protocol::messages::{
    routes, Disconnect, DisconnectReason, ErrorReason, HelloError, HelloOk,
};
use mokosh_protocol::{resume::*, PlayerId, SessionId};
use mokosh_protocol::{
    CodecType, Envelope, EnvelopeFlags, ReliabilityMode, CURRENT_PROTOCOL_VERSION,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

const CAPACITY: usize = 100;
const SHUTDOWN_GRACE: Duration = Duration::from_millis(100);

/// Limits apply to initial connection and separately to each recovery episode.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Preserve guest identity across transports; requires protected UDP.
    pub guest_resume: bool,
    pub max_attempts: u32,
    pub deadline: Duration,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    /// Symmetric fractional jitter, between 0 and 1 inclusive.
    pub jitter: f64,
    /// A connection must last this long to start a new recovery episode; shorter
    /// ones count against the current budget, so a flapping server cannot cause
    /// unbounded, undelayed reconnects. Must be greater than zero.
    pub stable_after: Duration,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            guest_resume: false,
            max_attempts: 10,
            deadline: Duration::from_secs(60),
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            jitter: 0.2,
            stable_after: Duration::from_secs(10),
        }
    }
}

impl ReconnectConfig {
    fn validate(&self) -> Result<(), ReconnectError> {
        if self.max_attempts == 0
            || self.deadline.is_zero()
            || self.stable_after.is_zero()
            || self.initial_delay.is_zero()
            || self.max_delay < self.initial_delay
            || self.max_delay.checked_mul(2).is_none()
            || !self.jitter.is_finite()
            || !(0.0..=1.0).contains(&self.jitter)
            || Instant::now().checked_add(self.deadline).is_none()
        {
            return Err(ReconnectError::InvalidConfig);
        }
        Ok(())
    }

    fn delay(&self, failure_count: u32, sample: f64) -> Duration {
        let mut base = self.initial_delay;
        for _ in 0..failure_count.saturating_sub(1).min(96) {
            base = base.saturating_mul(2).min(self.max_delay);
            if base == self.max_delay {
                break;
            }
        }
        base.mul_f64(1.0 + self.jitter * (sample * 2.0 - 1.0))
            .min(self.max_delay)
    }
}

/// A failure with its retry classification. Transport/protocol descriptions are
/// supplied by the peer or factory; custom errors must not contain credentials.
#[derive(Debug, Clone)]
pub enum ReconnectFailure {
    ResumeRejected(ResumeError),
    HelloTimeout,
    ConnectionTimeout,
    IncomingClosed,
    Transport { message: String, retryable: bool },
    HelloRejected(HelloError),
    ServerDisconnect(Disconnect),
    Protocol(String),
    AuthenticationRequired,
    RandomUnavailable,
}

impl ReconnectFailure {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::HelloTimeout | Self::ConnectionTimeout | Self::IncomingClosed => true,
            Self::Transport { retryable, .. } => *retryable,
            Self::HelloRejected(error) => matches!(
                error.reason,
                ErrorReason::ServerFull | ErrorReason::Maintenance
            ),
            Self::ServerDisconnect(disconnect) => matches!(
                disconnect.reason,
                DisconnectReason::Timeout | DisconnectReason::ServerShutdown
            ),
            _ => false,
        }
    }
}

impl From<ClientError> for ReconnectFailure {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::HelloTimeout => Self::HelloTimeout,
            ClientError::ConnectionTimeout => Self::ConnectionTimeout,
            ClientError::HelloRejected(error) => Self::HelloRejected(error),
            ClientError::ChannelSendError => Self::IncomingClosed,
            other => Self::Protocol(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ReconnectError {
    #[error("Invalid reconnect limits, delays, jitter, or stability threshold")]
    InvalidConfig,
    #[error("Reconnect attempt/deadline budget exhausted: {last:?}")]
    BudgetExceeded { last: Option<ReconnectFailure> },
    #[error("Connection stopped after a terminal failure: {0:?}")]
    Terminal(ReconnectFailure),
}

/// Current state, available even when no watcher was present during a transition.
#[derive(Debug, Clone)]
pub enum ReconnectState {
    Resuming {
        generation: u64,
    },
    Synchronizing {
        generation: u64,
        player_id: PlayerId,
        session_id: SessionId,
        snapshot_id: u64,
        snapshot: ResumeSnapshot,
    },
    Ready {
        generation: u64,
        player_id: PlayerId,
        session_id: SessionId,
        resumed: bool,
    },
    Connecting {
        attempt: u32,
        generation: u64,
    },
    Reconnecting {
        attempt: u32,
        generation: u64,
        last: ReconnectFailure,
    },
    Connected {
        generation: u64,
        hello: HelloOk,
    },
    Stopped,
    Failed(ReconnectError),
}

/// Delivered game payload, tagged so consumers can discard queued old snapshots.
#[derive(Debug)]
pub struct ReconnectMessage {
    pub generation: u64,
    pub envelope: Envelope,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconnectSendError {
    #[error("Client is not connected")]
    NotConnected,
    #[error(transparent)]
    Client(#[from] ClientError),
}

struct Shared {
    active: Mutex<Option<ClientHandle>>,
    synchronizing: Mutex<Option<(u64, u64, CodecType, ClientHandle)>>,
    state: watch::Sender<ReconnectState>,
    cancel: watch::Sender<bool>,
}

impl Shared {
    fn active(&self) -> std::sync::MutexGuard<'_, Option<ClientHandle>> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clear(&self) {
        *self.active() = None;
        *self.synchronizing.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// A stable, cloneable handle. Sends are synchronous and never buffered offline.
#[derive(Clone)]
pub struct ReconnectHandle {
    shared: Arc<Shared>,
}

impl ReconnectHandle {
    /// Call only after replacing prediction/input history and applying the supplied snapshot.
    pub fn confirm_snapshot(
        &self,
        generation: u64,
        snapshot_id: u64,
    ) -> Result<(), ReconnectSendError> {
        let sync = self
            .shared
            .synchronizing
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some((g, id, codec, handle)) = &*sync else {
            return Err(ReconnectSendError::NotConnected);
        };
        if *g != generation || *id != snapshot_id || *self.shared.cancel.borrow() {
            return Err(ReconnectSendError::NotConnected);
        }
        handle.send_control(
            *codec,
            routes::SNAPSHOT_APPLIED,
            &SnapshotApplied { snapshot_id },
        )?;
        Ok(())
    }
    pub fn subscribe(&self) -> watch::Receiver<ReconnectState> {
        self.shared.state.subscribe()
    }

    pub fn send_encoded(
        &self,
        route_id: u16,
        schema_hash: u64,
        payload: Bytes,
        mode: ReliabilityMode,
        ttl: Option<Duration>,
    ) -> Result<(), ReconnectSendError> {
        let active = self.shared.active();
        if *self.shared.cancel.borrow() {
            return Err(ReconnectSendError::NotConnected);
        }
        active
            .as_ref()
            .ok_or(ReconnectSendError::NotConnected)?
            .send_encoded(route_id, schema_hash, payload, mode, ttl)?;
        Ok(())
    }

    pub fn send_message<T: mokosh_protocol::GameMessage + serde::Serialize>(
        &self,
        codec: CodecType,
        message: &T,
        mode: ReliabilityMode,
        ttl: Option<Duration>,
    ) -> Result<(), ReconnectSendError> {
        let payload = codec
            .encode(message)
            .map_err(|error| ClientError::CodecError(error.to_string()))?;
        self.send_encoded(T::ROUTE_ID, T::SCHEMA_HASH, payload, mode, ttl)
    }

    /// Stops both connection attempts and backoff. Safe to call repeatedly.
    pub fn disconnect(&self) {
        self.shared.clear();
        self.shared.cancel.send_replace(true);
    }
}

/// Owns all attempt futures; dropping `run()` also drops the current socket/link.
pub struct ReconnectingClient<F> {
    factory: F,
    credentials: Option<ResumeAccepted>,
    operation: Option<[u8; 16]>,
    config: ReconnectConfig,
    shared: Arc<Shared>,
    messages_tx: mpsc::Sender<ReconnectMessage>,
    messages_rx: Option<mpsc::Receiver<ReconnectMessage>>,
    jitter_sample: fn() -> Result<f64, ReconnectFailure>,
}

fn random_sample() -> Result<f64, ReconnectFailure> {
    let mut bytes = [0; 4];
    getrandom::getrandom(&mut bytes).map_err(|_| ReconnectFailure::RandomUnavailable)?;
    Ok(u32::from_ne_bytes(bytes) as f64 / u32::MAX as f64)
}

impl<F> ReconnectingClient<F> {
    pub fn new(config: ReconnectConfig, factory: F) -> Result<Self, ReconnectError> {
        config.validate()?;
        let (state, _) = watch::channel(ReconnectState::Connecting {
            attempt: 1,
            generation: 0,
        });
        let (cancel, _) = watch::channel(false);
        let (messages_tx, messages_rx) = mpsc::channel(CAPACITY);
        Ok(Self {
            factory,
            credentials: None,
            operation: None,
            config,
            shared: Arc::new(Shared {
                active: Mutex::new(None),
                synchronizing: Mutex::new(None),
                state,
                cancel,
            }),
            messages_tx,
            messages_rx: Some(messages_rx),
            jitter_sample: random_sample,
        })
    }

    pub fn handle(&self) -> ReconnectHandle {
        ReconnectHandle {
            shared: self.shared.clone(),
        }
    }

    /// Take the bounded game stream before running; apply backpressure when full.
    pub fn take_messages(&mut self) -> Option<mpsc::Receiver<ReconnectMessage>> {
        self.messages_rx.take()
    }

    pub async fn run<C, E, T>(mut self) -> Result<(), ReconnectError>
    where
        C: Compressor,
        E: Encryptor,
        T: Transport,
        F: FnMut(mpsc::Receiver<Envelope>, mpsc::Sender<Envelope>) -> (Client<C, E>, T),
    {
        let _guard = RunGuard(self.shared.clone());
        // An application may observe status only; do not retain an unread stream.
        drop(self.messages_rx.take());
        let result = self.run_inner().await;
        self.shared.clear();
        self.shared.state.send_replace(match &result {
            Ok(()) => ReconnectState::Stopped,
            Err(error) => ReconnectState::Failed(error.clone()),
        });
        result
    }

    async fn run_inner<C, E, T>(&mut self) -> Result<(), ReconnectError>
    where
        C: Compressor,
        E: Encryptor,
        T: Transport,
        F: FnMut(mpsc::Receiver<Envelope>, mpsc::Sender<Envelope>) -> (Client<C, E>, T),
    {
        let mut cancel = self.shared.cancel.subscribe();
        let mut attempt = 0;
        let mut generation = 0;
        let mut deadline = Instant::now() + self.config.deadline;
        let mut last = None;
        loop {
            if *cancel.borrow() {
                return Ok(());
            }
            if attempt >= self.config.max_attempts || Instant::now() >= deadline {
                return Err(ReconnectError::BudgetExceeded { last });
            }
            if attempt > 0 {
                let sample = if self.config.jitter == 0.0 {
                    0.5
                } else {
                    (self.jitter_sample)().map_err(ReconnectError::Terminal)?
                };
                let delay = self.config.delay(attempt, sample);
                tokio::select! {
                    biased;
                    _ = cancelled(&mut cancel) => return Ok(()),
                    _ = tokio::time::sleep_until(deadline) => return Err(ReconnectError::BudgetExceeded { last }),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            attempt += 1;
            generation += 1;
            self.shared.state.send_replace(match &last {
                None => ReconnectState::Connecting {
                    attempt,
                    generation,
                },
                Some(last) => ReconnectState::Reconnecting {
                    attempt,
                    generation,
                    last: last.clone(),
                },
            });
            let outcome = self.attempt(generation, deadline, &mut cancel).await;
            match outcome {
                Attempt::Stopped => return Ok(()),
                Attempt::Deadline => return Err(ReconnectError::BudgetExceeded { last }),
                Attempt::Failed { failure, stable } => {
                    tracing::debug!(generation, attempt, ?failure, "Connection attempt finished");
                    if !failure.is_retryable() {
                        return Err(ReconnectError::Terminal(failure));
                    }
                    if stable {
                        attempt = 0;
                        deadline = Instant::now() + self.config.deadline;
                    }
                    last = Some(failure.clone());
                    self.shared
                        .state
                        .send_replace(ReconnectState::Reconnecting {
                            attempt: attempt.saturating_add(1),
                            generation: generation + 1,
                            last: failure,
                        });
                }
            }
        }
    }

    async fn attempt<C, E, T>(
        &mut self,
        generation: u64,
        deadline: Instant,
        cancel: &mut watch::Receiver<bool>,
    ) -> Attempt
    where
        C: Compressor,
        E: Encryptor,
        T: Transport,
        F: FnMut(mpsc::Receiver<Envelope>, mpsc::Sender<Envelope>) -> (Client<C, E>, T),
    {
        let (in_tx, in_rx) = mpsc::channel(CAPACITY);
        let (out_tx, out_rx) = mpsc::channel(CAPACITY);
        let (game_tx, mut game_rx) = mpsc::channel(CAPACITY);
        let (mut client, transport) = (self.factory)(in_rx, out_tx.clone());
        client.game_messages_tx = Some(game_tx);
        client.guest_resume = self.config.guest_resume;
        let (resume_tx, mut resume_rx) = mpsc::channel(CAPACITY);
        client.resume_tx = Some(resume_tx);
        if self.config.guest_resume
            && (!transport.protected_udp() || client.config.reliability.is_none())
        {
            return Attempt::Failed {
                failure: ReconnectFailure::ResumeRejected(ResumeError::Unsupported),
                stable: false,
            };
        }
        if self.config.guest_resume && self.operation.is_none() {
            let mut operation = [0; 16];
            if getrandom::getrandom(&mut operation).is_err() {
                return Attempt::Failed {
                    failure: ReconnectFailure::RandomUnavailable,
                    stable: false,
                };
            }
            self.operation = Some(operation);
        }
        let codec = client.control_codec;
        let mut hello_rx = client.subscribe_connected();
        let handle = client.handle();
        if let Err(error) = client.connect().await {
            return Attempt::Failed {
                failure: error.into(),
                stable: false,
            };
        }
        let mut network = Box::pin(transport.run(in_tx, out_rx));
        let mut client_loop = Box::pin(client.run_until_closed());
        let mut connected_at = None;
        let mut hello_done = false;
        let mut accepted: Option<ResumeAccepted> = None;
        let mut snapshot_id = None;
        let mut network_done = false;
        let mut pending = None;
        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancelled(cancel) => break Attempt::Stopped,
                _ = tokio::time::sleep_until(deadline), if connected_at.is_none() => break Attempt::Deadline,
                result = &mut network => {
                    network_done = true;
                    let failure = match result {
                        Ok(()) => ReconnectFailure::IncomingClosed,
                        Err(error) => {
                            let retryable = !matches!(
                                (&error as &dyn std::error::Error).downcast_ref::<UdpClientError>(),
                                Some(UdpClientError::EncryptionRequired)
                            );
                            ReconnectFailure::Transport { message: error.to_string(), retryable }
                        }
                    };
                    break Attempt::Failed { failure, stable: self.stable(connected_at) };
                }
                result = &mut client_loop => {
                    let failure = match result {
                        Ok(ClientExit::LocalDisconnect) => break Attempt::Stopped,
                        Ok(ClientExit::IncomingClosed) => ReconnectFailure::IncomingClosed,
                        Ok(ClientExit::ServerDisconnect(disconnect)) => ReconnectFailure::ServerDisconnect(disconnect),
                        Err(error) => error.into(),
                    };
                    break Attempt::Failed { failure, stable: self.stable(connected_at) };
                }
                Ok(()) = hello_rx.changed(), if !hello_done => {
                    let hello = hello_rx.borrow_and_update().clone();
                    if let Some(hello) = hello {
                        if hello.auth_required {
                            break Attempt::Failed { failure: ReconnectFailure::AuthenticationRequired, stable: false };
                        }
                        hello_done = true;
                        if self.config.guest_resume {
                            self.shared.state.send_replace(ReconnectState::Resuming { generation });
                            let request = ResumeRequest {
                                player_id: self.credentials.as_ref().map(|c|c.player_id),
                                token: self.credentials.as_ref().map(|c|c.token.clone()),
                                operation: self.operation.unwrap(),
                            };
                            if let Err(error) = handle.send_control(codec, routes::RESUME_REQUEST, &request) {
                                break Attempt::Failed { failure: error.into(), stable: false };
                            }
                            continue;
                        }
                        connected_at = Some(Instant::now());
                        let mut active = self.shared.active();
                        *active = Some(handle.clone());
                        self.shared.state.send_replace(ReconnectState::Connected { generation, hello });
                    }
                }
                Some(envelope) = resume_rx.recv(), if self.config.guest_resume => {
                    match envelope.route_id {
                        routes::RESUME_ERROR => {
                            if let Ok(error) = codec.decode::<ResumeError>(&envelope.payload) {
                                break Attempt::Failed { failure: ReconnectFailure::ResumeRejected(error), stable: false };
                            }
                        }
                        routes::RESUME_ACCEPTED if hello_done => {
                            if let Ok(reply) = codec.decode::<ResumeAccepted>(&envelope.payload) {
                                if Some(reply.operation) != self.operation || self.credentials.as_ref().is_some_and(|c|c.player_id != reply.player_id) { continue; }
                                self.credentials = Some(reply.clone()); accepted = Some(reply);
                            }
                        }
                        routes::RESUME_SNAPSHOT if accepted.is_some() && connected_at.is_none() => {
                            if let Ok(snapshot) = codec.decode::<ResumeSnapshot>(&envelope.payload) {
                                if snapshot.payload.len() > MAX_RESUME_SNAPSHOT_BYTES || snapshot.route_id < 100 { continue; }
                                if let Some(reply) = &accepted {
                                    let session_id = hello_rx.borrow().as_ref().and_then(|h|h.session_id.parse::<SessionId>().ok());
                                    if let Some(session_id) = session_id {
                                        snapshot_id = Some(snapshot.snapshot_id);
                                        *self.shared.synchronizing.lock().unwrap_or_else(|p|p.into_inner()) = Some((generation, snapshot.snapshot_id, codec, handle.clone()));
                                        self.shared.state.send_replace(ReconnectState::Synchronizing { generation, player_id: reply.player_id, session_id, snapshot_id: snapshot.snapshot_id, snapshot });
                                    }
                                }
                            }
                        }
                        routes::RESUME_READY if connected_at.is_none() => {
                            if let Ok(applied) = codec.decode::<SnapshotApplied>(&envelope.payload) {
                                if snapshot_id != Some(applied.snapshot_id) { continue; }
                                if let Some(reply) = &accepted {
                                    if let Some(session_id) = hello_rx.borrow().as_ref().and_then(|h|h.session_id.parse::<SessionId>().ok()) {
                                        connected_at = Some(Instant::now()); self.operation = None;
                                        *self.shared.synchronizing.lock().unwrap_or_else(|p|p.into_inner()) = None;
                                        *self.shared.active() = Some(handle.clone());
                                        self.shared.state.send_replace(ReconnectState::Ready { generation, player_id: reply.player_id, session_id, resumed: reply.resumed });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                permit = self.messages_tx.reserve(), if pending.is_some() => {
                    match permit {
                        Ok(permit) => { if let Some(message) = pending.take() { permit.send(message); } },
                        Err(_) => pending = None,
                    }
                }
                Some(envelope) = game_rx.recv(), if pending.is_none() => {
                    if connected_at.is_some() {
                        pending = Some(ReconnectMessage { generation, envelope });
                    }
                }
            }
        };
        self.shared.clear();
        drop(client_loop);
        // This control packet is best effort; game commands from the dead client
        // are never forwarded into another attempt.
        if !network_done && (!self.config.guest_resume || matches!(outcome, Attempt::Stopped)) {
            let disconnect = Disconnect {
                reason: DisconnectReason::ClientRequested,
                message: "connection attempt ended".into(),
            };
            if let Ok(payload) = codec.encode(&disconnect) {
                let _ = out_tx.try_send(Envelope::new_simple(
                    CURRENT_PROTOCOL_VERSION,
                    codec.id(),
                    0,
                    routes::DISCONNECT,
                    0,
                    EnvelopeFlags::empty(),
                    payload,
                ));
            }
            drop(out_tx);
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, &mut network).await;
        }
        outcome
    }

    fn stable(&self, connected_at: Option<Instant>) -> bool {
        connected_at.is_some_and(|at| at.elapsed() >= self.config.stable_after)
    }
}

struct RunGuard(Arc<Shared>);
impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.clear();
        let terminal = matches!(
            *self.0.state.borrow(),
            ReconnectState::Stopped | ReconnectState::Failed(_)
        );
        if !terminal {
            self.0.state.send_replace(ReconnectState::Stopped);
        }
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

enum Attempt {
    Stopped,
    Deadline,
    Failed {
        failure: ReconnectFailure,
        stable: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientConfig;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    enum Behavior {
        Accept,
        Silent,
        Error,
        Reject(ErrorReason),
        AuthRequired,
        /// Accepts HELLO, waits long enough for Connected to be observed, then closes.
        Flap(Duration),
    }

    struct Probe {
        live: AtomicUsize,
        attempts: AtomicUsize,
        seen: Mutex<Vec<(usize, u16)>>,
        inject: Mutex<Vec<mpsc::Sender<Envelope>>>,
    }

    struct MockTransport {
        behavior: Behavior,
        probe: Arc<Probe>,
        index: usize,
        inject_rx: mpsc::Receiver<Envelope>,
    }

    impl Drop for MockTransport {
        fn drop(&mut self) {
            self.probe.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn control<T: serde::Serialize>(route: u16, message: T) -> Envelope {
        Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            route,
            0,
            EnvelopeFlags::empty(),
            CodecType::from_id(1).unwrap().encode(&message).unwrap(),
        )
    }

    fn disconnect(reason: DisconnectReason) -> Envelope {
        control(
            routes::DISCONNECT,
            Disconnect {
                reason,
                message: "mock".into(),
            },
        )
    }

    #[async_trait]
    impl Transport for MockTransport {
        type Error = std::io::Error;
        async fn run(
            mut self,
            incoming: mpsc::Sender<Envelope>,
            mut outgoing: mpsc::Receiver<Envelope>,
        ) -> Result<(), Self::Error> {
            if matches!(self.behavior, Behavior::Error) {
                return Err(std::io::Error::other("mock socket error"));
            }
            loop {
                tokio::select! {
                    message = outgoing.recv() => {
                        let Some(message) = message else { return Ok(()); };
                        self.probe.seen.lock().unwrap().push((self.index, message.route_id));
                        if message.route_id == routes::HELLO {
                            let reply = match self.behavior {
                                Behavior::Silent => continue,
                                Behavior::Reject(reason) => control(routes::HELLO_ERROR, HelloError { reason, message: "mock refusal".into(), expected_schema_hash: 0 }),
                                _ => control(routes::HELLO_OK, HelloOk {
                                    server_version: CURRENT_PROTOCOL_VERSION,
                                    session_id: format!("session-{}", self.index),
                                    auth_required: matches!(self.behavior, Behavior::AuthRequired),
                                    available_auth_methods: vec![], reliability: false,
                                }),
                            };
                            if incoming.send(reply).await.is_err() { return Ok(()); }
                            if let Behavior::Flap(duration) = self.behavior {
                                tokio::time::sleep(duration).await;
                                let _ = incoming.send(disconnect(DisconnectReason::ServerShutdown)).await;
                            }
                        } else if message.route_id == routes::DISCONNECT
                            || (message.route_id >= 100 && incoming.send(message).await.is_err()) {
                            return Ok(());
                        }
                    }
                    Some(message) = self.inject_rx.recv() => {
                        if incoming.send(message).await.is_err() { return Ok(()); }
                    }
                }
            }
        }
    }

    type Factory = Box<
        dyn FnMut(mpsc::Receiver<Envelope>, mpsc::Sender<Envelope>) -> (Client, MockTransport)
            + Send,
    >;

    fn fixture(
        behaviors: Vec<Behavior>,
        config: ReconnectConfig,
    ) -> (ReconnectingClient<Factory>, Arc<Probe>) {
        let probe = Arc::new(Probe {
            live: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            seen: Mutex::new(vec![]),
            inject: Mutex::new(vec![]),
        });
        let observed = probe.clone();
        let mut behaviors = VecDeque::from(behaviors);
        let factory: Factory = Box::new(move |incoming, outgoing| {
            // The coordinator must release the old transport before constructing another.
            assert_eq!(observed.live.fetch_add(1, Ordering::SeqCst), 0);
            let index = observed.attempts.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::channel(100);
            observed.inject.lock().unwrap().push(tx);
            let client = Client::with_full_config(
                incoming,
                outgoing,
                CodecType::from_id(1).unwrap(),
                CodecType::from_id(1).unwrap(),
                ClientConfig {
                    hello_timeout: Duration::from_millis(100),
                    connection_timeout: Duration::from_millis(200),
                    keepalive_interval: Duration::from_secs(30),
                    ..Default::default()
                },
                None,
                mokosh_protocol::compression::NoCompressor,
                mokosh_protocol::encryption::NoEncryptor,
                None,
            );
            (
                client,
                MockTransport {
                    behavior: behaviors.pop_front().unwrap_or(Behavior::Accept),
                    probe: observed.clone(),
                    index,
                    inject_rx: rx,
                },
            )
        });
        (ReconnectingClient::new(config, factory).unwrap(), probe)
    }

    #[tokio::test]
    async fn guest_resume_rejects_unsupported_transport_before_sending_hello() {
        let (coordinator, probe) = fixture(
            vec![Behavior::Accept],
            ReconnectConfig {
                guest_resume: true,
                ..config()
            },
        );
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::Terminal(ReconnectFailure::ResumeRejected(
                ResumeError::Unsupported
            )))
        ));
        assert!(probe.seen.lock().unwrap().is_empty());
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    }

    fn config() -> ReconnectConfig {
        ReconnectConfig {
            jitter: 0.0,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(20),
            stable_after: Duration::from_millis(100),
            ..Default::default()
        }
    }

    async fn connected(status: &mut watch::Receiver<ReconnectState>) -> u64 {
        loop {
            let generation = match &*status.borrow_and_update() {
                ReconnectState::Connected { generation, .. } => Some(*generation),
                ReconnectState::Failed(error) => panic!("unexpected failure: {error}"),
                _ => None,
            };
            if let Some(generation) = generation {
                return generation;
            }
            status.changed().await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retries_socket_and_hello_failures_then_connects_and_cancels() {
        let (coordinator, probe) = fixture(
            vec![Behavior::Error, Behavior::Silent, Behavior::Accept],
            config(),
        );
        let handle = coordinator.handle();
        assert!(matches!(
            handle.send_encoded(100, 0, Bytes::new(), ReliabilityMode::Unreliable, None),
            Err(ReconnectSendError::NotConnected)
        ));
        let mut status = handle.subscribe();
        let task = tokio::spawn(coordinator.run());
        assert_eq!(connected(&mut status).await, 3);
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 3);
        handle.disconnect();
        task.await.unwrap().unwrap();
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
        assert!(matches!(*status.borrow(), ReconnectState::Stopped));
    }

    #[tokio::test(start_paused = true)]
    async fn connection_timeout_resets_budget_and_tags_messages() {
        let (mut coordinator, probe) = fixture(
            vec![Behavior::Error, Behavior::Accept, Behavior::Accept],
            ReconnectConfig {
                max_attempts: 2,
                ..config()
            },
        );
        let mut messages = coordinator.take_messages().unwrap();
        let handle = coordinator.handle();
        let mut status = handle.subscribe();
        let task = tokio::spawn(coordinator.run());
        let first = connected(&mut status).await;
        handle
            .send_encoded(
                100,
                0,
                Bytes::from_static(b"first"),
                ReliabilityMode::Unreliable,
                None,
            )
            .unwrap();
        assert_eq!(messages.recv().await.unwrap().generation, first);
        tokio::time::advance(Duration::from_millis(250)).await;
        while !matches!(&*status.borrow(), ReconnectState::Connected { generation, .. } if *generation > first)
        {
            status.changed().await.unwrap();
        }
        let second = connected(&mut status).await;
        assert!(second > first);
        handle
            .send_encoded(
                100,
                0,
                Bytes::from_static(b"second"),
                ReliabilityMode::Unreliable,
                None,
            )
            .unwrap();
        let message = messages.recv().await.unwrap();
        assert_eq!(message.generation, second);
        assert_eq!(message.envelope.payload, Bytes::from_static(b"second"));
        handle.disconnect();
        task.await.unwrap().unwrap();
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn attempt_and_deadline_budgets_stop_retries() {
        let (coordinator, probe) = fixture(
            vec![Behavior::Error; 10],
            ReconnectConfig {
                max_attempts: 3,
                ..config()
            },
        );
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::BudgetExceeded { .. })
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 3);
        let (coordinator, probe) = fixture(
            vec![Behavior::Silent],
            ReconnectConfig {
                deadline: Duration::from_millis(25),
                ..config()
            },
        );
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::BudgetExceeded { .. })
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn short_lived_connections_do_not_reset_budget() {
        let (coordinator, probe) = fixture(
            vec![Behavior::Flap(Duration::from_millis(40)); 10],
            ReconnectConfig {
                max_attempts: 3,
                ..config()
            },
        );
        let mut status = coordinator.handle().subscribe();
        let start = Instant::now();
        let mut run = Box::pin(coordinator.run());
        let mut connected_generations = vec![];
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    result = &mut run => break result,
                    changed = status.changed() => {
                        changed.unwrap();
                        if let ReconnectState::Connected { generation, .. } = &*status.borrow_and_update() {
                            connected_generations.push(*generation);
                        }
                    }
                }
            }
        }).await.expect("flapping must terminate within its retry budget");
        assert!(matches!(
            result,
            Err(ReconnectError::BudgetExceeded {
                last: Some(ReconnectFailure::ServerDisconnect(_))
            })
        ));
        assert_eq!(connected_generations, vec![1, 2, 3]);
        assert_eq!(start.elapsed(), Duration::from_millis(150));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 3);
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_terminal_hello_and_auth_but_retries_capacity_errors() {
        for reason in [
            ErrorReason::VersionMismatch,
            ErrorReason::SchemaMismatch,
            ErrorReason::ReliabilityMismatch,
        ] {
            let (coordinator, probe) = fixture(vec![Behavior::Reject(reason)], config());
            assert!(matches!(
                coordinator.run().await,
                Err(ReconnectError::Terminal(ReconnectFailure::HelloRejected(_)))
            ));
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
        }
        let (coordinator, _) = fixture(vec![Behavior::AuthRequired], config());
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::Terminal(
                ReconnectFailure::AuthenticationRequired
            ))
        ));
        for reason in [ErrorReason::ServerFull, ErrorReason::Maintenance] {
            let (coordinator, probe) =
                fixture(vec![Behavior::Reject(reason), Behavior::Accept], config());
            let handle = coordinator.handle();
            let mut status = handle.subscribe();
            let task = tokio::spawn(coordinator.run());
            assert_eq!(connected(&mut status).await, 2);
            handle.disconnect();
            task.await.unwrap().unwrap();
            assert_eq!(probe.live.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn server_disconnect_classification_and_stale_commands() {
        let (coordinator, probe) = fixture(vec![Behavior::Accept, Behavior::Accept], config());
        let handle = coordinator.handle();
        let mut status = handle.subscribe();
        let task = tokio::spawn(coordinator.run());
        assert_eq!(connected(&mut status).await, 1);
        let tx = probe.inject.lock().unwrap()[0].clone();
        tx.send(disconnect(DisconnectReason::ServerShutdown))
            .await
            .unwrap();
        // Commands queued in the first client's channel must not migrate.
        for _ in 0..100 {
            let _ = handle.send_encoded(101, 0, Bytes::new(), ReliabilityMode::Unreliable, None);
        }
        while !matches!(
            &*status.borrow(),
            ReconnectState::Connected { generation: 2, .. }
        ) {
            status.changed().await.unwrap();
        }
        assert!(!probe
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|&(index, route)| index == 1 && route == 101));
        let tx = probe.inject.lock().unwrap()[1].clone();
        tx.send(disconnect(DisconnectReason::AuthenticationFailed))
            .await
            .unwrap();
        assert!(matches!(
            task.await.unwrap(),
            Err(ReconnectError::Terminal(
                ReconnectFailure::ServerDisconnect(_)
            ))
        ));
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_hello_or_backoff_and_drop_running_future() {
        for behavior in [Behavior::Silent, Behavior::Error] {
            let (coordinator, probe) = fixture(vec![behavior], config());
            let handle = coordinator.handle();
            let task = tokio::spawn(coordinator.run());
            tokio::task::yield_now().await;
            handle.disconnect();
            task.await.unwrap().unwrap();
            assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(probe.live.load(Ordering::SeqCst), 0);
        }
        let (coordinator, probe) = fixture(vec![Behavior::Accept], config());
        let handle = coordinator.handle();
        let mut status = handle.subscribe();
        let task = tokio::spawn(coordinator.run());
        connected(&mut status).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(probe.live.load(Ordering::SeqCst), 0);
        assert!(matches!(*status.borrow(), ReconnectState::Stopped));
        assert!(matches!(
            handle.send_encoded(100, 0, Bytes::new(), ReliabilityMode::Unreliable, None),
            Err(ReconnectSendError::NotConnected)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_packets_do_not_extend_hello_deadline() {
        let (coordinator, probe) = fixture(
            vec![Behavior::Silent],
            ReconnectConfig {
                max_attempts: 1,
                ..config()
            },
        );
        let task = tokio::spawn(coordinator.run());
        tokio::task::yield_now().await;
        let tx = probe.inject.lock().unwrap()[0].clone();
        for _ in 0..5 {
            tx.send(Envelope::new_simple(
                CURRENT_PROTOCOL_VERSION,
                1,
                0,
                routes::HELLO_OK,
                0,
                EnvelopeFlags::empty(),
                Bytes::from_static(b"invalid json"),
            ))
            .await
            .unwrap();
            tokio::time::advance(Duration::from_millis(20)).await;
        }
        assert!(matches!(
            task.await.unwrap(),
            Err(ReconnectError::BudgetExceeded {
                last: Some(ReconnectFailure::HelloTimeout)
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn command_queue_is_bounded_and_offline_sends_are_rejected() {
        let (coordinator, _) = fixture(vec![Behavior::Accept], config());
        let handle = coordinator.handle();
        let mut status = handle.subscribe();
        let task = tokio::spawn(coordinator.run());
        connected(&mut status).await;
        for _ in 0..CAPACITY {
            handle
                .send_encoded(100, 0, Bytes::new(), ReliabilityMode::Unreliable, None)
                .unwrap();
        }
        assert!(matches!(
            handle.send_encoded(100, 0, Bytes::new(), ReliabilityMode::Unreliable, None),
            Err(ReconnectSendError::Client(ClientError::ChannelSendError))
        ));
        handle.disconnect();
        assert!(matches!(
            handle.send_encoded(100, 0, Bytes::new(), ReliabilityMode::Unreliable, None),
            Err(ReconnectSendError::NotConnected)
        ));
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_jitter_deadline_and_entropy_failure() {
        let (mut coordinator, probe) = fixture(
            vec![Behavior::Error; 3],
            ReconnectConfig {
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(1),
                jitter: 0.2,
                ..config()
            },
        );
        coordinator.jitter_sample = || Ok(1.0);
        let handle = coordinator.handle();
        let mut status = handle.subscribe();
        let start = Instant::now();
        let task = tokio::spawn(coordinator.run());
        connected(&mut status).await;
        assert_eq!(start.elapsed(), Duration::from_millis(840));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 4);
        handle.disconnect();
        task.await.unwrap().unwrap();

        let (coordinator, probe) = fixture(
            vec![Behavior::Error; 10],
            ReconnectConfig {
                deadline: Duration::from_millis(25),
                ..config()
            },
        );
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::BudgetExceeded { .. })
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);

        let (mut coordinator, probe) = fixture(
            vec![Behavior::Error],
            ReconnectConfig {
                jitter: 0.2,
                ..config()
            },
        );
        coordinator.jitter_sample = || Err(ReconnectFailure::RandomUnavailable);
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::Terminal(
                ReconnectFailure::RandomUnavailable
            ))
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn missing_required_psk_is_terminal_and_cancel_before_run_does_not_connect() {
        let coordinator = ReconnectingClient::new(config(), |incoming, outgoing| {
            (
                Client::new(incoming, outgoing),
                crate::transport::ReliableLink::new(
                    crate::transport::udp::UdpClient::new("127.0.0.1:1").require_encryption(true),
                    mokosh_protocol::ReliabilityConfig::default(),
                ),
            )
        })
        .unwrap();
        assert!(matches!(
            coordinator.run().await,
            Err(ReconnectError::Terminal(ReconnectFailure::Transport {
                retryable: false,
                ..
            }))
        ));
        let (coordinator, probe) = fixture(vec![Behavior::Accept], config());
        coordinator.handle().disconnect();
        coordinator.run().await.unwrap();
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn zero_stability_threshold_cannot_disable_retry_limits() {
        assert!(ReconnectConfig {
            stable_after: Duration::ZERO,
            ..config()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn configuration_backoff_and_retry_policy() {
        let settings = ReconnectConfig::default();
        assert_eq!(settings.delay(1, 0.0), Duration::from_millis(200));
        assert_eq!(settings.delay(1, 1.0), Duration::from_millis(300));
        assert_eq!(settings.delay(2, 0.5), Duration::from_millis(500));
        assert_eq!(settings.delay(100, 1.0), Duration::from_secs(5));
        for invalid in [
            ReconnectConfig {
                max_attempts: 0,
                ..config()
            },
            ReconnectConfig {
                deadline: Duration::ZERO,
                ..config()
            },
            ReconnectConfig {
                initial_delay: Duration::ZERO,
                ..config()
            },
            ReconnectConfig {
                max_delay: Duration::from_millis(1),
                ..config()
            },
            ReconnectConfig {
                jitter: f64::NAN,
                ..config()
            },
            ReconnectConfig {
                jitter: 1.1,
                ..config()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        for reason in [DisconnectReason::Timeout, DisconnectReason::ServerShutdown] {
            assert!(ReconnectFailure::ServerDisconnect(Disconnect {
                reason,
                message: String::new()
            })
            .is_retryable());
        }
        for reason in [
            DisconnectReason::ClientRequested,
            DisconnectReason::ProtocolError,
            DisconnectReason::AuthenticationFailed,
            DisconnectReason::ReplayAttack,
            DisconnectReason::RateLimitExceeded,
            DisconnectReason::MessageTooOld,
            DisconnectReason::ProtocolViolation,
            DisconnectReason::Overloaded,
        ] {
            assert!(!ReconnectFailure::ServerDisconnect(Disconnect {
                reason,
                message: String::new()
            })
            .is_retryable());
        }
    }
}
