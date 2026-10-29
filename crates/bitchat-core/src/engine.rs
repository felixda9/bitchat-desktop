//! The Nostr-side chat engine: location channels, geohash DMs, presence and
//! location notes. A synchronous state machine: the runtime feeds it user
//! actions and relay events and carries out the [`Effect`]s it returns.
//! Behaviour follows the iOS app (see docs/interop-notes.md).

use crate::limits::{RateLimiter, content_key};
use crate::model::*;
use crate::state::{State, clean_nickname};
use bitchat_nostr::embedded::{self, Addressing, Embedded};
use bitchat_nostr::envelope;
use bitchat_nostr::event::kind;
use bitchat_nostr::net::PoolEvent;
use bitchat_nostr::relay_directory::{Directory, RELAYS_PER_CHANNEL};
use bitchat_nostr::{Event, Filter, Keys, pow};
use bitchat_proto::geohash::{self, Level};
use bitchat_proto::payload::PRIVATE_MESSAGE_FIELD_MAX;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// History fetched when joining a channel.
pub const GEOHASH_LOOKBACK_SECS: i64 = 3600;
pub const GEOHASH_HISTORY_LIMIT: u32 = 200;
pub const DM_LOOKBACK_SECS: i64 = 86_400;
pub const TIMELINE_CAP: usize = 1337;
pub const NICKNAME_CAP: usize = 1337;
/// Someone counts as online for this long after their last event.
pub const ONLINE_WINDOW_SECS: i64 = 300;
pub const MAX_PUBLIC_CHARS: usize = 16_000;
/// Our own channel events newer than this are echoes of local sends.
const OWN_ECHO_SECS: i64 = 15;
const DUPLICATE_WINDOW_MS: u64 = 1000;
const SEEN_CAP: usize = 2000;
pub const NOTES_EXPIRY_SECS: i64 = 86_400;
const NOTES_CAP: usize = 500;
/// Presence is only sent for coarse, physical channels.
pub const PRESENCE_PRECISIONS: [usize; 3] = [2, 4, 5];

#[derive(Clone, Debug)]
pub enum Effect {
    Subscribe {
        id: String,
        filter: Filter,
        relays: Vec<String>,
    },
    Unsubscribe {
        id: String,
    },
    Publish {
        event: Event,
        relays: Vec<String>,
    },
    /// Mine proof of work for a pending public message, then call
    /// [`Engine::finish_public`] with the nonce tag.
    Mine(MineRequest),
    Ui(UiEvent),
    Save,
}

#[derive(Clone, Debug)]
pub struct MineRequest {
    pub id: u64,
    pub pubkey: String,
    pub created_at: i64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

struct PendingPublic {
    geohash: String,
    keys: Keys,
    created_at: i64,
    tags: Vec<Vec<String>>,
    content: String,
    /// Raw emote sends show a system echo instead of a normal local echo.
    echo: bool,
}

#[derive(Default)]
struct Participant {
    last_seen: i64,
    teleported: bool,
}

struct Channel {
    geohash: String,
    teleported: bool,
    keys: Keys,
    replaying: bool,
    timeline: VecDeque<PublicMessage>,
    participants: HashMap<String, Participant>,
    nicknames: HashMap<String, String>,
    recent_content: HashMap<String, u64>,
}

struct PendingAck {
    keys: Keys,
    peer: String,
    item: Embedded,
}

#[derive(Default)]
struct Conversation {
    geohash: String,
    messages: Vec<DmMessage>,
    unread: usize,
}

struct Fix {
    regional: BTreeMap<usize, String>,
}

/// Bounded insertion-ordered set.
#[derive(Default)]
struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > SEEN_CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

pub struct Engine {
    pub state: State,
    directory: Directory,
    channel: Option<Channel>,
    fix: Option<Fix>,
    dms: BTreeMap<String, Conversation>,
    open_dm: Option<String>,
    rate: RateLimiter,
    seen: Seen,
    seen_wraps: Seen,
    acks: VecDeque<PendingAck>,
    mining: HashMap<u64, PendingPublic>,
    next_mine: u64,
    notes: Vec<(Event, String)>,
    effects: Vec<Effect>,
    key_in_keychain: bool,
}

pub fn geo_sub(geohash: &str) -> String {
    format!("geo-{geohash}")
}

pub fn dm_sub(geohash: &str) -> String {
    format!("geo-dm-{geohash}")
}

const NOTES_SUB: &str = "locnotes";

/// `nick#abcd` (last four hex characters of the pubkey).
pub fn display_name(nickname: Option<&str>, pubkey: &str) -> String {
    let suffix = &pubkey[pubkey.len().saturating_sub(4)..];
    let nick = nickname.filter(|n| !n.trim().is_empty()).unwrap_or("anon");
    format!("{nick}#{suffix}")
}

fn is_emote(content: &str) -> bool {
    let t = content.trim();
    t.len() > 2
        && t.starts_with('*')
        && t.ends_with('*')
        && (t.contains('🫂') || t.contains('🐟') || t.contains("took a screenshot"))
}

fn new_message_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random number generator failed");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode_upper(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Region => "region",
        Level::Province => "province",
        Level::City => "city",
        Level::Neighborhood => "neighborhood",
        Level::Block => "block",
        Level::Building => "building",
    }
}

