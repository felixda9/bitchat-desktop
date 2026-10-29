//! Geo relay directory (iOS `GeoRelayDirectory`): which relays carry a
//! geohash channel. Every client must pick the same relays for a cell, so
//! the list, validation and tie-breaking follow iOS exactly.

use std::collections::{BTreeMap, BTreeSet};

/// Snapshot of upstream `relays/online_relays_gps.csv` at the pinned commit.
pub const BUNDLED_CSV: &str = include_str!("../data/online_relays_gps.csv");

/// Where iOS refreshes the list from (daily, over Tor).
pub const REMOTE_URL: &str = "https://raw.githubusercontent.com/permissionlesstech/bitchat/refs/heads/main/relays/online_relays_gps.csv";

/// Relays per geohash channel (`nostrGeoRelayCount`).
pub const RELAYS_PER_CHANNEL: usize = 5;

pub const MAX_BYTES: usize = 512 * 1024;
pub const MAX_ROWS: usize = 5_000;
pub const MAX_ENTRIES: usize = 5_000;
pub const MIN_REMOTE_ENTRIES: usize = 50;
/// A refresh must keep at least this share of the current entries.
pub const MIN_RETAINED_FRACTION: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Host, with `:port` when not 443.
    pub host: String,
    pub lat: f64,
    pub lon: f64,
}

impl Entry {
    fn key(&self) -> (String, u64, u64) {
        (self.host.clone(), self.lat.to_bits(), self.lon.to_bits())
    }
}

#[derive(Clone, Debug)]
pub struct Directory {
    entries: Vec<Entry>,
}

impl Default for Directory {
    fn default() -> Self {
        Directory::bundled()
    }
}

impl Directory {
    pub fn bundled() -> Directory {
        Directory {
            entries: validate(BUNDLED_CSV.as_bytes(), 1, None)
                .expect("bundled relay list is valid"),
        }
    }

