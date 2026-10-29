//! Gossip sync (iOS `GossipSyncManager`, `RequestSyncManager`,
//! `SyncResponseRateLimiter`): keep recent broadcast packets, advertise
//! them to neighbours as a GCS filter, and send back what a neighbour's
//! filter is missing.
//!
//! Prekey bundles and board posts are not stored yet, so they are never
//! requested or served.

use crate::gcs::{self, RequestSync, SyncTypes};
use bitchat_proto::{Packet, PeerId, message_type as t};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub seen_capacity: usize,
    pub gcs_max_bytes: usize,
    pub gcs_target_fpr: f64,
    /// Fragments, files and announces.
    pub max_age_ms: u64,
    /// Whole public and group messages.
    pub public_message_max_age_ms: u64,
    pub stale_peer_cleanup_interval_ms: u64,
    pub stale_peer_timeout_ms: u64,
    pub fragment_capacity: usize,
    pub file_transfer_capacity: usize,
    pub group_message_capacity: usize,
    pub message_byte_budget: usize,
    pub fragment_byte_budget: usize,
    pub file_transfer_byte_budget: usize,
    pub group_message_byte_budget: usize,
    pub message_interval_ms: u64,
    pub fragment_interval_ms: u64,
    pub file_transfer_interval_ms: u64,
    pub max_responses: usize,
    pub response_window_ms: u64,
    pub request_window_ms: u64,
    pub max_future_skew_ms: u64,
}

impl Default for SyncConfig {
    /// iOS `TransportConfig` values used by `BLEService`.
    fn default() -> Self {
        SyncConfig {
            seen_capacity: 1000,
            gcs_max_bytes: 400,
            gcs_target_fpr: 0.01,
            max_age_ms: 900_000,
            public_message_max_age_ms: 6 * 3600 * 1000,
            stale_peer_cleanup_interval_ms: 60_000,
            stale_peer_timeout_ms: 60_000,
            fragment_capacity: 600,
            file_transfer_capacity: 200,
            group_message_capacity: 200,
            message_byte_budget: 8 * 1024 * 1024,
            fragment_byte_budget: 1024 * 1024,
            file_transfer_byte_budget: 32 * 1024 * 1024,
            group_message_byte_budget: 4 * 1024 * 1024,
            message_interval_ms: 15_000,
            fragment_interval_ms: 30_000,
            file_transfer_interval_ms: 60_000,
            max_responses: 8,
            response_window_ms: 30_000,
            request_window_ms: 30_000,
            max_future_skew_ms: crate::peers::MAX_TIMESTAMP_SKEW_MS,
        }
    }
}

/// Maintenance interval callers should use.
pub const MAINTENANCE_INTERVAL_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOut {
    /// A REQUEST_SYNC to sign and send (TTL 0); `to: None` = broadcast.
    Request { to: Option<PeerId>, packet: Packet },
    /// A stored packet for `to`, already set to TTL 0 with the RSR flag.
    Reply { to: PeerId, packet: Packet },
}

/// Insertion-ordered packets, oldest evicted first past a count or byte cap.
#[derive(Default)]
struct PacketStore {
    packets: HashMap<[u8; 16], Packet>,
    order: Vec<[u8; 16]>,
    bytes: usize,
}

impl PacketStore {
    fn insert(&mut self, packet: &Packet, capacity: usize, byte_budget: usize) {
        if capacity == 0 || packet.payload.len() > byte_budget {
            return;
        }
        let id = gcs::packet_id(packet);
        match self.packets.insert(id, packet.clone()) {
            Some(old) => self.bytes = self.bytes + packet.payload.len() - old.payload.len(),
            None => {
                self.order.push(id);
                self.bytes += packet.payload.len();
            }
        }
        while self.order.len() > capacity || self.bytes > byte_budget {
            let victim = self.order.remove(0);
            if let Some(evicted) = self.packets.remove(&victim) {
                self.bytes -= evicted.payload.len();
            }
        }
    }

    fn all(&self, fresh: impl Fn(&Packet) -> bool) -> Vec<&Packet> {
        self.order
            .iter()
            .filter_map(|id| self.packets.get(id))
            .filter(|p| fresh(p))
            .collect()
    }

