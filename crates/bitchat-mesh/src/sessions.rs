//! Noise session manager: the observable behaviour of iOS
//! `NoiseSessionManager` + `BLENoisePacketHandler`.
//!
//! - Anything 32 bytes long is a fresh message 1. So is any handshake message
//!   when nothing exists for the peer, or a longer one while established.
//! - Crossed message 1s: the lower peer ID keeps the initiator role, the
//!   higher one yields and answers.
//! - Message 1 while established starts a new handshake; the old keys stay
//!   receive-only until it completes, and come back if it fails (followed
//!   by a 60 s cooldown on further unauthenticated attempts).
//! - The remote static key must hash to the peer ID it claims.
//! - Initiator gives up after 10 s, responder after 20 s.
//! - Ciphertext that overtakes message 3 is held (4 per peer, 20 s).
//! - Sessions are renewed after 21.6 h or 9e8 sent messages.
//! - Each side sends its authenticated peer state (payload `0x21`) once per
//!   session and echoes once after receiving the other's.
//! - Handshake messages: 10 per minute per peer, 30 overall. Incoming
//!   ciphertext: 100 per second per peer, 500 overall.

use crate::peer_state::PeerState;
use bitchat_noise::{Handshake, NoiseError, Role, Session};
use bitchat_proto::PeerId;
use bitchat_proto::limits::MAX_FRAMED_FILE_BYTES;
use std::collections::{HashMap, VecDeque};

pub const XX_FIRST_MESSAGE: usize = 32;
pub const INITIATOR_TIMEOUT_MS: u64 = 10_000;
pub const RESPONDER_TIMEOUT_MS: u64 = 20_000;
pub const COMPLETION_GRACE_MS: u64 = 1_000;
pub const RECOVERY_DELAY_MS: u64 = 200;
pub const ROLLBACK_COOLDOWN_MS: u64 = 60_000;
pub const SESSION_TIMEOUT_MS: u64 = 86_400_000;
pub const REKEY_AGE_MS: u64 = SESSION_TIMEOUT_MS / 10 * 9;
pub const REKEY_MESSAGES: u64 = 900_000_000;
pub const PEER_STATE_TYPE: u8 = 0x21;
pub const HANDSHAKES_PER_MINUTE: usize = 10;
pub const HANDSHAKES_PER_MINUTE_TOTAL: usize = 30;
pub const MESSAGES_PER_SECOND: usize = 100;
pub const MESSAGES_PER_SECOND_TOTAL: usize = 500;
const DEFERRED_PER_PEER: usize = 4;
const DEFERRED_TOTAL: usize = 32;
const DEFERRED_LIFETIME_MS: u64 = RESPONDER_TIMEOUT_MS;
/// Largest Noise frame inside a framed file packet (v2 header + two IDs).
const DEFERRED_MAX_FRAME: usize = MAX_FRAMED_FILE_BYTES - 32;
const DEFERRED_MAX_BYTES: usize = DEFERRED_MAX_FRAME + 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Send this handshake message (packet type 0x10) to the peer.
    SendHandshake { to: PeerId, message: Vec<u8> },
    /// Send this frame (packet type 0x11) to the peer.
    SendEncrypted { to: PeerId, frame: Vec<u8> },
    /// A new session is up (send a fresh announce).
    Established {
        peer: PeerId,
        remote_static: [u8; 32],
    },
    /// A decrypted Noise payload (type byte first), never `0x21`.
    Received { from: PeerId, plaintext: Vec<u8> },
    /// The peer's authenticated capabilities and signing key.
    PeerState { peer: PeerId, state: PeerState },
    /// No session and no retry pending; these payloads were not sent.
    Failed { peer: PeerId, unsent: Vec<Vec<u8>> },
}

struct Pending {
    handshake: Handshake,
    started_ms: u64,
    /// Became responder by yielding a crossed initiation.
    by_yield: bool,
    /// One recovery attempt is owed if this handshake fails.
    retry: bool,
}

struct Live {
    session: Session,
    since_ms: u64,
    sent_echo: bool,
}

enum Slot {
    Handshaking(Box<Pending>),
    Established(Box<Live>),
}

#[derive(Default)]
struct PeerSessions {
    slot: Option<Slot>,
    /// Old keys, receive-only, while answering a re-handshake.
    quarantine: Option<Box<Live>>,
    queue: VecDeque<Vec<u8>>,
    /// Restored keys after a timeout: hold the queue for the next handshake.
    parked: bool,
    initiator_done_ms: Option<u64>,
    recovery_at: Option<u64>,
    cooldown_until: Option<u64>,
}

