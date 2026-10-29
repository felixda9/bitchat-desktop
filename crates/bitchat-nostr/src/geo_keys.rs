//! Per-geohash Nostr identities (iOS `NostrIdentityBridge.deriveIdentity`).
//!
//! Each location channel gets its own key so personas cannot be linked:
//! `HMAC-SHA256(key = device seed, msg = utf8(geohash) || BE32(i))` for
//! i = 0..9 until the output is a valid scalar, falling back to
//! `SHA-256(seed || geohash)`.

use crate::Keys;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

pub fn derive(seed: &[u8; 32], geohash: &str) -> Keys {
    let msg = geohash.as_bytes();
    for i in 0u32..10 {
        let mut mac =
            <Hmac<Sha256> as KeyInit>::new_from_slice(seed).expect("HMAC takes any key length");
        mac.update(msg);
        mac.update(&i.to_be_bytes());
        let candidate: [u8; 32] = mac.finalize().into_bytes().into();
        if let Some(keys) = Keys::from_secret_bytes(candidate) {
            return keys;
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(seed);
    hasher.update(msg);
    Keys::from_secret_bytes(hasher.finalize().into()).expect("SHA-256 output is a valid scalar")
}

/// Identity for a mesh-bridge rendezvous cell, kept apart from the chat identity.
pub fn derive_bridge(seed: &[u8; 32], cell: &str) -> Keys {
    derive(seed, &format!("bridge|{cell}"))
}
