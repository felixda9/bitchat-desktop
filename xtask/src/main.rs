use bitchat_mesh::MeshIdentity;
use bitchat_mesh::gcs::{self, RequestSync, SyncTypes};
use bitchat_nostr::embedded::{self, Addressing, Embedded};
use bitchat_nostr::event::kind;
use bitchat_nostr::{Event, Keys, envelope, geo_keys, pow};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "export-rust-vectors" => export_rust_vectors(),
        "geo-watch" => geo_watch(std::env::args().nth(2).unwrap_or_default()),
        _ => {
            eprintln!("usage: cargo xtask export-rust-vectors | geo-watch <geohash>");
            std::process::exit(1);
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn key(fill: u8) -> Keys {
    Keys::from_secret_bytes([fill; 32]).unwrap()
}

fn write(name: &str, value: &Value) {
    let dir = root().join("vectors").join("rust");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    std::fs::write(dir.join(name), text).unwrap();
    println!("wrote vectors/rust/{name}");
}

const MESSAGE_ID: &str = "3F2504E0-4F89-11D3-9A0C-0305E82C3301";

/// Writes our envelopes, events and bitchat1 payloads for the Swift verifier.
fn export_rust_vectors() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let now_ms = now as u64 * 1000;
    let seed = [0x5a; 32];

    let geo_pm = embedded::encode(
        &Embedded::PrivateMessage {
            message_id: MESSAGE_ID.into(),
            content: "geohash dm from rust".into(),
        },
        Addressing::Geohash,
        now_ms,
    )
    .unwrap();
    let account_pm = embedded::encode(
        &Embedded::PrivateMessage {
            message_id: MESSAGE_ID.into(),
            content: "account dm from rust".into(),
        },
        Addressing::Account {
            sender: [1; 8],
            recipient: [2; 8],
        },
        now_ms,
    )
    .unwrap();
    let ack = embedded::encode(
        &Embedded::Delivered {
            message_id: MESSAGE_ID.into(),
        },
        Addressing::Geohash,
        now_ms,
    )
    .unwrap();
    let long = embedded::encode(
        &Embedded::PrivateMessage {
            message_id: MESSAGE_ID.into(),
            content: "abc ".repeat(63),
        },
        Addressing::Geohash,
        now_ms,
    )
    .unwrap();

    // Envelopes: (content, sender, recipient, rumor tags)
    let alice = geo_keys::derive(&seed, "u4pruy");
    let bob = geo_keys::derive(&seed, "u4pruydq");
    let p_tag = |k: &Keys| vec![vec!["p".to_string(), k.public_key_hex()]];
    let cases: Vec<(String, Keys, Keys, Vec<Vec<String>>)> = vec![
        ("hello from rust".into(), key(0x31), key(0x41), vec![]),
        (
            "héllo 🫂 / \"q\" \\ \nline\ttab".into(),
            key(0x32),
            key(0x42),
            vec![],
        ),
        (geo_pm.clone(), alice.clone(), bob.clone(), vec![]),
        (account_pm.clone(), key(0x33), key(0x43), vec![]),
        (ack.clone(), bob.clone(), alice.clone(), vec![]),
        (long.clone(), key(0x34), key(0x44), vec![]),
        (
            "android style".into(),
            key(0x35),
            key(0x45),
            p_tag(&key(0x45)),
        ),
    ];
    let envelopes: Vec<Value> = cases
        .into_iter()
        .map(|(content, sender, recipient, tags)| {
            let wrap = envelope::create_private_message_with_tags(
                &content,
                &recipient.public_key_hex(),
                &sender,
                now,
                tags,
            )
            .unwrap();
            json!({
                "event": wrap,
                "recipient_private_key": hex::encode(recipient.secret_bytes()),
                "expected_content": content,
                "expected_sender": sender.public_key_hex(),
                "expected_timestamp": now,
            })
        })
        .collect();
    write("envelopes.json", &Value::Array(envelopes));

    // Events
    let geo = geo_keys::derive(&seed, "u4pru");
    let mut tags = vec![
        vec!["g".to_string(), "u4pru".to_string()],
        vec!["n".to_string(), "rustacean".to_string()],
        vec!["t".to_string(), "teleport".to_string()],
    ];
    tags.push(pow::mine_nonce_tag(
        &geo.public_key_hex(),
        now,
        kind::EPHEMERAL,
        &tags,
        "mined from rust",
        pow::TARGET_BITS,
    ));
    let mut events = vec![json!({
        "event": Event::new(geo.public_key_hex(), now, kind::EPHEMERAL, tags, "mined from rust".into()).sign(&geo),
        "min_pow": pow::TARGET_BITS,
    })];
    let plain = |kind_: u32, tags: Vec<Vec<String>>, content: &str| json!({ "event": Event::new(geo.public_key_hex(), now, kind_, tags, content.into()).sign(&geo) });
    events.push(plain(
        kind::GEOHASH_PRESENCE,
        vec![vec!["g".into(), "u4pru".into()]],
        "",
    ));
    events.push(plain(
        kind::TEXT_NOTE,
        vec![
            vec!["g".into(), "u4pruydq".into()],
            vec!["n".into(), "rustacean".into()],
            vec!["expiration".into(), (now + 86_400).to_string()],
            vec!["t".into(), "urgent".into()],
        ],
        "note from rust",
    ));
    events.push(plain(
        kind::DELETION,
        vec![vec!["e".into(), "ab".repeat(32)]],
        "",
    ));
    for content in [
        "ctrl\u{01}\u{1f}\u{7f}",
        "bs\u{08}ff\u{0c}",
        "ls\u{2028}ps\u{2029}",
        "emoji 🫂🐟 é 中文",
        "quote \" backslash \\ slash /",
        "line\nbreak\r\ttab",
    ] {
        events.push(plain(
            kind::EPHEMERAL,
            vec![vec!["g".into(), "u4pru".into()]],
            content,
        ));
    }
    write("events.json", &Value::Array(events));

    // Embedded payloads
    let embedded_rows = vec![
        json!({ "encoded": geo_pm, "payload_type": 1, "message_id": MESSAGE_ID,
                "content": "geohash dm from rust", "sender": null, "recipient": null }),
        json!({ "encoded": account_pm, "payload_type": 1, "message_id": MESSAGE_ID,
                "content": "account dm from rust", "sender": "0101010101010101", "recipient": "0202020202020202" }),
        json!({ "encoded": ack, "payload_type": 3, "message_id": MESSAGE_ID,
                "content": null, "sender": null, "recipient": null }),
        json!({ "encoded": long, "payload_type": 1, "message_id": MESSAGE_ID,
                "content": "abc ".repeat(63), "sender": null, "recipient": null }),
        json!({ "encoded": embedded::encode(&Embedded::ReadReceipt { message_id: MESSAGE_ID.into() },
                    Addressing::Account { sender: [1; 8], recipient: [2; 8] }, now_ms).unwrap(),
                "payload_type": 2, "message_id": MESSAGE_ID,
                "content": null, "sender": "0101010101010101", "recipient": "0202020202020202" }),
    ];
    write("embedded.json", &Value::Array(embedded_rows));

    export_mesh();
}

