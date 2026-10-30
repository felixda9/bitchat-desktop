//! Checks against vectors exported from the iOS code
//! (tools/swift-vectors, vectors/swift/export.json).

use bitchat_mesh::announce::Announce;
use bitchat_mesh::gcs::{self, RequestSync, SyncTypes};
use bitchat_mesh::{MeshIdentity, verify};
use bitchat_proto::{Packet, message_type as t};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn export() -> Value {
    let path = format!(
        "{}/../../vectors/swift/export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}

fn sync_ids(count: u64, seed: u8) -> Vec<[u8; 16]> {
    (0..count as u32)
        .map(|i| {
            let mut input = vec![seed];
            input.extend_from_slice(&i.to_be_bytes());
            Sha256::digest(&input)[..16].try_into().unwrap()
        })
        .collect()
}

#[test]
fn packet_ids_match_swift() {
    let e = export();
    for row in e["sync"]["packet_ids"].as_array().unwrap() {
        let packet = Packet::new(
            row["type"].as_u64().unwrap() as u8,
            bytes(&row["sender"]).try_into().unwrap(),
            row["timestamp"].as_u64().unwrap(),
            bytes(&row["payload"]),
            7,
        );
        assert_eq!(
            hex::encode(gcs::packet_id(&packet)),
            row["id"].as_str().unwrap()
        );
    }
}

#[test]
fn gcs_filters_match_swift() {
    let e = export();
    let filters = e["sync"]["filters"].as_array().unwrap();
    assert_eq!(filters.len(), 10);
    for row in filters {
        let count = row["count"].as_u64().unwrap();
        let seed = row["seed"].as_u64().unwrap() as u8;
        let ids = if row["duplicate"].as_bool().unwrap() {
            vec![sync_ids(1, seed)[0]; count as usize]
        } else {
            sync_ids(count, seed)
        };
        let max_bytes = row["max_bytes"].as_u64().unwrap() as usize;
        let fpr = row["fpr"].as_f64().unwrap();
        let f = gcs::build_filter(&ids, max_bytes, fpr);
        let case = format!("count {count} seed {seed}");
        assert_eq!(f.p as u64, row["p"].as_u64().unwrap(), "{case}");
        assert_eq!(f.m as u64, row["m"].as_u64().unwrap(), "{case}");
        assert_eq!(
            hex::encode(&f.data),
            row["data"].as_str().unwrap(),
            "{case}"
        );
        assert_eq!(
            f.included as u64,
            row["included"].as_u64().unwrap(),
            "{case}"
        );

        let decoded: Vec<u64> = row["decoded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(gcs::decode(f.p, f.m, &f.data), decoded, "{case}");
        let buckets: Vec<u64> = row["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        let ours: Vec<u64> = ids.iter().take(5).map(|id| gcs::bucket(id, f.m)).collect();
        assert_eq!(ours, buckets, "{case}");
    }
}

#[test]
fn request_sync_encoding_matches_swift() {
    let e = export();
    let rows = e["sync"]["requests"].as_array().unwrap();
    let ours = [
        (
            "minimal",
            RequestSync {
                p: 7,
                m: 1,
                data: vec![],
                types: None,
                since: None,
                fragment_ids: None,
            },
        ),
        (
            "public",
            RequestSync {
                p: 7,
                m: 2485,
                data: vec![0xde, 0xad, 0xbe, 0xef],
                types: Some(SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE, t::GROUP_MESSAGE])),
                since: Some(1_790_000_000_000),
                fragment_ids: None,
            },
        ),
        (
            "fragments",
            RequestSync {
                p: 7,
                m: 100,
                data: vec![1],
                types: Some(SyncTypes::of(&[t::FRAGMENT])),
                since: None,
                fragment_ids: gcs::encode_fragment_ids(&[
                    [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
                    [0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
                ]),
            },
        ),
    ];
    for (name, request) in ours {
        let row = rows.iter().find(|r| r["name"] == name).unwrap();
        let swift = bytes(&row["encoded"]);
        assert_eq!(hex::encode(request.encode()), hex::encode(&swift), "{name}");
        assert_eq!(RequestSync::decode(&swift), Some(request), "{name}");
    }
}

#[test]
fn sync_types_match_swift() {
    let e = export();
    for row in e["sync"]["types"].as_array().unwrap() {
        let types = SyncTypes::from_bits(row["bits"].as_u64().unwrap());
        let expected = row["encoded"].as_str().map(|s| hex::decode(s).unwrap());
        assert_eq!(types.encode(), expected, "{}", row["name"]);
    }
    let all = e["sync"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "all")
        .unwrap();
    let ours = SyncTypes::of(&[
        t::ANNOUNCE,
        t::MESSAGE,
        t::LEAVE,
        t::NOISE_HANDSHAKE,
        t::NOISE_ENCRYPTED,
        t::FRAGMENT,
        t::REQUEST_SYNC,
        t::FILE_TRANSFER,
        t::BOARD_POST,
        t::PREKEY_BUNDLE,
        t::GROUP_MESSAGE,
        t::PING,
        t::VOICE_FRAME,
    ]);
    assert_eq!(ours.bits(), all["bits"].as_u64().unwrap());
    for row in e["sync"]["derive_p"].as_array().unwrap() {
        assert_eq!(
            gcs::derive_p(row["fpr"].as_f64().unwrap()) as u64,
            row["p"].as_u64().unwrap()
        );
    }
}

#[test]
fn swift_signed_packets_verify_and_match_our_preimages() {
    let e = export();
    let ours = MeshIdentity::new([0x24; 32], [0x42; 32]);
    let rows = e["mesh_signed"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    for row in rows {
        let name = row["name"].as_str().unwrap();
        assert_eq!(
            bytes(&row["noise_public"]),
            ours.noise_public(),
            "same Noise key"
        );
        assert_eq!(
            bytes(&row["signing_public"]),
            ours.signing_public(),
            "same signing key"
        );

        let packet = Packet::decode(&bytes(&row["wire"])).unwrap();
        assert_eq!(packet.sender_id, ours.peer_id(), "{name}");
        assert!(
            verify(&packet, &ours.signing_public()),
            "{name}: Swift signature checks out"
        );
        assert_eq!(
            packet.signing_bytes().unwrap(),
            bytes(&row["signing"]),
            "{name}: same pre-image"
        );
        assert_eq!(packet.payload, bytes(&row["payload"]), "{name}");

        // Our own packet with the same content has the same pre-image.
        let mut mine = Packet::new(
            packet.packet_type,
            ours.peer_id(),
            packet.timestamp,
            packet.payload.clone(),
            7,
        );
        assert!(ours.sign(&mut mine));
        assert_eq!(mine.signing_bytes(), packet.signing_bytes(), "{name}");
        assert!(verify(&mine, &ours.signing_public()));

        if packet.packet_type == t::ANNOUNCE {
            let a = Announce::decode(&packet.payload).unwrap();
            assert_eq!(a.nickname, "swift");
            let neighbors: &[[u8; 8]] = match &a.neighbors {
                Some(n) => n,
                None => &[],
            };
            let ours_announce = ours
                .announce("swift", neighbors, 0, packet.timestamp)
                .unwrap();
            if a.capabilities.is_some() {
                assert_eq!(ours_announce.payload, packet.payload, "{name}: same TLVs");
            }
        }
    }
}

#[test]
fn swift_directed_carrier_verifies() {
    let e = export();
    let Some(p) = e.get("bridge").map(|b| b["directed_packet"].clone()) else {
        eprintln!("no Swift bridge vectors yet; skipped");
        return;
    };
    let packet = Packet::decode(&bytes(&p["wire"])).unwrap();
    assert_eq!(packet.packet_type, t::NOSTR_CARRIER);
    assert_eq!(packet.recipient_id, Some([1, 2, 3, 4, 5, 6, 7, 8]));
    assert_eq!(packet.payload, bytes(&p["payload"]));
    assert!(verify(&packet, &bytes(&p["signing_public"])));
}

#[test]
fn swift_file_packets_decode() {
    let e = export();
    let Some(f) = e.get("files").cloned() else {
        eprintln!("no Swift file vectors yet; skipped");
        return;
    };
    for row in f["packets"].as_array().unwrap() {
        let data = bytes(&row["encoded"]);
        let p = bitchat_proto::file::FilePacket::decode(&data).unwrap();
        assert_eq!(p.file_name.as_deref(), row["file_name"].as_str());
        assert_eq!(p.mime_type.as_deref(), row["mime_type"].as_str());
        assert_eq!(p.content, bytes(&row["content"]));
        assert_eq!(
            bitchat_proto::file::validate(&data).is_some(),
            row["accepted"].as_bool().unwrap(),
            "{}",
            row["name"]
        );
        // Same fields, same bytes.
        assert_eq!(p.encode().unwrap(), data, "{}", row["name"]);
    }
    let b = &f["broadcast"];
    let packet = Packet::decode(&bytes(&b["wire"])).unwrap();
    assert_eq!((packet.packet_type, packet.version), (t::FILE_TRANSFER, 2));
    assert_eq!(packet.payload, bytes(&b["payload"]));
    assert!(verify(&packet, &bytes(&b["signing_public"])));
}