impl Engine {
    pub fn new(state: State, key_in_keychain: bool) -> Engine {
        Engine {
            state,
            directory: Directory::bundled(),
            channel: None,
            fix: None,
            dms: BTreeMap::new(),
            open_dm: None,
            rate: RateLimiter::default(),
            seen: Seen::default(),
            seen_wraps: Seen::default(),
            acks: VecDeque::new(),
            mining: HashMap::new(),
            next_mine: 0,
            notes: Vec::new(),
            effects: Vec::new(),
            key_in_keychain,
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn ui(&mut self, event: UiEvent) {
        self.effects.push(Effect::Ui(event));
    }

    fn system(&mut self, text: impl Into<String>) {
        self.ui(UiEvent::System { text: text.into() });
    }

    pub fn set_directory(&mut self, directory: Directory) {
        self.directory = directory;
    }

    pub fn directory(&self) -> &Directory {
        &self.directory
    }

    pub fn current_geohash(&self) -> Option<&str> {
        self.channel.as_ref().map(|c| c.geohash.as_str())
    }

    pub fn has_location(&self) -> bool {
        self.fix.is_some()
    }

    /// Replays the whole view: at start-up, after a wipe, and when the UI
    /// (re)connects and missed earlier events.
    pub fn announce(&mut self, now_ms: u64) {
        let keys = self.state.identity.nostr_keys();
        self.ui(UiEvent::Identity {
            nickname: self.state.nickname.clone(),
            npub: keys.npub(),
        });
        self.emit_settings();
        self.emit_location();
        self.emit_channel();
        self.ui(UiEvent::ClearTimeline);
        let timeline: Vec<PublicMessage> = self
            .channel
            .iter()
            .flat_map(|c| c.timeline.iter().cloned())
            .collect();
        for message in timeline {
            self.ui(UiEvent::Public { message });
        }
        self.emit_people((now_ms / 1000) as i64);
        self.emit_conversations();
        if let Some(peer) = self.open_dm.clone() {
            let messages = self
                .dms
                .get(&peer)
                .map(|c| c.messages.clone())
                .unwrap_or_default();
            let display = self.display_for(&peer);
            self.ui(UiEvent::DmOpened {
                peer: Some(peer),
                display,
                messages,
            });
        }
        self.emit_notes(now_ms);
    }

    pub fn emit_settings(&mut self) {
        let event = UiEvent::Settings {
            nickname: self.state.nickname.clone(),
            tor_enabled: self.state.tor_enabled,
            custom_relays: self.state.custom_relays.clone(),
            key_in_keychain: self.key_in_keychain,
            has_location: self.fix.is_some(),
        };
        self.ui(event);
    }

    fn emit_channel(&mut self) {
        let event = match &self.channel {
            Some(c) => UiEvent::Channel {
                geohash: Some(c.geohash.clone()),
                level: Some(level_name(Level::for_length(c.geohash.len())).to_string()),
                teleported: c.teleported,
            },
            None => UiEvent::Channel {
                geohash: None,
                level: None,
                teleported: false,
            },
        };
        self.ui(event);
    }

    // ---- location ----------------------------------------------------

    /// Sets or clears the OS location fix. Regional channels are the
    /// geohashes of the fix at every level.
    pub fn set_location(&mut self, fix: Option<(f64, f64)>, now_ms: u64) {
        self.fix = fix.map(|(lat, lon)| Fix {
            regional: Level::ALL
                .iter()
                .map(|l| (l.precision(), geohash::encode(lat, lon, l.precision())))
                .collect(),
        });
        self.subscribe_notes(now_ms);
        self.emit_location();
        self.emit_settings();
    }

    fn emit_location(&mut self) {
        // The building level is left out of the picker, as on iOS.
        let channels = self
            .regional_channels()
            .into_iter()
            .filter(|(l, _)| *l != Level::Building)
            .map(|(l, g)| RegionalChannel {
                level: level_name(l).to_string(),
                geohash: g,
            })
            .collect();
        self.ui(UiEvent::Location { channels });
    }

    /// Geohashes for the current location, coarsest first.
    pub fn regional_channels(&self) -> Vec<(Level, String)> {
        match &self.fix {
            Some(f) => Level::ALL
                .iter()
                .filter_map(|l| f.regional.get(&l.precision()).map(|g| (*l, g.clone())))
                .collect(),
            None => Vec::new(),
        }
    }

    fn is_regional(&self, geohash: &str) -> bool {
        self.fix
            .as_ref()
            .is_some_and(|f| f.regional.values().any(|g| g == geohash))
    }

    // ---- channels ----------------------------------------------------

    /// Joins a location channel. `manual` marks a typed or picked geohash,
    /// which iOS always treats as teleported.
    pub fn join_channel(&mut self, raw: &str, manual: bool, now_ms: u64) -> bool {
        let gh = raw.trim().trim_start_matches('#').to_lowercase();
        if !geohash::is_valid(&gh) {
            self.system(format!("'{raw}' is not a valid geohash"));
            return false;
        }
        self.leave_channel();
        let now = (now_ms / 1000) as i64;
        let keys = self.state.identity.geohash_keys(&gh);
        let relays = self.directory.closest_to_geohash(&gh, RELAYS_PER_CHANNEL);
        let teleported = manual || !self.is_regional(&gh);

        self.effects.push(Effect::Subscribe {
            id: geo_sub(&gh),
            filter: Filter::geohash_ephemeral(
                &gh,
                Some(now - GEOHASH_LOOKBACK_SECS),
                GEOHASH_HISTORY_LIMIT,
            ),
            relays,
        });
        self.effects.push(Effect::Subscribe {
            id: dm_sub(&gh),
            filter: Filter::gift_wraps_for(&keys.public_key_hex(), Some(now - DM_LOOKBACK_SECS)),
            relays: self.state.dm_relays(),
        });

        let mut channel = Channel {
            geohash: gh.clone(),
            teleported,
            keys,
            replaying: true,
            timeline: VecDeque::new(),
            participants: HashMap::new(),
            nicknames: HashMap::new(),
            recent_content: HashMap::new(),
        };
        channel.participants.insert(
            channel.keys.public_key_hex(),
            Participant {
                last_seen: now,
                teleported,
            },
        );
        self.channel = Some(channel);
        self.seen = Seen::default();
        self.ui(UiEvent::ClearTimeline);
        self.emit_channel();
        self.emit_people(now);
        true
    }

    pub fn leave_channel(&mut self) {
        if let Some(c) = self.channel.take() {
            self.effects.push(Effect::Unsubscribe {
                id: geo_sub(&c.geohash),
            });
            self.effects.push(Effect::Unsubscribe {
                id: dm_sub(&c.geohash),
            });
            self.emit_channel();
        }
    }

    fn display_for(&self, pubkey: &str) -> String {
        let nick = self
            .channel
            .as_ref()
            .and_then(|c| c.nicknames.get(pubkey))
            .map(String::as_str);
        display_name(nick, pubkey)
    }

    /// People seen in the last five minutes, not blocked.
    fn visible_people(&self, now: i64) -> Vec<Person> {
        let Some(c) = &self.channel else {
            return Vec::new();
        };
        let me = c.keys.public_key_hex();
        let mut people: Vec<Person> = c
            .participants
            .iter()
            .filter(|(pk, p)| {
                now - p.last_seen <= ONLINE_WINDOW_SECS && !self.state.blocked.contains(*pk)
            })
            .map(|(pk, p)| Person {
                pubkey: pk.clone(),
                display: if *pk == me {
                    display_name(Some(&self.state.nickname), pk)
                } else {
                    display_name(c.nicknames.get(pk).map(String::as_str), pk)
                },
                teleported: p.teleported,
                me: *pk == me,
            })
            .collect();
        // Nearby first, then teleported, alphabetical within each.
        people.sort_by(|a, b| {
            a.teleported
                .cmp(&b.teleported)
                .then_with(|| a.display.cmp(&b.display))
        });
        people
    }

    pub fn emit_people(&mut self, now: i64) {
        let Some(c) = &self.channel else { return };
        let geohash = c.geohash.clone();
        let people = self.visible_people(now);
        let others = people.iter().filter(|p| !p.me).count();
        let unknown = geohash.len() >= 6 && others == 0;
        self.ui(UiEvent::People {
            geohash,
            count: people.len(),
            unknown,
            people,
        });
    }

    // ---- inbound -----------------------------------------------------

    pub fn on_pool_event(&mut self, event: PoolEvent, now_ms: u64) {
        match event {
            PoolEvent::Event {
                subscription,
                event,
                ..
            } => {
                let Some(gh) = self.current_geohash().map(str::to_string) else {
                    if subscription == NOTES_SUB {
                        self.on_note(event, now_ms);
                    }
                    return;
                };
                if subscription == geo_sub(&gh) {
                    self.on_geo_event(event, now_ms);
                } else if subscription == dm_sub(&gh) {
                    self.on_gift_wrap(event, now_ms);
                } else if subscription == NOTES_SUB {
                    self.on_note(event, now_ms);
                }
            }
            PoolEvent::Eose { subscription } => {
                if let Some(c) = &mut self.channel
                    && subscription == geo_sub(&c.geohash)
                {
                    c.replaying = false;
                }
            }
            PoolEvent::Status { relay, status } => {
                self.ui(UiEvent::Relay {
                    url: relay,
                    status: format!("{status:?}").to_lowercase(),
                });
            }
            PoolEvent::Ok {
                relay,
                accepted,
                message,
                ..
            } => {
                if !accepted {
                    log::debug!("relay {relay} rejected an event: {message}");
                }
            }
        }
    }

    fn on_geo_event(&mut self, event: Event, now_ms: u64) {
        let now = (now_ms / 1000) as i64;
        if event.kind != kind::EPHEMERAL && event.kind != kind::GEOHASH_PRESENCE {
            return;
        }
        let pk = event.pubkey.to_lowercase();
        if self.state.blocked.contains(&pk) {
            return;
        }
        let Some(c) = self.channel.as_mut() else {
            return;
        };
        if event.tag_value("g") != Some(c.geohash.as_str()) || !self.seen.insert(&event.id) {
            return;
        }
        let own = pk == c.keys.public_key_hex();
        if own && now - event.created_at < OWN_ECHO_SECS {
            return;
        }
        if let Some(n) = event.tag_value("n")
            && (c.nicknames.len() < NICKNAME_CAP || c.nicknames.contains_key(&pk))
        {
            c.nicknames.insert(pk.clone(), n.to_string());
        }
        let seen_at = if c.replaying {
            event.created_at.min(now)
        } else {
            now
        };
        let teleported = event
            .tags
            .iter()
            .any(|t| t.len() >= 2 && t[0] == "t" && t[1] == "teleport");
        let p = c.participants.entry(pk.clone()).or_default();
        p.last_seen = p.last_seen.max(seen_at);
        if now - seen_at <= ONLINE_WINDOW_SECS {
            p.teleported = teleported;
        }

        if event.kind == kind::GEOHASH_PRESENCE {
            return;
        }
        let content = event.content.clone();
        if content.trim().is_empty() || content.chars().count() > MAX_PUBLIC_CHARS {
            return;
        }
        let pow_bits = pow::validated_difficulty(&event.id, &event.tags);
        let key = content_key(&content);
        if !self.rate.allow(&pk, &key, pow_bits, now_ms as f64 / 1000.0) {
            return;
        }
        let timestamp = (event.created_at.min(now).max(0) as u64) * 1000;
        let Some(c) = self.channel.as_mut() else {
            return;
        };
        if let Some(&prev) = c.recent_content.get(&key)
            && prev.abs_diff(timestamp) < DUPLICATE_WINDOW_MS
        {
            return;
        }
        c.recent_content.insert(key, timestamp);
        let message = PublicMessage {
            id: event.id.clone(),
            pubkey: pk.clone(),
            display: if own {
                display_name(Some(&self.state.nickname), &pk)
            } else {
                self.display_for(&pk)
            },
            content: content.clone(),
            timestamp,
            own,
            teleported,
            emote: is_emote(&content),
        };
        self.push_timeline(message);
    }

    fn push_timeline(&mut self, message: PublicMessage) {
        let Some(c) = self.channel.as_mut() else {
            return;
        };
        let pos = c
            .timeline
            .iter()
            .rposition(|m| m.timestamp <= message.timestamp)
            .map_or(0, |i| i + 1);
        c.timeline.insert(pos, message.clone());
        while c.timeline.len() > TIMELINE_CAP {
            c.timeline.pop_front();
        }
        self.ui(UiEvent::Public { message });
    }

    fn on_gift_wrap(&mut self, wrap: Event, now_ms: u64) {
        if !self.seen_wraps.insert(&wrap.id) {
            return;
        }
        let Some(c) = &self.channel else { return };
        let keys = c.keys.clone();
        let geohash = c.geohash.clone();
        let Ok(opened) = envelope::open_private_message(&wrap, &keys) else {
            return;
        };
        if !envelope::is_plausible_rumor_timestamp(opened.timestamp, (now_ms / 1000) as i64) {
            return;
        }
        let Some((_, item)) = embedded::decode(&opened.content) else {
            return;
        };
        let peer = opened.sender.to_lowercase();
        match item {
            Embedded::PrivateMessage {
                message_id,
                content,
            } => {
                if self.state.blocked.contains(&peer) || peer == keys.public_key_hex() {
                    return;
                }
                let is_open = self.open_dm.as_deref() == Some(peer.as_str());
                let conv = self.dms.entry(peer.clone()).or_default();
                conv.geohash = geohash;
                if conv
                    .messages
                    .iter()
                    .any(|m| m.id == message_id && !m.from_me)
                {
                    return;
                }
                let message = DmMessage {
                    id: message_id.clone(),
                    from_me: false,
                    content,
                    timestamp: (opened.timestamp.max(0) as u64) * 1000,
                    status: None,
                };
                conv.messages.push(message.clone());
                if !is_open {
                    conv.unread += 1;
                }
                self.acks.push_back(PendingAck {
                    keys: keys.clone(),
                    peer: peer.clone(),
                    item: Embedded::Delivered {
                        message_id: message_id.clone(),
                    },
                });
                if is_open {
                    self.acks.push_back(PendingAck {
                        keys,
                        peer: peer.clone(),
                        item: Embedded::ReadReceipt { message_id },
                    });
                }
                self.ui(UiEvent::Dm { peer, message });
                self.emit_conversations();
            }
            Embedded::Delivered { message_id } => {
                self.update_status(&peer, &message_id, DmStatus::Delivered)
            }
            Embedded::ReadReceipt { message_id } => {
                self.update_status(&peer, &message_id, DmStatus::Read)
            }
        }
    }

    fn update_status(&mut self, peer: &str, id: &str, status: DmStatus) {
        let Some(conv) = self.dms.get_mut(peer) else {
            return;
        };
        let Some(m) = conv.messages.iter_mut().find(|m| m.from_me && m.id == id) else {
            return;
        };
        if m.status.is_some_and(|s| s >= status) {
            return;
        }
        m.status = Some(status);
        self.ui(UiEvent::DmStatus {
            peer: peer.to_string(),
            id: id.to_string(),
            status,
        });
    }

    fn emit_conversations(&mut self) {
        let items = self
            .dms
            .iter()
            .map(|(peer, c)| ConversationSummary {
                peer: peer.clone(),
                display: self.display_for(peer),
                geohash: c.geohash.clone(),
                unread: c.unread,
                last: c.messages.last().map(|m| m.content.clone()),
            })
            .collect();
        self.ui(UiEvent::Conversations { items });
    }

    /// Next paced ack (one per tick, iOS paces at 0.35 s).
    pub fn next_ack(&mut self, now_ms: u64) {
        let Some(ack) = self.acks.pop_front() else {
            return;
        };
        let Some(content) = embedded::encode(&ack.item, Addressing::Geohash, now_ms) else {
            return;
        };
        let Ok(wrap) = envelope::create_private_message(
            &content,
            &ack.peer,
            &ack.keys,
            (now_ms / 1000) as i64,
        ) else {
            return;
        };
        let relays = self.state.dm_relays();
        self.effects.push(Effect::Publish {
            event: wrap,
            relays,
        });
    }

    pub fn pending_acks(&self) -> usize {
        self.acks.len()
    }

    // ---- outbound ----------------------------------------------------

    /// Sends text from the composer: a command, a DM in the open
    /// conversation, or a public message in the current channel.
    pub fn submit(&mut self, text: &str, now_ms: u64) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            crate::commands::run(self, text, now_ms);
            return;
        }
        match self.open_dm.clone() {
            Some(peer) => self.send_dm(&peer, text, now_ms),
            None => self.send_public(text, now_ms, true),
        }
    }

