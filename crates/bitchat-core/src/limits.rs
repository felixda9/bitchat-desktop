//! Inbound public-message limits (iOS `MessageRateLimiter`, `ContentNormalizer`).

use std::collections::HashMap;

pub const SENDER_CAPACITY: f64 = 5.0;
pub const SENDER_REFILL_PER_SEC: f64 = 1.0;
pub const CONTENT_CAPACITY: f64 = 3.0;
pub const CONTENT_REFILL_PER_SEC: f64 = 0.5;
pub const MAX_BUCKETS: usize = 2000;
const BUCKET_IDLE_SECS: f64 = 600.0;

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: f64,
}

impl Bucket {
    fn allow(&mut self, capacity: f64, refill: f64, now: f64) -> bool {
        self.tokens = (self.tokens + (now - self.last).max(0.0) * refill).min(capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

fn bucket(map: &mut HashMap<String, Bucket>, key: &str, capacity: f64, now: f64) -> Bucket {
    if let Some(b) = map.get(key) {
        return *b;
    }
    if map.len() >= MAX_BUCKETS {
        map.retain(|_, b| now - b.last < BUCKET_IDLE_SECS);
        if map.len() >= MAX_BUCKETS
            && let Some(oldest) = map
                .iter()
                .min_by(|a, b| a.1.last.total_cmp(&b.1.last))
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
    }
    Bucket {
        tokens: capacity,
        last: now,
    }
}

/// Per-sender and per-content token buckets. Validated proof of work of
/// 8+ bits skips the sender bucket only.
#[derive(Default)]
pub struct RateLimiter {
    senders: HashMap<String, Bucket>,
    contents: HashMap<String, Bucket>,
}

impl RateLimiter {
    pub fn allow(&mut self, sender: &str, content_key: &str, pow_bits: u32, now_secs: f64) -> bool {
        if pow_bits < bitchat_nostr::pow::RATE_LIMIT_BYPASS_BITS {
            let mut b = bucket(&mut self.senders, sender, SENDER_CAPACITY, now_secs);
            let ok = b.allow(SENDER_CAPACITY, SENDER_REFILL_PER_SEC, now_secs);
            self.senders.insert(sender.to_string(), b);
            if !ok {
                return false;
            }
        }
        let mut b = bucket(&mut self.contents, content_key, CONTENT_CAPACITY, now_secs);
        let ok = b.allow(CONTENT_CAPACITY, CONTENT_REFILL_PER_SEC, now_secs);
        self.contents.insert(content_key.to_string(), b);
        ok
    }

    pub fn reset(&mut self) {
        self.senders.clear();
        self.contents.clear();
    }
}

/// Near-duplicate key: lowercase, URL query/fragment dropped, whitespace
/// collapsed, first 256 characters.
pub fn content_key(content: &str) -> String {
    let lowered = content.to_lowercase();
    let mut simplified = String::with_capacity(lowered.len());
    let mut rest = lowered.as_str();
    while let Some(pos) = rest
        .find("http://")
        .into_iter()
        .chain(rest.find("https://"))
        .min()
    {
        simplified.push_str(&rest[..pos]);
        let url = &rest[pos..];
        let end = url.find(char::is_whitespace).unwrap_or(url.len());
        let token = &url[..end];
        let cut = token.find(['?', '#']).unwrap_or(token.len());
        simplified.push_str(&token[..cut]);
        rest = &url[end..];
    }
    simplified.push_str(rest);
    let collapsed = simplified.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_burst_then_refill() {
        let mut r = RateLimiter::default();
        for i in 0..5 {
            assert!(r.allow("a", &format!("m{i}"), 0, 0.0));
        }
        assert!(!r.allow("a", "m5", 0, 0.0));
        assert!(r.allow("a", "m6", 0, 1.0));
        // Proof of work skips the sender bucket.
        assert!(r.allow("a", "m7", 8, 1.0));
    }

    #[test]
    fn content_flood_across_senders() {
        let mut r = RateLimiter::default();
        assert!(r.allow("a", "spam", 8, 0.0));
        assert!(r.allow("b", "spam", 8, 0.0));
        assert!(r.allow("c", "spam", 8, 0.0));
        assert!(!r.allow("d", "spam", 8, 0.0));
        assert!(r.allow("d", "spam", 8, 2.0));
    }

    #[test]
    fn content_keys() {
        assert_eq!(content_key("  Hello   World "), "hello world");
        assert_eq!(
            content_key("see https://x.org/a?b=1#c now"),
            "see https://x.org/a now"
        );
    }
}
