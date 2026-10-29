use bitchat_mesh::gcs::{self, RequestSync, SyncTypes};
use bitchat_mesh::sync::{GossipSync, SyncConfig, SyncOut};
use bitchat_proto::{Packet, PeerId, message_type as t};
use sha2::{Digest, Sha256};

const NOW: u64 = 1_790_000_000_000;
const A: PeerId = [0xAA; 8];
const B: PeerId = [0xBB; 8];
const C: PeerId = [0xCC; 8];

fn ids(count: u32, seed: u8) -> Vec<[u8; 16]> {
    (0..count)
        .map(|i| {
            let mut input = vec![seed];
            input.extend_from_slice(&i.to_be_bytes());
            Sha256::digest(&input)[..16].try_into().unwrap()
        })
        .collect()
}

// --- GCS (cases from iOS GCSFilterTests) ---

#[test]
fn filter_contains_every_included_id_and_few_others() {
    let members = ids(100, 1);
    let f = gcs::build_filter(&members, 400, 0.01);
    assert_eq!((f.p, f.included), (7, 100));
    let values = gcs::decode(f.p, f.m, &f.data);
    for id in &members {
        assert!(values.binary_search(&gcs::bucket(id, f.m)).is_ok());
    }
    let false_hits = ids(10_000, 2)
        .iter()
        .filter(|id| values.binary_search(&gcs::bucket(*id, f.m)).is_ok())
        .count();
    assert!(false_hits < 200, "{false_hits} false positives");
}

#[test]
fn duplicate_ids_encode_once() {
    let f = gcs::build_filter(&vec![[0xAB; 16]; 64], 128, 0.01);
    assert!(f.m >= 1);
    assert!(gcs::decode(f.p, f.m, &f.data).len() <= 1);
}

#[test]
fn bucket_avoids_zero() {
    let b = gcs::bucket(&[1; 16], 2);
    assert!(b != 0 && b < 2);
}

#[test]
fn decode_rejects_out_of_range_parameters() {
    let junk = [0xFF; 64];
    assert!(gcs::decode(0, 1000, &junk).is_empty());
    assert!(gcs::decode(gcs::MAX_P + 1, 1000, &junk).is_empty());
    assert!(gcs::decode(255, u32::MAX, &junk).is_empty());
    assert!(gcs::decode(8, 0, &junk).is_empty());
    assert!(gcs::decode(8, 1, &junk).is_empty());
}

#[test]
fn truncated_filters_only_lose_values() {
    let f = gcs::build_filter(&ids(32, 3), 128, 0.01);
    let full = gcs::decode(f.p, f.m, &f.data);
    let half = gcs::decode(f.p, f.m, &f.data[..f.data.len() / 2]);
    assert!(half.len() <= full.len());
    assert!(half.iter().all(|v| full.contains(v)));
}

#[test]
fn filters_trim_the_tail_to_fit() {
    assert_eq!(gcs::build_filter(&ids(8, 4), 1024, 0.01).included, 8);
    let f = gcs::build_filter(&ids(200, 5), 32, 0.01);
    assert!(f.included > 0 && f.included < 200);
    assert!(f.data.len() <= 32);
}

#[test]
fn derive_p_matches_fpr() {
    assert_eq!(gcs::derive_p(0.01), 7);
    assert_eq!(gcs::derive_p(0.25), 2);
    assert_eq!(gcs::derive_p(0.5), 2, "clamped to 0.25");
    assert_eq!(gcs::derive_p(0.0), 20, "clamped to 1e-6");
}

// --- wire formats ---

