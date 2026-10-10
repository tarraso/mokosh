//! In-memory guest ownership. A transport UUID is never a player identity.
use mokosh_protocol::{resume::*, SessionId};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct GuestResumeConfig {
    pub grace: Duration,
    /// None uses the server's maximum client count.
    pub max_pending: Option<usize>,
}
impl GuestResumeConfig {
    pub(crate) fn valid(&self) -> bool {
        !self.grace.is_zero() && Instant::now().checked_add(self.grace).is_some()
    }
}
impl Default for GuestResumeConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(60),
            max_pending: None,
        }
    }
}
struct Operation {
    id: [u8; 16],
    spent: Option<blake3::Hash>,
    reply: Option<ResumeAccepted>,
    until: Instant,
}
struct Guest {
    hash: blake3::Hash,
    initial_operation: [u8; 16],
    last_ordinal: u64,
    owner: Option<(SessionId, u64)>,
    detached_until: Option<Instant>,
    operation: Operation,
    snapshot: Option<u64>,
    ready: bool,
}
#[derive(Default)]
pub(crate) struct Guests {
    owners: HashMap<SessionId, PlayerId>,
    players: HashMap<PlayerId, Guest>,
    ended: HashMap<PlayerId, (ResumeError, Instant)>,
    tombstone_cap: usize,
}
impl Guests {
    pub fn maintenance(&mut self, now: Instant) {
        self.ended.retain(|_, (_, until)| now < *until);
        for g in self.players.values_mut() {
            if now >= g.operation.until {
                g.operation.reply = None;
                g.operation.spent = None;
            }
        }
    }
    pub fn len(&self) -> usize {
        self.players.len()
    }
    pub fn player(&self, session: SessionId) -> Option<PlayerId> {
        self.owners.get(&session).copied()
    }
    pub fn ready(&self, session: SessionId) -> bool {
        self.player(session)
            .is_some_and(|id| self.players[&id].ready)
    }
    pub fn bind(
        &mut self,
        session: SessionId,
        ordinal: u64,
        request: ResumeRequest,
        max: usize,
        now: Instant,
    ) -> Result<(ResumeAccepted, Option<SessionId>, bool), ResumeError> {
        if let Some(id) = self.player(session) {
            let guest = &self.players[&id];
            if request.player_id != Some(id)
                && !(request.player_id.is_none() && request.operation == guest.initial_operation)
            {
                return Err(ResumeError::InvalidToken);
            }
        }
        self.ended.retain(|_, (_, until)| now < *until);
        self.tombstone_cap = max.max(1);
        for g in self.players.values_mut() {
            if now >= g.operation.until {
                g.operation.reply = None;
                g.operation.spent = None;
            }
        }
        if let Some(id) = request.player_id {
            if let Some((error, _)) = self.ended.get(&id) {
                return Err(*error);
            }
            let g = self.players.get_mut(&id).ok_or(ResumeError::Missing)?;
            if g.detached_until.is_some_and(|until| now >= until) {
                return Err(ResumeError::Expired);
            }
            if ordinal < g.last_ordinal {
                return Err(ResumeError::StaleTransport);
            }
            let token = request.token.ok_or(ResumeError::InvalidToken)?;
            let hash = blake3::hash(&token.0);
            // blake3::Hash equality uses constant-time comparison.
            let same = g.operation.id == request.operation && now < g.operation.until;
            if same && (g.operation.spent == Some(hash) || g.hash == hash) {
                let previous = g.owner.map(|(s, _)| s).filter(|s| *s != session);
                let handover = previous.is_some() || g.owner.is_none();
                if let Some(old) = previous {
                    self.owners.remove(&old);
                }
                self.owners.insert(session, id);
                g.last_ordinal = ordinal;
                g.owner = Some((session, ordinal));
                g.detached_until = None;
                if handover {
                    g.ready = false;
                    g.snapshot = None;
                }
                return Ok((
                    g.operation.reply.clone().ok_or(ResumeError::Expired)?,
                    previous,
                    handover,
                ));
            }
            if g.hash != hash {
                return Err(ResumeError::InvalidToken);
            }
            // An operation id may not rotate credentials twice after its cache expires.
            if g.operation.id == request.operation {
                return Err(ResumeError::Expired);
            }
            let token = new_token()?;
            let reply = ResumeAccepted {
                player_id: id,
                token: token.clone(),
                operation: request.operation,
                resumed: true,
            };
            let previous = g.owner.map(|(s, _)| s).filter(|s| *s != session);
            g.hash = blake3::hash(&token.0);
            g.operation = Operation {
                id: request.operation,
                spent: Some(hash),
                reply: Some(reply.clone()),
                until: now + Duration::from_secs(60),
            };
            if let Some(old) = previous {
                self.owners.remove(&old);
            }
            self.owners.insert(session, id);
            g.last_ordinal = ordinal;
            g.owner = Some((session, ordinal));
            g.detached_until = None;
            g.ready = false;
            g.snapshot = None;
            Ok((reply, previous, true))
        } else {
            if request.token.is_some() {
                return Err(ResumeError::InvalidToken);
            }
            // Initial guest creation is also idempotent if its first reply is lost.
            if let Some((id, g)) = self
                .players
                .iter_mut()
                .find(|(_, g)| g.initial_operation == request.operation)
            {
                if g.detached_until.is_some_and(|until| now >= until)
                    || g.operation.id != request.operation
                    || now >= g.operation.until
                {
                    return Err(ResumeError::Expired);
                }
                if ordinal < g.last_ordinal {
                    return Err(ResumeError::StaleTransport);
                }
                let old = g.owner.map(|(s, _)| s).filter(|s| *s != session);
                if let Some(old) = old {
                    self.owners.remove(&old);
                }
                self.owners.insert(session, *id);
                g.last_ordinal = ordinal;
                g.owner = Some((session, ordinal));
                g.detached_until = None;
                g.ready = false;
                g.snapshot = None;
                return Ok((
                    g.operation.reply.clone().ok_or(ResumeError::Expired)?,
                    old,
                    true,
                ));
            }
            if self.len() >= max {
                return Err(ResumeError::Capacity);
            }
            let token = new_token()?;
            let id = PlayerId(SessionId::new_v4());
            let reply = ResumeAccepted {
                player_id: id,
                token: token.clone(),
                operation: request.operation,
                resumed: false,
            };
            self.owners.insert(session, id);
            self.players.insert(
                id,
                Guest {
                    initial_operation: request.operation,
                    last_ordinal: ordinal,
                    hash: blake3::hash(&token.0),
                    owner: Some((session, ordinal)),
                    detached_until: None,
                    operation: Operation {
                        id: request.operation,
                        spent: None,
                        reply: Some(reply.clone()),
                        until: now + Duration::from_secs(60),
                    },
                    snapshot: None,
                    ready: false,
                },
            );
            Ok((reply, None, true))
        }
    }
    pub fn detach(
        &mut self,
        session: SessionId,
        now: Instant,
        grace: Duration,
    ) -> Option<PlayerId> {
        let id = self.player(session)?;
        let g = self.players.get_mut(&id)?;
        self.owners.remove(&session);
        g.owner = None;
        g.ready = false;
        g.snapshot = None;
        g.detached_until = Some(now + grace);
        Some(id)
    }
    pub fn remove(&mut self, id: PlayerId, reason: ResumeError, now: Instant) -> Option<SessionId> {
        let guest = self.players.remove(&id)?;
        if let Some((session, _)) = guest.owner {
            self.owners.remove(&session);
        }
        self.ended.retain(|_, (_, until)| now < *until);
        if self.ended.len() >= self.tombstone_cap.max(1) {
            if let Some(oldest) = self
                .ended
                .iter()
                .min_by_key(|(_, (_, until))| *until)
                .map(|(id, _)| *id)
            {
                self.ended.remove(&oldest);
            }
        }
        self.ended
            .insert(id, (reason, now + Duration::from_secs(60)));
        guest.owner.map(|(s, _)| s)
    }
    pub fn contains(&self, id: PlayerId) -> bool {
        self.players.contains_key(&id)
    }
    pub fn expired(&self, now: Instant) -> Vec<PlayerId> {
        self.players
            .iter()
            .filter_map(|(id, g)| g.detached_until.is_some_and(|t| now >= t).then_some(*id))
            .collect()
    }
    pub fn set_snapshot(&mut self, id: PlayerId, snapshot: u64) -> Option<SessionId> {
        let g = self.players.get_mut(&id)?;
        let (s, _) = g.owner?;
        if g.ready {
            return None;
        }
        g.snapshot = Some(snapshot);
        Some(s)
    }
    pub fn confirm(&mut self, session: SessionId, snapshot: u64) -> Option<(PlayerId, bool)> {
        let id = self.player(session)?;
        let g = self.players.get_mut(&id)?;
        if g.snapshot != Some(snapshot) {
            return None;
        }
        let newly_ready = !g.ready;
        g.ready = true;
        Some((id, newly_ready))
    }
}
fn new_token() -> Result<ResumeToken, ResumeError> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| ResumeError::Unsupported)?;
    Ok(ResumeToken(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn create(op: u8) -> ResumeRequest {
        ResumeRequest {
            player_id: None,
            token: None,
            operation: [op; 16],
        }
    }
    fn restore(reply: &ResumeAccepted, op: u8) -> ResumeRequest {
        ResumeRequest {
            player_id: Some(reply.player_id),
            token: Some(reply.token.clone()),
            operation: [op; 16],
        }
    }
    #[test]
    fn rotation_is_idempotent_and_old_transports_cannot_reclaim_owner() {
        let mut guests = Guests::default();
        let now = Instant::now();
        let first = SessionId::new_v4();
        let (a, _, _) = guests.bind(first, 1, create(1), 1, now).unwrap();
        let second = SessionId::new_v4();
        let request = restore(&a, 2);
        let (b, old, _) = guests.bind(second, 2, request.clone(), 1, now).unwrap();
        assert_eq!(old, Some(first));
        assert_eq!(a.player_id, b.player_id);
        assert_ne!(a.token, b.token);
        let third = SessionId::new_v4();
        let (duplicate, _, _) = guests.bind(third, 3, request.clone(), 1, now).unwrap();
        assert_eq!(duplicate.token, b.token);
        assert_eq!(guests.len(), 1);
        assert_eq!(
            guests.bind(second, 2, request, 1, now).unwrap_err(),
            ResumeError::StaleTransport
        );
        assert_eq!(
            guests
                .bind(SessionId::new_v4(), 4, restore(&a, 3), 1, now)
                .unwrap_err(),
            ResumeError::InvalidToken
        );
        // Reply received, snapshot confirmation lost: current token + SAME operation.
        let (same, _, _) = guests
            .bind(SessionId::new_v4(), 4, restore(&b, 2), 1, now)
            .unwrap();
        assert_eq!(same.token, b.token);
    }
    #[test]
    fn cache_expiry_erases_reply_but_preserves_live_credential_and_detached_ordinal() {
        let mut guests = Guests::default();
        let now = Instant::now();
        let session = SessionId::new_v4();
        let (a, _, _) = guests.bind(session, 5, create(1), 1, now).unwrap();
        guests.maintenance(now + Duration::from_secs(60));
        assert!(guests.players[&a.player_id].operation.reply.is_none());
        assert!(guests.players[&a.player_id].operation.spent.is_none());
        assert_eq!(
            guests
                .bind(
                    SessionId::new_v4(),
                    6,
                    create(1),
                    1,
                    now + Duration::from_secs(60)
                )
                .unwrap_err(),
            ResumeError::Expired
        );
        let (b, _, _) = guests
            .bind(session, 5, restore(&a, 2), 1, now + Duration::from_secs(60))
            .unwrap();
        guests.detach(
            session,
            now + Duration::from_secs(60),
            Duration::from_secs(60),
        );
        assert_eq!(
            guests
                .bind(
                    SessionId::new_v4(),
                    4,
                    restore(&b, 2),
                    1,
                    now + Duration::from_secs(61)
                )
                .unwrap_err(),
            ResumeError::StaleTransport
        );
    }

    #[test]
    fn detached_guests_reserve_capacity_and_fail_explicitly_after_expiry_or_revocation() {
        let mut guests = Guests::default();
        let now = Instant::now();
        let first = SessionId::new_v4();
        let (a, _, _) = guests.bind(first, 1, create(1), 1, now).unwrap();
        guests.detach(first, now, Duration::from_secs(60));
        assert_eq!(
            guests
                .bind(SessionId::new_v4(), 2, create(2), 1, now)
                .unwrap_err(),
            ResumeError::Capacity
        );
        assert_eq!(
            guests
                .bind(
                    SessionId::new_v4(),
                    2,
                    restore(&a, 2),
                    1,
                    now + Duration::from_secs(60)
                )
                .unwrap_err(),
            ResumeError::Expired
        );
        guests.remove(
            a.player_id,
            ResumeError::Expired,
            now + Duration::from_secs(60),
        );
        assert_eq!(
            guests
                .bind(
                    SessionId::new_v4(),
                    2,
                    restore(&a, 2),
                    1,
                    now + Duration::from_secs(61)
                )
                .unwrap_err(),
            ResumeError::Expired
        );
        let (b, _, _) = guests.bind(first, 3, create(3), 1, now).unwrap();
        guests.remove(b.player_id, ResumeError::Revoked, now);
        assert_eq!(
            guests
                .bind(SessionId::new_v4(), 4, restore(&b, 4), 1, now)
                .unwrap_err(),
            ResumeError::Revoked
        );
    }
    #[test]
    fn late_cleanup_and_snapshot_ack_are_bound_to_the_current_owner() {
        let mut guests = Guests::default();
        let now = Instant::now();
        let old = SessionId::new_v4();
        let (a, _, _) = guests.bind(old, 1, create(1), 1, now).unwrap();
        assert!(!guests.ready(old));
        guests.set_snapshot(a.player_id, 10);
        let new = SessionId::new_v4();
        guests.bind(new, 2, restore(&a, 2), 1, now).unwrap();
        assert_eq!(guests.detach(old, now, Duration::from_secs(60)), None);
        assert_eq!(guests.confirm(old, 10), None);
        assert_eq!(guests.confirm(new, 10), None);
        guests.set_snapshot(a.player_id, 11);
        assert_eq!(guests.confirm(new, 10), None);
        assert_eq!(guests.confirm(new, 11), Some((a.player_id, true)));
        assert!(guests.ready(new));
        assert_eq!(guests.confirm(new, 11), Some((a.player_id, false)));
    }
    #[test]
    fn lost_initial_reply_reuses_guest_and_token_and_errors_do_not_extend_grace() {
        let mut guests = Guests::default();
        let now = Instant::now();
        let first = SessionId::new_v4();
        let (a, _, _) = guests.bind(first, 1, create(1), 2, now).unwrap();
        let second = SessionId::new_v4();
        let (b, _, _) = guests.bind(second, 2, create(1), 2, now).unwrap();
        assert_eq!(a.token, b.token);
        assert_eq!(a.player_id, b.player_id);
        assert_eq!(guests.len(), 1);
        guests.detach(second, now, Duration::from_secs(60));
        let mut invalid = restore(&a, 2);
        invalid.token = Some(ResumeToken([0; 32]));
        assert_eq!(
            guests
                .bind(
                    SessionId::new_v4(),
                    3,
                    invalid,
                    2,
                    now + Duration::from_secs(59)
                )
                .unwrap_err(),
            ResumeError::InvalidToken
        );
        assert_eq!(
            guests.expired(now + Duration::from_secs(60)),
            vec![a.player_id]
        );
        assert!(!format!("{:?}", a.token).contains(&format!("{:?}", a.token.0)));
    }
}
