//! Mesh bridge and internet gateway policy (iOS `BridgeService` and
//! `GatewayService`). No I/O: the engine passes in what the rest of the app
//! knows ([`Ctx`]) and carries out the [`Action`]s.
//!
//! The bridge joins Bluetooth islands that share a place: our public #mesh
//! messages are also signed with a per-cell key and published to the
//! neighbourhood cell's geo relays, and messages from other islands in that
//! cell (or the 8 around it) show up in #mesh marked as bridged. With
//! internet we also serve the island: phones without internet deposit their
//! signed events with us (directed carriers) and we carry remote messages
//! onto the radio (broadcast carriers). The gateway does the same for the
//! location channel we are viewing.
//!
//! Loop rules (as iOS): an event learned from a mesh broadcast is never
//! published or rebroadcast; an event we published is never rebroadcast; a
//! relay event is rebroadcast at most once, and not when the island already
//! has the radio copy.

use bitchat_nostr::bridge::{self as wire, Carrier, Direction, is_valid_geohash};
use bitchat_nostr::event::kind;
use bitchat_nostr::{Event, Keys, geo_keys};
use bitchat_proto::geohash;
use std::collections::{HashMap, HashSet, VecDeque};

pub type PeerId = [u8; 8];

/// Bridge limits (`BridgeService.Limits`).
pub const MAX_QUEUED_UPLINKS: usize = 20;
pub const MAX_QUEUED_UPLINKS_PER_DEPOSITOR: usize = 5;
pub const UPLINKS_PER_MINUTE_PER_DEPOSITOR: usize = 10;
pub const BRIDGE_DOWNLINKS_PER_MINUTE: usize = 20;
pub const GATEWAY_DOWNLINKS_PER_MINUTE: usize = 30;
pub const MAX_PENDING_DOWNLINKS: usize = 30;
pub const MAX_EVENT_AGE_SECS: i64 = 15 * 60;
pub const MAX_TRACKED_IDS: usize = 512;
pub const PRESENCE_INTERVAL_MS: u64 = 4 * 60 * 1000;
const PRESENCE_MIN_GAP_MS: u64 = 30_000;
pub const PARTICIPANT_FRESH_MS: u64 = 10 * 60 * 1000;
pub const INBOUND_PER_MINUTE: usize = 600;
pub const INBOUND_PER_MINUTE_PER_SIGNER: usize = 120;
pub const SIGNATURE_CHECKS_PER_MINUTE: f64 = 720.0;
pub const MAX_PARTICIPANTS: usize = 128;
pub const MAX_CONTENT_BYTES: usize = 16_000;
/// Rendezvous cell precision: neighbourhood (~1.2 km).
pub const CELL_PRECISION: usize = 6;
/// Rendezvous history fetched on (re)subscribe, and its size.
pub const RENDEZVOUS_LIMIT: u32 = 200;
const MINUTE_MS: u64 = 60_000;

/// What the rest of the app knows right now.
pub struct Ctx<'a> {
    pub now_ms: u64,
    /// Any relay connection is up.
    pub relays_connected: bool,
    /// Neighbourhood geohash from the location fix.
    pub location_cell: Option<String>,
    /// A cell advertised by a reachable bridge peer's announce.
    pub advertised_cell: Option<String>,
    /// Reachable mesh peers advertising the bridge / gateway bit.
    pub bridge_peers: Vec<PeerId>,
    pub gateway_peers: Vec<PeerId>,
    /// The location channel being viewed.
    pub current_geohash: Option<String>,
    pub seed: [u8; 32],
    pub nickname: String,
    /// The #mesh timeline holds a row with this ID.
    pub in_timeline: &'a dyn Fn(&str) -> bool,
}

impl Ctx<'_> {
    fn now_secs(&self) -> i64 {
        (self.now_ms / 1000) as i64
    }

    fn fresh(&self, event: &Event) -> bool {
        (self.now_secs() - event.created_at).abs() <= MAX_EVENT_AGE_SECS
    }
}

/// A validated rendezvous message for the #mesh timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Inbound {
    /// The signed event ID owns the row.
    pub id: String,
    /// Stable ID of the radio copy this claims to be (unauthenticated).
    pub radio_hint: Option<String>,
    /// `nick#abcd`
    pub display: String,
    pub nickname: Option<String>,
    pub pubkey: String,
    pub content: String,
    pub timestamp_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Participant {
    pub pubkey: String,
    pub nickname: Option<String>,
    pub last_seen_ms: u64,
}

