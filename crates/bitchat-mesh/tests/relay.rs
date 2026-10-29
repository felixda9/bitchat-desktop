//! Cases from iOS `RelayControllerTests`, `BLERouteForwardingPolicyTests`
//! and `BLEReceivePipelineTests`.

use bitchat_mesh::relay::{
    Deduplicator, HIGH_DEGREE, PacketKey, RelayDecision, RelayQueue, RoutePlan, decide,
    duplicate_cancels_relay, pick_delay, route_plan, should_deduplicate,
};
use bitchat_proto::{Packet, PeerId, message_type as t};
use sha2::{Digest, Sha256};

const LOCAL: PeerId = [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11];
const REMOTE: PeerId = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

fn packet(kind: u8, ttl: u8) -> Packet {
    Packet::new(kind, REMOTE, 1, vec![1, 2], ttl)
}

fn directed(kind: u8, ttl: u8, recipient: PeerId) -> Packet {
    let mut p = packet(kind, ttl);
    p.recipient_id = Some(recipient);
    p
}

fn relay(p: &Packet, degree: usize) -> Option<RelayDecision> {
    decide(p, LOCAL, degree)
}

#[test]
fn ttl_one_does_not_relay() {
    assert_eq!(relay(&packet(t::MESSAGE, 1), 0), None);
}

#[test]
fn handshakes_always_relay_with_ttl_decrement() {
    let d = relay(&packet(t::NOISE_HANDSHAKE, 3), 3).unwrap();
    assert_eq!(d.new_ttl, 2);
    assert_eq!(d.delay_ms, 10..=35);
}

#[test]
fn directed_traffic_for_us_or_from_us_is_not_relayed() {
    assert_eq!(relay(&directed(t::NOISE_ENCRYPTED, 7, LOCAL), 3), None);
    let mut own = packet(t::MESSAGE, 7);
    own.sender_id = LOCAL;
    assert_eq!(relay(&own, 3), None);
}

#[test]
fn directed_traffic_for_others_relays_deterministically() {
    for kind in [
        t::NOISE_ENCRYPTED,
        t::COURIER_ENVELOPE,
        t::PING,
        t::PONG,
        t::NOSTR_CARRIER,
    ] {
        let d = relay(&directed(kind, 7, [0x33; 8]), 10).unwrap();
        assert_eq!(d.new_ttl, 6, "no density clamp for {kind:#x}");
        assert_eq!(d.delay_ms, 20..=60);
    }
    let d = relay(&directed(t::FRAGMENT, 7, [0x33; 8]), 10).unwrap();
    assert_eq!((d.new_ttl, d.delay_ms), (6, 20..=60));
}

#[test]
fn fragments_and_voice_use_the_fragment_cap() {
    let d = relay(&packet(t::FRAGMENT, 10), 3).unwrap();
    assert_eq!((d.new_ttl, d.delay_ms), (6, 8..=25));
    assert_eq!(
        relay(&packet(t::FRAGMENT, 10), HIGH_DEGREE)
            .unwrap()
            .new_ttl,
        4
    );
    assert_eq!(relay(&packet(t::VOICE_FRAME, 7), 3).unwrap().new_ttl, 6);
    assert_eq!(
        relay(&packet(t::VOICE_FRAME, 7), HIGH_DEGREE)
            .unwrap()
            .new_ttl,
        4
    );
}

#[test]
fn request_sync_never_relays() {
    assert_eq!(relay(&packet(t::REQUEST_SYNC, 7), 3), None);
}

#[test]
fn broadcast_ttl_depends_on_density() {
    // Thin chains relay at full depth.
    assert_eq!(relay(&packet(t::MESSAGE, 7), 2).unwrap().new_ttl, 6);
    // Dense graphs clamp to 5.
    assert_eq!(
        relay(&packet(t::MESSAGE, 10), HIGH_DEGREE).unwrap().new_ttl,
        4
    );
    // In between: messages 6, announces and urgent board posts 7.
    assert_eq!(relay(&packet(t::MESSAGE, 7), 3).unwrap().new_ttl, 5);
    assert_eq!(relay(&packet(t::ANNOUNCE, 7), 3).unwrap().new_ttl, 6);
    let mut urgent = packet(t::BOARD_POST, 7);
    urgent.payload = vec![0x01, 0x00, 0x01, 0xAA, 0x09, 0x00, 0x01, 0x01];
    assert_eq!(relay(&urgent, 3).unwrap().new_ttl, 6);
    urgent.payload[7] = 0;
    assert_eq!(relay(&urgent, 3).unwrap().new_ttl, 5);
    // Never below 2 before the decrement.
    assert_eq!(
        relay(&packet(t::MESSAGE, 2), HIGH_DEGREE).unwrap().new_ttl,
        1
    );

    let delays: Vec<_> = [0, 2, 3, 5, 6, 9, 10, 40]
        .iter()
        .map(|&d| relay(&packet(t::MESSAGE, 7), d).unwrap().delay_ms)
        .collect();
    assert_eq!(
        delays,
        vec![
            10..=40,
            10..=40,
            60..=150,
            60..=150,
            80..=180,
            80..=180,
            100..=220,
            100..=220
        ]
    );
}