/// Signed mesh packets, GCS filters and sync requests for the Swift verifier.
fn export_mesh() {
    let id = MeshIdentity::new([0x24; 32], [0x42; 32]);
    let ts = 1_790_000_000_000;
    let long = "mesh message ".repeat(20);
    let packets = [
        (
            "announce",
            id.announce("rust", &[[1, 2, 3, 4, 5, 6, 7, 8]], 0, ts),
            Some("rust"),
        ),
        (
            "announce_bare",
            id.announce("rust", &[], 0, ts),
            Some("rust"),
        ),
        ("message", id.public_message("hello from rust", ts), None),
        ("message_compressible", id.public_message(&long, ts), None),
        ("leave", id.leave(ts), None),
    ];
    let packet_rows: Vec<Value> = packets
        .iter()
        .map(|(name, packet, nickname)| {
            let packet = packet.as_ref().unwrap();
            json!({
                "name": name,
                "wire": hex::encode(packet.encode(true).unwrap()),
                "noise_public": hex::encode(id.noise_public()),
                "signing_public": hex::encode(id.signing_public()),
                "nickname": nickname,
            })
        })
        .collect();

    let ids = |count: u32, seed: u8| -> Vec<[u8; 16]> {
        (0..count)
            .map(|i| {
                let mut input = vec![seed];
                input.extend_from_slice(&i.to_be_bytes());
                gcs_id(&input)
            })
            .collect()
    };
    let filter_rows: Vec<Value> = [(1, 11, 400), (40, 12, 400), (400, 13, 400), (300, 14, 64)]
        .iter()
        .map(|&(count, seed, max_bytes)| {
            let ids = ids(count, seed);
            let f = gcs::build_filter(&ids, max_bytes, 0.01);
            json!({
                "ids": ids.iter().map(hex::encode).collect::<Vec<_>>(),
                "max_bytes": max_bytes,
                "p": f.p, "m": f.m, "data": hex::encode(&f.data), "included": f.included,
            })
        })
        .collect();

    let requests = [
        RequestSync {
            p: 7,
            m: 1,
            data: vec![],
            types: None,
            since: None,
            fragment_ids: None,
        },
        RequestSync {
            p: 7,
            m: 4242,
            data: vec![0xca, 0xfe],
            types: Some(SyncTypes::from_bits(0b100_0000_0011)),
            since: Some(1_790_000_000_000),
            fragment_ids: gcs::encode_fragment_ids(&[[0xab; 8]]),
        },
    ];
    let request_rows: Vec<Value> = requests
        .iter()
        .map(|r| {
            json!({
                "encoded": hex::encode(r.encode()),
                "p": r.p, "m": r.m, "data": hex::encode(&r.data),
                "types": r.types.map(|t| t.bits()), "since": r.since, "fragment_ids": r.fragment_ids,
            })
        })
        .collect();

    write(
        "mesh.json",
        &json!({ "packets": packet_rows, "gcs": filter_rows, "request_sync": request_rows }),
    );
}

fn gcs_id(input: &[u8]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    Sha256::digest(input)[..16].try_into().unwrap()
}

/// Prints the last hour of a geohash channel straight from its relays (direct, no Tor).
fn geo_watch(geohash: String) {
    use bitchat_nostr::Filter;
    use bitchat_nostr::net::{Dialer, PoolEvent, RelayPool};
    use bitchat_nostr::relay_directory::{Directory, RELAYS_PER_CHANNEL};
    let relays = Directory::bundled().closest_to_geohash(&geohash, RELAYS_PER_CHANNEL);
    println!("relays for #{geohash}: {relays:?}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        let (pool, mut rx) = RelayPool::spawn(Dialer::Direct);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        pool.subscribe(
            "watch",
            Filter::geohash_ephemeral(&geohash, Some(now - 3600), 200),
            &relays,
        );
        let secs = std::env::args()
            .nth(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        while let Ok(Some(e)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            match e {
                PoolEvent::Event { relay, event, .. } => println!(
                    "{relay}  kind={} age={}s n={:?} {:?}",
                    event.kind,
                    now - event.created_at,
                    event.tag_value("n"),
                    event.content
                ),
                PoolEvent::Status { relay, status } => println!("{relay}: {status:?}"),
                PoolEvent::Eose { .. } => println!("-- end of stored events"),
                _ => {}
            }
        }
    });
}
