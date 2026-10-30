//! A mesh node with the radio abstracted away: frames arrive on links and
//! frames leave on links. It runs the iOS `BLEService` receive pipeline
//! (ingress checks, duplicate filter, per-type handling, source routing,
//! jittered relays) and its link selection. The BLE transport and the
//! loopback simulator both drive it; time is passed in, nothing blocks.

use crate::fragment::{self, Added, FragmentHeader, Reassembler};
use crate::gcs::RequestSync;
use crate::identity::{DEFAULT_TTL, MeshIdentity, verify};
use crate::peer_state::PeerState;
use crate::peers::{AnnounceOutcome, Peer, PeerTable, timestamp_ok};
use crate::relay::{self, Deduplicator, PacketKey, RelayQueue, RoutePlan};
use crate::sessions::{Event as SessionEvent, SessionManager};
use crate::sync::{GossipSync, MAINTENANCE_INTERVAL_MS, SyncConfig, SyncOut};
use bitchat_proto::{Packet, PeerId, message_type as t};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Identifies one radio link (a connection to one neighbour).
pub type LinkId = u64;

const BROADCAST: PeerId = [0xff; 8];
/// Periodic announce while connected (sparse graphs), plus up to 4 s jitter.
pub const ANNOUNCE_BASE_MS: u64 = 15_000;
pub const ANNOUNCE_JITTER_MS: u64 = 4_000;
const ANNOUNCE_MIN_MS: u64 = 1_000;
const FORCED_ANNOUNCE_MIN_MS: u64 = 150;
/// We answer a peer's announce with our own at most this often.
pub const ANNOUNCE_BACK_INTERVAL_MS: u64 = 300_000;
/// First sync request goes out this long after a new direct neighbour.
pub const INITIAL_SYNC_DELAY_MS: u64 = 1_000;
/// Fragment frame overhead: v1 header, sender, recipient, fragment header.
const FRAGMENT_FRAME_OVERHEAD: usize = 14 + 8 + 8 + 13;