    /// Queues a kind-20000 message for mining. `echo` shows it in the
    /// timeline once sent; emotes pass `false` and show a system line instead.
    pub fn send_public(&mut self, text: &str, now_ms: u64, echo: bool) {
        let Some(c) = &self.channel else {
            self.system("join a location channel first");
            return;
        };
        let mut tags = vec![vec!["g".to_string(), c.geohash.clone()]];
        let nick = clean_nickname(&self.state.nickname);
        if !nick.is_empty() {
            tags.push(vec!["n".to_string(), nick]);
        }
        if c.teleported {
            tags.push(vec!["t".to_string(), "teleport".to_string()]);
        }
        let id = self.next_mine;
        self.next_mine += 1;
        let pending = PendingPublic {
            geohash: c.geohash.clone(),
            keys: c.keys.clone(),
            created_at: (now_ms / 1000) as i64,
            tags,
            content: text.to_string(),
            echo,
        };
        self.effects.push(Effect::Mine(MineRequest {
            id,
            pubkey: pending.keys.public_key_hex(),
            created_at: pending.created_at,
            tags: pending.tags.clone(),
            content: pending.content.clone(),
        }));
        self.mining.insert(id, pending);
    }

    /// Signs and publishes a mined public message.
    pub fn finish_public(&mut self, id: u64, nonce_tag: Vec<String>) {
        let Some(p) = self.mining.remove(&id) else {
            return;
        };
        let mut tags = p.tags;
        tags.push(nonce_tag);
        let event = Event::new(
            p.keys.public_key_hex(),
            p.created_at,
            kind::EPHEMERAL,
            tags,
            p.content.clone(),
        )
        .sign(&p.keys);
        let relays = self
            .directory
            .closest_to_geohash(&p.geohash, RELAYS_PER_CHANNEL);
        if relays.is_empty() {
            self.system("no geo relays for this channel; not sent");
            return;
        }
        self.effects.push(Effect::Publish {
            event: event.clone(),
            relays,
        });
        let on_channel = self.current_geohash() == Some(p.geohash.as_str());
        if !on_channel {
            return;
        }
        self.seen.insert(&event.id);
        if p.echo {
            let pk = event.pubkey.clone();
            let teleported = self.channel.as_ref().is_some_and(|c| c.teleported);
            self.push_timeline(PublicMessage {
                id: event.id.clone(),
                display: display_name(Some(&self.state.nickname), &pk),
                pubkey: pk,
                content: p.content.clone(),
                timestamp: (p.created_at as u64) * 1000,
                own: true,
                teleported,
                emote: is_emote(&p.content),
            });
        }
    }