impl Participant {
    pub fn display(&self) -> String {
        let nick = self
            .nickname
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("anon");
        format!(
            "{nick}#{}",
            &self.pubkey[self.pubkey.len().saturating_sub(4)..]
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Publish to the geo relays closest to `geohash`.
    Publish {
        event: Event,
        geohash: String,
    },
    /// Open the rendezvous subscription for these cells (own cell first).
    Subscribe {
        cells: Vec<String>,
    },
    Unsubscribe,
    SendCarrier {
        to: PeerId,
        payload: Vec<u8>,
    },
    BroadcastCarrier {
        payload: Vec<u8>,
    },
    /// Show a bridged message in #mesh.
    Inject(Inbound),
    /// Drop an earlier bridged row: its radio copy arrived.
    Remove {
        id: String,
    },
    /// A carried event for the location channel being viewed.
    InjectGeo(Event),
    /// Capability bits or the active cell changed.
    Advertise,
    /// The people across the bridge changed.
    People,
}

/// Insertion-ordered set that forgets the oldest IDs (`BoundedIDSet`).
struct Ids {
    cap: usize,
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Ids {
    fn new(cap: usize) -> Ids {
        Ids {
            cap,
            set: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// True when newly added.
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
}

fn random_ms(lo: u64, hi: u64) -> u64 {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    lo + u64::from_le_bytes(b) % (hi - lo + 1)
}

fn tag<'a>(event: &'a Event, name: &str) -> Option<&'a [String]> {
    event
        .tags
        .iter()
        .find(|t| t.len() >= 2 && t[0] == name)
        .map(Vec::as_slice)
}

fn has_tag(event: &Event, name: &str, value: &str) -> bool {
    event
        .tags
        .iter()
        .any(|t| t.len() >= 2 && t[0] == name && t[1] == value)
}

/// Per-depositor deposits in the last minute.
fn allow_deposit(times: &mut HashMap<PeerId, Vec<u64>>, depositor: PeerId, now: u64) -> bool {
    let cutoff = now.saturating_sub(MINUTE_MS);
    let list = times.entry(depositor).or_default();
    list.retain(|t| *t >= cutoff);
    if list.len() >= UPLINKS_PER_MINUTE_PER_DEPOSITOR {
        return false;
    }
    list.push(now);
    if times.len() > MAX_TRACKED_IDS {
        times.retain(|_, v| v.iter().any(|t| *t >= cutoff));
    }
    true
}

fn enqueue(queue: &mut Vec<(PeerId, String, Event)>, item: (PeerId, String, Event)) -> bool {
    if queue.iter().filter(|q| q.0 == item.0).count() >= MAX_QUEUED_UPLINKS_PER_DEPOSITOR {
        return false;
    }
    if queue.len() >= MAX_QUEUED_UPLINKS {
        queue.drain(..queue.len() + 1 - MAX_QUEUED_UPLINKS);
    }
    queue.push(item);
    true
}

enum Kind {
    Presence,
    Message(Inbound),
}

pub struct Bridge {
    enabled: bool,
    /// Per-session "nearby only": no rendezvous copy of our messages.
    pub nearby_only: bool,
    active_cell: Option<String>,
    subscribed: HashSet<String>,
    mesh_broadcast_ids: Ids,
    published_ids: Ids,
    rebroadcast_ids: Ids,
    injected_ids: Ids,
    received_ids: Ids,
    observed_radio_ids: Ids,
    /// Bridged row (event ID) → the radio message it claims to copy.
    aliases: HashMap<String, String>,
    alias_order: Vec<String>,
    max_aliases: usize,
    queued: Vec<(PeerId, String, Event)>,
    deposit_times: HashMap<PeerId, Vec<u64>>,
    downlink_times: Vec<u64>,
    inbound_times: Vec<u64>,
    inbound_by_signer: HashMap<String, Vec<u64>>,
    sig_tokens: f64,
    sig_refill_at: Option<u64>,
    pending: VecDeque<(Event, String)>,
    drain_at: Option<u64>,
    presence_at: Option<u64>,
    last_presence: Option<u64>,
    participants: HashMap<String, (u64, Option<String>)>,
    people: Vec<Participant>,
    identity: Option<(String, Keys)>,
    out: Vec<Action>,
}

impl Bridge {
    /// `max_aliases`: rows the #mesh timeline keeps.
    pub fn new(enabled: bool, max_aliases: usize) -> Bridge {
        Bridge {
            enabled,
            nearby_only: false,
            active_cell: None,
            subscribed: HashSet::new(),
            mesh_broadcast_ids: Ids::new(MAX_TRACKED_IDS),
            published_ids: Ids::new(MAX_TRACKED_IDS),
            rebroadcast_ids: Ids::new(MAX_TRACKED_IDS),
            injected_ids: Ids::new(MAX_TRACKED_IDS),
            received_ids: Ids::new(MAX_TRACKED_IDS),
            observed_radio_ids: Ids::new(MAX_TRACKED_IDS),
            aliases: HashMap::new(),
            alias_order: Vec::new(),
            max_aliases,
            queued: Vec::new(),
            deposit_times: HashMap::new(),
            downlink_times: Vec::new(),
            inbound_times: Vec::new(),
            inbound_by_signer: HashMap::new(),
            sig_tokens: SIGNATURE_CHECKS_PER_MINUTE,
            sig_refill_at: None,
            pending: VecDeque::new(),
            drain_at: None,
            presence_at: None,
            last_presence: None,
            participants: HashMap::new(),
            people: Vec::new(),
            identity: None,
            out: Vec::new(),
        }
    }

    pub fn take(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.out)
    }

    /// A timer is due.
    pub fn due(&self, now: u64) -> bool {
        self.presence_at.is_some_and(|t| now >= t) || self.drain_at.is_some_and(|t| now >= t)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn active_cell(&self) -> Option<&str> {
        self.active_cell.as_deref()
    }

    /// Advertise the bridge bit and cell: bridging with a known cell.
    pub fn advertised_cell(&self) -> Option<&str> {
        self.active_cell.as_deref().filter(|_| self.enabled)
    }

    /// People seen across the bridge in the last 10 minutes, newest first.
    pub fn people(&self) -> &[Participant] {
        &self.people
    }

    fn keys(&mut self, seed: &[u8; 32], cell: &str) -> Keys {
        if let Some((c, k)) = &self.identity
            && c == cell
        {
            return k.clone();
        }
        let keys = geo_keys::derive_bridge(seed, cell);
        self.identity = Some((cell.to_string(), keys.clone()));
        keys
    }

    fn is_own(&mut self, event: &Event, cell: &str, ctx: &Ctx) -> bool {
        self.keys(&ctx.seed, cell).public_key_hex() == event.pubkey.to_lowercase()
    }

    // ---- toggle and rendezvous ----

    pub fn set_enabled(&mut self, enabled: bool, ctx: &Ctx) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.queued.clear();
            self.pending.clear();
            self.drain_at = None;
            self.presence_at = None;
            self.deposit_times.clear();
            self.inbound_times.clear();
            self.inbound_by_signer.clear();
            self.participants.clear();
            self.recompute_people(ctx.now_ms);
        }
        self.refresh(ctx);
        self.out.push(Action::Advertise);
    }

    /// Our location's cell, else one a reachable bridge peer advertises.
    fn current_cell(ctx: &Ctx) -> Option<String> {
        if let Some(own) = ctx.location_cell.as_deref().filter(|c| !c.is_empty()) {
            return Some(own.chars().take(CELL_PRECISION).collect());
        }
        ctx.advertised_cell
            .as_deref()
            .filter(|c| is_valid_geohash(c))
            .map(|c| c.chars().take(CELL_PRECISION).collect())
    }

    /// Recomputes the cell and (re)opens or closes the subscription.
    /// Idempotent: call on toggles, location and mesh changes.
    pub fn refresh(&mut self, ctx: &Ctx) {
        let cell = if self.enabled {
            Self::current_cell(ctx)
        } else {
            None
        };
        if cell == self.active_cell {
            self.arm_presence(ctx.now_ms);
            return;
        }
        if self.active_cell.is_some() {
            self.out.push(Action::Unsubscribe);
            self.subscribed.clear();
        }
        self.active_cell = cell.clone();
        self.out.push(Action::Advertise);
        let Some(cell) = cell else {
            self.arm_presence(ctx.now_ms);
            return;
        };
        // Own cell plus neighbours: islands straddling an edge still meet.
        let mut cells = vec![cell.clone()];
        cells.extend(geohash::neighbors(&cell));
        self.subscribed = cells.iter().cloned().collect();
        log::info!("bridge: rendezvous open for cell {cell}");
        self.out.push(Action::Subscribe { cells });
        self.publish_presence(ctx);
        self.arm_presence(ctx.now_ms);
    }

    fn arm_presence(&mut self, now: u64) {
        if self.enabled && self.presence_at.is_none() {
            self.presence_at = Some(now + PRESENCE_INTERVAL_MS);
        }
    }

    /// Heartbeat so silent participants still count across the bridge.
    pub fn publish_presence(&mut self, ctx: &Ctx) {
        let Some(cell) = self.active_cell.clone() else {
            return;
        };
        if !self.enabled || !ctx.relays_connected {
            return;
        }
        if self
            .last_presence
            .is_some_and(|t| ctx.now_ms.saturating_sub(t) < PRESENCE_MIN_GAP_MS)
        {
            return;
        }
        self.last_presence = Some(ctx.now_ms);
        let keys = self.keys(&ctx.seed, &cell);
        let event = wire::presence_event(&cell, &keys, ctx.now_secs());
        self.published_ids.insert(&event.id);
        self.out.push(Action::Publish {
            event,
            geohash: cell,
        });
    }

    /// Timers: presence heartbeat and the downlink drain.
    pub fn tick(&mut self, ctx: &Ctx) {
        if self.presence_at.is_some_and(|t| ctx.now_ms >= t) {
            self.presence_at = None;
            self.publish_presence(ctx);
            self.prune_participants(ctx.now_ms);
            if self.active_cell.is_none() {
                self.refresh(ctx);
            }
            self.arm_presence(ctx.now_ms);
        }
        if self.drain_at.is_some_and(|t| ctx.now_ms >= t) {
            self.drain_at = None;
            self.drain(ctx);
        }
    }

    // ---- sending ----

    /// The rendezvous copy of a public mesh message we just sent.
    /// `sender_hex`/`timestamp_ms` are the radio packet's.
    pub fn outgoing(&mut self, content: &str, sender_hex: &str, timestamp_ms: u64, ctx: &Ctx) {
        if !self.enabled || self.nearby_only || content.len() > MAX_CONTENT_BYTES {
            return;
        }
        let Some(cell) = self.active_cell.clone().or_else(|| Self::current_cell(ctx)) else {
            return;
        };
        let keys = self.keys(&ctx.seed, &cell);
        let event = wire::mesh_event(
            content,
            &cell,
            &keys,
            &ctx.nickname,
            sender_hex,
            timestamp_ms,
            ctx.now_secs(),
        );
        self.published_ids.insert(&event.id);
        self.injected_ids.insert(&event.id);
        if ctx.relays_connected {
            self.out.push(Action::Publish {
                event,
                geohash: cell,
            });
        } else if let Some(carrier) = Carrier::new(Direction::ToBridge, &cell, &event)
            && let Some(to) = ctx.bridge_peers.first()
        {
            self.out.push(Action::SendCarrier {
                to: *to,
                payload: carrier.encode(),
            });
        }
    }

    // ---- relay subscription ----

    pub fn on_rendezvous_event(&mut self, event: Event, ctx: &Ctx) {
        if !self.enabled {
            return;
        }
        // Trust only the event's own signed `r` tag, within our ring.
        let Some(cell) = tag(&event, "r").map(|t| t[1].clone()) else {
            return;
        };
        if !self.subscribed.contains(&cell) || self.published_ids.contains(&event.id) {
            return;
        }
        // Our own events come back (also from before a restart).
        if self.is_own(&event, &cell, ctx) {
            self.published_ids.insert(&event.id);
            return;
        }
        if !self.allow_signature_check(ctx.now_ms) || !event.verify() {
            return;
        }
        if !self.received_ids.insert(&event.id) || !self.allow_inbound(&event.pubkey, ctx.now_ms) {
            return;
        }
        match classify(&event, &cell, ctx) {
            None => {}
            Some(Kind::Presence) => self.record_participant(&event.pubkey, None, ctx.now_ms),
            Some(Kind::Message(m)) => {
                let local = m
                    .radio_hint
                    .as_deref()
                    .is_some_and(|h| self.radio_present(h, ctx));
                let nickname = m.nickname.clone();
                if !local && self.inject(m, ctx) {
                    self.record_participant(&event.pubkey, nickname, ctx.now_ms);
                }
                // Serving: carry other islands' messages onto the radio,
                // after a jitter so other bridges nearby can go first.
                if !local
                    && !self.mesh_broadcast_ids.contains(&event.id)
                    && !self.rebroadcast_ids.contains(&event.id)
                    && !self.pending.iter().any(|(e, _)| e.id == event.id)
                {
                    self.pending.push_back((event, cell));
                    while self.pending.len() > MAX_PENDING_DOWNLINKS {
                        self.pending.pop_front();
                    }
                    self.schedule_drain(true, ctx.now_ms);
                }
            }
        }
    }

    /// A public mesh message passed its signature check: the radio copy
    /// wins over any bridged row that claimed to be it.
    pub fn on_radio_message(&mut self, message_id: &str, ctx: &Ctx) {
        if message_id.is_empty() {
            return;
        }
        self.observed_radio_ids.insert(message_id);
        let matching: Vec<String> = self
            .aliases
            .iter()
            .filter(|(_, radio)| *radio == message_id)
            .map(|(event, _)| event.clone())
            .collect();
        if matching.is_empty() {
            return;
        }
        for id in &matching {
            self.aliases.remove(id);
            self.out.push(Action::Remove { id: id.clone() });
        }
        self.alias_order.retain(|id| !matching.contains(id));
        self.pending
            .retain(|(event, cell)| match classify(event, cell, ctx) {
                Some(Kind::Message(m)) => m.radio_hint.as_deref() != Some(message_id),
                _ => true,
            });
    }

    // ---- mesh carriers ----

    pub fn on_carrier(&mut self, carrier: &Carrier, from: PeerId, directed: bool, ctx: &Ctx) {
        match carrier.direction {
            Direction::ToBridge if directed => self.on_deposit(carrier, from, ctx),
            Direction::FromBridge if !directed => self.on_downlink(carrier, ctx),
            _ => {}
        }
    }

    /// A mesh-only peer asks us to publish its signed event.
    fn on_deposit(&mut self, carrier: &Carrier, depositor: PeerId, ctx: &Ctx) {
        if !self.enabled {
            return;
        }
        let Some(event) = structurally_valid(carrier, ctx) else {
            return;
        };
        if self.mesh_broadcast_ids.contains(&event.id)
            || self.published_ids.contains(&event.id)
            || self.queued.iter().any(|q| q.2.id == event.id)
        {
            return;
        }
        if !allow_deposit(&mut self.deposit_times, depositor, ctx.now_ms) {
            return;
        }
        if !self.allow_signature_check(ctx.now_ms) || !event.verify() {
            return;
        }
        if ctx.relays_connected {
            self.publish(event, carrier.geohash.clone());
        } else {
            enqueue(
                &mut self.queued,
                (depositor, carrier.geohash.clone(), event),
            );
        }
    }

    /// Publishes deposits held while no relay was reachable.
    pub fn flush(&mut self, ctx: &Ctx) {
        if !self.enabled || !ctx.relays_connected {
            return;
        }
        for (_, cell, event) in std::mem::take(&mut self.queued) {
            if !self.published_ids.contains(&event.id) {
                self.publish(event, cell);
            }
        }
    }

    fn publish(&mut self, event: Event, cell: String) {
        self.published_ids.insert(&event.id);
        log::info!("bridge: published carried event for cell {cell}");
        self.out.push(Action::Publish {
            event,
            geohash: cell,
        });
    }

    /// Another island's message, carried onto our radio by a bridge peer.
    /// Received even with our bridge off: it is passive.
    fn on_downlink(&mut self, carrier: &Carrier, ctx: &Ctx) {
        let Some(event) = structurally_valid(carrier, ctx) else {
            return;
        };
        if self.published_ids.contains(&event.id)
            || self.is_own(&event, &carrier.geohash, ctx)
            || !self.allow_signature_check(ctx.now_ms)
            || !event.verify()
        {
            return;
        }
        let first = self.mesh_broadcast_ids.insert(&event.id);
        if !first
            || !self.received_ids.insert(&event.id)
            || !self.allow_inbound(&event.pubkey, ctx.now_ms)
        {
            return;
        }
        if let Some(Kind::Message(m)) = classify(&event, &carrier.geohash, ctx) {
            let nickname = m.nickname.clone();
            if self.inject(m, ctx) {
                self.record_participant(&event.pubkey, nickname, ctx.now_ms);
            }
        }
    }

    fn schedule_drain(&mut self, jitter: bool, now: u64) {
        if self.pending.is_empty() || self.drain_at.is_some() {
            return;
        }
        let delay = if jitter {
            random_ms(200, 1500)
        } else {
            let oldest = self.downlink_times.iter().min().copied().unwrap_or(now);
            (MINUTE_MS.saturating_sub(now.saturating_sub(oldest))).max(50)
        };
        self.drain_at = Some(now + delay);
    }

    fn drain(&mut self, ctx: &Ctx) {
        let cutoff = ctx.now_ms.saturating_sub(MINUTE_MS);
        self.downlink_times.retain(|t| *t >= cutoff);
        while self.downlink_times.len() < BRIDGE_DOWNLINKS_PER_MINUTE {
            let Some((event, cell)) = self.pending.pop_front() else {
                break;
            };
            // Another bridge, or the radio copy, may have beaten us to it.
            if !ctx.fresh(&event)
                || self.mesh_broadcast_ids.contains(&event.id)
                || self.rebroadcast_ids.contains(&event.id)
            {
                continue;
            }
            if let Some(Kind::Message(m)) = classify(&event, &cell, ctx)
                && m.radio_hint
                    .as_deref()
                    .is_some_and(|h| self.radio_present(h, ctx))
            {
                continue;
            }
            let Some(carrier) = Carrier::new(Direction::FromBridge, &cell, &event) else {
                continue;
            };
            self.out.push(Action::BroadcastCarrier {
                payload: carrier.encode(),
            });
            self.rebroadcast_ids.insert(&event.id);
            self.downlink_times.push(ctx.now_ms);
        }
        self.schedule_drain(false, ctx.now_ms);
    }

    // ---- timeline and people ----

    fn inject(&mut self, m: Inbound, ctx: &Ctx) -> bool {
        if !self.injected_ids.insert(&m.id) {
            return false;
        }
        if m.radio_hint
            .as_deref()
            .is_some_and(|h| self.radio_present(h, ctx))
        {
            return false;
        }
        if let Some(radio) = m.radio_hint.clone() {
            self.record_alias(&m.id, radio, ctx);
        }
        self.out.push(Action::Inject(m));
        true
    }

    fn radio_present(&self, id: &str, ctx: &Ctx) -> bool {
        self.observed_radio_ids.contains(id) || (ctx.in_timeline)(id)
    }

    /// Keeps the alias of every bridged row that can still be on screen.
    fn record_alias(&mut self, event_id: &str, radio: String, ctx: &Ctx) {
        if self.aliases.contains_key(event_id) {
            return;
        }
        self.aliases.insert(event_id.to_string(), radio);
        self.alias_order.push(event_id.to_string());
        let mut overflow = self.alias_order.len().saturating_sub(self.max_aliases);
        if overflow == 0 {
            return;
        }
        let aliases = &mut self.aliases;
        self.alias_order.retain(|id| {
            if overflow > 0 && !(ctx.in_timeline)(id) {
                aliases.remove(id);
                overflow -= 1;
                false
            } else {
                true
            }
        });
    }

    fn record_participant(&mut self, pubkey: &str, nickname: Option<String>, now: u64) {
        let cutoff = now.saturating_sub(PARTICIPANT_FRESH_MS);
        self.participants.retain(|_, (seen, _)| *seen >= cutoff);
        if !self.participants.contains_key(pubkey)
            && self.participants.len() >= MAX_PARTICIPANTS
            && let Some(oldest) = self
                .participants
                .iter()
                .min_by_key(|(_, (seen, _))| *seen)
                .map(|(k, _)| k.clone())
        {
            self.participants.remove(&oldest);
        }
        let previous = self.participants.get(pubkey).and_then(|p| p.1.clone());
        let nickname = nickname
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .or(previous);
        self.participants
            .insert(pubkey.to_string(), (now, nickname));
        self.recompute_people(now);
    }

    fn prune_participants(&mut self, now: u64) {
        let cutoff = now.saturating_sub(PARTICIPANT_FRESH_MS);
        self.participants.retain(|_, (seen, _)| *seen >= cutoff);
        self.recompute_people(now);
    }

    fn recompute_people(&mut self, now: u64) {
        let cutoff = now.saturating_sub(PARTICIPANT_FRESH_MS);
        let mut people: Vec<Participant> = self
            .participants
            .iter()
            .filter(|(_, (seen, _))| *seen >= cutoff)
            .map(|(pk, (seen, nick))| Participant {
                pubkey: pk.clone(),
                nickname: nick.clone(),
                last_seen_ms: *seen,
            })
            .collect();
        people.sort_by(|a, b| {
            b.last_seen_ms
                .cmp(&a.last_seen_ms)
                .then_with(|| a.pubkey.cmp(&b.pubkey))
        });
        let changed = people.len() != self.people.len()
            || people
                .iter()
                .zip(&self.people)
                .any(|(a, b)| a.pubkey != b.pubkey || a.nickname != b.nickname);
        self.people = people;
        if changed {
            self.out.push(Action::People);
        }
    }

    // ---- limits ----

    fn allow_inbound(&mut self, signer: &str, now: u64) -> bool {
        let cutoff = now.saturating_sub(MINUTE_MS);
        self.inbound_times.retain(|t| *t >= cutoff);
        if self.inbound_times.len() >= INBOUND_PER_MINUTE {
            return false;
        }
        let list = self
            .inbound_by_signer
            .entry(signer.to_string())
            .or_default();
        list.retain(|t| *t >= cutoff);
        if list.len() >= INBOUND_PER_MINUTE_PER_SIGNER {
            return false;
        }
        list.push(now);
        self.inbound_times.push(now);
        if self.inbound_by_signer.len() > MAX_TRACKED_IDS {
            self.inbound_by_signer
                .retain(|_, v| v.iter().any(|t| *t >= cutoff));
        }
        true
    }

    /// Token bucket in front of Schnorr checks, valid or not.
    fn allow_signature_check(&mut self, now: u64) -> bool {
        if let Some(last) = self.sig_refill_at {
            let elapsed = now.saturating_sub(last) as f64 / 1000.0;
            self.sig_tokens = (self.sig_tokens + elapsed * SIGNATURE_CHECKS_PER_MINUTE / 60.0)
                .min(SIGNATURE_CHECKS_PER_MINUTE);
        }
        self.sig_refill_at = Some(now);
        if self.sig_tokens < 1.0 {
            return false;
        }
        self.sig_tokens -= 1.0;
        true
    }
}

/// Rendezvous event checks short of the signature; `None` rejects.
fn classify(event: &Event, cell: &str, ctx: &Ctx) -> Option<Kind> {
    if !ctx.fresh(event) || !has_tag(event, "r", cell) || !is_valid_geohash(cell) {
        return None;
    }
    match event.kind {
        kind::GEOHASH_PRESENCE => Some(Kind::Presence),
        kind::EPHEMERAL => {
            let content = &event.content;
            if content.trim().is_empty() || content.len() > MAX_CONTENT_BYTES {
                return None;
            }
            let nickname = tag(event, "n")
                .map(|t| t[1].trim().to_string())
                .filter(|n| !n.is_empty());
            // `m` = [stable ID, mesh sender, ms]: public, so only a hint.
            let radio_hint = tag(event, "m").and_then(|m| {
                if m.len() < 4 || m[2].len() != 16 || !m[2].chars().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let ts: u64 = m[3].parse().ok()?;
                Some(wire::stable_id(&m[2], ts, content))
            });
            let pk = &event.pubkey;
            Some(Kind::Message(Inbound {
                id: event.id.clone(),
                radio_hint,
                display: format!(
                    "{}#{}",
                    nickname.as_deref().unwrap_or("anon"),
                    &pk[pk.len().saturating_sub(4)..]
                ),
                nickname,
                pubkey: pk.clone(),
                content: content.clone(),
                timestamp_ms: (event.created_at.max(0) as u64) * 1000,
            }))
        }
        _ => None,
    }
}

/// Parse, size, cell, kind, `r` tag and age, without the signature.
fn structurally_valid(carrier: &Carrier, ctx: &Ctx) -> Option<Event> {
    if !is_valid_geohash(&carrier.geohash) {
        return None;
    }
    let event = carrier.event()?;
    classify(&event, &carrier.geohash, ctx)?;
    Some(event)
}

/// The location-channel gateway (`GatewayService`).
pub struct Gateway {
    enabled: bool,
    mesh_broadcast_ids: Ids,
    published_ids: Ids,
    rebroadcast_ids: Ids,
    queued: Vec<(PeerId, String, Event)>,
    deposit_times: HashMap<PeerId, Vec<u64>>,
    downlink_times: Vec<u64>,
    pending: VecDeque<(Event, String)>,
    drain_at: Option<u64>,
    out: Vec<Action>,
}

impl Gateway {
    pub fn new(enabled: bool) -> Gateway {
        Gateway {
            enabled,
            mesh_broadcast_ids: Ids::new(MAX_TRACKED_IDS),
            published_ids: Ids::new(MAX_TRACKED_IDS),
            rebroadcast_ids: Ids::new(MAX_TRACKED_IDS),
            queued: Vec::new(),
            deposit_times: HashMap::new(),
            downlink_times: Vec::new(),
            pending: VecDeque::new(),
            drain_at: None,
            out: Vec::new(),
        }
    }