#[test]
fn sync_types_are_minimal_little_endian() {
    let public = SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE]);
    assert_eq!(public.encode(), Some(vec![0x03]));
    let with_groups = public.union(SyncTypes::of(&[t::GROUP_MESSAGE]));
    assert_eq!(with_groups.encode(), Some(vec![0x03, 0x04]));
    assert_eq!(SyncTypes::NONE.encode(), None);
    assert_eq!(
        SyncTypes::of(&[t::PING, t::VOICE_FRAME]),
        SyncTypes::NONE,
        "no sync bit"
    );
    assert_eq!(
        SyncTypes::decode(&[0x03, 0xF8]),
        Some(public),
        "unknown bits dropped"
    );
    assert_eq!(SyncTypes::decode(&[]), None);
    assert_eq!(SyncTypes::decode(&[0; 9]), None);
    assert!(with_groups.contains(t::GROUP_MESSAGE) && !with_groups.contains(t::FRAGMENT));
}

#[test]
fn request_sync_round_trip_and_rejections() {
    let r = RequestSync {
        p: 7,
        m: 2485,
        data: vec![0xde, 0xad],
        types: Some(SyncTypes::of(&[t::FRAGMENT])),
        since: Some(NOW),
        fragment_ids: gcs::encode_fragment_ids(&[[0x11; 8], [0x22; 8]]),
    };
    let bytes = r.encode();
    assert_eq!(&bytes[..4], &[0x01, 0x00, 0x01, 0x07]);
    assert_eq!(RequestSync::decode(&bytes), Some(r.clone()));

    let mut with_unknown = bytes.clone();
    with_unknown.extend_from_slice(&[0x7F, 0x00, 0x02, 0xAA, 0xBB]);
    assert_eq!(RequestSync::decode(&with_unknown), Some(r));
    assert_eq!(
        RequestSync::decode(&bytes[..bytes.len() - 1]),
        None,
        "truncated"
    );

    let oversized_p = RequestSync {
        p: 200,
        m: 4096,
        data: vec![1, 2],
        types: None,
        since: None,
        fragment_ids: None,
    };
    assert_eq!(RequestSync::decode(&oversized_p.encode()), None);
    let big = RequestSync {
        p: 7,
        m: 4096,
        data: vec![0; 1025],
        types: None,
        since: None,
        fragment_ids: None,
    };
    assert_eq!(RequestSync::decode(&big.encode()), None);
    let no_m = [0x01, 0x00, 0x01, 0x07, 0x03, 0x00, 0x00];
    assert_eq!(RequestSync::decode(&no_m), None);
}

#[test]
fn fragment_id_lists() {
    assert_eq!(gcs::encode_fragment_ids(&[]), None);
    let many: Vec<[u8; 8]> = (0..70u8).map(|i| [i; 8]).collect();
    let encoded = gcs::encode_fragment_ids(&many).unwrap();
    assert_eq!(encoded.split(',').count(), 60);
    assert_eq!(
        gcs::decode_fragment_ids("zz,0011223344556677,,0011223344556677,abc"),
        Some(vec![[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]])
    );
    assert_eq!(gcs::decode_fragment_ids("nope"), None);
}

// --- gossip sync ---

fn message(sender: PeerId, ts: u64, text: &str) -> Packet {
    Packet::new(t::MESSAGE, sender, ts, text.as_bytes().to_vec(), 7)
}

fn request_payload(out: &SyncOut) -> RequestSync {
    match out {
        SyncOut::Request { packet, .. } => {
            assert_eq!((packet.packet_type, packet.ttl), (t::REQUEST_SYNC, 0));
            RequestSync::decode(&packet.payload).unwrap()
        }
        other => panic!("{other:?}"),
    }
}