    pub fn open_dm(&mut self, peer: Option<&str>) {
        let peer = peer.map(str::to_lowercase);
        self.open_dm = peer.clone();
        let Some(peer) = peer else {
            self.ui(UiEvent::DmOpened {
                peer: None,
                display: String::new(),
                messages: Vec::new(),
            });
            return;
        };
        let keys = self.channel.as_ref().map(|c| c.keys.clone());
        let conv = self.dms.entry(peer.clone()).or_default();
        if conv.geohash.is_empty() {
            conv.geohash = self
                .channel
                .as_ref()
                .map(|c| c.geohash.clone())
                .unwrap_or_default();
        }
        let unread: Vec<String> = if conv.unread > 0 {
            conv.messages
                .iter()
                .filter(|m| !m.from_me)
                .rev()
                .take(conv.unread)
                .map(|m| m.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        conv.unread = 0;
        let messages = conv.messages.clone();
        if let Some(keys) = keys {
            for message_id in unread.into_iter().rev() {
                self.acks.push_back(PendingAck {
                    keys: keys.clone(),
                    peer: peer.clone(),
                    item: Embedded::ReadReceipt { message_id },
                });
            }
        }
        let display = self.display_for(&peer);
        self.ui(UiEvent::DmOpened {
            peer: Some(peer),
            display,
            messages,
        });
        self.emit_conversations();
    }

    pub fn open_dm_peer(&self) -> Option<&str> {
        self.open_dm.as_deref()
    }

    pub fn send_dm(&mut self, peer: &str, text: &str, now_ms: u64) {
        let Some(c) = &self.channel else {
            self.system("you're not in a location channel — join one to send private messages");
            return;
        };
        let peer = peer.to_lowercase();
        if self.state.blocked.contains(&peer) {
            self.system("you blocked this person — /unblock them first");
            return;
        }
        if peer == c.keys.public_key_hex() {
            self.system("that's you");
            return;
        }
        if text.len() > PRIVATE_MESSAGE_FIELD_MAX {
            self.system(format!(
                "message too long for a private message ({} of {PRIVATE_MESSAGE_FIELD_MAX} bytes)",
                text.len()
            ));
            return;
        }
        let keys = c.keys.clone();
        let geohash = c.geohash.clone();
        let id = new_message_id();
        let item = Embedded::PrivateMessage {
            message_id: id.clone(),
            content: text.to_string(),
        };
        let Some(content) = embedded::encode(&item, Addressing::Geohash, now_ms) else {
            return;
        };
        let Ok(wrap) =
            envelope::create_private_message(&content, &peer, &keys, (now_ms / 1000) as i64)
        else {
            self.system("could not encrypt the message");
            return;
        };
        let relays = self.state.dm_relays();
        self.effects.push(Effect::Publish {
            event: wrap,
            relays,
        });
        let message = DmMessage {
            id,
            from_me: true,
            content: text.to_string(),
            timestamp: now_ms,
            status: Some(DmStatus::Sent),
        };
        let conv = self.dms.entry(peer.clone()).or_default();
        conv.geohash = geohash;
        conv.messages.push(message.clone());
        self.ui(UiEvent::Dm { peer, message });
        self.emit_conversations();
    }

    pub fn clear_current(&mut self) {
        if let Some(peer) = self.open_dm.clone() {
            if let Some(c) = self.dms.get_mut(&peer) {
                c.messages.clear();
            }
            self.ui(UiEvent::ClearDm { peer });
        } else if let Some(c) = self.channel.as_mut() {
            c.timeline.clear();
            self.ui(UiEvent::ClearTimeline);
        }
    }

    // ---- people lookups for commands ---------------------------------

    /// `/msg` target lookup: exact `nick#abcd` among visible people, else a
    /// unique case-insensitive nickname match.
    pub fn resolve_nickname(&self, raw: &str, now_ms: u64) -> Option<String> {
        let name = clean_nickname(raw.trim_start_matches('@'));
        let people = self.visible_people((now_ms / 1000) as i64);
        if name.contains('#') {
            let hits: Vec<_> = people.iter().filter(|p| p.display == name).collect();
            return (hits.len() == 1).then(|| hits[0].pubkey.clone());
        }
        let c = self.channel.as_ref()?;
        let lower = name.to_lowercase();
        let hits: Vec<&String> = c
            .nicknames
            .iter()
            .filter(|(pk, n)| n.to_lowercase() == lower && !self.state.blocked.contains(*pk))
            .map(|(pk, _)| pk)
            .collect();
        (hits.len() == 1).then(|| hits[0].clone())
    }

    /// `/block` lookup: exact display name among visible people, then an
    /// exact raw nickname.
    pub fn pubkey_for_display_name(&self, raw: &str, now_ms: u64) -> Option<String> {
        let name = raw.trim_start_matches('@');
        if let Some(p) = self
            .visible_people((now_ms / 1000) as i64)
            .into_iter()
            .find(|p| p.display == name)
        {
            return Some(p.pubkey);
        }
        let c = self.channel.as_ref()?;
        c.nicknames
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(pk, _)| pk.clone())
    }

    pub fn people_names(&self, now_ms: u64) -> Vec<String> {
        self.visible_people((now_ms / 1000) as i64)
            .into_iter()
            .filter(|p| !p.me)
            .map(|p| p.display)
            .collect()
    }

    pub fn display_of(&self, pubkey: &str) -> String {
        self.display_for(pubkey)
    }

    pub fn in_channel(&self) -> bool {
        self.channel.is_some()
    }

    pub fn blocked_names(&self) -> Vec<String> {
        self.state
            .blocked
            .iter()
            .map(|pk| self.display_for(pk))
            .collect()
    }

    pub fn set_blocked(&mut self, pubkey: &str, blocked: bool) {
        let pk = pubkey.to_lowercase();
        if blocked {
            self.state.blocked.insert(pk);
        } else {
            self.state.blocked.remove(&pk);
        }
        self.effects.push(Effect::Save);
    }

    pub fn reply(&mut self, text: impl Into<String>) {
        self.system(text);
    }

    // ---- presence ----------------------------------------------------

    /// Presence heartbeats for the coarse levels of the current location
    /// (never for teleported channels). The runtime spaces them out.
    pub fn presence_events(&self, now_ms: u64) -> Vec<(Event, Vec<String>)> {
        let Some(fix) = &self.fix else {
            return Vec::new();
        };
        let now = (now_ms / 1000) as i64;
        PRESENCE_PRECISIONS
            .iter()
            .filter_map(|p| fix.regional.get(p))
            .map(|gh| {
                let keys = self.state.identity.geohash_keys(gh);
                let event = Event::new(
                    keys.public_key_hex(),
                    now,
                    kind::GEOHASH_PRESENCE,
                    vec![vec!["g".into(), gh.clone()]],
                    String::new(),
                )
                .sign(&keys);
                (
                    event,
                    self.directory.closest_to_geohash(gh, RELAYS_PER_CHANNEL),
                )
            })
            .collect()
    }

    // ---- location notes ----------------------------------------------

    fn subscribe_notes(&mut self, now_ms: u64) {
        self.notes.clear();
        let Some(fix) = &self.fix else {
            self.effects.push(Effect::Unsubscribe {
                id: NOTES_SUB.into(),
            });
            return;
        };
        let Some(center) = fix.regional.get(&Level::Building.precision()).cloned() else {
            return;
        };
        let mut cells = vec![center.clone()];
        cells.extend(geohash::neighbors(&center));
        self.effects.push(Effect::Subscribe {
            id: NOTES_SUB.into(),
            filter: Filter::geohash_notes(&cells, None, 200),
            relays: self
                .directory
                .closest_to_geohash(&center, RELAYS_PER_CHANNEL),
        });
        self.emit_notes(now_ms);
    }

    /// `/drop`: a building-level kind-1 note that expires in 24 hours.
    pub fn drop_note(&mut self, text: &str, now_ms: u64) -> Result<(), &'static str> {
        let Some(fix) = &self.fix else {
            return Err("leaving a note needs location — turn it on in settings");
        };
        let Some(gh) = fix.regional.get(&Level::Building.precision()).cloned() else {
            return Err("still finding this place — try again in a moment");
        };
        let relays = self.directory.closest_to_geohash(&gh, RELAYS_PER_CHANNEL);
        if relays.is_empty() {
            return Err("no geo relays reachable — note not left");
        }
        let now = (now_ms / 1000) as i64;
        let keys = self.state.identity.geohash_keys(&gh);
        let mut tags = vec![vec!["g".to_string(), gh.clone()]];
        let nick = clean_nickname(&self.state.nickname);
        if !nick.is_empty() {
            tags.push(vec!["n".to_string(), nick]);
        }
        tags.push(vec![
            "expiration".to_string(),
            (now + NOTES_EXPIRY_SECS).to_string(),
        ]);
        let event = Event::new(
            keys.public_key_hex(),
            now,
            kind::TEXT_NOTE,
            tags,
            text.to_string(),
        )
        .sign(&keys);
        self.effects.push(Effect::Publish {
            event: event.clone(),
            relays,
        });
        self.on_note(event, now_ms);
        Ok(())
    }