/// Chunk size so a fragment frame fits a link's write limit.
pub fn fragment_chunk_size(link_limit: usize) -> usize {
    link_limit
        .saturating_sub(FRAGMENT_FRAME_OVERHEAD)
        .max(fragment::MIN_CHUNK)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeEvent {
    /// A verified peer heard directly (new, or back after going quiet).
    PeerConnected(Peer),
    /// A verified peer heard through other nodes.
    PeerSeen(Peer),
    PeerLeft(PeerId),
    PublicMessage {
        sender: PeerId,
        nickname: String,
        text: String,
        timestamp: u64,
    },
    /// A decrypted Noise payload (type byte first).
    Private {
        from: PeerId,
        plaintext: Vec<u8>,
    },
    PeerState {
        peer: PeerId,
        state: PeerState,
    },
    SessionUp(PeerId),
    /// No session could be set up; these payloads were not delivered.
    Unsent {
        peer: PeerId,
        payloads: Vec<Vec<u8>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Write {
        link: LinkId,
        frame: Vec<u8>,
    },
    /// Disconnect this link: the same peer is on a newer one.
    Close {
        link: LinkId,
    },
    Event(NodeEvent),
}

/// Who opened a link. A phone is often linked both ways at once (each side
/// is central and peripheral); that is normal, not a duplicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRole {
    /// We connected to them (we are the central).
    Outgoing,
    /// They connected to us (we are the peripheral).
    Incoming,
}

struct Link {
    max_write: usize,
    role: LinkRole,
    /// Bound by a verified direct announce received on this link.
    peer: Option<PeerId>,
}

pub struct MeshNode {
    identity: MeshIdentity,
    nickname: String,
    peers: PeerTable,
    sessions: SessionManager,
    dedup: Deduplicator,
    relays: RelayQueue,
    relay_ingress: HashMap<PacketKey, LinkId>,
    sync: GossipSync,
    fragments: Reassembler,
    links: BTreeMap<LinkId, Link>,
    initial_syncs: Vec<(PeerId, u64)>,
    announced_back: HashMap<PeerId, u64>,
    next_announce_ms: u64,
    last_announce_ms: Option<u64>,
    next_maintenance_ms: u64,
    out: Vec<Output>,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    let _ = getrandom::fill(&mut b);
    b
}

/// iOS `BLEFanoutSelector.subsetSize`: about log2(n) + 1 links.
fn subset_size(count: usize) -> usize {
    if count <= 2 {
        return count;
    }
    let bits = usize::BITS - (count - 1).leading_zeros();
    count.min((bits as usize + 1).max(1))
}

fn message_id(key: &PacketKey) -> String {
    format!(
        "{}-{}-{}-{}",
        hex::encode(key.sender),
        key.timestamp,
        key.packet_type,
        hex::encode(key.digest)
    )
}

impl MeshNode {
    pub fn new(identity: MeshIdentity, nickname: &str, now_ms: u64) -> MeshNode {
        let local = identity.peer_id();
        let state = PeerState {
            capabilities: 0,
            signing_public: identity.signing_public(),
        };
        MeshNode {
            nickname: nickname.to_string(),
            peers: PeerTable::new(local),
            sessions: SessionManager::new(*identity.noise_secret(), &state),
            dedup: Deduplicator::default(),
            relays: RelayQueue::default(),
            relay_ingress: HashMap::new(),
            sync: GossipSync::new(local, SyncConfig::default()),
            fragments: Reassembler::default(),
            links: BTreeMap::new(),
            initial_syncs: Vec::new(),
            announced_back: HashMap::new(),
            next_announce_ms: now_ms,
            last_announce_ms: None,
            next_maintenance_ms: now_ms + MAINTENANCE_INTERVAL_MS,
            out: Vec::new(),
            identity,
        }
    }

    pub fn peer_id(&self) -> PeerId {
        self.identity.peer_id()
    }

    pub fn peer(&self, id: &PeerId) -> Option<&Peer> {
        self.peers.get(id)
    }

    pub fn peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.peers()
    }

    pub fn has_session(&self, peer: &PeerId) -> bool {
        self.sessions.is_established(peer)
    }

    /// Peers we have a bound link to.
    pub fn connected_peers(&self) -> Vec<PeerId> {
        let mut ids: Vec<PeerId> = self.links.values().filter_map(|l| l.peer).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    fn take(&mut self) -> Vec<Output> {
        std::mem::take(&mut self.out)
    }

    fn emit(&mut self, event: NodeEvent) {
        self.out.push(Output::Event(event));
    }

    // --- inputs ---

    /// A new connection we opened; `max_write` is the largest frame it takes.
    pub fn link_up(&mut self, link: LinkId, max_write: usize, now_ms: u64) -> Vec<Output> {
        self.link_up_as(link, max_write, LinkRole::Outgoing, now_ms)
    }

    pub fn link_up_as(
        &mut self,
        link: LinkId,
        max_write: usize,
        role: LinkRole,
        now_ms: u64,
    ) -> Vec<Output> {
        self.links.insert(
            link,
            Link {
                max_write,
                role,
                peer: None,
            },
        );
        self.announce(now_ms, true);
        self.take()
    }

    pub fn link_down(&mut self, link: LinkId) -> Vec<Output> {
        self.links.remove(&link);
        self.take()
    }

    pub fn on_frame(&mut self, link: LinkId, frame: &[u8], now_ms: u64) -> Vec<Output> {
        if self.links.contains_key(&link)
            && let Some(packet) = Packet::decode(frame)
        {
            self.receive(link, packet, now_ms);
        }
        self.take()
    }

    pub fn set_nickname(&mut self, nickname: &str, now_ms: u64) -> Vec<Output> {
        self.nickname = nickname.to_string();
        self.announce(now_ms, true);
        self.take()
    }

    pub fn send_public(&mut self, text: &str, now_ms: u64) -> Vec<Output> {
        if let Some(packet) = self.identity.public_message(text, now_ms) {
            self.dedup.mark(PacketKey::of(&packet), now_ms);
            self.sync.on_packet_seen(&packet, now_ms);
            self.transmit(packet, None, None);
        }
        self.take()
    }

    /// Sends a Noise payload (type byte first), setting up a session first if needed.
    pub fn send_private(&mut self, to: PeerId, payload: Vec<u8>, now_ms: u64) -> Vec<Output> {
        let events = self.sessions.send(to, payload, now_ms);
        self.on_session_events(events, now_ms);
        self.take()
    }

    pub fn leave(&mut self, now_ms: u64) -> Vec<Output> {
        if let Some(packet) = self.identity.leave(now_ms) {
            self.dedup.mark(PacketKey::of(&packet), now_ms);
            self.transmit(packet, None, None);
        }
        self.take()
    }

    /// Timers: relays, handshakes, sync, announces, expiry. Call every ~50 ms.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Output> {
        for packet in self.relays.due(now_ms) {
            let ingress = self.relay_ingress.remove(&PacketKey::of(&packet));
            self.transmit(packet, ingress, None);
        }
        let events = self.sessions.tick(now_ms);
        self.on_session_events(events, now_ms);
        // A neighbour with a live link stays, announce or not (iOS
        // announces rarely on an idle link).
        for peer in self.connected_peers() {
            self.peers.touch(&peer, now_ms);
        }
        for peer in self.peers.expire(now_ms) {
            self.emit(NodeEvent::PeerLeft(peer.peer_id));
        }
        self.fragments.expire(now_ms);

        let (due, later): (Vec<_>, Vec<_>) = self
            .initial_syncs
            .drain(..)
            .partition(|(_, at)| *at <= now_ms);
        self.initial_syncs = later;
        for (peer, _) in due {
            let out = self.sync.initial_sync(peer, now_ms);
            self.send_sync(out);
        }

        if now_ms >= self.next_maintenance_ms {
            self.next_maintenance_ms = now_ms + MAINTENANCE_INTERVAL_MS;
            let connected = self.connected_peers();
            for out in self.sync.maintenance(&connected, now_ms) {
                self.send_sync(out);
            }
            self.dedup.cleanup(now_ms);
            self.relays.trim();
            if self.relays.is_empty() {
                self.relay_ingress.clear();
            }
        }
        if !self.links.is_empty() && now_ms >= self.next_announce_ms {
            self.announce(now_ms, false);
        }
        self.take()
    }

    // --- receive pipeline ---

    fn receive(&mut self, link: LinkId, packet: Packet, now_ms: u64) {
        let local = self.peer_id();
        if packet.sender_id == local {
            return; // our own packet, echoed back
        }
        // A peer sends its announce down every link, so copies on other
        // links are duplicates; they still show which peer a link reaches.
        if packet.packet_type == t::ANNOUNCE && packet.ttl == DEFAULT_TTL {
            self.bind_known(link, &packet);
        }
        let bound = self.links.get(&link).and_then(|l| l.peer);
        if let Some(neighbour) = bound {
            self.peers.touch(&neighbour, now_ms);
        }
        // Sync requests are link-local: the sender must be the link's peer.
        if packet.packet_type == t::REQUEST_SYNC && bound.is_some_and(|b| b != packet.sender_id) {
            return;
        }
        // Sync replies only from a neighbour we asked.
        if packet.is_rsr
            && !self
                .sync
                .is_valid_response(&bound.unwrap_or(packet.sender_id), now_ms)
        {
            return;
        }
        if !timestamp_ok(&packet, now_ms) {
            return;
        }

        let key = PacketKey::of(&packet);
        if relay::should_deduplicate(&packet, local) && self.dedup.is_duplicate(key, now_ms) {
            if relay::duplicate_cancels_relay(self.links.len()) && self.relays.cancel(&key) {
                self.relay_ingress.remove(&key);
            }
            return;
        }

        let relay_ok = match packet.packet_type {
            t::ANNOUNCE => {
                self.handle_announce(link, &packet, now_ms);
                true
            }
            t::MESSAGE => {
                self.handle_message(&packet, now_ms);
                true
            }
            t::LEAVE => self.handle_leave(&packet),
            t::NOISE_HANDSHAKE => {
                if packet.recipient_id == Some(local) {
                    let events =
                        self.sessions
                            .on_handshake(packet.sender_id, &packet.payload, now_ms);
                    self.on_session_events(events, now_ms);
                }
                true
            }
            t::NOISE_ENCRYPTED => {
                if packet.recipient_id == Some(local) {
                    let events =
                        self.sessions
                            .on_encrypted(packet.sender_id, &packet.payload, now_ms);
                    self.on_session_events(events, now_ms);
                }
                true
            }
            t::FRAGMENT => {
                self.handle_fragment(link, &packet, now_ms);
                true
            }
            t::REQUEST_SYNC => {
                self.handle_request_sync(&packet, now_ms);
                false
            }
            // Broadcast media that fails sender checks must not spread.
            t::FILE_TRANSFER | t::BOARD_POST | t::VOICE_FRAME => {
                let directed = packet.recipient_id.is_some_and(|r| r != BROADCAST);
                directed
                    || self
                        .peers
                        .get(&packet.sender_id)
                        .is_some_and(|p| verify(&packet, &p.signing_public))
            }
            _ => true,
        };
        if !relay_ok {
            return;
        }

        let links = &self.links;
        match relay::route_plan(&packet, local, |peer| {
            links.values().any(|l| l.peer == Some(*peer))
        }) {
            RoutePlan::Suppress => return,
            RoutePlan::Forward { packet, next_hop } => {
                self.transmit(packet, None, Some(next_hop));
                return;
            }
            RoutePlan::Flood => {}
        }
        if let Some(decision) = relay::decide(&packet, local, self.links.len()) {
            let mut relayed = packet;
            relayed.ttl = decision.new_ttl;
            let delay = relay::pick_delay(&decision.delay_ms) as u64;
            self.relays.schedule(key, relayed, now_ms + delay);
            self.relay_ingress.entry(key).or_insert(link);
        }
    }

    /// Binds a link to an already known peer whose direct announce verifies.
    fn bind_known(&mut self, link: LinkId, packet: &Packet) {
        let sender = packet.sender_id;
        if self.links.get(&link).is_none_or(|l| l.peer == Some(sender)) {
            return;
        }
        let Some(key) = self.peers.get(&sender).map(|p| p.signing_public) else {
            return;
        };
        if verify(packet, &key) {
            self.bind(link, sender);
        }
    }

    /// iOS `BLERedundantLinkPolicy`: a phone that changed its address can sit
    /// on several of our outgoing links at once. Keep the newest, close the
    /// rest. Links the phone opened to us are its to manage.
    fn bind(&mut self, link: LinkId, peer: PeerId) {
        let Some(l) = self.links.get_mut(&link) else {
            return;
        };
        l.peer = Some(peer);
        if l.role != LinkRole::Outgoing {
            return;
        }
        let same: Vec<LinkId> = self
            .links
            .iter()
            .filter(|(_, l)| l.peer == Some(peer) && l.role == LinkRole::Outgoing)
            .map(|(id, _)| *id)
            .collect();
        let newest = same.iter().copied().max().unwrap_or(link);
        for old in same.into_iter().filter(|id| *id != newest) {
            self.links.remove(&old);
            self.out.push(Output::Close { link: old });
        }
    }

    fn handle_announce(&mut self, link: LinkId, packet: &Packet, now_ms: u64) {
        let Ok(outcome) = self.peers.on_announce(packet, now_ms) else {
            return;
        };
        let (peer, fresh) = match outcome {
            AnnounceOutcome::New(p) => (p, true),
            AnnounceOutcome::Updated(p) => (p, false),
        };
        let direct = packet.ttl == DEFAULT_TTL;
        if direct {
            self.bind(link, peer.peer_id);
        }
        self.sync.on_packet_seen(packet, now_ms);
        if fresh {
            if direct {
                self.initial_syncs
                    .push((peer.peer_id, now_ms + INITIAL_SYNC_DELAY_MS));
                self.emit(NodeEvent::PeerConnected(peer.clone()));
            } else {
                self.emit(NodeEvent::PeerSeen(peer.clone()));
            }
        }
        let answer = self
            .announced_back
            .get(&peer.peer_id)
            .is_none_or(|at| now_ms.saturating_sub(*at) >= ANNOUNCE_BACK_INTERVAL_MS);
        if answer {
            self.announced_back.insert(peer.peer_id, now_ms);
            self.announce(now_ms, true);
        }
    }

    fn handle_message(&mut self, packet: &Packet, now_ms: u64) {
        if packet.recipient_id.is_some_and(|r| r != BROADCAST) {
            return;
        }
        let Ok(text) = self.peers.accept_public_message(packet, now_ms) else {
            return;
        };
        self.sync.on_packet_seen(packet, now_ms);
        let nickname = self
            .peers
            .get(&packet.sender_id)
            .map(|p| p.nickname.clone())
            .unwrap_or_default();
        self.emit(NodeEvent::PublicMessage {
            sender: packet.sender_id,
            nickname,
            text,
            timestamp: packet.timestamp,
        });
    }

    /// Only a leave signed by the peer counts (and spreads).
    fn handle_leave(&mut self, packet: &Packet) -> bool {
        let Some(peer) = self.peers.on_leave(packet) else {
            return false;
        };
        self.sessions.remove(&peer.peer_id);
        self.sync.remove_peer(&peer.peer_id);
        self.emit(NodeEvent::PeerLeft(peer.peer_id));
        true
    }

    fn handle_fragment(&mut self, link: LinkId, packet: &Packet, now_ms: u64) {
        let Some(header) = FragmentHeader::parse(packet) else {
            return;
        };
        let broadcast = packet.recipient_id.is_none_or(|r| r == BROADCAST);
        if broadcast {
            self.sync.on_packet_seen(packet, now_ms);
        }
        if !broadcast && packet.recipient_id != Some(self.peer_id()) {
            return; // someone else's directed transfer: relay only
        }
        if let Added::Complete(inner) = self.fragments.add(header, now_ms) {
            // The whole packet is processed here but never relayed.
            let mut inner = *inner;
            inner.ttl = 0;
            self.receive(link, inner, now_ms);
        }
    }

    fn handle_request_sync(&mut self, packet: &Packet, now_ms: u64) {
        let local = self.peer_id();
        if packet.ttl != 0
            || packet
                .recipient_id
                .is_some_and(|r| r != local && r != BROADCAST)
        {
            return;
        }
        // A reply can replay the whole store: the request must be signed.
        let Some(key) = self.peers.get(&packet.sender_id).map(|p| p.signing_public) else {
            return;
        };
        if !verify(packet, &key) {
            return;
        }
        let Some(request) = RequestSync::decode(&packet.payload) else {
            return;
        };
        for out in self.sync.handle_request(packet.sender_id, &request, now_ms) {
            self.send_sync(out);
        }
    }

    // --- sending ---

    fn announce(&mut self, now_ms: u64, forced: bool) {
        if let Some(last) = self.last_announce_ms {
            let min = if forced {
                FORCED_ANNOUNCE_MIN_MS
            } else {
                ANNOUNCE_MIN_MS
            };
            if now_ms.saturating_sub(last) < min {
                // Throttled: send it as soon as allowed instead.
                self.next_announce_ms = self.next_announce_ms.min(last + min);
                return;
            }
        }
        let neighbors = self.peers.direct_neighbors();
        let Some(packet) = self
            .identity
            .announce(&self.nickname, &neighbors, 0, now_ms)
        else {
            return;
        };
        let jitter = u64::from(u32::from_le_bytes(random_bytes())) % (ANNOUNCE_JITTER_MS + 1);
        self.last_announce_ms = Some(now_ms);
        self.next_announce_ms = now_ms + ANNOUNCE_BASE_MS + jitter;
        self.dedup.mark(PacketKey::of(&packet), now_ms);
        self.sync.on_packet_seen(&packet, now_ms);
        self.transmit(packet, None, None);
    }

    fn send_sync(&mut self, out: SyncOut) {
        match out {
            SyncOut::Request { to, mut packet } => {
                if self.identity.sign(&mut packet) {
                    self.transmit(packet, None, to);
                }
            }
            SyncOut::Reply { to, packet } => self.transmit(packet, None, Some(to)),
        }
    }

    fn on_session_events(&mut self, events: Vec<SessionEvent>, now_ms: u64) {
        for event in events {
            match event {
                SessionEvent::SendHandshake { to, message } => {
                    self.send_noise(t::NOISE_HANDSHAKE, to, message, now_ms)
                }
                SessionEvent::SendEncrypted { to, frame } => {
                    self.send_noise(t::NOISE_ENCRYPTED, to, frame, now_ms)
                }
                SessionEvent::Established { peer, .. } => {
                    self.emit(NodeEvent::SessionUp(peer));
                    self.announce(now_ms, true);
                }
                SessionEvent::Received { from, plaintext } => {
                    self.emit(NodeEvent::Private { from, plaintext })
                }
                SessionEvent::PeerState { peer, state } => {
                    self.emit(NodeEvent::PeerState { peer, state })
                }
                SessionEvent::Failed { peer, unsent } => self.emit(NodeEvent::Unsent {
                    peer,
                    payloads: unsent,
                }),
            }
        }
    }

    /// Noise packets: directed, TTL 7, unsigned, padded.
    fn send_noise(&mut self, kind: u8, to: PeerId, payload: Vec<u8>, now_ms: u64) {
        let mut packet = Packet::new(kind, self.peer_id(), now_ms, payload, DEFAULT_TTL);
        packet.recipient_id = Some(to);
        self.dedup.mark(PacketKey::of(&packet), now_ms);
        self.transmit(packet, None, Some(to));
    }

    /// Encodes once (only Noise packets are padded), picks links, and
    /// fragments when the frame is larger than the smallest chosen link takes.
    fn transmit(&mut self, packet: Packet, ingress: Option<LinkId>, to: Option<PeerId>) {
        let pad = matches!(packet.packet_type, t::NOISE_ENCRYPTED | t::NOISE_HANDSHAKE);
        let Some(frame) = packet.encode(pad) else {
            return;
        };
        let targets = self.select_links(&packet, ingress, to);
        let Some(limit) = targets
            .iter()
            .filter_map(|l| self.links.get(l))
            .map(|l| l.max_write)
            .min()
        else {
            return;
        };
        if packet.packet_type != t::FRAGMENT && frame.len() > limit {
            let Some(parts) = fragment::split(
                &packet,
                pad,
                fragment_chunk_size(limit),
                random_bytes(),
                None,
            ) else {
                return;
            };
            let now_ms = packet.timestamp;
            for part in parts {
                if part.recipient_id.is_none_or(|r| r == BROADCAST)
                    && packet.sender_id == self.peer_id()
                {
                    self.sync.on_packet_seen(&part, now_ms);
                }
                if let Some(bytes) = part.encode(false) {
                    for link in &targets {
                        self.out.push(Output::Write {
                            link: *link,
                            frame: bytes.clone(),
                        });
                    }
                }
            }
            return;
        }
        for link in targets {
            self.out.push(Output::Write {
                link,
                frame: frame.clone(),
            });
        }
    }

    /// iOS `BLEFanoutSelector`: never back out the way it came; a directed
    /// packet takes the recipient's own link if there is one; broadcasts go
    /// to one link per peer, thinned to ~log2(n)+1 links for chat traffic.
    fn select_links(
        &self,
        packet: &Packet,
        ingress: Option<LinkId>,
        to: Option<PeerId>,
    ) -> Vec<LinkId> {
        let ingress_peer = ingress
            .and_then(|l| self.links.get(&l))
            .and_then(|l| l.peer);
        // Outgoing links first: per peer, the one we opened is preferred
        // (iOS keeps the write side; notifications share one queue).
        let mut allowed: Vec<(LinkId, Option<PeerId>, LinkRole)> = self
            .links
            .iter()
            .filter(|(id, l)| {
                Some(**id) != ingress && (ingress_peer.is_none() || l.peer != ingress_peer)
            })
            .map(|(id, l)| (*id, l.peer, l.role))
            .collect();
        allowed.sort_by_key(|(id, _, role)| (*role != LinkRole::Outgoing, *id));
        let allowed: Vec<(LinkId, Option<PeerId>)> = allowed
            .into_iter()
            .map(|(id, peer, _)| (id, peer))
            .collect();

        let hint = to.or(packet.recipient_id);
        if let Some(target) = hint {
            if let Some((id, _)) = allowed.iter().find(|(_, p)| *p == Some(target)) {
                return vec![*id];
            }
            if self.links.values().any(|l| l.peer == Some(target)) {
                return Vec::new(); // reachable only back the way it came
            }
        }

        let direct_announce = packet.packet_type == t::ANNOUNCE && packet.ttl == DEFAULT_TTL;
        let mut seen = HashSet::new();
        let links: Vec<LinkId> = allowed
            .iter()
            .filter(|(_, p)| direct_announce || p.is_none_or(|p| seen.insert(p)))
            .map(|(id, _)| *id)
            .collect();
        let thin = hint.is_none()
            && !matches!(
                packet.packet_type,
                t::FRAGMENT | t::ANNOUNCE | t::REQUEST_SYNC
            );
        let k = subset_size(links.len());
        if !thin || links.len() <= k {
            return links;
        }
        let seed = message_id(&PacketKey::of(packet));
        let mut scored: Vec<([u8; 32], LinkId)> = links
            .iter()
            .map(|id| (Sha256::digest(format!("{seed}::{id}")).into(), *id))
            .collect();
        scored.sort();
        scored.into_iter().take(k).map(|(_, id)| id).collect()
    }
}
