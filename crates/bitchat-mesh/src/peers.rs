//! Peer table: which announces to trust and which public messages to accept
//! (iOS `BLEAnnouncePreflightPolicy`, `BLEAnnounceTrustPolicy`,
//! `BLEPublicMessagePolicy`, `BLEIngressPacketGuard`).

use crate::announce::Announce;
use crate::identity::{DEFAULT_TTL, verify};
use bitchat_proto::{Packet, PeerId, message_type as t};
use std::collections::HashMap;

/// Announces older than this are ignored.
pub const ANNOUNCE_MAX_AGE_MS: u64 = 900_000;
/// Public messages are accepted as old as gossip sync serves them (6 h).
pub const PUBLIC_MESSAGE_MAX_AGE_MS: u64 = 6 * 3600 * 1000;
/// Packets (other than sync replies) whose clock is further off are dropped.
pub const MAX_TIMESTAMP_SKEW_MS: u64 = 120_000;
/// A verified peer stays reachable this long after its last announce.
pub const REACHABLE_MS: u64 = 60_000;
/// Delivered public text is capped at a v1 frame.
pub const MAX_PUBLIC_PAYLOAD: usize = 65_535;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub peer_id: PeerId,
    pub nickname: String,
    pub noise_public: [u8; 32],
    pub signing_public: [u8; 32],
    pub capabilities: u64,
    pub neighbors: Vec<PeerId>,
    /// Last verified announce (unix ms, local clock).
    pub last_seen_ms: u64,
    /// The last announce arrived with TTL 7: a direct radio neighbour.
    pub direct: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Malformed,
    SenderMismatch,
    SelfAnnounce,
    Stale,
    ClockSkew,
    KeyMismatch,
    SigningKeyMismatch,
    MissingSignature,
    InvalidSignature,
    UnknownSender,
    NotText,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnnounceOutcome {
    /// First verified announce from this peer (or after it timed out).
    New(Peer),
    Updated(Peer),
}

/// `true` when the packet's clock is acceptable. Sync replies (RSR) may be
/// old but not more than the skew into the future.
pub fn timestamp_ok(packet: &Packet, now_ms: u64) -> bool {
    if packet.is_rsr {
        return packet.timestamp <= now_ms + MAX_TIMESTAMP_SKEW_MS;
    }
    packet.timestamp.abs_diff(now_ms) <= MAX_TIMESTAMP_SKEW_MS
}

fn stale(timestamp: u64, now_ms: u64, max_age: u64) -> bool {
    now_ms >= max_age && timestamp < now_ms - max_age
}

pub struct PeerTable {
    local: PeerId,
    peers: HashMap<PeerId, Peer>,
    /// Trust on first use: keys seen for a peer ID, kept after it goes away.
    pins: HashMap<PeerId, ([u8; 32], [u8; 32])>,
}

impl PeerTable {
    pub fn new(local: PeerId) -> PeerTable {
        PeerTable {
            local,
            peers: HashMap::new(),
            pins: HashMap::new(),
        }
    }

    pub fn get(&self, peer: &PeerId) -> Option<&Peer> {
        self.peers.get(peer)
    }