    pub fn take(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.out)
    }

    pub fn due(&self, now: u64) -> bool {
        self.drain_at.is_some_and(|t| now >= t)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.queued.clear();
            self.pending.clear();
            self.drain_at = None;
            self.deposit_times.clear();
        }
        self.out.push(Action::Advertise);
    }

    pub fn on_carrier(&mut self, carrier: &Carrier, from: PeerId, directed: bool, ctx: &Ctx) {
        match carrier.direction {
            Direction::ToGateway if directed => self.on_deposit(carrier, from, ctx),
            Direction::FromGateway if !directed => self.on_downlink(carrier, ctx),
            _ => {}
        }
    }

    fn structurally_valid(carrier: &Carrier, ctx: &Ctx) -> Option<Event> {
        if !is_valid_geohash(&carrier.geohash) {
            return None;
        }
        let event = carrier.event()?;
        (event.kind == kind::EPHEMERAL
            && has_tag(&event, "g", &carrier.geohash)
            && ctx.fresh(&event))
        .then_some(event)
    }

    fn on_deposit(&mut self, carrier: &Carrier, depositor: PeerId, ctx: &Ctx) {
        if !self.enabled {
            return;
        }
        let Some(event) = Self::structurally_valid(carrier, ctx) else {
            return;
        };
        if self.mesh_broadcast_ids.contains(&event.id)
            || self.published_ids.contains(&event.id)
            || self.queued.iter().any(|q| q.2.id == event.id)
        {
            return;
        }
        if !allow_deposit(&mut self.deposit_times, depositor, ctx.now_ms) || !event.verify() {
            return;
        }
        let accepted = if ctx.relays_connected {
            self.publish(event.clone(), carrier.geohash.clone());
            true
        } else {
            enqueue(
                &mut self.queued,
                (depositor, carrier.geohash.clone(), event.clone()),
            )
        };
        // Only show what will really reach the channel.
        if accepted && ctx.current_geohash.as_deref() == Some(carrier.geohash.as_str()) {
            self.out.push(Action::InjectGeo(event));
        }
    }

    pub fn flush(&mut self, ctx: &Ctx) {
        if !self.enabled || !ctx.relays_connected {
            return;
        }
        for (_, geohash, event) in std::mem::take(&mut self.queued) {
            if !self.published_ids.contains(&event.id) {
                self.publish(event, geohash);
            }
        }
    }

    fn publish(&mut self, event: Event, geohash: String) {
        self.published_ids.insert(&event.id);
        self.out.push(Action::Publish { event, geohash });
    }

    /// A relay event for the channel we are viewing: carry it onto the mesh.
    pub fn rebroadcast(&mut self, event: &Event, geohash: &str, ctx: &Ctx) {
        if !self.enabled || event.kind != kind::EPHEMERAL {
            return;
        }
        if !ctx.fresh(event) || !has_tag(event, "g", geohash) {
            return;
        }
        if self.mesh_broadcast_ids.contains(&event.id)
            || self.published_ids.contains(&event.id)
            || self.rebroadcast_ids.contains(&event.id)
            || self.pending.iter().any(|(e, _)| e.id == event.id)
            || !event.verify()
        {
            return;
        }
        self.pending.push_back((event.clone(), geohash.to_string()));
        while self.pending.len() > MAX_PENDING_DOWNLINKS {
            self.pending.pop_front();
        }
        self.drain(ctx);
    }

    pub fn tick(&mut self, ctx: &Ctx) {
        if self.drain_at.is_some_and(|t| ctx.now_ms >= t) {
            self.drain_at = None;
            self.drain(ctx);
        }
    }

    fn drain(&mut self, ctx: &Ctx) {
        let cutoff = ctx.now_ms.saturating_sub(MINUTE_MS);
        self.downlink_times.retain(|t| *t >= cutoff);
        while self.downlink_times.len() < GATEWAY_DOWNLINKS_PER_MINUTE {
            let Some((event, geohash)) = self.pending.pop_front() else {
                break;
            };
            if !ctx.fresh(&event) {
                continue;
            }
            let Some(carrier) = Carrier::new(Direction::FromGateway, &geohash, &event) else {
                continue;
            };
            self.out.push(Action::BroadcastCarrier {
                payload: carrier.encode(),
            });
            self.rebroadcast_ids.insert(&event.id);
            self.downlink_times.push(ctx.now_ms);
        }
        if !self.pending.is_empty() && self.drain_at.is_none() {
            let oldest = self
                .downlink_times
                .iter()
                .min()
                .copied()
                .unwrap_or(ctx.now_ms);
            let delay = MINUTE_MS
                .saturating_sub(ctx.now_ms.saturating_sub(oldest))
                .max(50);
            self.drain_at = Some(ctx.now_ms + delay);
        }
    }

    /// A gateway peer carried a channel event onto the mesh.
    fn on_downlink(&mut self, carrier: &Carrier, ctx: &Ctx) {
        let Some(event) = Self::structurally_valid(carrier, ctx) else {
            return;
        };
        if !event.verify() || !self.mesh_broadcast_ids.insert(&event.id) {
            return;
        }
        if ctx.current_geohash.as_deref() == Some(carrier.geohash.as_str()) {
            self.out.push(Action::InjectGeo(event));
        }
    }

    /// No relay reachable: hand our freshly signed channel message to a
    /// gateway peer. True when sent.
    pub fn uplink(&mut self, event: &Event, geohash: &str, ctx: &Ctx) -> bool {
        if ctx.relays_connected
            || self.mesh_broadcast_ids.contains(&event.id)
            || self.published_ids.contains(&event.id)
        {
            return false;
        }
        let (Some(to), Some(carrier)) = (
            ctx.gateway_peers.first(),
            Carrier::new(Direction::ToGateway, geohash, event),
        ) else {
            return false;
        };
        self.out.push(Action::SendCarrier {
            to: *to,
            payload: carrier.encode(),
        });
        true
    }
}
