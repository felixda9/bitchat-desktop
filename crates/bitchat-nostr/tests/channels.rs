use bitchat_nostr::embedded::{self, Addressing, Embedded};
use bitchat_nostr::{Event, geo_keys, pow};
use serde::Deserialize;
use serde_json::Value;

fn export() -> Value {
    let path = format!(
        "{}/../../vectors/swift/export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[derive(Deserialize)]
struct GeoIdentity {
    label: String,
    private_key: String,
    public_key: String,
}

#[test]
fn per_geohash_keys_match_ios() {
    let e = export();
    let seed: [u8; 32] = hex::decode(e["geohash_identities"]["seed"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let rows: Vec<GeoIdentity> =
        serde_json::from_value(e["geohash_identities"]["identities"].clone()).unwrap();
    for r in rows {
        let keys = match r.label.strip_prefix("bridge|") {
            Some(cell) => geo_keys::derive_bridge(&seed, cell),
            None => geo_keys::derive(&seed, &r.label),
        };
        assert_eq!(
            hex::encode(keys.secret_bytes()),
            r.private_key,
            "{}",
            r.label
        );
        assert_eq!(keys.public_key_hex(), r.public_key, "{}", r.label);
    }
}

#[derive(Deserialize)]
struct PowCase {
    id: String,
    tags: Vec<Vec<String>>,
    difficulty: u32,
}

#[test]
fn pow_scoring_matches_ios() {
    let e = export();
    let cases: Vec<PowCase> = serde_json::from_value(e["pow"].clone()).unwrap();
    for c in cases {
        assert_eq!(
            pow::validated_difficulty(&c.id, &c.tags),
            c.difficulty,
            "{} {:?}",
            c.id,
            c.tags
        );
    }
    let mined: Event = serde_json::from_value(e["mined_event"].clone()).unwrap();
    let expected = e["mined_event"]["validated_difficulty"].as_u64().unwrap() as u32;
    assert_eq!(pow::validated_difficulty(&mined.id, &mined.tags), expected);
    assert!(expected >= pow::TARGET_BITS);
}

#[test]
fn ios_embedded_payloads_decode() {
    let e = export();
    let id = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
    let sender = hex::decode("0102030405060708").unwrap();
    let recipient = hex::decode("1112131415161718").unwrap();
    for row in e["embedded"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let (packet, item) =
            embedded::decode(row["encoded"].as_str().unwrap()).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(packet.ttl, 7, "{name}");
        let expected = match name {
            "pm" => Embedded::PrivateMessage {
                message_id: id.into(),
                content: "hi there".into(),
            },
            "pm_long" => Embedded::PrivateMessage {
                message_id: id.into(),
                content: "abc ".repeat(50),
            },
            "geo_pm" => Embedded::PrivateMessage {
                message_id: id.into(),
                content: "hi geo".into(),
            },
            "delivered" | "geo_delivered" => Embedded::Delivered {
                message_id: id.into(),
            },
            "read" | "geo_read" => Embedded::ReadReceipt {
                message_id: id.into(),
            },
            other => panic!("unknown case {other}"),
        };
        assert_eq!(item, expected, "{name}");
        if name.starts_with("geo_") {
            assert_eq!(packet.recipient_id, None, "{name}");
        } else {
            assert_eq!(packet.sender_id.to_vec(), sender, "{name}");
            assert_eq!(
                packet.recipient_id.map(|r| r.to_vec()),
                Some(recipient.clone()),
                "{name}"
            );
        }
    }
}

#[test]
fn encode_decode_round_trip() {
    let pm = Embedded::PrivateMessage {
        message_id: "m1".into(),
        content: "héllo".into(),
    };
    let s = embedded::encode(&pm, Addressing::Geohash, 1_790_000_000_000).unwrap();
    assert!(s.starts_with("bitchat1:"));
    let (packet, item) = embedded::decode(&s).unwrap();
    assert_eq!(item, pm);
    assert_eq!(packet.recipient_id, None);
    assert_eq!(packet.timestamp, 1_790_000_000_000);

    let account = Addressing::Account {
        sender: [1; 8],
        recipient: [2; 8],
    };
    let ack = Embedded::Delivered {
        message_id: "m1".into(),
    };
    let (packet, item) = embedded::decode(&embedded::encode(&ack, account, 5).unwrap()).unwrap();
    assert_eq!(item, ack);
    assert_eq!(packet.sender_id, [1; 8]);
    assert_eq!(packet.recipient_id, Some([2; 8]));

    let too_long = Embedded::PrivateMessage {
        message_id: "m".into(),
        content: "x".repeat(256),
    };
    assert!(embedded::encode(&too_long, Addressing::Geohash, 0).is_none());
    assert!(embedded::decode("hello").is_none());
}