    pub fn peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.values()
    }

    pub fn on_announce(
        &mut self,
        packet: &Packet,
        now_ms: u64,
    ) -> Result<AnnounceOutcome, Rejection> {
        if packet.packet_type != t::ANNOUNCE {
            return Err(Rejection::Malformed);
        }
        let a = Announce::decode(&packet.payload).ok_or(Rejection::Malformed)?;
        let noise: [u8; 32] = a
            .noise_public
            .as_slice()
            .try_into()
            .map_err(|_| Rejection::Malformed)?;
        let signing: [u8; 32] = a
            .signing_public
            .as_slice()
            .try_into()
            .map_err(|_| Rejection::Malformed)?;
        if bitchat_noise::peer_id(&noise) != packet.sender_id {
            return Err(Rejection::SenderMismatch);
        }
        if packet.sender_id == self.local {
            return Err(Rejection::SelfAnnounce);
        }
        if stale(packet.timestamp, now_ms, ANNOUNCE_MAX_AGE_MS) {
            return Err(Rejection::Stale);
        }
        if !timestamp_ok(packet, now_ms) {
            return Err(Rejection::ClockSkew);
        }
        if let Some((pinned_noise, pinned_signing)) = self.pins.get(&packet.sender_id) {
            if *pinned_noise != noise {
                return Err(Rejection::KeyMismatch);
            }
            if *pinned_signing != signing {
                return Err(Rejection::SigningKeyMismatch);
            }
        }
        if packet.signature.is_none() {
            return Err(Rejection::MissingSignature);
        }
        if !verify(packet, &signing) {
            return Err(Rejection::InvalidSignature);
        }

        self.pins.insert(packet.sender_id, (noise, signing));
        let peer = Peer {
            peer_id: packet.sender_id,
            nickname: a.nickname,
            noise_public: noise,
            signing_public: signing,
            capabilities: a.capabilities.unwrap_or(0),
            neighbors: a.neighbors.unwrap_or_default(),
            last_seen_ms: now_ms,
            direct: packet.ttl == DEFAULT_TTL,
        };
        let fresh = self
            .peers
            .get(&packet.sender_id)
            .is_none_or(|old| now_ms.saturating_sub(old.last_seen_ms) > REACHABLE_MS);
        self.peers.insert(packet.sender_id, peer.clone());
        Ok(if fresh {
            AnnounceOutcome::New(peer)
        } else {
            AnnounceOutcome::Updated(peer)
        })
    }

    /// A public message must be signed by the sender's announced key.
    /// Returns the text.
    pub fn accept_public_message(&self, packet: &Packet, now_ms: u64) -> Result<String, Rejection> {
        let broadcast = packet.recipient_id.is_none_or(|r| r == [0xff; 8]);
        if broadcast && stale(packet.timestamp, now_ms, PUBLIC_MESSAGE_MAX_AGE_MS) {
            return Err(Rejection::Stale);
        }
        if !timestamp_ok(packet, now_ms) {
            return Err(Rejection::ClockSkew);
        }
        let key = self
            .peers
            .get(&packet.sender_id)
            .map(|p| p.signing_public)
            .or_else(|| self.pins.get(&packet.sender_id).map(|(_, s)| *s))
            .ok_or(Rejection::UnknownSender)?;
        if !verify(packet, &key) {
            return Err(Rejection::InvalidSignature);
        }
        if packet.payload.len() > MAX_PUBLIC_PAYLOAD {
            return Err(Rejection::Malformed);
        }
        String::from_utf8(packet.payload.clone()).map_err(|_| Rejection::NotText)
    }

    /// A signed leave removes the peer.
    pub fn on_leave(&mut self, packet: &Packet) -> Option<Peer> {
        let key = self.peers.get(&packet.sender_id)?.signing_public;
        if !verify(packet, &key) {
            return None;
        }
        self.peers.remove(&packet.sender_id)
    }

    /// Drops peers not heard from within the reachability window.
    pub fn expire(&mut self, now_ms: u64) -> Vec<Peer> {
        let gone: Vec<PeerId> = self
            .peers
            .values()
            .filter(|p| now_ms.saturating_sub(p.last_seen_ms) > REACHABLE_MS)
            .map(|p| p.peer_id)
            .collect();
        gone.iter().filter_map(|id| self.peers.remove(id)).collect()
    }

    /// Up to 10 direct neighbours to list in our own announce.
    pub fn direct_neighbors(&self) -> Vec<PeerId> {
        let mut ids: Vec<PeerId> = self
            .peers
            .values()
            .filter(|p| p.direct)
            .map(|p| p.peer_id)
            .collect();
        ids.sort();
        ids.truncate(crate::announce::MAX_NEIGHBORS);
        ids
    }
}
