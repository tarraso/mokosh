//! Guest identity and protected session-resume control messages.
use serde::{Deserialize, Serialize};

/// Stable application identity, independent of the current transport UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlayerId(pub uuid::Uuid);
impl std::fmt::Display for PlayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl From<crate::SessionId> for PlayerId {
    fn from(id: crate::SessionId) -> Self {
        Self(id)
    }
}
impl std::str::FromStr for PlayerId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}
/// Single-purpose 256-bit bearer credential. Never include its contents in logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeToken(pub [u8; 32]);
impl std::fmt::Debug for ResumeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResumeToken([REDACTED])")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeRequest {
    /// None creates a guest. Some restores only this identity; no fallback.
    pub player_id: Option<PlayerId>,
    pub token: Option<ResumeToken>,
    /// Remains unchanged across retries until snapshot readiness completes.
    pub operation: [u8; 16],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeAccepted {
    pub player_id: PlayerId,
    pub token: ResumeToken,
    pub operation: [u8; 16],
    pub resumed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeError {
    Missing,
    Expired,
    Revoked,
    InvalidToken,
    Unsupported,
    Capacity,
    StaleTransport,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeSnapshot {
    pub snapshot_id: u64,
    pub route_id: u16,
    pub schema_hash: u64,
    pub codec_id: u8,
    pub payload: Vec<u8>,
}
/// JSON's byte-array representation fits within a single authenticated UDP datagram.
/// The final encoded envelope is also checked against the UDP size bound.
pub const MAX_RESUME_SNAPSHOT_BYTES: usize = 12_000;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotApplied {
    pub snapshot_id: u64,
}