#[test]
fn jitter_stays_in_range() {
    for _ in 0..200 {
        let d = pick_delay(&(10..=12));
        assert!((10..=12).contains(&d));
    }
}

fn routed(sender: PeerId, recipient: PeerId, ttl: u8, route: Vec<PeerId>) -> Packet {
    let mut p = Packet::new(t::NOISE_ENCRYPTED, sender, 1, vec![1, 2], ttl);
    p.version = 2;
    p.recipient_id = Some(recipient);
    p.route = route;
    p
}

#[test]
fn source_routes() {
    let (one, two, three, four) = ([0x11; 8], [0x22; 8], [0x33; 8], [0x44; 8]);
    let none = |_: &PeerId| false;

    assert_eq!(
        route_plan(&routed(two, one, 7, vec![]), one, none),
        RoutePlan::Suppress,
        "for us"
    );
    assert_eq!(
        route_plan(&routed(two, three, 7, vec![]), one, none),
        RoutePlan::Flood,
        "no route"
    );

    // Origin forwards to the first hop, or floods if it is not connected.
    let p = routed(one, three, 6, vec![two]);
    match route_plan(&p, one, |id| *id == two) {
        RoutePlan::Forward { packet, next_hop } => assert_eq!((next_hop, packet.ttl), (two, 5)),
        other => panic!("{other:?}"),
    }
    assert_eq!(route_plan(&p, one, none), RoutePlan::Flood);

    // Intermediate forwards to the next hop; the last one to the recipient.
    let p = routed(one, four, 4, vec![two, three]);
    match route_plan(&p, two, |id| *id == three) {
        RoutePlan::Forward { packet, next_hop } => assert_eq!((next_hop, packet.ttl), (three, 3)),
        other => panic!("{other:?}"),
    }
    let p = routed(one, three, 4, vec![two]);
    match route_plan(&p, two, |id| *id == three) {
        RoutePlan::Forward { packet, next_hop } => assert_eq!((next_hop, packet.ttl), (three, 3)),
        other => panic!("{other:?}"),
    }

    // Expired routed packets stop; sync requests are never forwarded.
    assert_eq!(
        route_plan(&routed(one, three, 1, vec![two]), one, |_| true),
        RoutePlan::Suppress
    );
    let mut sync = routed(one, four, 7, vec![two, three]);
    sync.packet_type = t::REQUEST_SYNC;
    assert_eq!(route_plan(&sync, two, |_| true), RoutePlan::Suppress);
}

#[test]
fn packet_key_includes_a_payload_digest() {
    let p = Packet::new(t::MESSAGE, REMOTE, 1234, vec![1, 2], 7);
    let key = PacketKey::of(&p);
    assert_eq!(key.digest, Sha256::digest([1u8, 2])[..4]);
    assert_eq!(
        (key.sender, key.timestamp, key.packet_type),
        (REMOTE, 1234, t::MESSAGE)
    );
    // Same sender/millisecond/type but different payload: distinct.
    let other = Packet::new(t::MESSAGE, REMOTE, 1234, vec![3], 7);
    assert_ne!(PacketKey::of(&other), key);
}

#[test]
fn fragments_and_own_sync_replays_skip_dedup() {
    assert!(should_deduplicate(&packet(t::MESSAGE, 7), LOCAL));
    assert!(!should_deduplicate(&packet(t::FRAGMENT, 7), LOCAL));
    let mut replay = packet(t::MESSAGE, 0);
    replay.sender_id = LOCAL;
    assert!(!should_deduplicate(&replay, LOCAL));
    assert!(!duplicate_cancels_relay(2));
    assert!(duplicate_cancels_relay(3));
}

fn key(n: u64) -> PacketKey {
    PacketKey {
        sender: REMOTE,
        timestamp: n,
        packet_type: t::MESSAGE,
        digest: [0; 4],
    }
}

#[test]
fn dedup_expires_and_trims() {
    let mut d = Deduplicator::new(300_000, 1000);
    assert!(!d.is_duplicate(key(1), 0));
    assert!(d.is_duplicate(key(1), 299_999));
    assert!(!d.is_duplicate(key(1), 300_001), "expired after 5 minutes");

    let mut d = Deduplicator::new(300_000, 1000);
    for n in 0..=1000 {
        d.mark(key(n), 0);
    }
    // Past 1000 entries the oldest go, down to 750.
    assert!(!d.contains(&key(250)));
    assert!(d.contains(&key(251)) && d.contains(&key(1000)));
}

#[test]
fn relay_queue_fires_after_delay_and_cancels() {
    let mut q = RelayQueue::default();
    let a = packet(t::MESSAGE, 6);
    let mut b = packet(t::MESSAGE, 6);
    b.timestamp = 2;
    q.schedule(PacketKey::of(&a), a.clone(), 100);
    q.schedule(PacketKey::of(&b), b.clone(), 50);
    let mut lower = a.clone();
    lower.ttl = 3;
    q.schedule(PacketKey::of(&a), lower, 10);
    assert!(q.due(49).is_empty());
    assert_eq!(
        q.due(100),
        vec![b, a.clone()],
        "oldest first, first copy kept"
    );

    q.schedule(PacketKey::of(&a), a.clone(), 10);
    assert!(q.cancel(&PacketKey::of(&a)));
    assert!(q.due(1000).is_empty());
}
