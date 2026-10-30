//! Events sent to the UI.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicMessage {
    pub id: String,
    pub pubkey: String,
    /// `nick#abcd`
    pub display: String,
    pub content: String,
    /// Unix milliseconds.
    pub timestamp: u64,
    pub own: bool,
    pub teleported: bool,
    /// A `* … *` hug/slap action, rendered like a system line.
    pub emote: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Person {
    pub pubkey: String,
    pub display: String,
    pub teleported: bool,
    pub me: bool,
    /// A mesh peer we favorited.
    pub favorite: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FavoriteSummary {
    /// `mesh:<peer id>`
    pub peer: String,
    pub display: String,
    /// Around on Bluetooth right now.
    pub online: bool,
    /// We have their Nostr key, so DMs work when they're away.
    pub reachable: bool,
    pub ours: bool,
    pub theirs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DmStatus {
    Sent,
    Delivered,
    Read,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DmMessage {
    pub id: String,
    pub from_me: bool,
    pub content: String,
    pub timestamp: u64,
    pub status: Option<DmStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConversationSummary {
    pub peer: String,
    pub display: String,
    pub favorite: bool,
    pub geohash: String,
    pub unread: usize,
    pub last: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Note {
    pub id: String,
    pub display: String,
    pub content: String,
    pub timestamp: u64,
    pub own: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RegionalChannel {
    pub level: String,
    pub geohash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    /// Channels for the current location fix (empty without one).
    Location {
        channels: Vec<RegionalChannel>,
    },
    Identity {
        nickname: String,
        npub: String,
    },
    Channel {
        geohash: Option<String>,
        level: Option<String>,
        teleported: bool,
    },
    Public {
        message: PublicMessage,
    },
    /// A local line in the current view (command replies, notices).
    System {
        text: String,
    },
    People {
        geohash: String,
        count: usize,
        /// High-precision channel with nobody seen: shown as `[? people]`.
        unknown: bool,
        people: Vec<Person>,
    },
    Conversations {
        items: Vec<ConversationSummary>,
    },
    DmOpened {
        peer: Option<String>,
        display: String,
        messages: Vec<DmMessage>,
    },
    Dm {
        peer: String,
        message: DmMessage,
    },
    DmStatus {
        peer: String,
        id: String,
        status: DmStatus,
    },
    ClearTimeline,
    ClearDm {
        peer: String,
    },
    Notes {
        geohash: String,
        notes: Vec<Note>,
    },
    Tor {
        state: String,
        progress: f32,
        summary: String,
    },
    Relay {
        url: String,
        status: String,
    },
    Settings {
        nickname: String,
        tor_enabled: bool,
        custom_relays: Vec<String>,
        key_in_keychain: bool,
        has_location: bool,
    },
    Wiped,
    /// Bluetooth for #mesh: `on`, `off` or `unavailable`, and live links.
    Mesh {
        state: String,
        links: usize,
    },
    Favorites {
        items: Vec<FavoriteSummary>,
    },
}