    fn on_note(&mut self, event: Event, now_ms: u64) {
        if event.kind != kind::TEXT_NOTE
            || self.state.blocked.contains(&event.pubkey.to_lowercase())
        {
            return;
        }
        if self.notes.iter().any(|(e, _)| e.id == event.id) {
            return;
        }
        let Some(gh) = event.tag_value("g").map(str::to_string) else {
            return;
        };
        self.notes.push((event, gh));
        self.emit_notes(now_ms);
    }

    fn emit_notes(&mut self, now_ms: u64) {
        let Some(center) = self
            .fix
            .as_ref()
            .and_then(|f| f.regional.get(&Level::Building.precision()).cloned())
        else {
            return;
        };
        let now = (now_ms / 1000) as i64;
        let own: HashSet<String> = std::iter::once(center.clone())
            .chain(geohash::neighbors(&center))
            .map(|g| self.state.identity.geohash_keys(&g).public_key_hex())
            .collect();
        self.notes.retain(|(e, _)| {
            e.tag_value("expiration")
                .and_then(|v| v.parse::<i64>().ok())
                .is_none_or(|exp| exp > now)
        });
        self.notes
            .sort_by_key(|(e, _)| std::cmp::Reverse(e.created_at));
        self.notes.truncate(NOTES_CAP);
        let notes = self
            .notes
            .iter()
            .map(|(e, _)| Note {
                id: e.id.clone(),
                display: display_name(e.tag_value("n"), &e.pubkey),
                content: e.content.clone(),
                timestamp: (e.created_at.max(0) as u64) * 1000,
                own: own.contains(&e.pubkey),
            })
            .collect();
        self.ui(UiEvent::Notes {
            geohash: center,
            notes,
        });
    }

