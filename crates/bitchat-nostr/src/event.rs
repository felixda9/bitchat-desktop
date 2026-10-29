//! Nostr events: NIP-01 ids and BIP-340 signatures (iOS `NostrEvent`).

use crate::keys::{self, Keys};
use secp256k1::schnorr;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod kind {
    pub const METADATA: u32 = 0;
    pub const TEXT_NOTE: u32 = 1;
    pub const DELETION: u32 = 5;
    /// Sender-signed seal inside a private envelope.
    pub const SEAL: u32 = 13;
    /// Unsigned inner message of a private envelope.
    pub const DM: u32 = 14;
    /// Public outer private envelope, signed by a one-time key.
    pub const GIFT_WRAP: u32 = 1059;
    pub const COURIER_DROP: u32 = 1401;
    /// Geohash channel message.
    pub const EPHEMERAL: u32 = 20000;
    /// Geohash presence heartbeat.
    pub const GEOHASH_PRESENCE: u32 = 20001;
}

/// Inbound tag limits (`TransportConfig.nostrMaxEventTags…`).
pub const MAX_TAGS: usize = 64;
pub const MAX_TAG_VALUES: usize = 16;
pub const MAX_TAG_VALUE_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

impl Event {
    /// Unsigned event with an empty id.
    pub fn new(
        pubkey: String,
        created_at: i64,
        kind: u32,
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Self {
        Event {
            id: String::new(),
            pubkey,
            created_at,
            kind,
            tags,
            content,
            sig: None,
        }
    }

    /// SHA-256 of the NIP-01 serialization `[0,pubkey,created_at,kind,tags,content]`.
    pub fn compute_id(&self) -> [u8; 32] {
        Sha256::digest(serialize_for_id(
            &self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &self.content,
        ))
        .into()
    }

    /// Sets the id and a BIP-340 signature with 32 random auxiliary bytes.
    pub fn sign(mut self, keys: &Keys) -> Event {
        let id = self.compute_id();
        let aux: [u8; 32] = keys::random_bytes();
        let sig = schnorr::sign_with_aux_rand(&id, keys.keypair(), &aux);
        self.id = hex::encode(id);
        self.sig = Some(hex::encode(sig.to_byte_array()));
        self
    }

    /// The id matches the content and the signature verifies against `pubkey`.
    pub fn verify(&self) -> bool {
        let Some(sig_hex) = &self.sig else {
            return false;
        };
        let Some(sig) = hex::decode(sig_hex)
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
        else {
            return false;
        };
        let Some(pubkey) = keys::xonly_from_hex(&self.pubkey) else {
            return false;
        };
        let id = self.compute_id();
        if hex::encode(id) != self.id {
            return false;
        }
        schnorr::verify(&schnorr::Signature::from_byte_array(sig), &id, &pubkey).is_ok()
    }

    pub fn within_tag_limits(&self) -> bool {
        within_tag_limits(&self.tags)
    }

    /// First value of the first tag named `name`.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1))
            .map(String::as_str)
    }
}

pub fn within_tag_limits(tags: &[Vec<String>]) -> bool {
    tags.len() <= MAX_TAGS
        && tags
            .iter()
            .all(|t| t.len() <= MAX_TAG_VALUES && t.iter().all(|v| v.len() <= MAX_TAG_VALUE_BYTES))
}

/// Canonical id serialization: no whitespace, `"` and `\` escaped, control
/// characters escaped (`\b \t \n \f \r`, otherwise `\u00XX`), `/` and all
/// other characters verbatim.
pub fn serialize_for_id(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> Vec<u8> {
    let mut out = String::with_capacity(96 + content.len());
    out.push_str("[0,");
    push_json_string(&mut out, pubkey);
    out.push(',');
    out.push_str(&created_at.to_string());
    out.push(',');
    out.push_str(&kind.to_string());
    out.push_str(",[");
    for (i, tag) in tags.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, value) in tag.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            push_json_string(&mut out, value);
        }
        out.push(']');
    }
    out.push_str("],");
    push_json_string(&mut out, content);
    out.push(']');
    out.into_bytes()
}

fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_shape() {
        let s = serialize_for_id("ab", 1, 20000, &[vec!["g".into(), "u4".into()]], "a/b\"c\n");
        assert_eq!(
            String::from_utf8(s).unwrap(),
            r#"[0,"ab",1,20000,[["g","u4"]],"a/b\"c\n"]"#
        );
    }

    #[test]
    fn sign_and_verify() {
        let keys = Keys::generate();
        let event = Event::new(
            keys.public_key_hex(),
            1_790_000_000,
            kind::EPHEMERAL,
            vec![],
            "hi".into(),
        )
        .sign(&keys);
        assert!(event.verify());
        let mut tampered = event.clone();
        tampered.content = "ho".into();
        assert!(!tampered.verify());
        let mut unsigned = event;
        unsigned.sig = None;
        assert!(!unsigned.verify());
    }
}
