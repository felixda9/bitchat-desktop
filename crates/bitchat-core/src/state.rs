//! Persisted state: identities and settings.

use bitchat_nostr::Keys;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

/// Most hand-added relays (`NostrRelaySettings`).
pub const MAX_CUSTOM_RELAYS: usize = 8;

/// Built-in relays for private messages (iOS `NostrRelayManager.builtInRelays`).
pub const BUILT_IN_DM_RELAYS: [&str; 4] = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://offchain.pub",
];

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| serde::de::Error::custom("expected 32 hex bytes"))
    }
}

/// Long-term secrets. The Noise and Ed25519 keys are created now so the
/// mesh peer ID stays the same once Bluetooth arrives.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    #[serde(with = "hex32")]
    pub noise_static: [u8; 32],
    #[serde(with = "hex32")]
    pub signing_seed: [u8; 32],
    #[serde(with = "hex32")]
    pub nostr_secret: [u8; 32],
    /// Seed for per-geohash Nostr identities.
    #[serde(with = "hex32")]
    pub device_seed: [u8; 32],
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Identity { .. }")
    }
}

fn random32() -> [u8; 32] {
    let mut out = [0u8; 32];
    getrandom::fill(&mut out).expect("OS random number generator failed");
    out
}

impl Identity {
    pub fn generate() -> Identity {
        Identity {
            noise_static: random32(),
            signing_seed: random32(),
            nostr_secret: Keys::generate().secret_bytes(),
            device_seed: random32(),
        }
    }

    pub fn nostr_keys(&self) -> Keys {
        Keys::from_secret_bytes(self.nostr_secret).expect("stored key is valid")
    }

    pub fn geohash_keys(&self, geohash: &str) -> Keys {
        bitchat_nostr::geo_keys::derive(&self.device_seed, geohash)
    }
}

/// A mesh contact one of us favorited (iOS favorites). Favorites who gave us
/// their Nostr key can be messaged over the internet when out of range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    #[serde(with = "hex32")]
    pub noise_public: [u8; 32],
    pub nickname: String,
    /// Their Nostr key (lowercase hex), from their `[FAVORITED]:npub` message.
    pub nostr_pubkey: Option<String>,
    /// We favorited them.
    pub ours: bool,
    /// They favorited us.
    pub theirs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub identity: Identity,
    pub nickname: String,
    /// Tor routing for all relay traffic (on by default, as on iOS).
    pub tor_enabled: bool,
    pub custom_relays: Vec<String>,
    pub bookmarks: Vec<String>,
    /// Blocked Nostr pubkeys (lowercase hex).
    pub blocked: BTreeSet<String>,
    /// Last joined channel, rejoined at start (wiped with everything else).
    #[serde(default)]
    pub last_channel: Option<String>,
    #[serde(default)]
    pub last_channel_manual: bool,
    #[serde(default)]
    pub favorites: Vec<Favorite>,
}

pub fn random_nickname() -> String {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).expect("OS random number generator failed");
    format!("anon{}", 1000 + u16::from_le_bytes(b) % 9000)
}

/// Trimmed and NFC-normalized, as iOS stores nicknames.
pub fn clean_nickname(raw: &str) -> String {
    raw.trim().nfc().collect()
}

impl State {
    pub fn fresh() -> State {
        State {
            identity: Identity::generate(),
            nickname: random_nickname(),
            tor_enabled: true,
            custom_relays: Vec::new(),
            bookmarks: Vec::new(),
            blocked: BTreeSet::new(),
            last_channel: None,
            last_channel_manual: false,
            favorites: Vec::new(),
        }
    }

    /// Relays that carry private messages: built-ins plus hand-added ones.
    pub fn dm_relays(&self) -> Vec<String> {
        let mut out: Vec<String> = BUILT_IN_DM_RELAYS.iter().map(|s| s.to_string()).collect();
        for r in &self.custom_relays {
            if !out.contains(r) {
                out.push(r.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nicknames() {
        let n = random_nickname();
        assert!(n.starts_with("anon") && n.len() == 8);
        assert_eq!(clean_nickname("  e\u{301}ve "), "\u{e9}ve");
    }

    #[test]
    fn json_round_trip() {
        let s = State::fresh();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), s);
        assert_eq!(s.dm_relays().len(), 4);
    }
}