    // ---- settings ----------------------------------------------------

    pub fn set_nickname(&mut self, raw: &str) {
        let nick = clean_nickname(raw);
        if nick.is_empty() {
            return;
        }
        self.state.nickname = nick;
        self.effects.push(Effect::Save);
        let keys = self.state.identity.nostr_keys();
        self.ui(UiEvent::Identity {
            nickname: self.state.nickname.clone(),
            npub: keys.npub(),
        });
        self.emit_settings();
    }

    pub fn set_tor_enabled(&mut self, enabled: bool) {
        self.state.tor_enabled = enabled;
        self.effects.push(Effect::Save);
        self.emit_settings();
    }

    /// Adds a hand-picked relay for private messages (at most 8).
    pub fn add_relay(&mut self, raw: &str, now_ms: u64) {
        let Some(url) = bitchat_nostr::relay_url::normalize(raw, Some("wss")) else {
            self.system("that doesn't look like a relay address. try wss://host.");
            return;
        };
        if self.state.dm_relays().contains(&url) {
            self.system(format!("{url} is already in the list"));
            return;
        }
        if self.state.custom_relays.len() >= crate::state::MAX_CUSTOM_RELAYS {
            self.system(format!(
                "at most {} extra relays",
                crate::state::MAX_CUSTOM_RELAYS
            ));
            return;
        }
        self.state.custom_relays.push(url);
        self.relays_changed(now_ms);
    }

    pub fn remove_relay(&mut self, url: &str, now_ms: u64) {
        let before = self.state.custom_relays.len();
        self.state.custom_relays.retain(|r| r != url);
        if self.state.custom_relays.len() != before {
            self.relays_changed(now_ms);
        }
    }

    fn relays_changed(&mut self, now_ms: u64) {
        self.effects.push(Effect::Save);
        if let Some(c) = &self.channel {
            let now = (now_ms / 1000) as i64;
            self.effects.push(Effect::Subscribe {
                id: dm_sub(&c.geohash),
                filter: Filter::gift_wraps_for(
                    &c.keys.public_key_hex(),
                    Some(now - DM_LOOKBACK_SECS),
                ),
                relays: self.state.dm_relays(),
            });
        }
        self.emit_settings();
    }
}