    fn retain(&mut self, keep: impl Fn(&Packet) -> bool) {
        let packets = &mut self.packets;
        let bytes = &mut self.bytes;
        self.order.retain(|id| {
            let Some(p) = packets.get(id) else {
                return false;
            };
            if keep(p) {
                return true;
            }
            *bytes -= p.payload.len();
            packets.remove(id);
            false
        });
    }

    fn len(&self) -> usize {
        self.order.len()
    }
}

struct Schedule {
    types: SyncTypes,
    interval_ms: u64,
    last_sent_ms: Option<u64>,
}

/// Age windows bound the past; anything dated past the skew is never kept.
fn is_fresh(c: &SyncConfig, packet: &Packet, now_ms: u64) -> bool {
    let max_age = match packet.packet_type {
        t::MESSAGE | t::GROUP_MESSAGE => c.public_message_max_age_ms,
        _ => c.max_age_ms,
    };
    if packet.timestamp > now_ms && packet.timestamp - now_ms > c.max_future_skew_ms {
        return false;
    }
    now_ms < max_age || packet.timestamp >= now_ms - max_age
}

fn is_broadcast(packet: &Packet) -> bool {
    packet.recipient_id.is_none_or(|r| r == [0xff; 8])
}

pub struct GossipSync {
    local: PeerId,
    config: SyncConfig,
    messages: PacketStore,
    fragments: PacketStore,
    file_transfers: PacketStore,
    group_messages: PacketStore,
    announces: HashMap<PeerId, Packet>,
    schedules: Vec<Schedule>,
    /// When we last asked each peer (for RSR attribution).
    requests: HashMap<PeerId, u64>,
    responses: HashMap<PeerId, Vec<u64>>,
    last_stale_cleanup_ms: Option<u64>,
}

