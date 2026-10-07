//! Per-session, per-direction record layer for authenticated UDP datagrams.
//!
//! Whole-datagram AEAD with a *shared* key proves only "sealed with the PSK"; it
//! does not bind a datagram to a session, direction, or sequence, so an attacker
//! *without the key* can still replay, redirect, or reflect captured ciphertext
//! (UDP security #3). This module closes that by deriving **per-session,
//! per-direction** keys from the PSK and framing every datagram with an explicit
//! counter that feeds a sliding **anti-replay window**.
//!
//! ## Wire format
//!
//! ```text
//! [ epoch: u8 ][ nonce: 12 bytes ][ ChaCha20-Poly1305 ciphertext + 16-byte tag ]
//! ```
//!
//! - **epoch** selects the key set: `0` = bootstrap (handshake only), `1` = the
//!   per-session key. Selecting the wrong key just fails authentication → drop.
//! - **nonce** is the 96-bit AEAD nonce, carried explicitly:
//!   - epoch 0 (bootstrap key, shared across peers): a random nonce, because many
//!     peers share the bootstrap key — a counter from 0 would collide. Handshake
//!     volume is tiny, so the birthday bound is irrelevant here.
//!   - epoch 1 (per-session, per-direction key): `[0u8; 4] ++ counter.to_be()`,
//!     i.e. a monotonic counter. A per-direction key means the (key, nonce) pair
//!     never repeats, which removes the random-nonce birthday budget entirely and
//!     gives the receiver a counter to anti-replay on.
//! - **AAD** = `[epoch, direction]`, binding the metadata into the tag.
//!
//! ## Key schedule (blake3 `derive_key`)
//!
//! ```text
//! k_boot_c2s = KDF("mokosh-udp bootstrap c2s v1", psk)
//! k_boot_s2c = KDF("mokosh-udp bootstrap s2c v1", psk)
//! secret     = KDF("mokosh-udp session v1", psk ++ client_random ++ server_random)
//! k_sess_c2s = KDF("mokosh-udp c2s v1", secret)
//! k_sess_s2c = KDF("mokosh-udp s2c v1", secret)
//! ```
//!
//! Per-session keys make redirection/replay/reflection fail for an outsider: a
//! datagram sealed for session A (key A) cannot be opened under session B's key,
//! a stale datagram from a previous session cannot be opened under the new
//! session's key, and a client's own `c2s` datagram cannot be opened with the
//! `s2c` key. This is a PSK-derived scheme with **no forward secrecy**; a
//! malicious holder of the PSK is out of scope.

use crate::encryption::{random_nonce, EncryptionError, EncryptionResult};
use bytes::{BufMut, Bytes, BytesMut};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};

/// Size of the random values exchanged during the handshake to derive the
/// per-session key.
pub const SESSION_RANDOM_SIZE: usize = 32;

/// Epoch for handshake datagrams, sealed with the bootstrap key.
pub const EPOCH_BOOTSTRAP: u8 = 0;
/// Epoch for established-session datagrams, sealed with the per-session key.
pub const EPOCH_SESSION: u8 = 1;

const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
/// `epoch (1) + nonce (12)` prefix before the ciphertext.
const RECORD_PREFIX: usize = 1 + NONCE_SIZE;
/// Smallest valid record: prefix + an empty ciphertext's auth tag.
pub const MIN_RECORD_SIZE: usize = RECORD_PREFIX + TAG_SIZE;

/// Direction of travel of a datagram, used for key selection and AAD binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Client → server.
    ClientToServer,
    /// Server → client.
    ServerToClient,
}

impl Direction {
    fn tag(self) -> u8 {
        match self {
            Direction::ClientToServer => 1,
            Direction::ServerToClient => 2,
        }
    }

    /// The opposite direction (what a peer opens vs. what it seals).
    pub fn opposite(self) -> Direction {
        match self {
            Direction::ClientToServer => Direction::ServerToClient,
            Direction::ServerToClient => Direction::ClientToServer,
        }
    }
}

/// Derives the two bootstrap (epoch-0) directional keys directly from the PSK.
/// Returns `(k_boot_c2s, k_boot_s2c)`.
pub fn bootstrap_keys(psk: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    (
        blake3::derive_key("mokosh-udp bootstrap c2s v1", psk),
        blake3::derive_key("mokosh-udp bootstrap s2c v1", psk),
    )
}