struct Deferred {
    peer: PeerId,
    frame: Vec<u8>,
    at_ms: u64,
}

/// Sliding-window counter (iOS `NoiseRateLimiter`).
struct RateLimiter {
    window_ms: u64,
    per_peer: usize,
    total: usize,
    peers: HashMap<PeerId, Vec<u64>>,
    all: Vec<u64>,
}

impl RateLimiter {
    fn new(window_ms: u64, per_peer: usize, total: usize) -> RateLimiter {
        RateLimiter {
            window_ms,
            per_peer,
            total,
            peers: HashMap::new(),
            all: Vec::new(),
        }
    }

    fn allow(&mut self, peer: PeerId, now_ms: u64) -> bool {
        let cutoff = now_ms.saturating_sub(self.window_ms);
        self.all.retain(|&t| t > cutoff);
        if self.all.len() >= self.total {
            return false;
        }
        let times = self.peers.entry(peer).or_default();
        times.retain(|&t| t > cutoff);
        if times.len() >= self.per_peer {
            return false;
        }
        times.push(now_ms);
        self.all.push(now_ms);
        true
    }

    fn prune(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.window_ms);
        self.peers.retain(|_, times| {
            times.retain(|&t| t > cutoff);
            !times.is_empty()
        });
    }
}

struct Local {
    static_key: [u8; 32],
    /// `0x21 || PeerState`.
    state: Vec<u8>,
    handshakes: RateLimiter,
}

pub struct SessionManager {
    local: Local,
    local_id: PeerId,
    messages: RateLimiter,
    peers: HashMap<PeerId, PeerSessions>,
    deferred: Vec<Deferred>,
}

fn new_handshake(role: Role, local_static: &[u8; 32]) -> Handshake {
    Handshake::new(role, local_static).expect("fixed noise parameters")
}

/// Starts an initiator, replacing whatever the slot holds.
fn initiate(
    peer: PeerId,
    p: &mut PeerSessions,
    local: &mut Local,
    retry: bool,
    now_ms: u64,
    events: &mut Vec<Event>,
) {
    if !local.handshakes.allow(peer, now_ms) {
        return;
    }
    let mut handshake = new_handshake(Role::Initiator, &local.static_key);
    let Ok(message) = handshake.write_message() else {
        return;
    };
    p.slot = Some(Slot::Handshaking(Box::new(Pending {
        handshake,
        started_ms: now_ms,
        by_yield: false,
        retry,
    })));
    p.quarantine = None;
    p.initiator_done_ms = None;
    p.recovery_at = None;
    p.cooldown_until = None;
    events.push(Event::SendHandshake { to: peer, message });
}

/// Encrypts queued payloads if the session can send.
fn flush(
    peer: PeerId,
    p: &mut PeerSessions,
    local: &mut Local,
    now_ms: u64,
    events: &mut Vec<Event>,
) {
    if p.parked {
        return;
    }
    let Some(Slot::Established(live)) = &mut p.slot else {
        return;
    };
    let mut renew = now_ms.saturating_sub(live.since_ms) > SESSION_TIMEOUT_MS;
    while !renew && let Some(plaintext) = p.queue.pop_front() {
        match live.session.encrypt(&plaintext) {
            Ok(frame) => events.push(Event::SendEncrypted { to: peer, frame }),
            Err(_) => {
                p.queue.push_front(plaintext);
                renew = true;
            }
        }
    }
    if renew {
        initiate(peer, p, local, true, now_ms, events);
    }
}

fn deliver(
    peer: PeerId,
    live: &mut Live,
    plaintext: Vec<u8>,
    local_state: &[u8],
    events: &mut Vec<Event>,
) {
    match plaintext.first() {
        None => {}
        Some(&PEER_STATE_TYPE) => {
            let Some(state) = PeerState::decode(&plaintext[1..]) else {
                return;
            };
            events.push(Event::PeerState { peer, state });
            if !live.sent_echo {
                live.sent_echo = true;
                if let Ok(frame) = live.session.encrypt(local_state) {
                    events.push(Event::SendEncrypted { to: peer, frame });
                }
            }
        }
        Some(_) => events.push(Event::Received {
            from: peer,
            plaintext,
        }),
    }
}