fn replies(out: Vec<SyncOut>) -> Vec<Packet> {
    out.into_iter()
        .map(|o| match o {
            SyncOut::Reply { packet, .. } => packet,
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn a_peer_gets_what_it_is_missing() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    let mut b = GossipSync::new(B, SyncConfig::default());
    let msgs: Vec<Packet> = (0..5)
        .map(|i| message(C, NOW - 1000 + i, &format!("m{i}")))
        .collect();
    let announce = Packet::new(t::ANNOUNCE, C, NOW - 10, vec![1, 2, 3], 7);
    for m in &msgs {
        a.on_packet_seen(m, NOW);
    }
    a.on_packet_seen(&announce, NOW);
    b.on_packet_seen(&msgs[0], NOW);
    b.on_packet_seen(&msgs[1], NOW);

    let req = b.initial_sync(A, NOW);
    let SyncOut::Request { to, packet } = &req else {
        panic!()
    };
    assert_eq!((*to, packet.recipient_id), (Some(A), Some(A)));
    let request = request_payload(&req);
    assert!(
        request.types.unwrap().contains(t::FRAGMENT),
        "initial sync covers all kinds"
    );

    let got = replies(a.handle_request(B, &request, NOW));
    let mut texts: Vec<_> = got
        .iter()
        .filter(|p| p.packet_type == t::MESSAGE)
        .map(|p| String::from_utf8(p.payload.clone()).unwrap())
        .collect();
    texts.sort();
    assert_eq!(texts, ["m2", "m3", "m4"]);
    assert!(got.iter().any(|p| p.packet_type == t::ANNOUNCE));
    assert!(got.iter().all(|p| p.ttl == 0 && p.is_rsr));

    // B accepts RSR from A for 30 s after asking.
    assert!(b.is_valid_response(&A, NOW + 30_000));
    assert!(!b.is_valid_response(&A, NOW + 30_001));
    assert!(!b.is_valid_response(&C, NOW));
}

#[test]
fn full_filters_carry_a_since_cursor() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    for i in 0..500 {
        a.on_packet_seen(&message(C, NOW - 100_000 + i * 10, &format!("m{i}")), NOW);
    }
    let SyncOut::Request { packet, .. } = a.initial_sync(B, NOW) else {
        panic!()
    };
    let r = RequestSync::decode(&packet.payload).unwrap();
    let since = r.since.expect("filter can't hold 500 IDs");
    assert!(since > NOW - 100_000);

    // The other side holds the same messages: nothing comes back, the old
    // tail is outside the cursor rather than missing.
    let mut b = GossipSync::new(B, SyncConfig::default());
    for i in 0..500 {
        b.on_packet_seen(&message(C, NOW - 100_000 + i * 10, &format!("m{i}")), NOW);
    }
    let back = replies(b.handle_request(A, &r, NOW));
    assert!(
        back.len() <= 5,
        "only filter false positives: {}",
        back.len()
    );
}

#[test]
fn responses_are_rate_limited() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    a.on_packet_seen(&message(C, NOW, "x"), NOW);
    let empty = RequestSync {
        p: 7,
        m: 1,
        data: vec![],
        types: None,
        since: None,
        fragment_ids: None,
    };
    for i in 0..8 {
        assert_eq!(a.handle_request(B, &empty, NOW + i).len(), 1);
    }
    assert!(a.handle_request(B, &empty, NOW + 9).is_empty());
    assert_eq!(a.handle_request(C, &empty, NOW + 9).len(), 1, "per peer");
    assert_eq!(a.handle_request(B, &empty, NOW + 30_001).len(), 1);
}

#[test]
fn only_fresh_broadcasts_are_kept() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    a.on_packet_seen(&message(C, NOW - 6 * 3600 * 1000 - 1, "too old"), NOW);
    a.on_packet_seen(&message(C, NOW - 6 * 3600 * 1000 + 1, "six hours"), NOW);
    a.on_packet_seen(&message(C, NOW + 120_001, "future"), NOW);
    let mut directed = message(C, NOW, "dm");
    directed.recipient_id = Some(B);
    a.on_packet_seen(&directed, NOW);
    let mut broadcast = message(C, NOW, "all");
    broadcast.recipient_id = Some([0xff; 8]);
    a.on_packet_seen(&broadcast, NOW);
    let old_fragment = Packet::new(t::FRAGMENT, C, NOW - 900_001, vec![0; 20], 7);
    a.on_packet_seen(&old_fragment, NOW);
    let texts: Vec<_> = a
        .public_messages(NOW)
        .iter()
        .map(|p| String::from_utf8(p.payload.clone()).unwrap())
        .collect();
    assert_eq!(texts, ["six hours", "all"]);

    let all_types = RequestSync {
        p: 7,
        m: 1,
        data: vec![],
        types: Some(SyncTypes::of(&[t::FRAGMENT])),
        since: None,
        fragment_ids: None,
    };
    assert!(a.handle_request(B, &all_types, NOW).is_empty());
}