/// Derives the two per-session (epoch-1) directional keys from the PSK and the
/// handshake randoms. Both ends compute the same keys. Returns
/// `(k_sess_c2s, k_sess_s2c)`.
pub fn session_keys(
    psk: &[u8; 32],
    client_random: &[u8; SESSION_RANDOM_SIZE],
    server_random: &[u8; SESSION_RANDOM_SIZE],
) -> ([u8; 32], [u8; 32]) {
    let mut material = [0u8; 32 + 2 * SESSION_RANDOM_SIZE];
    material[..32].copy_from_slice(psk);
    material[32..32 + SESSION_RANDOM_SIZE].copy_from_slice(client_random);
    material[32 + SESSION_RANDOM_SIZE..].copy_from_slice(server_random);
    let secret = blake3::derive_key("mokosh-udp session v1", &material);
    (
        blake3::derive_key("mokosh-udp c2s v1", &secret),
        blake3::derive_key("mokosh-udp s2c v1", &secret),
    )
}

/// Builds the counter-based nonce used for epoch-1 (per-session) records.
pub fn counter_nonce(counter: u64) -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

/// Extracts the counter from an epoch-1 nonce produced by [`counter_nonce`].
pub fn nonce_counter(nonce: &[u8; NONCE_SIZE]) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&nonce[4..]);
    u64::from_be_bytes(bytes)
}

/// A keyed record cipher for one `(epoch, direction)`. Stateless: the caller owns
/// the send counter and the [`ReplayWindow`], so the same key can be used for
/// both sealing (with a chosen nonce) and opening.
pub struct RecordKey {
    cipher: ChaCha20Poly1305,
    epoch: u8,
    direction: Direction,
}

impl RecordKey {
    /// Creates a record cipher for the given key, epoch, and direction of travel.
    pub fn new(key: &[u8; 32], epoch: u8, direction: Direction) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            epoch,
            direction,
        }
    }

    fn aad(&self) -> [u8; 2] {
        [self.epoch, self.direction.tag()]
    }

    /// Seals `plaintext` with the supplied nonce, producing the full wire record
    /// `[epoch][nonce][ciphertext+tag]`.
    pub fn seal(&self, nonce: &[u8; NONCE_SIZE], plaintext: &[u8]) -> EncryptionResult<Bytes> {
        let aad = self.aad();
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        let mut out = BytesMut::with_capacity(RECORD_PREFIX + ciphertext.len());
        out.put_u8(self.epoch);
        out.put_slice(nonce);
        out.put_slice(&ciphertext);
        Ok(out.freeze())
    }

    /// Parses and opens a wire record, returning its nonce (so the caller can
    /// drive anti-replay on the counter) and the authenticated plaintext. Fails
    /// if the record is too short, its epoch byte does not match this key's
    /// epoch, or authentication fails.
    pub fn open(&self, datagram: &[u8]) -> EncryptionResult<([u8; NONCE_SIZE], Bytes)> {
        if datagram.len() < MIN_RECORD_SIZE {
            return Err(EncryptionError::CiphertextTooShort {
                min: MIN_RECORD_SIZE,
                actual: datagram.len(),
            });
        }
        if datagram[0] != self.epoch {
            return Err(EncryptionError::DecryptionFailed(
                "epoch mismatch".to_string(),
            ));
        }
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&datagram[1..RECORD_PREFIX]);
        let aad = self.aad();
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &datagram[RECORD_PREFIX..],
                    aad: &aad,
                },
            )
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;
        Ok((nonce, Bytes::from(plaintext)))
    }

    /// Reads the epoch byte of a record without opening it (for key selection).
    pub fn peek_epoch(datagram: &[u8]) -> Option<u8> {
        datagram.first().copied()
    }
}

/// Sliding anti-replay window over a monotonic 64-bit counter, per direction of a
/// session (DTLS/IPsec style). Bit `k` of `bitmap` records that `highest - k` has
/// been accepted.
///
/// Deliberately not shared with the reliability layer's receiver: that one tracks a
/// *cumulative* base plus the bits above it (what ACKs need), which stalls on a
/// permanently lost datagram. Anti-replay must instead follow the highest counter
/// and reject anything older than the window, fail-closed.
#[derive(Debug, Default, Clone)]
pub struct ReplayWindow {
    highest: u64,
    bitmap: u64,
    seen_any: bool,
}

