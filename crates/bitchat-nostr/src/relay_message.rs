//! NIP-01 wire messages between client and relay.

use crate::Event;
use crate::filter::Filter;
use serde_json::{Value, json};

/// Inbound frames larger than this are dropped before parsing.
pub const MAX_INBOUND_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum RelayMessage {
    Event {
        subscription: String,
        event: Event,
    },
    Eose {
        subscription: String,
    },
    Ok {
        event_id: String,
        accepted: bool,
        message: String,
    },
    Notice {
        message: String,
    },
    Closed {
        subscription: String,
        message: String,
    },
    Auth {
        challenge: String,
    },
}

impl RelayMessage {
    pub fn parse(text: &str) -> Option<RelayMessage> {
        if text.len() > MAX_INBOUND_MESSAGE_BYTES {
            return None;
        }
        let value: Value = serde_json::from_str(text).ok()?;
        let items = value.as_array()?;
        let label = items.first()?.as_str()?;
        let str_at = |i: usize| items.get(i).and_then(Value::as_str).map(str::to_string);
        Some(match label {
            "EVENT" => RelayMessage::Event {
                subscription: str_at(1)?,
                event: serde_json::from_value(items.get(2)?.clone()).ok()?,
            },
            "EOSE" => RelayMessage::Eose {
                subscription: str_at(1)?,
            },
            "OK" => RelayMessage::Ok {
                event_id: str_at(1)?,
                accepted: items.get(2)?.as_bool()?,
                message: str_at(3).unwrap_or_default(),
            },
            "NOTICE" => RelayMessage::Notice {
                message: str_at(1)?,
            },
            "CLOSED" => RelayMessage::Closed {
                subscription: str_at(1)?,
                message: str_at(2).unwrap_or_default(),
            },
            "AUTH" => RelayMessage::Auth {
                challenge: str_at(1)?,
            },
            _ => return None,
        })
    }
}

pub fn req(subscription: &str, filter: &Filter) -> String {
    json!(["REQ", subscription, filter.to_json()]).to_string()
}

pub fn close(subscription: &str) -> String {
    json!(["CLOSE", subscription]).to_string()
}

pub fn event(event: &Event) -> String {
    json!(["EVENT", event]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relay_messages() {
        assert_eq!(
            RelayMessage::parse(r#"["EOSE","geo-u4"]"#),
            Some(RelayMessage::Eose {
                subscription: "geo-u4".into()
            })
        );
        assert_eq!(
            RelayMessage::parse(r#"["OK","ab",false,"blocked"]"#),
            Some(RelayMessage::Ok {
                event_id: "ab".into(),
                accepted: false,
                message: "blocked".into()
            })
        );
        assert!(RelayMessage::parse(r#"["EVENT","s",{"bad":1}]"#).is_none());
        assert!(RelayMessage::parse(r#"{"x":1}"#).is_none());
        assert!(
            RelayMessage::parse(&format!(
                r#"["NOTICE","{}"]"#,
                "a".repeat(MAX_INBOUND_MESSAGE_BYTES)
            ))
            .is_none()
        );
    }

    #[test]
    fn client_messages() {
        let f = Filter::gift_wraps_for("ab", Some(5));
        assert_eq!(
            req("dm", &f),
            r##"["REQ","dm",{"#p":["ab"],"kinds":[1059],"limit":100,"since":5}]"##
        );
        assert_eq!(close("dm"), r#"["CLOSE","dm"]"#);
    }
}