fn fail_if_idle(peer: PeerId, p: &mut PeerSessions, events: &mut Vec<Event>) {
    if p.slot.is_none() && p.recovery_at.is_none() && !p.queue.is_empty() {
        events.push(Event::Failed {
            peer,
            unsent: p.queue.drain(..).collect(),
        });
    }
}

impl SessionManager {
    pub fn new(local_static: [u8; 32], local_state: &PeerState) -> SessionManager {
        let local_id = bitchat_noise::peer_id(&bitchat_noise::public_key(&local_static));
        let mut manager = SessionManager {
            local: Local {
                static_key: local_static,
                state: Vec::new(),
                handshakes: RateLimiter::new(
                    60_000,
                    HANDSHAKES_PER_MINUTE,
                    HANDSHAKES_PER_MINUTE_TOTAL,
                ),
            },
            local_id,
            messages: RateLimiter::new(1_000, MESSAGES_PER_SECOND, MESSAGES_PER_SECOND_TOTAL),
            peers: HashMap::new(),
            deferred: Vec::new(),
        };
        manager.set_local_state(local_state);
        manager
    }

    /// Used for sessions established from now on.
    pub fn set_local_state(&mut self, state: &PeerState) {
        let mut payload = vec![PEER_STATE_TYPE];
        payload.extend(state.encode());
        self.local.state = payload;
    }

    pub fn local_id(&self) -> PeerId {
        self.local_id
    }

    /// A session exists and sends go out immediately.
    pub fn is_established(&self, peer: &PeerId) -> bool {
        self.peers
            .get(peer)
            .is_some_and(|p| !p.parked && matches!(p.slot, Some(Slot::Established(_))))
    }

    pub fn has_session(&self, peer: &PeerId) -> bool {
        self.peers.get(peer).is_some_and(|p| p.slot.is_some())
    }

    pub fn remote_static(&self, peer: &PeerId) -> Option<[u8; 32]> {
        let p = self.peers.get(peer)?;
        match &p.slot {
            Some(Slot::Established(live)) => Some(live.session.remote_static()),
            _ => p.quarantine.as_ref().map(|l| l.session.remote_static()),
        }
    }

    /// Encrypts for `to`, or queues and starts a handshake.
    pub fn send(&mut self, to: PeerId, plaintext: Vec<u8>, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let p = self.peers.entry(to).or_default();
        p.queue.push_back(plaintext);
        match p.slot {
            None => initiate(to, p, &mut self.local, false, now_ms, &mut events),
            Some(Slot::Established(_)) => flush(to, p, &mut self.local, now_ms, &mut events),
            Some(Slot::Handshaking(_)) => {}
        }
        events
    }

    /// Starts a handshake if there is nothing for this peer yet.
    pub fn connect(&mut self, to: PeerId, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let p = self.peers.entry(to).or_default();
        if p.slot.is_none() {
            initiate(to, p, &mut self.local, false, now_ms, &mut events);
        }
        events
    }

    pub fn on_handshake(&mut self, from: PeerId, message: &[u8], now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if !self.local.handshakes.allow(from, now_ms) {
            return events;
        }
        let local_static = self.local.static_key;
        let we_lead = self.local_id < from;
        let p = self.peers.entry(from).or_default();
        let established = matches!(p.slot, Some(Slot::Established(_)));
        let fresh = message.len() == XX_FIRST_MESSAGE
            || p.slot.is_none()
            || (established && message.len() > XX_FIRST_MESSAGE);

        let mut by_yield = false;
        let mut retry = false;
        if fresh {
            if let Some(until) = p.cooldown_until {
                if until > now_ms && established {
                    return events;
                }
                p.cooldown_until = None;
            }
            if p.recovery_at.is_some() && we_lead && established {
                return events;
            }
            if let Some(done) = p.initiator_done_ms.take()
                && now_ms.saturating_sub(done) < COMPLETION_GRACE_MS
                && we_lead
                && established
            {
                // Late crossed message 1 after we finished as initiator:
                // ignore it, then re-check with one handshake of our own.
                p.recovery_at = Some(done + COMPLETION_GRACE_MS);
                return events;
            }
            if let Some(Slot::Handshaking(pending)) = &p.slot
                && pending.handshake.role() == Role::Initiator
            {
                if we_lead {
                    return events;
                }
                by_yield = true;
                retry = pending.retry;
                p.slot = None;
            }
        }

        let mut started_ms = now_ms;
        let mut handshake = match p.slot.take() {
            Some(Slot::Established(live)) => {
                if !fresh {
                    p.slot = Some(Slot::Established(live));
                    return events;
                }
                p.quarantine = Some(live);
                new_handshake(Role::Responder, &local_static)
            }
            Some(Slot::Handshaking(pending)) => {
                by_yield = pending.by_yield;
                retry = pending.retry;
                started_ms = pending.started_ms; // a repeat never extends the deadline
                if fresh {
                    new_handshake(Role::Responder, &local_static)
                } else {
                    pending.handshake
                }
            }
            None => new_handshake(Role::Responder, &local_static),
        };
        let role = handshake.role();

        let step = handshake.read_message(message).and_then(|_| {
            if handshake.is_finished() {
                Ok(None)
            } else {
                handshake.write_message().map(Some)
            }
        });
        let mismatch = handshake.is_finished()
            && handshake
                .remote_static()
                .map(|k| bitchat_noise::peer_id(&k))
                != Some(from);
        let info = FailInfo {
            role,
            by_yield,
            retry,
            mismatch,
        };
        let Ok(response) = step else {
            self.fail(from, info, now_ms, &mut events);
            return events;
        };
        if mismatch {
            self.fail(from, info, now_ms, &mut events);
            return events;
        }
        if let Some(message) = response {
            events.push(Event::SendHandshake { to: from, message });
        }
        if !handshake.is_finished() {
            p.slot = Some(Slot::Handshaking(Box::new(Pending {
                handshake,
                started_ms,
                by_yield,
                retry,
            })));
            return events;
        }
        match handshake.into_session() {
            Ok(session) => self.establish(from, session, role, now_ms, &mut events),
            Err(_) => self.fail(from, info, now_ms, &mut events),
        }
        events
    }