const REPLAY_WINDOW: u64 = 64;

impl ReplayWindow {
    /// A fresh window that has accepted nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if `counter` is a replay or too old to verify, and must be
    /// rejected. Read-only: call [`record`](Self::record) only *after* the record
    /// authenticates, so a forged counter cannot advance the window.
    pub fn is_replay(&self, counter: u64) -> bool {
        if !self.seen_any {
            return false;
        }
        if counter > self.highest {
            return false;
        }
        let diff = self.highest - counter;
        if diff >= REPLAY_WINDOW {
            return true;
        }
        self.bitmap & (1u64 << diff) != 0
    }

    /// Commits an authenticated `counter` to the window.
    pub fn record(&mut self, counter: u64) {
        if !self.seen_any {
            self.seen_any = true;
            self.highest = counter;
            self.bitmap = 1;
            return;
        }
        if counter > self.highest {
            let diff = counter - self.highest;
            self.bitmap = if diff >= REPLAY_WINDOW {
                1
            } else {
                (self.bitmap << diff) | 1
            };
            self.highest = counter;
        } else {
            let diff = self.highest - counter;
            if diff < REPLAY_WINDOW {
                self.bitmap |= 1u64 << diff;
            }
        }
    }
}

/// Which end of the connection a record context belongs to. Selects which
/// directional key seals (outbound) and which opens (inbound).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The UDP client: seals client → server, opens server → client.
    Client,
    /// The UDP server: seals server → client, opens client → server.
    Server,
}

impl Role {
    /// Direction of the datagrams this role sends.
    pub fn outbound(self) -> Direction {
        match self {
            Role::Client => Direction::ClientToServer,
            Role::Server => Direction::ServerToClient,
        }
    }

    /// Orders a `(c2s, s2c)` key pair as `(outbound, inbound)` for this role.
    fn order_keys(self, c2s: [u8; 32], s2c: [u8; 32]) -> ([u8; 32], [u8; 32]) {
        match self {
            Role::Client => (c2s, s2c),
            Role::Server => (s2c, c2s),
        }
    }
}

/// Epoch-0 (bootstrap) records for the handshake. The bootstrap keys are shared
/// by every peer of a PSK, so sealing uses a random nonce (a counter from 0 would
/// collide across peers) and there is no replay window — epoch 0 must only ever
/// carry handshake routes.
pub struct BootstrapRecords {
    outbound: RecordKey,
    inbound: RecordKey,
}

impl BootstrapRecords {
    /// Derives the bootstrap records for `role` from the PSK.
    pub fn derive(psk: &[u8; 32], role: Role) -> Self {
        let (c2s, s2c) = bootstrap_keys(psk);
        let (outbound, inbound) = role.order_keys(c2s, s2c);
        let direction = role.outbound();
        Self {
            outbound: RecordKey::new(&outbound, EPOCH_BOOTSTRAP, direction),
            inbound: RecordKey::new(&inbound, EPOCH_BOOTSTRAP, direction.opposite()),
        }
    }

    /// Seals an outbound handshake datagram under a fresh random nonce. Fails
    /// (never falls back) if the CSPRNG fails.
    pub fn seal(&self, plaintext: &[u8]) -> EncryptionResult<Bytes> {
        self.outbound.seal(&random_nonce()?, plaintext)
    }

    /// Opens an inbound handshake datagram.
    pub fn open(&self, datagram: &[u8]) -> EncryptionResult<Bytes> {
        self.inbound
            .open(datagram)
            .map(|(_nonce, plaintext)| plaintext)
    }
}

/// Epoch-1 per-session records: directional keys derived from the handshake, the
/// outbound counter (used as the nonce), and the inbound anti-replay window.
pub struct SessionRecords {
    outbound: RecordKey,
    inbound: RecordKey,
    send_counter: u64,
    replay: ReplayWindow,
}

