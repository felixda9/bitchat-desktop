//! Subscription filters (iOS `NostrFilter`). Encoded as a JSON object with
//! sorted keys; tag filters use `#<tag>` keys.

use crate::event::kind;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Default `limit` for DM subscriptions (`nostrRelayDefaultFetchLimit`).
pub const DEFAULT_FETCH_LIMIT: u32 = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub ids: Option<Vec<String>>,
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u32>>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub limit: Option<u32>,
    pub tags: BTreeMap<String, Vec<String>>,
}

impl Filter {
    /// Gift wraps addressed to `pubkey` (kind 1059, `#p`).
    pub fn gift_wraps_for(pubkey: &str, since: Option<i64>) -> Filter {
        Filter {
            kinds: Some(vec![kind::GIFT_WRAP]),
            since,
            limit: Some(DEFAULT_FETCH_LIMIT),
            tags: [("p".to_string(), vec![pubkey.to_string()])].into(),
            ..Default::default()
        }
    }

    /// Geohash chat and presence (kinds 20000/20001, `#g`).
    pub fn geohash_ephemeral(geohash: &str, since: Option<i64>, limit: u32) -> Filter {
        Filter {
            kinds: Some(vec![kind::EPHEMERAL, kind::GEOHASH_PRESENCE]),
            since,
            limit: Some(limit),
            tags: [("g".to_string(), vec![geohash.to_string()])].into(),
            ..Default::default()
        }
    }

    /// Mesh-bridge rendezvous (kinds 20000/20001, `#r` cells).
    pub fn bridge_rendezvous(cells: &[String], since: Option<i64>, limit: u32) -> Filter {
        Filter {
            kinds: Some(vec![kind::EPHEMERAL, kind::GEOHASH_PRESENCE]),
            since,
            limit: Some(limit),
            tags: [("r".to_string(), cells.to_vec())].into(),
            ..Default::default()
        }
    }

    /// Location notes (kind 1) for one or more geohashes.
    pub fn geohash_notes(geohashes: &[String], since: Option<i64>, limit: u32) -> Filter {
        Filter {
            kinds: Some(vec![kind::TEXT_NOTE]),
            since,
            limit: Some(limit),
            tags: [("g".to_string(), geohashes.to_vec())].into(),
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(v) = &self.ids {
            map.insert("ids".into(), json!(v));
        }
        if let Some(v) = &self.authors {
            map.insert("authors".into(), json!(v));
        }
        if let Some(v) = &self.kinds {
            map.insert("kinds".into(), json!(v));
        }
        if let Some(v) = self.since {
            map.insert("since".into(), json!(v));
        }
        if let Some(v) = self.until {
            map.insert("until".into(), json!(v));
        }
        if let Some(v) = self.limit {
            map.insert("limit".into(), json!(v));
        }
        for (tag, values) in &self.tags {
            map.insert(format!("#{tag}"), json!(values));
        }
        Value::Object(map)
    }

    /// Whether an event satisfies this filter (used by the test relay and for
    /// sanity checks on relay output).
    pub fn matches(&self, event: &crate::Event) -> bool {
        if let Some(ids) = &self.ids
            && !ids.contains(&event.id)
        {
            return false;
        }
        if let Some(authors) = &self.authors
            && !authors.contains(&event.pubkey)
        {
            return false;
        }
        if let Some(kinds) = &self.kinds
            && !kinds.contains(&event.kind)
        {
            return false;
        }
        if self.since.is_some_and(|s| event.created_at < s)
            || self.until.is_some_and(|u| event.created_at > u)
        {
            return false;
        }
        self.tags.iter().all(|(name, values)| {
            event
                .tags
                .iter()
                .any(|t| t.first() == Some(name) && t.get(1).is_some_and(|v| values.contains(v)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_sorted_and_tags_prefixed() {
        let f = Filter::geohash_ephemeral("u4pruy", Some(100), 200);
        assert_eq!(
            serde_json::to_string(&f.to_json()).unwrap(),
            r##"{"#g":["u4pruy"],"kinds":[20000,20001],"limit":200,"since":100}"##
        );
        let dm = Filter::gift_wraps_for("ab", None);
        assert_eq!(
            serde_json::to_string(&dm.to_json()).unwrap(),
            r##"{"#p":["ab"],"kinds":[1059],"limit":100}"##
        );
    }
}