    fn establish(
        &mut self,
        peer: PeerId,
        session: Session,
        role: Role,
        now_ms: u64,
        events: &mut Vec<Event>,
    ) {
        let p = self.peers.get_mut(&peer).expect("peer exists");
        p.quarantine = None;
        p.cooldown_until = None;
        p.recovery_at = None;
        p.parked = false;
        p.initiator_done_ms = (role == Role::Initiator).then_some(now_ms);
        events.push(Event::Established {
            peer,
            remote_static: session.remote_static(),
        });
        let mut live = Live {
            session,
            since_ms: now_ms,
            sent_echo: false,
        };

        // Ciphertext that overtook message 3.
        let (early, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.deferred)
            .into_iter()
            .partition(|d| d.peer == peer);
        self.deferred = rest;
        for d in early {
            if now_ms.saturating_sub(d.at_ms) <= DEFERRED_LIFETIME_MS
                && let Ok(plaintext) = live.session.decrypt(&d.frame)
            {
                deliver(peer, &mut live, plaintext, &self.local.state, events);
            }
        }
        if let Ok(frame) = live.session.encrypt(&self.local.state) {
            events.push(Event::SendEncrypted { to: peer, frame });
        }
        p.slot = Some(Slot::Established(Box::new(live)));
        flush(peer, p, &mut self.local, now_ms, events);
    }

    fn fail(&mut self, peer: PeerId, info: FailInfo, now_ms: u64, events: &mut Vec<Event>) {
        let local = &mut self.local;
        let p = self.peers.get_mut(&peer).expect("peer exists");
        p.slot = None;
        p.initiator_done_ms = None;
        let recover = info.retry && !info.mismatch;
        if let Some(live) = p.quarantine.take() {
            p.slot = Some(Slot::Established(live));
            p.cooldown_until = Some(now_ms + ROLLBACK_COOLDOWN_MS);
            if recover {
                p.parked = true;
            }
        }
        if recover {
            p.recovery_at = Some(now_ms + RECOVERY_DELAY_MS);
        } else if !info.mismatch
            && info.role == Role::Responder
            && !info.by_yield
            && p.slot.is_none()
        {
            initiate(peer, p, local, false, now_ms, events);
        }
        flush(peer, p, local, now_ms, events);
        fail_if_idle(peer, p, events);
    }

