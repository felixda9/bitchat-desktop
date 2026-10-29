//! Flooding: duplicate suppression, relay TTL and jitter, source routes.
//! Ports iOS `RelayController`, `BLEReceivePipeline`, `MessageDeduplicator`,
//! `BLEScheduledRelayStore` and `BLERouteForwardingPolicy`.

use crate::identity::DEFAULT_TTL;
use bitchat_proto::{Packet, PeerId, message_type as t};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;

/// Connected-peer count from which the graph counts as dense.
pub const HIGH_DEGREE: usize = 6;
pub const FRAGMENT_TTL_CAP: u8 = 7;
pub const FRAGMENT_TTL_CAP_DENSE: u8 = 5;
pub const DEDUP_MAX_AGE_MS: u64 = 300_000;
pub const DEDUP_MAX_COUNT: usize = 1000;
/// Scheduled relays beyond this are all dropped at maintenance.
pub const MAX_SCHEDULED: usize = 512;
const BOARD_FLAGS_TLV: u8 = 0x09;
const BOARD_URGENT: u8 = 0x01;

/// Identity of a packet for duplicate checks: sender, timestamp, type and
/// the first 4 bytes of SHA-256 over the (decompressed) payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PacketKey {
    pub sender: PeerId,
    pub timestamp: u64,
    pub packet_type: u8,
    pub digest: [u8; 4],
}

impl PacketKey {
    pub fn of(packet: &Packet) -> PacketKey {
        let hash = Sha256::digest(&packet.payload);
        PacketKey {
            sender: packet.sender_id,
            timestamp: packet.timestamp,
            packet_type: packet.packet_type,
            digest: [hash[0], hash[1], hash[2], hash[3]],
        }
    }
}

/// Fragments skip the duplicate filter (reassembly handles them), and so do
/// our own packets replayed by sync with TTL 0.
pub fn should_deduplicate(packet: &Packet, local: PeerId) -> bool {
    let self_sync_replay = packet.ttl == 0 && packet.sender_id == local;
    packet.packet_type != t::FRAGMENT && !self_sync_replay
}

/// Seen packets, kept 5 minutes; past 1000 the oldest are dropped down to 750.
pub struct Deduplicator {
    max_age_ms: u64,
    max_count: usize,
    entries: VecDeque<(PacketKey, u64)>,
    seen: HashSet<PacketKey>,
}

impl Default for Deduplicator {
    fn default() -> Self {
        Deduplicator::new(DEDUP_MAX_AGE_MS, DEDUP_MAX_COUNT)
    }
}

impl Deduplicator {
    pub fn new(max_age_ms: u64, max_count: usize) -> Deduplicator {
        Deduplicator {
            max_age_ms,
            max_count,
            entries: VecDeque::new(),
            seen: HashSet::new(),
        }
    }

    /// `true` if already seen; otherwise records it.
    pub fn is_duplicate(&mut self, key: PacketKey, now_ms: u64) -> bool {
        self.cleanup(now_ms);
        if self.seen.contains(&key) {
            return true;
        }
        self.mark(key, now_ms);
        false
    }

    /// Records a packet we sent so its echoes are dropped.
    pub fn mark(&mut self, key: PacketKey, now_ms: u64) {
        if self.seen.insert(key) {
            self.entries.push_back((key, now_ms));
        }
        if self.entries.len() > self.max_count {
            let target = self.max_count * 3 / 4;
            while self.entries.len() > target {
                if let Some((old, _)) = self.entries.pop_front() {
                    self.seen.remove(&old);
                }
            }
        }
    }

    pub fn contains(&self, key: &PacketKey) -> bool {
        self.seen.contains(key)
    }

