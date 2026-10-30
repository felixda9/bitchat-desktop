//! Mesh-bridge carriers and IDs, checked against the iOS code where vectors
//! exist (vectors/swift/export.json, "bridge").

use bitchat_nostr::bridge::{self, Carrier, Direction};
use bitchat_nostr::{Event, Keys};
use serde_json::Value;

fn swift() -> Option<Value> {
    let path = format!(
        "{}/../../vectors/swift/export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let e: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let b = e.get("bridge").cloned();
    if b.is_none() {
        eprintln!("no Swift bridge vectors yet; skipped");
    }
    b
}

fn tlv(t: u8, v: &[u8]) -> Vec<u8> {
    let mut out = vec![t, (v.len() >> 8) as u8, v.len() as u8];
    out.extend_from_slice(v);
    out
}

#[test]
fn carriers_round_trip() {
    let keys = Keys::from_secret_bytes([5; 32]).unwrap();
    let event = bridge::mesh_event(
        "hi",
        "u4pruy",
        &keys,
        "alice",
        "a1b2c3d4e5f60718",
        1,
        1_790_000_000,
    );
    assert!(event.verify());
    for d in [
        Direction::ToGateway,
        Direction::FromGateway,
        Direction::ToBridge,
        Direction::FromBridge,
    ] {
        let c = Carrier::new(d, "u4pruy", &event).unwrap();
        let back = Carrier::decode(&c.encode()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.event().unwrap(), event);
    }
    assert!(Carrier::new(Direction::ToBridge, "", &event).is_none());
    assert!(Carrier::new(Direction::ToBridge, "u4pruyu4pruyu", &event).is_none());
}

#[test]
fn carrier_decoding_edges() {
    let good = [tlv(1, &[3]), tlv(2, b"u4pruy"), tlv(3, b"{}")].concat();
    let dir = |d: &[u8]| Carrier::decode(d).map(|c| c.direction as u8);
    assert_eq!(dir(&good), Some(3));
    assert_eq!(dir(&[tlv(9, &[1, 2, 3]), good.clone()].concat()), Some(3));
    assert_eq!(dir(&[good.clone(), vec![0]].concat()), None);
    assert_eq!(dir(&good[..good.len() - 1]), None);
    assert_eq!(
        dir(&[tlv(1, &[5]), tlv(2, b"u4pruy"), tlv(3, b"{}")].concat()),
        None
    );
    assert_eq!(dir(&[tlv(1, &[3]), tlv(2, b"u4pruy")].concat()), None);
    assert_eq!(dir(&[tlv(1, &[1]), good.clone()].concat()), Some(3));
}

#[test]
fn bridge_events_have_the_ios_shape() {
    let keys = Keys::from_secret_bytes([5; 32]).unwrap();
    let event = bridge::mesh_event(
        " hi ",
        "u4pruy",
        &keys,
        " alice ",
        "a1b2c3d4e5f60718",
        42,
        1_790_000_000,
    );
    assert_eq!(event.kind, 20000);
    assert_eq!(event.tags[0], ["r", "u4pruy"]);
    assert_eq!(event.tags[1], ["n", "alice"]);
    let id = bridge::stable_id("a1b2c3d4e5f60718", 42, "hi");
    assert_eq!(event.tags[2], ["m", id.as_str(), "a1b2c3d4e5f60718", "42"]);
    let anon = bridge::mesh_event(
        "x",
        "u4pruy",
        &keys,
        "  ",
        "a1b2c3d4e5f60718",
        42,
        1_790_000_000,
    );
    assert_eq!(anon.tags.len(), 2, "no n tag without a nickname");
    let presence = bridge::presence_event("u4pruy", &keys, 1_790_000_000);
    assert_eq!((presence.kind, presence.content.as_str()), (20001, ""));
    assert_eq!(presence.tags, [["r", "u4pruy"]]);
}

#[test]
fn swift_carriers_and_ids_match() {
    let Some(b) = swift() else { return };
    let swift_event: Event = serde_json::from_value(b["mesh_event"].clone()).unwrap();
    assert!(swift_event.verify());
    let m = swift_event.tags.iter().find(|t| t[0] == "m").unwrap();
    assert_eq!(
        bridge::stable_id(&m[2], m[3].parse().unwrap(), &swift_event.content),
        m[1]
    );
    // Same inputs, same tags (the signature and time differ).
    let ours = bridge::mesh_event(
        "  hello across the bridge\n",
        "u4pruy",
        &Keys::from_secret_bytes({
            let mut k = [0; 32];
            k[31] = 5;
            k
        })
        .unwrap(),
        " alice ",
        "a1b2c3d4e5f60718",
        1_790_000_000_123,
        swift_event.created_at,
    );
    assert_eq!(ours.tags, swift_event.tags);
    assert_eq!(ours.pubkey, swift_event.pubkey);
    assert_eq!(ours.id, swift_event.id);
    let presence: Event = serde_json::from_value(b["presence_event"].clone()).unwrap();
    assert!(presence.verify());
    assert_eq!(
        bridge::presence_event(
            "u4pruy",
            &Keys::from_secret_bytes({
                let mut k = [0; 32];
                k[31] = 5;
                k
            })
            .unwrap(),
            presence.created_at
        )
        .id,
        presence.id
    );

    for row in b["carriers"].as_array().unwrap() {
        let bytes = hex::decode(row["encoded"].as_str().unwrap()).unwrap();
        let c = Carrier::decode(&bytes).unwrap();
        assert_eq!(c.direction as u64, row["direction"].as_u64().unwrap());
        assert_eq!(c.geohash, row["geohash"].as_str().unwrap());
        let event = c.event().unwrap();
        assert!(event.verify());
        assert_eq!(event.id, swift_event.id);
    }
    for row in b["stable_ids"].as_array().unwrap() {
        assert_eq!(
            bridge::stable_id(
                row["sender"].as_str().unwrap(),
                row["timestamp_ms"].as_u64().unwrap(),
                row["content"].as_str().unwrap()
            ),
            row["id"].as_str().unwrap(),
            "{row}"
        );
    }
    for row in b["decode"].as_array().unwrap() {
        let data = hex::decode(row["data"].as_str().unwrap()).unwrap();
        assert_eq!(
            Carrier::decode(&data).map(|c| c.direction as u64),
            row["direction"].as_u64(),
            "{}",
            row["name"]
        );
    }
}