    pub fn from_entries(entries: Vec<Entry>) -> Directory {
        Directory { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Validates a downloaded CSV against this directory; `None` rejects it.
    pub fn accept_update(&self, csv: &[u8]) -> Option<Directory> {
        validate(csv, MIN_REMOTE_ENTRIES, Some(&self.entries)).map(Directory::from_entries)
    }

    /// Up to `count` relays nearest the geohash cell centre, ties broken by host.
    pub fn closest_to_geohash(&self, geohash: &str, count: usize) -> Vec<String> {
        let (lat, lon) = bitchat_proto::geohash::decode_center(geohash);
        self.closest(lat, lon, count)
    }

    pub fn closest(&self, lat: f64, lon: f64, count: usize) -> Vec<String> {
        let mut ranked: Vec<(f64, &str)> = self
            .entries
            .iter()
            .map(|e| (haversine_km(lat, lon, e.lat, e.lon), e.host.as_str()))
            .collect();
        ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        ranked
            .into_iter()
            .take(count)
            .map(|(_, h)| format!("wss://{h}"))
            .collect()
    }
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0;
    let d_lat = (lat2 - lat1) * std::f64::consts::PI / 180.0;
    let d_lon = (lon2 - lon1) * std::f64::consts::PI / 180.0;
    let a = (d_lat / 2.0).sin() * (d_lat / 2.0).sin()
        + (lat1 * std::f64::consts::PI / 180.0).cos()
            * (lat2 * std::f64::consts::PI / 180.0).cos()
            * (d_lon / 2.0).sin()
            * (d_lon / 2.0).sin();
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
}

/// One malformed or conflicting row rejects the whole file.
pub fn validate(data: &[u8], min_entries: usize, baseline: Option<&[Entry]>) -> Option<Vec<Entry>> {
    if data.is_empty() || data.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    if text.starts_with('\u{feff}') {
        return None;
    }
    let lines: Vec<&str> = text
        .split(|c: char| {
            matches!(
                c,
                '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
            )
        })
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let header = lines.first()?;
    if lines.len() - 1 > MAX_ROWS {
        return None;
    }
    let header: Vec<String> = header.split(',').map(|p| p.trim().to_lowercase()).collect();
    if header != ["relay url", "latitude", "longitude"] && header != ["relay url", "lat", "lon"] {
        return None;
    }

    let mut by_host: BTreeMap<String, Entry> = BTreeMap::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        if parts.len() != 3 {
            return None;
        }
        let host = validated_address(parts[0])?;
        let lat = parse_double(parts[1]).filter(|v| (-90.0..=90.0).contains(v))?;
        let lon = parse_double(parts[2]).filter(|v| (-180.0..=180.0).contains(v))?;
        let entry = Entry {
            host: host.clone(),
            lat,
            lon,
        };
        if let Some(existing) = by_host.get(&host)
            && *existing != entry
        {
            return None;
        }
        by_host.insert(host, entry);
        if by_host.len() > MAX_ENTRIES {
            return None;
        }
    }

    let parsed: Vec<Entry> = by_host.into_values().collect();
    if parsed.len() < min_entries {
        return None;
    }
    if let Some(base) = baseline {
        let base_keys: BTreeSet<_> = base.iter().map(Entry::key).collect();
        let kept = parsed
            .iter()
            .filter(|e| base_keys.contains(&e.key()))
            .count();
        let required = (base.len() as f64 * MIN_RETAINED_FRACTION).ceil() as usize;
        if kept < required {
            return None;
        }
    }
    Some(parsed)
}

/// Swift `Double(String)`: finite decimal or exponent forms only.
fn parse_double(s: &str) -> Option<f64> {
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Host (with `:port` unless 443) of a `wss`/`https` relay with no path,
/// query, credentials or local-only name.
fn validated_address(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii() && !b.is_ascii_control()) {
        return None;
    }
    let candidate = if value.contains("://") {
        value.to_string()
    } else {
        format!("wss://{value}")
    };
    let (scheme, rest) = candidate.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "wss" | "https") {
        return None;
    }
    if rest.contains(['?', '#', '@']) {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if !path.is_empty() && path != "/" {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port: u32 = p.parse().ok()?;
            if !(1..=65_535).contains(&port) {
                return None;
            }
            (h, Some(port))
        }
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || host.len() > 253
        || host.ends_with('.')
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if labels.len() < 2
        || labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit()))
        || !labels.iter().all(label_ok)
    {
        return None;
    }
    Some(match port {
        Some(p) if p != 443 => format!("{host}:{p}"),
        _ => host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_list_loads() {
        let d = Directory::bundled();
        assert!(d.entries().len() >= MIN_REMOTE_ENTRIES);
        let near_paris = d.closest_to_geohash("u09tvw", RELAYS_PER_CHANNEL);
        assert_eq!(near_paris.len(), RELAYS_PER_CHANNEL);
        assert!(near_paris.iter().all(|r| r.starts_with("wss://")));
    }

    #[test]
    fn address_rules() {
        assert_eq!(
            validated_address("Relay.Example.com").as_deref(),
            Some("relay.example.com")
        );
        assert_eq!(
            validated_address("wss://relay.example.com:443/").as_deref(),
            Some("relay.example.com")
        );
        assert_eq!(
            validated_address("relay.example.com:7777").as_deref(),
            Some("relay.example.com:7777")
        );
        for bad in [
            "ws://relay.example.com",
            "relay.local",
            "localhost",
            "1.2.3.4",
            "a.b/path",
            "x.y?q",
            "-a.com",
            "a",
        ] {
            assert_eq!(validated_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn whole_file_rejected_on_bad_row() {
        let good = "Relay URL,Latitude,Longitude\na.example.com,1,2\n";
        assert_eq!(validate(good.as_bytes(), 1, None).unwrap().len(), 1);
        let bad = "Relay URL,Latitude,Longitude\na.example.com,1,2\nb.example.com,99,2\n";
        assert!(validate(bad.as_bytes(), 1, None).is_none());
        let conflict = "Relay URL,Latitude,Longitude\na.example.com,1,2\na.example.com,1,3\n";
        assert!(validate(conflict.as_bytes(), 1, None).is_none());
        let base = validate(good.as_bytes(), 1, None).unwrap();
        let replaced = "Relay URL,Latitude,Longitude\nc.example.com,1,2\n";
        assert!(validate(replaced.as_bytes(), 1, Some(&base)).is_none());
    }
}