#[test]
fn periodic_requests_follow_their_schedules() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    let kinds = |out: &[SyncOut]| -> Vec<SyncTypes> {
        out.iter()
            .map(|o| request_payload(o).types.unwrap())
            .collect()
    };
    let public = SyncTypes::of(&[t::ANNOUNCE, t::MESSAGE, t::GROUP_MESSAGE]);
    let frag = SyncTypes::of(&[t::FRAGMENT]);
    let file = SyncTypes::of(&[t::FILE_TRANSFER]);

    // Alone: broadcast. All three are due the first time.
    let first = a.maintenance(&[], NOW);
    assert_eq!(kinds(&first), [public, frag, file]);
    assert!(
        first
            .iter()
            .all(|o| matches!(o, SyncOut::Request { to: None, .. }))
    );
    assert_eq!(kinds(&a.maintenance(&[], NOW + 15_000)), [public]);
    // With neighbours: one unicast per peer per due schedule.
    let out = a.maintenance(&[B, C], NOW + 30_000);
    assert_eq!(kinds(&out), [public, public, frag, frag]);
    assert!(a.is_valid_response(&C, NOW + 30_000));
    assert_eq!(
        kinds(&a.maintenance(&[B], NOW + 60_000)),
        [public, frag, file]
    );
}

#[test]
fn leaving_and_stale_peers_are_forgotten() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    a.on_packet_seen(&message(B, NOW, "from b"), NOW);
    a.on_packet_seen(&message(C, NOW, "from c"), NOW);
    a.on_packet_seen(&Packet::new(t::ANNOUNCE, C, NOW - 30_000, vec![1], 7), NOW);
    a.remove_peer(&B);
    assert_eq!(a.message_count(), 1);

    // C's announce is stale (> 60 s old) by NOW + 40 s, but the stale check
    // runs at most once a minute.
    a.maintenance(&[], NOW + 1);
    a.maintenance(&[], NOW + 40_000);
    assert_eq!(a.message_count(), 1);
    a.maintenance(&[], NOW + 60_001);
    assert_eq!(a.message_count(), 0, "its messages go with it");
}

#[test]
fn fragment_requests_name_their_streams() {
    let mut a = GossipSync::new(A, SyncConfig::default());
    let frag = |stream: u8, index: u8| {
        let mut payload = vec![stream; 8];
        payload.extend_from_slice(&[0, index, 0, 2, t::MESSAGE, 9, 9]);
        Packet::new(t::FRAGMENT, C, NOW - 5, payload, 7)
    };
    for p in [frag(1, 0), frag(1, 1), frag(2, 0)] {
        a.on_packet_seen(&p, NOW);
    }
    let mut b = GossipSync::new(B, SyncConfig::default());
    let out = b.request_fragments(&[[2; 8]], &[A], NOW);
    let got = replies(a.handle_request(B, &request_payload(&out[0]), NOW));
    assert_eq!(got.len(), 1);
    assert_eq!(&got[0].payload[..8], &[2; 8]);
}

#[test]
fn saved_messages_restore_newest_first() {
    let config = SyncConfig {
        seen_capacity: 2,
        ..SyncConfig::default()
    };
    let mut a = GossipSync::new(A, config);
    let saved = vec![
        message(C, NOW - 7 * 3600 * 1000, "expired"),
        message(C, NOW - 3, "one"),
        message(C, NOW - 2, "two"),
        message(C, NOW - 1, "three"),
    ];
    a.restore_public_messages(saved, NOW);
    let texts: Vec<_> = a
        .public_messages(NOW)
        .iter()
        .map(|p| String::from_utf8(p.payload.clone()).unwrap())
        .collect();
    assert_eq!(texts, ["two", "three"]);
}