impl GossipSync {
    pub fn new(local: PeerId, config: SyncConfig) -> GossipSync {
        let mut schedules = Vec::new();
        if config.seen_capacity > 0 && config.message_interval_ms > 0 {
            let mut types = SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE]);
            if config.group_message_capacity > 0 {
                types = types.union(SyncTypes::of(&[t::GROUP_MESSAGE]));
            }
            schedules.push(Schedule {
                types,
                interval_ms: config.message_interval_ms,
                last_sent_ms: None,
            });
        }
        if config.fragment_capacity > 0 && config.fragment_interval_ms > 0 {
            schedules.push(Schedule {
                types: SyncTypes::of(&[t::FRAGMENT]),
                interval_ms: config.fragment_interval_ms,
                last_sent_ms: None,
            });
        }
        if config.file_transfer_capacity > 0 && config.file_transfer_interval_ms > 0 {
            schedules.push(Schedule {
                types: SyncTypes::of(&[t::FILE_TRANSFER]),
                interval_ms: config.file_transfer_interval_ms,
                last_sent_ms: None,
            });
        }
        GossipSync {
            local,
            config,
            messages: PacketStore::default(),
            fragments: PacketStore::default(),
            file_transfers: PacketStore::default(),
            group_messages: PacketStore::default(),
            announces: HashMap::new(),
            schedules,
            requests: HashMap::new(),
            responses: HashMap::new(),
            last_stale_cleanup_ms: None,
        }
    }

    fn fresh(&self, packet: &Packet, now_ms: u64) -> bool {
        is_fresh(&self.config, packet, now_ms)
    }

    fn announce_fresh(&self, packet: &Packet, now_ms: u64) -> bool {
        let timeout = self.config.stale_peer_timeout_ms;
        timeout == 0 || now_ms < timeout || packet.timestamp >= now_ms - timeout
    }

    /// Record a verified broadcast packet (ours or received) for sync.
    pub fn on_packet_seen(&mut self, packet: &Packet, now_ms: u64) {
        let c = &self.config;
        match packet.packet_type {
            t::ANNOUNCE => {
                if !self.fresh(packet, now_ms) {
                    return;
                }
                if !self.announce_fresh(packet, now_ms) {
                    self.remove_peer(&packet.sender_id);
                    return;
                }
                self.announces.insert(packet.sender_id, packet.clone());
            }
            t::MESSAGE | t::FRAGMENT | t::FILE_TRANSFER | t::GROUP_MESSAGE => {
                if !is_broadcast(packet) || !self.fresh(packet, now_ms) {
                    return;
                }
                let (store, cap, budget) = match packet.packet_type {
                    t::MESSAGE => (&mut self.messages, c.seen_capacity, c.message_byte_budget),
                    t::FRAGMENT => (
                        &mut self.fragments,
                        c.fragment_capacity,
                        c.fragment_byte_budget,
                    ),
                    t::FILE_TRANSFER => (
                        &mut self.file_transfers,
                        c.file_transfer_capacity,
                        c.file_transfer_byte_budget,
                    ),
                    _ => (
                        &mut self.group_messages,
                        c.group_message_capacity,
                        c.group_message_byte_budget,
                    ),
                };
                store.insert(packet, cap.max(1), budget);
            }
            _ => {}
        }
    }

    /// Types for the first sync with a newly connected peer.
    fn initial_types(&self) -> SyncTypes {
        let c = &self.config;
        let mut types = SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE]);
        if c.group_message_capacity > 0 {
            types = types.union(SyncTypes::of(&[t::GROUP_MESSAGE]));
        }
        if c.fragment_capacity > 0 && c.fragment_interval_ms > 0 {
            types = types.union(SyncTypes::of(&[t::FRAGMENT]));
        }
        if c.file_transfer_capacity > 0 && c.file_transfer_interval_ms > 0 {
            types = types.union(SyncTypes::of(&[t::FILE_TRANSFER]));
        }
        types
    }

    /// First request to a peer we just connected to (iOS sends it 1–5 s later).
    pub fn initial_sync(&mut self, peer: PeerId, now_ms: u64) -> SyncOut {
        let types = self.initial_types();
        self.request(Some(peer), types, None, now_ms)
    }

    /// Ask connected peers for specific stalled fragment streams.
    pub fn request_fragments(
        &mut self,
        ids: &[[u8; 8]],
        connected: &[PeerId],
        now_ms: u64,
    ) -> Vec<SyncOut> {
        let Some(filter) = gcs::encode_fragment_ids(ids) else {
            return Vec::new();
        };
        let types = SyncTypes::of(&[t::FRAGMENT]);
        connected
            .iter()
            .map(|peer| self.request(Some(*peer), types, Some(filter.clone()), now_ms))
            .collect()
    }

    fn request(
        &mut self,
        to: Option<PeerId>,
        types: SyncTypes,
        fragment_ids: Option<String>,
        now_ms: u64,
    ) -> SyncOut {
        if let Some(peer) = to {
            self.requests.insert(peer, now_ms);
        }
        let payload = self.build_request(types, fragment_ids, now_ms).encode();
        let mut packet = Packet::new(t::REQUEST_SYNC, self.local, now_ms, payload, 0);
        packet.recipient_id = to;
        SyncOut::Request { to, packet }
    }

    fn candidates(&self, types: SyncTypes, now_ms: u64) -> Vec<&Packet> {
        let fresh = |p: &Packet| self.fresh(p, now_ms);
        let mut out: Vec<&Packet> = Vec::new();
        if types.contains(t::ANNOUNCE) {
            out.extend(self.announces.values().filter(|p| fresh(p)));
        }
        if types.contains(t::MESSAGE) {
            out.extend(self.messages.all(fresh));
        }
        if types.contains(t::FRAGMENT) {
            out.extend(self.fragments.all(fresh));
        }
        if types.contains(t::FILE_TRANSFER) {
            out.extend(self.file_transfers.all(fresh));
        }
        if types.contains(t::GROUP_MESSAGE) {
            out.extend(self.group_messages.all(fresh));
        }
        out
    }

    fn build_request(
        &self,
        types: SyncTypes,
        fragment_ids: Option<String>,
        now_ms: u64,
    ) -> RequestSync {
        let c = &self.config;
        let p = gcs::derive_p(c.gcs_target_fpr);
        let empty = RequestSync {
            p,
            m: 1,
            data: Vec::new(),
            types: Some(types),
            since: None,
            fragment_ids: fragment_ids.clone(),
        };
        let mut candidates = self.candidates(types, now_ms);
        if candidates.is_empty() {
            return empty;
        }
        candidates.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        let cap = if types == SyncTypes::of(&[t::FRAGMENT]) {
            c.fragment_capacity
        } else if types == SyncTypes::of(&[t::FILE_TRANSFER]) {
            c.file_transfer_capacity
        } else {
            c.seen_capacity
        }
        .max(1);
        let take = candidates
            .len()
            .min(gcs::estimate_max_elements(c.gcs_max_bytes, p))
            .min(cap);
        let included = &candidates[..take];
        let ids: Vec<[u8; 16]> = included.iter().map(|p| gcs::packet_id(p)).collect();
        let filter = gcs::build_filter(&ids, c.gcs_max_bytes, c.gcs_target_fpr);
        // If the filter can't cover everything, say how far back it reaches.
        let covered = filter.included;
        let since =
            (covered > 0 && covered < candidates.len()).then(|| included[covered - 1].timestamp);
        RequestSync {
            p: filter.p,
            m: filter.m,
            data: filter.data,
            types: Some(types),
            since,
            fragment_ids,
        }
    }

    /// Answer a neighbour's REQUEST_SYNC. The caller has checked that it has
    /// TTL 0 and a valid signature from the sender's announced key.
    pub fn handle_request(
        &mut self,
        from: PeerId,
        request: &RequestSync,
        now_ms: u64,
    ) -> Vec<SyncOut> {
        let window = self.config.response_window_ms;
        let history = self.responses.entry(from).or_default();
        history.retain(|&at| at + window >= now_ms);
        if history.len() >= self.config.max_responses.max(1) {
            return Vec::new();
        }
        history.push(now_ms);

        let types = request
            .types
            .unwrap_or(SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE]));
        let values = gcs::decode(request.p, request.m, &request.data);
        let missing = |p: &Packet| {
            values
                .binary_search(&gcs::bucket(&gcs::packet_id(p), request.m))
                .is_err()
        };
        let after_since = |p: &Packet| request.since.is_none_or(|s| p.timestamp >= s);
        let fresh = |p: &Packet| self.fresh(p, now_ms);
        let fragment_ids = request
            .fragment_ids
            .as_deref()
            .and_then(gcs::decode_fragment_ids);

        let mut out: Vec<&Packet> = Vec::new();
        // Announces ignore the cursor: they carry the keys to verify the rest.
        if types.contains(t::ANNOUNCE) {
            out.extend(self.announces.values().filter(|p| fresh(p) && missing(p)));
        }
        if types.contains(t::MESSAGE) {
            out.extend(
                self.messages
                    .all(fresh)
                    .into_iter()
                    .filter(|p| after_since(p) && missing(p)),
            );
        }
        if types.contains(t::FRAGMENT) {
            out.extend(self.fragments.all(fresh).into_iter().filter(|p| {
                let wanted = match &fragment_ids {
                    Some(ids) => {
                        p.payload.len() >= 8 && ids.iter().any(|id| p.payload[..8] == id[..])
                    }
                    None => after_since(p),
                };
                wanted && missing(p)
            }));
        }
        if types.contains(t::FILE_TRANSFER) {
            out.extend(
                self.file_transfers
                    .all(fresh)
                    .into_iter()
                    .filter(|p| after_since(p) && missing(p)),
            );
        }
        if types.contains(t::GROUP_MESSAGE) {
            out.extend(
                self.group_messages
                    .all(fresh)
                    .into_iter()
                    .filter(|p| after_since(p) && missing(p)),
            );
        }
        out.into_iter()
            .map(|p| {
                let mut packet = p.clone();
                packet.ttl = 0;
                packet.is_rsr = true;
                SyncOut::Reply { to: from, packet }
            })
            .collect()
    }

    /// An RSR packet is only accepted from a peer we asked in the last 30 s.
    pub fn is_valid_response(&self, from: &PeerId, now_ms: u64) -> bool {
        self.requests
            .get(from)
            .is_some_and(|&at| now_ms.saturating_sub(at) <= self.config.request_window_ms)
    }

    /// Run every [`MAINTENANCE_INTERVAL_MS`]: expiry and the periodic requests
    /// (unicast to each connected peer, or broadcast when alone).
    pub fn maintenance(&mut self, connected: &[PeerId], now_ms: u64) -> Vec<SyncOut> {
        self.cleanup(now_ms);
        let mut due = Vec::new();
        for s in self.schedules.iter_mut() {
            if s.last_sent_ms
                .is_none_or(|at| now_ms.saturating_sub(at) >= s.interval_ms)
            {
                s.last_sent_ms = Some(now_ms);
                due.push(s.types);
            }
        }
        let mut out = Vec::new();
        for types in due {
            if connected.is_empty() {
                out.push(self.request(None, types, None, now_ms));
            } else {
                for peer in connected {
                    out.push(self.request(Some(*peer), types, None, now_ms));
                }
            }
        }
        out
    }

    fn cleanup(&mut self, now_ms: u64) {
        let config = self.config.clone();
        let fresh = |p: &Packet| is_fresh(&config, p, now_ms);
        self.announces.retain(|_, p| fresh(p));
        self.messages.retain(fresh);
        self.fragments.retain(fresh);
        self.file_transfers.retain(fresh);
        self.group_messages.retain(fresh);

        if self
            .last_stale_cleanup_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= config.stale_peer_cleanup_interval_ms)
        {
            self.last_stale_cleanup_ms = Some(now_ms);
            if now_ms >= config.stale_peer_timeout_ms {
                let cutoff = now_ms - config.stale_peer_timeout_ms;
                let stale: Vec<PeerId> = self
                    .announces
                    .iter()
                    .filter(|(_, p)| p.timestamp < cutoff)
                    .map(|(id, _)| *id)
                    .collect();
                for peer in stale {
                    self.remove_peer(&peer);
                }
            }
        }
        let window = config.request_window_ms;
        self.requests
            .retain(|_, at| now_ms.saturating_sub(*at) <= window);
        let window = config.response_window_ms;
        self.responses.retain(|_, history| {
            history.retain(|&at| at + window >= now_ms);
            !history.is_empty()
        });
    }

    /// A peer left or went stale: forget its announce and broadcasts.
    pub fn remove_peer(&mut self, peer: &PeerId) {
        self.announces.remove(peer);
        let from_peer = |p: &Packet| p.sender_id != *peer;
        self.messages.retain(from_peer);
        self.fragments.retain(from_peer);
        self.file_transfers.retain(from_peer);
        self.group_messages.retain(from_peer);
    }

    /// Blocked sender: stop carrying their public messages.
    pub fn remove_public_messages(&mut self, from: &PeerId) {
        self.messages.retain(|p| p.sender_id != *from);
    }

    pub fn remove_all_public_messages(&mut self) {
        self.messages.retain(|_| false);
    }

    /// Carried public messages, oldest first (for saving across restarts).
    pub fn public_messages(&self, now_ms: u64) -> Vec<Packet> {
        self.messages
            .all(|p| self.fresh(p, now_ms))
            .into_iter()
            .cloned()
            .collect()
    }

    /// Restore saved public messages (oldest first), newest kept on overflow.
    pub fn restore_public_messages(&mut self, packets: Vec<Packet>, now_ms: u64) {
        let capacity = self.config.seen_capacity.max(1);
        let mut kept: Vec<Packet> = Vec::new();
        let mut bytes = 0;
        for p in packets.into_iter().rev() {
            if kept.len() >= capacity {
                break;
            }
            if p.packet_type != t::MESSAGE || !self.fresh(&p, now_ms) {
                continue;
            }
            if bytes + p.payload.len() > self.config.message_byte_budget {
                break;
            }
            bytes += p.payload.len();
            kept.push(p);
        }
        for p in kept.iter().rev() {
            self.messages
                .insert(p, capacity, self.config.message_byte_budget);
        }
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}
