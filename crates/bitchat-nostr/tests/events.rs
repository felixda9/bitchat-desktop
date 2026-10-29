use bitchat_nostr::Event;
use bitchat_nostr::keys::{self, Keys};
use serde::Deserialize;
use serde_json::Value;

fn read(path: &str) -> Value {
    let full = format!("{}/../../vectors/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&full).unwrap()).unwrap()
}

#[test]
fn ios_event_ids_and_signatures() {
    let export = read("swift/export.json");
    let events: Vec<Event> = serde_json::from_value(export["event_ids"].clone()).unwrap();
    assert!(events.len() >= 10);
    for event in events {
        assert_eq!(
            hex::encode(event.compute_id()),
            event.id,
            "id for {:?}",
            event.content
        );
        assert!(event.verify(), "signature for {:?}", event.content);
    }
    let mined: Event = serde_json::from_value(export["mined_event"].clone()).unwrap();
    assert!(mined.verify());
}

#[derive(Deserialize)]
struct Identity {
    private_key: String,
    public_key: String,
    npub: String,
}

#[test]
fn ios_identities_encode_the_same() {
    let export = read("swift/export.json");
    let ids: Vec<Identity> =
        serde_json::from_value(export["geohash_identities"]["identities"].clone()).unwrap();
    for id in ids {
        let secret: [u8; 32] = hex::decode(&id.private_key).unwrap().try_into().unwrap();
        let k = Keys::from_secret_bytes(secret).unwrap();
        assert_eq!(k.public_key_hex(), id.public_key);
        assert_eq!(k.npub(), id.npub);
        assert_eq!(keys::npub_decode(&id.npub).unwrap(), k.public_key());
    }
}

#[test]
fn android_bech32_vectors() {
    let wire = read("imported/android-wire.json");
    for v in wire["bech32"]["valid"].as_array().unwrap() {
        let key = keys::npub_decode(v["npub"].as_str().unwrap()).unwrap();
        assert_eq!(hex::encode(key), v["hex"].as_str().unwrap());
        assert_eq!(keys::npub_encode(&key), v["npub"].as_str().unwrap());
    }
    for v in wire["bech32"]["invalid"].as_array().unwrap() {
        assert!(keys::bech32_decode(v.as_str().unwrap()).is_none(), "{v}");
    }
}

#[test]
fn signed_events_round_trip_json() {
    let keys = Keys::generate();
    let event = Event::new(
        keys.public_key_hex(),
        1,
        1,
        vec![vec!["g".into(), "u4".into()]],
        "x".into(),
    )
    .sign(&keys);
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert_eq!(back, event);
    assert!(back.verify());
}
