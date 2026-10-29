use bitchat_mesh::announce::{Announce, caps};
use bitchat_mesh::fragment::{self, Added, FragmentHeader, Reassembler};
use bitchat_mesh::{MeshIdentity, verify};
use bitchat_proto::{Packet, message_type as t};
use serde_json::Value;

fn read(path: &str) -> Value {
    let full = format!("{}/../../vectors/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap()
}

#[test]
fn android_announce_vector() {
    let wire = read("imported/android-wire.json");
    let a = &wire["announce"];
    let bytes = hex::decode(a["hex"].as_str().unwrap()).unwrap();
    let decoded = Announce::decode(&bytes).unwrap();
    assert_eq!(decoded.nickname, "bob");
    assert_eq!(
        hex::encode(&decoded.noise_public),
        a["noise_public_key"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(&decoded.signing_public),
        a["signing_public_key"].as_str().unwrap()
    );
    assert_eq!(decoded.capabilities, Some(caps::PRIVATE_MEDIA));
    // Re-encoding drops only the unknown TLV.
    let expected = a["hex"].as_str().unwrap().replace("7f02cafe", "");
    assert_eq!(hex::encode(decoded.encode().unwrap()), expected);
    for bad in wire["must_reject"]["announce"].as_array().unwrap() {
        assert!(Announce::decode(&hex::decode(bad.as_str().unwrap()).unwrap()).is_none());
    }
}

#[test]
fn android_fragment_vector() {
    let wire = read("imported/android-wire.json");
    let f = &wire["fragment"];
    let packet = Packet::new(
        t::FRAGMENT,
        [9; 8],
        1,
        hex::decode(f["hex"].as_str().unwrap()).unwrap(),
        7,
    );
    let h = FragmentHeader::parse(&packet).unwrap();
    assert_eq!(hex::encode(h.id), f["fragment_id"].as_str().unwrap());
    assert_eq!(h.index, 1);
    assert_eq!(h.total, 3);
    assert_eq!(h.original_type, 2);
    assert_eq!(hex::encode(&h.data), "aabb");
}

#[test]
fn ios_signing_key_vector() {
    let courier = read("imported/upstream/courier-test-vectors.json");
    let seed: [u8; 32] = hex::decode(courier["inputs"]["signingSeed"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let id = MeshIdentity::new([1; 32], seed);
    assert_eq!(
        hex::encode(id.signing_public()),
        courier["signature"]["publicKey"].as_str().unwrap()
    );
}

#[test]
fn signed_packets_survive_relaying() {
    let id = MeshIdentity::new([3; 32], [4; 32]);
    let mut msg = id
        .public_message(
            "hello mesh, this is long enough to be compressed by deflate "
                .repeat(3)
                .trim(),
            1_790_000_000_000,
        )
        .unwrap();
    assert_eq!(msg.sender_id, id.peer_id());
    assert!(verify(&msg, &id.signing_public()));
    msg.ttl = 3;
    assert!(verify(&msg, &id.signing_public()), "TTL is not signed");
    let wire = msg.encode(false).unwrap();
    assert!(verify(
        &Packet::decode(&wire).unwrap(),
        &id.signing_public()
    ));
    msg.payload.push(b'!');
    assert!(!verify(&msg, &id.signing_public()));

    let announce = id
        .announce("desk", &[[7; 8]], 0, 1_790_000_000_000)
        .unwrap();
    assert!(verify(&announce, &id.signing_public()));
    let a = Announce::decode(&announce.payload).unwrap();
    assert_eq!(a.noise_public, id.noise_public());
    assert_eq!(a.capabilities, Some(0));
    assert_eq!(a.neighbors, Some(vec![[7; 8]]));
    assert_eq!(
        bitchat_noise::peer_id(&a.noise_public.clone().try_into().unwrap()),
        announce.sender_id
    );
}

#[test]
fn fragments_reassemble_in_any_order() {
    let id = MeshIdentity::new([5; 32], [6; 32]);
    // Pseudo-random bytes, so compression doesn't shrink it to one fragment.
    let mut original = id.public_message("placeholder", 1_790_000_000_000).unwrap();
    original.payload = (0..3000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    id.sign(&mut original);

    let parts = fragment::split(&original, false, fragment::DEFAULT_CHUNK, [1; 8], None).unwrap();
    assert!(parts.len() > 5);
    assert!(
        parts
            .iter()
            .all(|p| p.packet_type == t::FRAGMENT && p.signature.is_none())
    );

    let mut r = Reassembler::default();
    let now = 1_000_000u64;
    let mut order: Vec<usize> = (0..parts.len()).rev().collect();
    order.insert(2, order[0]); // a duplicate before the set completes
    let mut done = None;
    for i in order {
        match r.add(FragmentHeader::parse(&parts[i]).unwrap(), now) {
            Added::Stored => {}
            Added::Complete(p) => done = Some(*p),
            other => panic!("{other:?}"),
        }
    }
    let done = done.expect("completed");
    assert_eq!(done, original);
    assert!(verify(&done, &id.signing_public()));
    assert_eq!(r.in_flight(), 0);

    // Stale assemblies expire.
    r.add(FragmentHeader::parse(&parts[0]).unwrap(), now);
    r.expire(now + 31_000);
    assert_eq!(r.in_flight(), 0);
}

#[test]
fn reassembly_rejects_oversize_and_wrong_type() {
    let mut r = Reassembler::default();
    let now = 1_000_000u64;
    // Announce cap is 4 KiB + overhead.
    let big = FragmentHeader {
        sender: [1; 8],
        id: [2; 8],
        index: 0,
        total: 2,
        original_type: t::ANNOUNCE,
        data: vec![0; 8000],
    };
    assert_eq!(r.add(big, now), Added::Oversized);

    let id = MeshIdentity::new([5; 32], [6; 32]);
    let msg = id.public_message("hi", 1).unwrap();
    let mut parts = fragment::split(&msg, false, 64, [3; 8], None).unwrap();
    for p in &mut parts {
        p.payload[12] = t::ANNOUNCE; // claim a different type
    }
    let mut last = Added::Stored;
    for p in &parts {
        last = r.add(FragmentHeader::parse(p).unwrap(), now);
    }
    assert_eq!(last, Added::Invalid);
}