impl SessionRecords {
    /// Derives the per-session records for `role`. Both ends call this with the
    /// same PSK and randoms and end up with mirrored keys.
    pub fn derive(
        psk: &[u8; 32],
        client_random: &[u8; SESSION_RANDOM_SIZE],
        server_random: &[u8; SESSION_RANDOM_SIZE],
        role: Role,
    ) -> Self {
        let (c2s, s2c) = session_keys(psk, client_random, server_random);
        let (outbound, inbound) = role.order_keys(c2s, s2c);
        let direction = role.outbound();
        Self {
            outbound: RecordKey::new(&outbound, EPOCH_SESSION, direction),
            inbound: RecordKey::new(&inbound, EPOCH_SESSION, direction.opposite()),
            send_counter: 0,
            replay: ReplayWindow::new(),
        }
    }

    /// Seals an outbound datagram with the next counter as its nonce.
    pub fn seal(&mut self, plaintext: &[u8]) -> EncryptionResult<Bytes> {
        let counter = self.send_counter;
        self.send_counter = counter.checked_add(1).ok_or_else(|| {
            EncryptionError::NonceGenerationFailed("session record counter exhausted".to_string())
        })?;
        self.outbound.seal(&counter_nonce(counter), plaintext)
    }

    /// Opens an inbound datagram and enforces the anti-replay window. The window
    /// only advances after the record authenticates, so forged counters cannot
    /// push legitimate traffic out of it.
    pub fn open(&mut self, datagram: &[u8]) -> EncryptionResult<Bytes> {
        let (nonce, plaintext) = self.inbound.open(datagram)?;
        let counter = nonce_counter(&nonce);
        if self.replay.is_replay(counter) {
            return Err(EncryptionError::ReplayDetected(counter));
        }
        self.replay.record(counter);
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_schedule_is_deterministic_and_separated() {
        let psk = [0x11; 32];
        let cr = [0x22; SESSION_RANDOM_SIZE];
        let sr = [0x33; SESSION_RANDOM_SIZE];

        // Deterministic for identical inputs.
        assert_eq!(session_keys(&psk, &cr, &sr), session_keys(&psk, &cr, &sr));
        assert_eq!(bootstrap_keys(&psk), bootstrap_keys(&psk));

        // Directions differ.
        let (c2s, s2c) = session_keys(&psk, &cr, &sr);
        assert_ne!(c2s, s2c);
        let (bc2s, bs2c) = bootstrap_keys(&psk);
        assert_ne!(bc2s, bs2c);

        // Different randoms / PSK ⇒ different session keys.
        assert_ne!(session_keys(&psk, &cr, &sr), session_keys(&psk, &sr, &cr));
        assert_ne!(
            session_keys(&psk, &cr, &sr),
            session_keys(&[0x99; 32], &cr, &sr)
        );
        // Session keys are not the bootstrap keys.
        assert_ne!(c2s, bc2s);
    }

    #[test]
    fn record_round_trips() {
        let key = [0x42; 32];
        let sealer = RecordKey::new(&key, EPOCH_SESSION, Direction::ClientToServer);
        let opener = RecordKey::new(&key, EPOCH_SESSION, Direction::ClientToServer);

        let nonce = counter_nonce(7);
        let sealed = sealer.seal(&nonce, b"hello world").unwrap();
        assert_eq!(RecordKey::peek_epoch(&sealed), Some(EPOCH_SESSION));

        let (got_nonce, plaintext) = opener.open(&sealed).unwrap();
        assert_eq!(nonce_counter(&got_nonce), 7);
        assert_eq!(plaintext.as_ref(), b"hello world");
    }

    #[test]
    fn open_fails_on_wrong_key_direction_or_epoch() {
        let key = [0x42; 32];
        let other = [0x43; 32];
        let nonce = counter_nonce(1);
        let sealed = RecordKey::new(&key, EPOCH_SESSION, Direction::ClientToServer)
            .seal(&nonce, b"secret")
            .unwrap();

        // Wrong key.
        assert!(
            RecordKey::new(&other, EPOCH_SESSION, Direction::ClientToServer)
                .open(&sealed)
                .is_err()
        );
        // Wrong direction (AAD mismatch) — this is what defeats reflection.
        assert!(
            RecordKey::new(&key, EPOCH_SESSION, Direction::ServerToClient)
                .open(&sealed)
                .is_err()
        );
        // Wrong epoch.
        assert!(
            RecordKey::new(&key, EPOCH_BOOTSTRAP, Direction::ClientToServer)
                .open(&sealed)
                .is_err()
        );
    }

    #[test]
    fn open_rejects_short_datagram() {
        let key = [0x42; 32];
        let opener = RecordKey::new(&key, EPOCH_SESSION, Direction::ClientToServer);
        assert!(matches!(
            opener.open(&[0u8; 4]),
            Err(EncryptionError::CiphertextTooShort { .. })
        ));
    }

    #[test]
    fn replay_window_accepts_in_order_and_rejects_duplicates() {
        let mut w = ReplayWindow::new();
        for c in 0..10u64 {
            assert!(!w.is_replay(c), "counter {c} should be fresh");
            w.record(c);
        }
        // Duplicates rejected.
        for c in 0..10u64 {
            assert!(w.is_replay(c), "counter {c} should be a replay");
        }
        // New high counter accepted.
        assert!(!w.is_replay(100));
        w.record(100);
    }

    #[test]
    fn replay_window_tolerates_reorder_within_window() {
        let mut w = ReplayWindow::new();
        w.record(100);
        // Older-but-within-window arrivals are accepted once.
        assert!(!w.is_replay(95));
        w.record(95);
        assert!(w.is_replay(95));
        assert!(!w.is_replay(99));
        w.record(99);
        // Exactly the window edge and beyond are rejected (diff >= 64).
        assert!(w.is_replay(100 - REPLAY_WINDOW));
        assert!(w.is_replay(0));
        // Just inside the window is still acceptable.
        assert!(!w.is_replay(100 - REPLAY_WINDOW + 1));
    }

    #[test]
    fn replay_window_handles_large_jumps() {
        let mut w = ReplayWindow::new();
        w.record(5);
        // Jump far beyond the window must not panic and resets the bitmap.
        assert!(!w.is_replay(1_000_000));
        w.record(1_000_000);
        assert!(w.is_replay(5));
        assert!(w.is_replay(1_000_000));
        assert!(!w.is_replay(1_000_001));
    }

    fn session_pair() -> (SessionRecords, SessionRecords) {
        let psk = [0x11; 32];
        let cr = [0x22; SESSION_RANDOM_SIZE];
        let sr = [0x33; SESSION_RANDOM_SIZE];
        (
            SessionRecords::derive(&psk, &cr, &sr, Role::Client),
            SessionRecords::derive(&psk, &cr, &sr, Role::Server),
        )
    }

    #[test]
    fn session_records_round_trip_both_directions() {
        let (mut client, mut server) = session_pair();

        let c2s = client.seal(b"from client").unwrap();
        assert_eq!(server.open(&c2s).unwrap().as_ref(), b"from client");

        let s2c = server.seal(b"from server").unwrap();
        assert_eq!(client.open(&s2c).unwrap().as_ref(), b"from server");
    }

    #[test]
    fn session_records_reject_replay_and_reflection() {
        let (mut client, mut server) = session_pair();

        let c2s = client.seal(b"once").unwrap();
        server.open(&c2s).unwrap();
        // Same datagram again: replay window.
        assert!(matches!(
            server.open(&c2s),
            Err(EncryptionError::ReplayDetected(0))
        ));
        // A client's own datagram reflected back to it: wrong direction key.
        let reflected = client.seal(b"mine").unwrap();
        assert!(client.open(&reflected).is_err());
    }

    #[test]
    fn session_records_are_isolated_per_session() {
        let psk = [0x11; 32];
        let mut a = SessionRecords::derive(&psk, &[1; 32], &[2; 32], Role::Client);
        let mut b_server = SessionRecords::derive(&psk, &[3; 32], &[4; 32], Role::Server);
        let from_a = a.seal(b"a").unwrap();
        assert!(b_server.open(&from_a).is_err());
    }

    #[test]
    fn bootstrap_records_round_trip_and_reject_reflection() {
        let psk = [0x42; 32];
        let client = BootstrapRecords::derive(&psk, Role::Client);
        let server = BootstrapRecords::derive(&psk, Role::Server);

        let hello = client.seal(b"hello").unwrap();
        assert_eq!(server.open(&hello).unwrap().as_ref(), b"hello");
        assert!(client.open(&hello).is_err());

        let challenge = server.seal(b"challenge").unwrap();
        assert_eq!(client.open(&challenge).unwrap().as_ref(), b"challenge");
        // Session records never open bootstrap records (epoch mismatch).
        let (_, mut session_server) = session_pair();
        assert!(session_server.open(&hello).is_err());
    }
}