    pub fn on_encrypted(&mut self, from: PeerId, frame: &[u8], now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        if !self.messages.allow(from, now_ms) {
            return events;
        }
        let local = &mut self.local;
        let p = self.peers.entry(from).or_default();
        match &mut p.slot {
            Some(Slot::Established(live)) => {
                if now_ms.saturating_sub(live.since_ms) > SESSION_TIMEOUT_MS {
                    initiate(from, p, local, false, now_ms, &mut events);
                    return events;
                }
                // Forged, replayed or malformed frames never touch the session.
                if let Ok(plaintext) = live.session.decrypt(frame) {
                    deliver(from, live, plaintext, &local.state, &mut events);
                }
            }
            Some(Slot::Handshaking(pending)) if pending.handshake.role() == Role::Responder => {
                let defer = match p.quarantine.as_mut() {
                    None => true,
                    Some(old) => match old.session.decrypt(frame) {
                        Ok(plaintext) => {
                            // Old keys still read traffic, but their peer
                            // state is no longer current.
                            if plaintext.first() != Some(&PEER_STATE_TYPE) && !plaintext.is_empty()
                            {
                                events.push(Event::Received { from, plaintext });
                            }
                            false
                        }
                        Err(NoiseError::InvalidCiphertext) => false,
                        Err(_) => true,
                    },
                };
                if defer {
                    self.defer(from, frame, now_ms);
                }
            }
            Some(Slot::Handshaking(_)) => {}
            None => initiate(from, p, local, false, now_ms, &mut events),
        }
        events
    }

    fn defer(&mut self, peer: PeerId, frame: &[u8], now_ms: u64) {
        self.purge_deferred(now_ms);
        let bytes: usize = self.deferred.iter().map(|d| d.frame.len()).sum();
        if frame.len() > DEFERRED_MAX_FRAME
            || self.deferred.iter().filter(|d| d.peer == peer).count() >= DEFERRED_PER_PEER
            || self.deferred.len() >= DEFERRED_TOTAL
            || bytes + frame.len() > DEFERRED_MAX_BYTES
        {
            return;
        }
        self.deferred.push(Deferred {
            peer,
            frame: frame.to_vec(),
            at_ms: now_ms,
        });
    }

    fn purge_deferred(&mut self, now_ms: u64) {
        self.deferred
            .retain(|d| now_ms.saturating_sub(d.at_ms) <= DEFERRED_LIFETIME_MS);
    }

    /// Timeouts, recoveries and scheduled renewals. Call about once a second.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        self.purge_deferred(now_ms);
        self.local.handshakes.prune(now_ms);
        self.messages.prune(now_ms);
        let local = &mut self.local;
        let mut timed_out = Vec::new();
        for (&peer, p) in self.peers.iter_mut() {
            match &p.slot {
                Some(Slot::Handshaking(pending)) => {
                    let limit = match pending.handshake.role() {
                        Role::Initiator => INITIATOR_TIMEOUT_MS,
                        Role::Responder => RESPONDER_TIMEOUT_MS,
                    };
                    if now_ms.saturating_sub(pending.started_ms) > limit {
                        timed_out.push(peer);
                    }
                }
                Some(Slot::Established(live)) => {
                    if p.recovery_at.is_some_and(|at| now_ms >= at) {
                        initiate(peer, p, local, false, now_ms, &mut events);
                    } else if now_ms.saturating_sub(live.since_ms) > REKEY_AGE_MS
                        || live.session.messages_sent() >= REKEY_MESSAGES
                    {
                        initiate(peer, p, local, true, now_ms, &mut events);
                    }
                }
                None => {
                    if p.recovery_at.is_some_and(|at| now_ms >= at) {
                        initiate(peer, p, local, false, now_ms, &mut events);
                    }
                }
            }
        }
        for peer in timed_out {
            let p = self.peers.get_mut(&peer).expect("peer exists");
            let Some(Slot::Handshaking(pending)) = p.slot.take() else {
                continue;
            };
            p.initiator_done_ms = None;
            let mut recover = pending.retry;
            if let Some(live) = p.quarantine.take() {
                // Message 3 never came: the old keys come back for receiving,
                // and sending waits for one fresh handshake of our own.
                p.slot = Some(Slot::Established(live));
                p.cooldown_until = Some(now_ms + ROLLBACK_COOLDOWN_MS);
                p.parked = true;
                recover = true;
            }
            if recover {
                p.recovery_at = Some(now_ms);
                if p.slot.is_none() {
                    initiate(peer, p, local, false, now_ms, &mut events);
                }
            }
            fail_if_idle(peer, p, &mut events);
        }
        events
    }

    /// Forget a peer (signed leave, or a wipe).
    pub fn remove(&mut self, peer: &PeerId) {
        self.peers.remove(peer);
        self.deferred.retain(|d| d.peer != *peer);
    }
}

#[derive(Clone, Copy)]
struct FailInfo {
    role: Role,
    by_yield: bool,
    retry: bool,
    mismatch: bool,
}
