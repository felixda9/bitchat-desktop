use bitchat_mesh::MeshIdentity;
use bitchat_mesh::peers::{AnnounceOutcome, PeerTable, Rejection};

const NOW: u64 = 1_790_000_000_000;

fn me() -> MeshIdentity {
    MeshIdentity::new([1; 32], [2; 32])
}

#[test]
fn announces_are_verified_and_pinned() {
    let local = me();
    let alice = MeshIdentity::new([3; 32], [4; 32]);
    let mut table = PeerTable::new(local.peer_id());

    let a = alice.announce("alice", &[], 0, NOW).unwrap();
    let AnnounceOutcome::New(p) = table.on_announce(&a, NOW).unwrap() else {
        panic!("expected new")
    };
    assert_eq!(p.nickname, "alice");
    assert!(p.direct);
    assert!(matches!(
        table.on_announce(&a, NOW + 1000),
        Ok(AnnounceOutcome::Updated(_))
    ));
    assert!(matches!(
        table.on_announce(&a, NOW + 130_000),
        Err(Rejection::ClockSkew)
    ));

    // Relayed copies (TTL < 7) are not direct.
    let mut relayed = alice.announce("alice", &[], 0, NOW + 2000).unwrap();
    relayed.ttl = 5;
    let Ok(AnnounceOutcome::Updated(p)) = table.on_announce(&relayed, NOW + 2000) else {
        panic!()
    };
    assert!(!p.direct);

    // Back after the reachability window counts as new again.
    let later = alice.announce("alice", &[], 0, NOW + 100_000).unwrap();
    assert!(matches!(
        table.on_announce(&later, NOW + 100_000),
        Ok(AnnounceOutcome::New(_))
    ));

    // Same Noise key, different signing key: refused after first use.
    let impostor = MeshIdentity::new([3; 32], [9; 32]);
    let fake = impostor.announce("alice", &[], 0, NOW + 101_000).unwrap();
    assert_eq!(
        table.on_announce(&fake, NOW + 101_000),
        Err(Rejection::SigningKeyMismatch)
    );
}

#[test]
fn announce_rejections() {
    let local = me();
    let bob = MeshIdentity::new([5; 32], [6; 32]);
    let mut table = PeerTable::new(local.peer_id());

    let own = local.announce("me", &[], 0, NOW).unwrap();
    assert_eq!(table.on_announce(&own, NOW), Err(Rejection::SelfAnnounce));

    let old = bob.announce("bob", &[], 0, NOW - 901_000).unwrap();
    assert_eq!(table.on_announce(&old, NOW), Err(Rejection::Stale));

    let mut spoofed = bob.announce("bob", &[], 0, NOW).unwrap();
    spoofed.sender_id = [7; 8];
    assert_eq!(
        table.on_announce(&spoofed, NOW),
        Err(Rejection::SenderMismatch)
    );

    let mut unsigned = bob.announce("bob", &[], 0, NOW).unwrap();
    unsigned.signature = None;
    assert_eq!(
        table.on_announce(&unsigned, NOW),
        Err(Rejection::MissingSignature)
    );

    let mut tampered = bob.announce("bob", &[], 0, NOW).unwrap();
    tampered.timestamp += 1;
    assert_eq!(
        table.on_announce(&tampered, NOW),
        Err(Rejection::InvalidSignature)
    );
}

#[test]
fn public_messages_need_the_senders_signature() {
    let local = me();
    let alice = MeshIdentity::new([3; 32], [4; 32]);
    let mallory = MeshIdentity::new([8; 32], [9; 32]);
    let mut table = PeerTable::new(local.peer_id());

    let msg = alice.public_message("hi mesh", NOW).unwrap();
    assert_eq!(
        table.accept_public_message(&msg, NOW),
        Err(Rejection::UnknownSender)
    );
    table
        .on_announce(&alice.announce("alice", &[], 0, NOW).unwrap(), NOW)
        .unwrap();
    assert_eq!(table.accept_public_message(&msg, NOW).unwrap(), "hi mesh");

    let mut forged = mallory.public_message("i am alice", NOW).unwrap();
    forged.sender_id = alice.peer_id();
    assert_eq!(
        table.accept_public_message(&forged, NOW),
        Err(Rejection::InvalidSignature)
    );

    // Synced history (RSR) may be hours old.
    let mut synced = alice
        .public_message("from earlier", NOW - 3 * 3600 * 1000)
        .unwrap();
    synced.is_rsr = true;
    assert_eq!(
        table.accept_public_message(&synced, NOW).unwrap(),
        "from earlier"
    );

    let leave = alice.leave(NOW).unwrap();
    assert!(table.on_leave(&leave).is_some());
    assert!(table.get(&alice.peer_id()).is_none());
}
