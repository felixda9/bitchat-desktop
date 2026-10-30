//! Mesh bridge and gateway wire pieces (iOS `NostrCarrierPacket`,
//! `NostrProtocol.createBridgeMeshEvent`/`createBridgePresenceEvent`,
//! `MeshMessageIdentity`).
//!
//! A carrier is mesh packet type 0x28 holding a complete signed Nostr event:
//! TLV `[type u8][len BE16][value]`, 01 direction, 02 geohash, 03 event JSON.

use crate::event::kind;
use crate::{Event, Keys};
use sha2::{Digest, Sha256};

pub const MAX_EVENT_JSON_BYTES: usize = 16 * 1024;
pub const MAX_GEOHASH_LEN: usize = 12;
const GEOHASH_ALPHABET: &str = "0123456789bcdefghjkmnpqrstuvwxyz";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Mesh-only peer → gateway: publish this geohash event (directed).
    ToGateway = 0x01,
    /// Gateway → mesh: a geohash event from the relays (broadcast).
    FromGateway = 0x02,
    /// Mesh-only peer → bridge: publish this rendezvous event (directed).
    ToBridge = 0x03,
    /// Bridge → mesh: a rendezvous event from another island (broadcast).
    FromBridge = 0x04,
}

impl Direction {
    fn from_byte(b: u8) -> Option<Direction> {
        Some(match b {
            0x01 => Direction::ToGateway,
            0x02 => Direction::FromGateway,
            0x03 => Direction::ToBridge,
            0x04 => Direction::FromBridge,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carrier {
    pub direction: Direction,
    pub geohash: String,
    pub event_json: Vec<u8>,
}

impl Carrier {
    pub fn new(direction: Direction, geohash: &str, event: &Event) -> Option<Carrier> {
        let event_json = serde_json::to_vec(event).ok()?;
        if geohash.is_empty()
            || geohash.len() > MAX_GEOHASH_LEN
            || event_json.len() > MAX_EVENT_JSON_BYTES
        {
            return None;
        }
        Some(Carrier {
            direction,
            geohash: geohash.to_string(),
            event_json,
        })
    }

    /// The carried event, unverified: check `verify()` before using it.
    pub fn event(&self) -> Option<Event> {
        serde_json::from_slice(&self.event_json).ok()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.event_json.len() + self.geohash.len() + 12);
        let mut put = |t: u8, v: &[u8]| {
            out.push(t);
            out.extend_from_slice(&(v.len() as u16).to_be_bytes());
            out.extend_from_slice(v);
        };
        put(0x01, &[self.direction as u8]);
        put(0x02, self.geohash.as_bytes());
        put(0x03, &self.event_json);
        out
    }

    /// Unknown TLVs are skipped; trailing bytes or a bad direction reject.
    pub fn decode(data: &[u8]) -> Option<Carrier> {
        let mut off = 0;
        let (mut direction, mut geohash, mut event_json) = (None, None, None);
        while off + 3 <= data.len() {
            let t = data[off];
            let len = u16::from_be_bytes([data[off + 1], data[off + 2]]) as usize;
            off += 3;
            let v = data.get(off..off + len)?;
            off += len;
            match t {
                0x01 => {
                    if v.len() != 1 {
                        return None;
                    }
                    direction = Some(Direction::from_byte(v[0])?);
                }
                0x02 => geohash = Some(String::from_utf8(v.to_vec()).ok()?),
                0x03 => event_json = Some(v.to_vec()),
                _ => {}
            }
        }
        if off != data.len() {
            return None;
        }
        let (direction, geohash, event_json) = (direction?, geohash?, event_json?);
        if geohash.is_empty()
            || geohash.len() > MAX_GEOHASH_LEN
            || event_json.is_empty()
            || event_json.len() > MAX_EVENT_JSON_BYTES
        {
            return None;
        }
        Some(Carrier {
            direction,
            geohash,
            event_json,
        })
    }
}

/// 1–12 characters of the geohash alphabet.
pub fn is_valid_geohash(g: &str) -> bool {
    (1..=MAX_GEOHASH_LEN).contains(&g.len()) && g.chars().all(|c| GEOHASH_ALPHABET.contains(c))
}

/// Cross-device ID of a public mesh message: SHA-256 of
/// `lower(senderHex) | timestampMs | trimmed content`, first 32 hex chars.
pub fn stable_id(sender_hex: &str, timestamp_ms: u64, content: &str) -> String {
    let input = format!(
        "{}|{}|{}",
        sender_hex.to_lowercase(),
        timestamp_ms,
        content.trim()
    );
    hex::encode(&Sha256::digest(input.as_bytes())[..16])
}

/// A mesh message copied to a rendezvous cell (kind 20000): `r` cell, `n`
/// nickname, `m` [stable ID, mesh sender hex, mesh timestamp ms].
pub fn mesh_event(
    content: &str,
    cell: &str,
    keys: &Keys,
    nickname: &str,
    mesh_sender_hex: &str,
    mesh_timestamp_ms: u64,
    created_at: i64,
) -> Event {
    let mut tags = vec![vec!["r".to_string(), cell.to_string()]];
    let nick = nickname.trim();
    if !nick.is_empty() {
        tags.push(vec!["n".to_string(), nick.to_string()]);
    }
    tags.push(vec![
        "m".to_string(),
        stable_id(mesh_sender_hex, mesh_timestamp_ms, content),
        mesh_sender_hex.to_string(),
        mesh_timestamp_ms.to_string(),
    ]);
    Event::new(
        keys.public_key_hex(),
        created_at,
        kind::EPHEMERAL,
        tags,
        content.to_string(),
    )
    .sign(keys)
}

/// Bridge presence heartbeat (kind 20001): empty content, `r` tag only.
pub fn presence_event(cell: &str, keys: &Keys, created_at: i64) -> Event {
    Event::new(
        keys.public_key_hex(),
        created_at,
        kind::GEOHASH_PRESENCE,
        vec![vec!["r".to_string(), cell.to_string()]],
        String::new(),
    )
    .sign(keys)
}
