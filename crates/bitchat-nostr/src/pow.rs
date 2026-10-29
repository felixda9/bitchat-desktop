//! NIP-13 proof of work for geohash messages (iOS `NostrPoW`).
//!
//! Outgoing kind-20000 messages mine `["nonce", <16 hex>, "<target>"]`.
//! Inbound events are only scored, never rejected for missing work.

use crate::event::serialize_for_id;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// Bits mined onto outgoing geohash messages.
pub const TARGET_BITS: u32 = 8;
/// Validated work at or above this skips the per-sender rate limit.
pub const RATE_LIMIT_BYPASS_BITS: u32 = 8;
/// Wall-clock cap for the first attempt; afterwards the target halves.
pub const MINING_TIME_CAP: Duration = Duration::from_secs(2);
const FALLBACK_TIME_CAP: Duration = Duration::from_millis(150);
const CHECK_INTERVAL: u64 = 1024;
const NONCE_LEN: usize = 16;

pub fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut total = 0;
    for &b in bytes {
        if b == 0 {
            total += 8;
        } else {
            total += b.leading_zeros();
            break;
        }
    }
    total
}

/// The committed target of the last `nonce` tag, if the id meets it; else 0.
/// Extra work beyond the commitment earns nothing.
pub fn validated_difficulty(id_hex: &str, tags: &[Vec<String>]) -> u32 {
    let Some(tag) = tags
        .iter()
        .rev()
        .find(|t| t.first().map(String::as_str) == Some("nonce"))
    else {
        return 0;
    };
    let Some(committed) = tag.get(2).and_then(|s| parse_swift_int(s)) else {
        return 0;
    };
    if !(1..=256).contains(&committed) {
        return 0;
    }
    let Ok(id) = hex::decode(id_hex) else {
        return 0;
    };
    let committed = committed as u32;
    if leading_zero_bits(&id) >= committed {
        committed
    } else {
        0
    }
}

/// Swift `Int(String)`: optional sign, ASCII digits only.
fn parse_swift_int(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Mines a nonce tag for these event fields. On timeout the committed target
/// halves (down to 0, which always succeeds), so sending is never blocked.
/// CPU-bound: run off the async runtime.
pub fn mine_nonce_tag(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
    target_bits: u32,
) -> Vec<String> {
    let mut target = target_bits.min(256);
    let mut budget = MINING_TIME_CAP;
    loop {
        if let Some(tag) = mine_attempt(pubkey, created_at, kind, tags, content, target, budget) {
            return tag;
        }
        target /= 2;
        budget = FALLBACK_TIME_CAP;
    }
}

fn mine_attempt(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    base_tags: &[Vec<String>],
    content: &str,
    target: u32,
    budget: Duration,
) -> Option<Vec<String>> {
    let target_str = target.to_string();
    let template = |placeholder: &str| {
        let mut tags = base_tags.to_vec();
        tags.push(vec!["nonce".into(), placeholder.into(), target_str.clone()]);
        serialize_for_id(pubkey, created_at, kind, &tags, content)
    };
    let zeros = template(&"0".repeat(NONCE_LEN));
    let effs = template(&"f".repeat(NONCE_LEN));
    let first = zeros.iter().zip(&effs).position(|(a, b)| a != b)?;
    let last = zeros.iter().zip(&effs).rposition(|(a, b)| a != b)?;
    if zeros.len() != effs.len() || last - first + 1 != NONCE_LEN {
        return None;
    }

    let mut buffer = zeros;
    let deadline = Instant::now() + budget;
    let mut nonce = u64::from_le_bytes(crate::keys::random_bytes());
    let mut attempts: u64 = 0;
    loop {
        let hex = format!("{nonce:016x}");
        buffer[first..first + NONCE_LEN].copy_from_slice(hex.as_bytes());
        if leading_zero_bits(&Sha256::digest(&buffer)) >= target {
            return Some(vec!["nonce".into(), hex, target_str]);
        }
        nonce = nonce.wrapping_add(1);
        attempts += 1;
        if attempts.is_multiple_of(CHECK_INTERVAL) && Instant::now() >= deadline {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Keys, event::kind};

    #[test]
    fn mined_event_meets_target() {
        let keys = Keys::generate();
        let tags = vec![vec!["g".to_string(), "u4pruy".to_string()]];
        let nonce = mine_nonce_tag(
            &keys.public_key_hex(),
            1_790_000_000,
            kind::EPHEMERAL,
            &tags,
            "hi",
            8,
        );
        assert_eq!(nonce[0], "nonce");
        assert_eq!(nonce[1].len(), 16);
        let mut all = tags;
        all.push(nonce);
        let event = Event::new(
            keys.public_key_hex(),
            1_790_000_000,
            kind::EPHEMERAL,
            all,
            "hi".into(),
        )
        .sign(&keys);
        assert!(validated_difficulty(&event.id, &event.tags) >= 8);
        assert!(event.verify());
    }

    #[test]
    fn zero_bits() {
        assert_eq!(leading_zero_bits(&[0, 0x0f]), 12);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0, 0]), 16);
    }
}