    pub fn cleanup(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.max_age_ms);
        while let Some(&(key, at)) = self.entries.front()
            && at < cutoff
        {
            self.entries.pop_front();
            self.seen.remove(&key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayDecision {
    pub new_ttl: u8,
    pub delay_ms: RangeInclusive<u32>,
}

fn urgent_board_post(payload: &[u8]) -> bool {
    let mut off = 0;
    while off + 3 <= payload.len() {
        let kind = payload[off];
        let len = u16::from_be_bytes([payload[off + 1], payload[off + 2]]) as usize;
        off += 3;
        if off + len > payload.len() {
            return false;
        }
        if kind == BOARD_FLAGS_TLV && len == 1 {
            return payload[off] & BOARD_URGENT != 0;
        }
        off += len;
    }
    false
}

/// Whether to relay a packet we received, with what TTL and after what
/// delay. `degree` is the number of connected peers.
pub fn decide(packet: &Packet, local: PeerId, degree: usize) -> Option<RelayDecision> {
    let kind = packet.packet_type;
    let ttl = packet.ttl.min(DEFAULT_TTL);
    // Sync requests are link-local.
    if kind == t::REQUEST_SYNC {
        return None;
    }
    if ttl <= 1 || packet.sender_id == local || packet.recipient_id == Some(local) {
        return None;
    }

    let directed = packet.recipient_id.is_some()
        && matches!(
            kind,
            t::NOISE_ENCRYPTED | t::COURIER_ENVELOPE | t::PING | t::PONG | t::NOSTR_CARRIER
        );
    let handshake = kind == t::NOISE_HANDSHAKE;
    let directed_fragment = kind == t::FRAGMENT && packet.recipient_id.is_some();
    if handshake || directed || directed_fragment {
        return Some(RelayDecision {
            new_ttl: ttl - 1,
            delay_ms: if handshake { 10..=35 } else { 20..=60 },
        });
    }

    if kind == t::FRAGMENT || kind == t::VOICE_FRAME {
        let cap = if degree >= HIGH_DEGREE {
            FRAGMENT_TTL_CAP_DENSE
        } else {
            FRAGMENT_TTL_CAP
        };
        let limit = ttl.min(cap);
        if limit <= 1 {
            return None;
        }
        return Some(RelayDecision {
            new_ttl: limit - 1,
            delay_ms: 8..=25,
        });
    }

    let limit = if degree >= HIGH_DEGREE {
        ttl.clamp(2, 5)
    } else if degree <= 2 {
        ttl
    } else {
        let roomy =
            kind == t::ANNOUNCE || (kind == t::BOARD_POST && urgent_board_post(&packet.payload));
        ttl.clamp(2, if roomy { 7 } else { 6 })
    };
    let delay_ms = match degree {
        0..=2 => 10..=40,
        3..=5 => 60..=150,
        6..=9 => 80..=180,
        _ => 100..=220,
    };
    Some(RelayDecision {
        new_ttl: limit - 1,
        delay_ms,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutePlan {
    /// No usable route: flood-relay as usual.
    Flood,
    /// Don't relay at all.
    Suppress,
    /// Send to this neighbour only (TTL already lowered).
    Forward { packet: Packet, next_hop: PeerId },
}

/// Source-routed packets (route flag) go hop by hop when the next hop is
/// connected, and fall back to flooding otherwise.
pub fn route_plan(
    packet: &Packet,
    local: PeerId,
    is_connected: impl Fn(&PeerId) -> bool,
) -> RoutePlan {
    if packet.packet_type == t::REQUEST_SYNC || packet.recipient_id == Some(local) {
        return RoutePlan::Suppress;
    }
    if packet.route.is_empty() {
        return RoutePlan::Flood;
    }
    if packet.ttl <= 1 {
        return RoutePlan::Suppress;
    }
    let next = match packet.route.iter().position(|hop| *hop == local) {
        None => Some(packet.route[0]),
        Some(i) if i + 1 == packet.route.len() => packet.recipient_id,
        Some(i) => Some(packet.route[i + 1]),
    };
    match next {
        Some(hop) if is_connected(&hop) => {
            let mut forwarded = packet.clone();
            forwarded.ttl -= 1;
            RoutePlan::Forward {
                packet: forwarded,
                next_hop: hop,
            }
        }
        _ => RoutePlan::Flood,
    }
}

/// Relays waiting for their jitter delay. A duplicate seen in the meantime
/// cancels the relay when more than two peers are connected.
#[derive(Default)]
pub struct RelayQueue {
    pending: HashMap<PacketKey, (Packet, u64)>,
}

impl RelayQueue {
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Queues `packet` (with its TTL already set) to go out at `due_ms`.
    /// A copy already waiting keeps its place.
    pub fn schedule(&mut self, key: PacketKey, packet: Packet, due_ms: u64) {
        self.pending.entry(key).or_insert((packet, due_ms));
    }

    pub fn cancel(&mut self, key: &PacketKey) -> bool {
        self.pending.remove(key).is_some()
    }

    /// Packets whose delay has passed, oldest first.
    pub fn due(&mut self, now_ms: u64) -> Vec<Packet> {
        let keys: Vec<PacketKey> = self
            .pending
            .iter()
            .filter(|(_, (_, due))| *due <= now_ms)
            .map(|(k, _)| *k)
            .collect();
        let mut out: Vec<(u64, Packet)> = keys
            .iter()
            .filter_map(|k| self.pending.remove(k))
            .map(|(p, due)| (due, p))
            .collect();
        out.sort_by_key(|(due, _)| *due);
        out.into_iter().map(|(_, p)| p).collect()
    }

    /// Maintenance: a backlog this large is dropped wholesale.
    pub fn trim(&mut self) {
        if self.pending.len() > MAX_SCHEDULED {
            self.pending.clear();
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// Cancel a waiting relay on a duplicate only in denser graphs.
pub fn duplicate_cancels_relay(degree: usize) -> bool {
    degree > 2
}

/// Uniform pick from a jitter range.
pub fn pick_delay(range: &RangeInclusive<u32>) -> u32 {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        return *range.start();
    }
    let span = range.end() - range.start() + 1;
    range.start() + u32::from_le_bytes(b) % span
}
