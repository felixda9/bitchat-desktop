use bitchat_mesh::peer_state::PeerState;
use bitchat_mesh::sessions::{
    Event, HANDSHAKES_PER_MINUTE, INITIATOR_TIMEOUT_MS, REKEY_AGE_MS, RESPONDER_TIMEOUT_MS,
    SessionManager,
};
use bitchat_proto::PeerId;
use std::collections::VecDeque;

const T0: u64 = 1_700_000_000_000;

fn manager(seed: u8) -> (SessionManager, PeerId) {
    let secret = [seed; 32];
    let state = PeerState {
        capabilities: seed as u64,
        signing_public: [seed; 32],
    };
    let m = SessionManager::new(secret, &state);
    let id = m.local_id();
    (m, id)
}

/// Two nodes wired back to back. `lo` has the lower peer ID.
struct Net {
    lo: SessionManager,
    hi: SessionManager,
    lo_id: PeerId,
    hi_id: PeerId,
    lo_log: Vec<Event>,
    hi_log: Vec<Event>,
}

impl Net {
    fn new() -> Net {
        let (a, a_id) = manager(0x11);
        let (b, b_id) = manager(0x22);
        let ((lo, lo_id), (hi, hi_id)) = if a_id < b_id {
            ((a, a_id), (b, b_id))
        } else {
            ((b, b_id), (a, a_id))
        };
        Net {
            lo,
            hi,
            lo_id,
            hi_id,
            lo_log: Vec::new(),
            hi_log: Vec::new(),
        }
    }

    /// Delivers everything `events` (produced by lo if `from_lo`) sends,
    /// and the replies, until quiet. Other events go to the producer's log.
    fn pump(&mut self, from_lo: bool, events: Vec<Event>, now: u64) {
        let mut q: VecDeque<(bool, Event)> = events.into_iter().map(|e| (from_lo, e)).collect();
        while let Some((from_lo, e)) = q.pop_front() {
            let (dst, src) = if from_lo {
                (&mut self.hi, self.lo_id)
            } else {
                (&mut self.lo, self.hi_id)
            };
            let out = match e {
                Event::SendHandshake { message, .. } => dst.on_handshake(src, &message, now),
                Event::SendEncrypted { frame, .. } => dst.on_encrypted(src, &frame, now),
                other => {
                    if from_lo {
                        self.lo_log.push(other)
                    } else {
                        self.hi_log.push(other)
                    }
                    continue;
                }
            };
            q.extend(out.into_iter().map(|e| (!from_lo, e)));
        }
    }

    fn connect(&mut self, now: u64) {
        let events = self.lo.send(self.hi_id, b"\x01hello".to_vec(), now);
        self.pump(true, events, now);
        assert!(self.lo.is_established(&self.hi_id) && self.hi.is_established(&self.lo_id));
        self.lo_log.clear();
        self.hi_log.clear();
    }
}

fn received(log: &[Event]) -> Vec<Vec<u8>> {
    log.iter()
        .filter_map(|e| match e {
            Event::Received { plaintext, .. } => Some(plaintext.clone()),
            _ => None,
        })
        .collect()
}

fn count(log: &[Event], f: impl Fn(&Event) -> bool) -> usize {
    log.iter().filter(|e| f(e)).count()
}

fn handshake_message(events: &[Event]) -> Vec<u8> {
    match events {
        [Event::SendHandshake { message, .. }] => message.clone(),
        other => panic!("expected one handshake message, got {other:?}"),
    }
}

fn frames(events: Vec<Event>) -> Vec<Vec<u8>> {
    events
        .into_iter()
        .filter_map(|e| match e {
            Event::SendEncrypted { frame, .. } => Some(frame),
            _ => None,
        })
        .collect()
}

#[test]
fn queued_message_goes_out_after_handshake_and_state_is_exchanged() {
    let mut net = Net::new();
    let first = net.lo.send(net.hi_id, b"\x01hello".to_vec(), T0);
    assert_eq!(handshake_message(&first).len(), 32);
    net.pump(true, first, T0);

    assert!(net.lo.is_established(&net.hi_id));
    assert!(net.hi.is_established(&net.lo_id));
    assert_eq!(received(&net.hi_log), vec![b"\x01hello".to_vec()]);
    assert!(
        received(&net.lo_log).is_empty(),
        "peer state is not a payload"
    );

    // Each side got the other's state (initial + echo arrive, both decoded).
    let hi_state = PeerState {
        capabilities: 0x22,
        signing_public: [0x22; 32],
    };
    let lo_state = PeerState {
        capabilities: 0x11,
        signing_public: [0x11; 32],
    };
    let (lo_expect, hi_expect) = if net.lo_id == manager(0x11).1 {
        (hi_state, lo_state)
    } else {
        (lo_state, hi_state)
    };
    assert!(net.lo_log.contains(&Event::PeerState {
        peer: net.hi_id,
        state: lo_expect.clone()
    }));
    assert!(net.hi_log.contains(&Event::PeerState {
        peer: net.lo_id,
        state: hi_expect
    }));
    assert_eq!(
        count(&net.lo_log, |e| matches!(e, Event::PeerState { .. })),
        2,
        "initial + one echo"
    );
    assert_eq!(
        count(&net.lo_log, |e| matches!(e, Event::Established { .. })),
        1
    );

    // Both directions work afterwards without new handshakes.
    let back = net.hi.send(net.lo_id, b"\x01back".to_vec(), T0 + 1);
    assert_eq!(back.len(), 1);
    net.pump(false, back, T0 + 1);
    assert_eq!(received(&net.lo_log), vec![b"\x01back".to_vec()]);
}

#[test]
fn crossed_initiations_converge_on_one_session() {
    let mut net = Net::new();
    let from_lo = net.lo.send(net.hi_id, b"\x01from lo".to_vec(), T0);
    let from_hi = net.hi.send(net.lo_id, b"\x01from hi".to_vec(), T0);
    let lo_msg1 = handshake_message(&from_lo);
    let hi_msg1 = handshake_message(&from_hi);

    // lo keeps the initiator role and ignores hi's message 1.
    assert!(net.lo.on_handshake(net.hi_id, &hi_msg1, T0).is_empty());
    // hi yields and answers lo's.
    let reply = net.hi.on_handshake(net.lo_id, &lo_msg1, T0);
    net.pump(false, reply, T0);

    assert!(net.lo.is_established(&net.hi_id) && net.hi.is_established(&net.lo_id));
    assert_eq!(received(&net.hi_log), vec![b"\x01from lo".to_vec()]);
    assert_eq!(received(&net.lo_log), vec![b"\x01from hi".to_vec()]);
}

#[test]
fn late_crossed_initiation_is_ignored_then_rechecked() {
    let mut net = Net::new();
    let lo_msg1 = handshake_message(&net.lo.connect(net.hi_id, T0));
    let hi_msg1 = handshake_message(&net.hi.connect(net.lo_id, T0));
    let reply = net.hi.on_handshake(net.lo_id, &lo_msg1, T0);
    net.pump(false, reply, T0);
    assert!(net.lo.is_established(&net.hi_id));

    // hi's own message 1 shows up after lo finished as initiator.
    assert!(
        net.lo
            .on_handshake(net.hi_id, &hi_msg1, T0 + 100)
            .is_empty()
    );
    assert!(net.lo.is_established(&net.hi_id), "session untouched");
    // After the grace period lo runs one handshake to be sure both agree.
    assert!(net.lo.tick(T0 + 500).is_empty());
    let recheck = net.lo.tick(T0 + 1_000);
    assert_eq!(handshake_message(&recheck).len(), 32);
    net.pump(true, recheck, T0 + 1_000);
    assert!(net.lo.is_established(&net.hi_id) && net.hi.is_established(&net.lo_id));
    let ping = net.lo.send(net.hi_id, b"\x01ping".to_vec(), T0 + 1_001);
    net.pump(true, ping, T0 + 1_001);
    assert_eq!(received(&net.hi_log).last().unwrap(), b"\x01ping");
}

#[test]
fn static_key_must_match_claimed_peer_id() {
    let (mut real, real_id) = manager(0x33);
    let (mut victim, victim_id) = manager(0x44);
    let claimed: PeerId = [0xAB; 8];

    // Initiator side: we expected `claimed`, but the responder proves another key.
    let msg1 = handshake_message(&victim.connect(claimed, T0));
    let msg2 = handshake_message(&real.on_handshake(victim_id, &msg1, T0));
    let out = victim.on_handshake(claimed, &msg2, T0);
    assert!(
        !out.iter()
            .any(|e| matches!(e, Event::SendHandshake { .. } | Event::Established { .. })),
        "no message 3 for an impostor: {out:?}"
    );
    assert!(!victim.is_established(&claimed));

    // Responder side: message 3 proves a key that is not the sender's ID.
    let (mut victim, victim_id) = manager(0x44);
    real.remove(&victim_id);
    let msg1 = handshake_message(&real.connect(victim_id, T0));
    let msg2 = handshake_message(&victim.on_handshake(claimed, &msg1, T0));
    let msg3 = real
        .on_handshake(victim_id, &msg2, T0)
        .into_iter()
        .find_map(|e| match e {
            Event::SendHandshake { message, .. } => Some(message),
            _ => None,
        })
        .unwrap();
    let out = victim.on_handshake(claimed, &msg3, T0);
    assert!(!out.iter().any(|e| matches!(e, Event::Established { .. })));
    assert!(!victim.is_established(&claimed) && !victim.is_established(&real_id));
}

#[test]
fn forged_message_one_keeps_old_keys_for_receiving_then_recovers() {
    let mut net = Net::new();
    net.connect(T0);
    let (mut stranger, _) = manager(0x55);
    let forged = handshake_message(&stranger.connect(net.hi_id, T0));

    // Unauthenticated message 1 "from lo": hi answers it, pauses sending,
    // but still reads lo's traffic under the old keys.
    let t = T0 + 10;
    assert_eq!(net.hi.on_handshake(net.lo_id, &forged, t).len(), 1);
    assert!(!net.hi.is_established(&net.lo_id));
    let queued = net.hi.send(net.lo_id, b"\x01parked".to_vec(), t);
    assert!(queued.is_empty());
    let from_lo = net.lo.send(net.hi_id, b"\x01still works".to_vec(), t);
    net.pump(true, from_lo, t);
    assert_eq!(received(&net.hi_log), vec![b"\x01still works".to_vec()]);

    // Message 3 never comes: old keys return, then hi re-handshakes and
    // only then sends what it queued.
    let t = t + RESPONDER_TIMEOUT_MS + 1;
    let timeout = net.hi.tick(t);
    assert!(timeout.is_empty(), "{timeout:?}");
    stranger.remove(&net.hi_id);
    let again = handshake_message(&stranger.connect(net.hi_id, t));
    assert!(
        net.hi.on_handshake(net.lo_id, &again, t).is_empty(),
        "cooldown"
    );
    let recovery = net.hi.tick(t + 1);
    assert_eq!(handshake_message(&recovery).len(), 32);
    net.pump(false, recovery, t + 1);
    assert!(net.lo.is_established(&net.hi_id) && net.hi.is_established(&net.lo_id));
    assert_eq!(received(&net.lo_log), vec![b"\x01parked".to_vec()]);
}

#[test]
fn ciphertext_ahead_of_message_three_is_held() {
    let mut net = Net::new();
    let msg1 = handshake_message(&net.lo.send(net.hi_id, b"\x01early".to_vec(), T0));
    let msg2 = handshake_message(&net.hi.on_handshake(net.lo_id, &msg1, T0));
    let out = net.lo.on_handshake(net.hi_id, &msg2, T0);
    let msg3 = match &out[0] {
        Event::SendHandshake { message, .. } => message.clone(),
        other => panic!("{other:?}"),
    };
    // Frames sent right after message 3 arrive first.
    for frame in frames(out) {
        assert!(net.hi.on_encrypted(net.lo_id, &frame, T0).is_empty());
    }
    let done = net.hi.on_handshake(net.lo_id, &msg3, T0 + 5);
    let got: Vec<_> = done
        .iter()
        .filter_map(|e| match e {
            Event::Received { plaintext, .. } => Some(plaintext.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(got, vec![b"\x01early".to_vec()]);
    assert!(done.iter().any(|e| matches!(e, Event::PeerState { .. })));
}

#[test]
fn unanswered_handshake_times_out_and_returns_the_queue() {
    let (mut a, _) = manager(0x11);
    let silent: PeerId = [9; 8];
    let first = a.send(silent, b"\x01one".to_vec(), T0);
    assert_eq!(first.len(), 1);
    assert!(
        a.send(silent, b"\x01two".to_vec(), T0).is_empty(),
        "queued behind the handshake"
    );
    assert!(a.tick(T0 + INITIATOR_TIMEOUT_MS).is_empty());
    assert_eq!(
        a.tick(T0 + INITIATOR_TIMEOUT_MS + 1),
        vec![Event::Failed {
            peer: silent,
            unsent: vec![b"\x01one".to_vec(), b"\x01two".to_vec()]
        }]
    );
    assert!(!a.has_session(&silent));
}

#[test]
fn old_sessions_are_renewed() {
    let mut net = Net::new();
    net.connect(T0);
    let old_frame = frames(net.lo.send(net.hi_id, b"\x01old".to_vec(), T0 + 1)).remove(0);
    net.pump(
        true,
        vec![Event::SendEncrypted {
            to: net.hi_id,
            frame: old_frame.clone(),
        }],
        T0 + 1,
    );

    let t = T0 + REKEY_AGE_MS + 1;
    let rekey = net.lo.tick(t);
    assert_eq!(handshake_message(&rekey).len(), 32);
    net.pump(true, rekey, t);
    assert!(net.lo.is_established(&net.hi_id) && net.hi.is_established(&net.lo_id));
    assert!(
        net.hi.tick(t + 1).is_empty(),
        "hi's clock restarted with the new session"
    );

    let new = net.lo.send(net.hi_id, b"\x01new".to_vec(), t + 2);
    net.pump(true, new, t + 2);
    assert_eq!(received(&net.hi_log).last().unwrap(), b"\x01new");
    assert!(
        net.hi.on_encrypted(net.lo_id, &old_frame, t + 3).is_empty(),
        "old keys are gone"
    );
}

#[test]
fn bad_frames_do_not_disturb_the_session() {
    let mut net = Net::new();
    net.connect(T0);
    let frame = frames(net.lo.send(net.hi_id, b"\x01once".to_vec(), T0)).remove(0);
    assert_eq!(net.hi.on_encrypted(net.lo_id, &frame, T0).len(), 1);
    assert!(
        net.hi.on_encrypted(net.lo_id, &frame, T0).is_empty(),
        "replay"
    );
    let mut forged = frame.clone();
    forged[10] ^= 1;
    assert!(
        net.hi.on_encrypted(net.lo_id, &forged, T0).is_empty(),
        "forgery"
    );
    assert!(
        net.hi.on_encrypted(net.lo_id, &[0; 5], T0).is_empty(),
        "too short"
    );
    // Non-initial handshake bytes for an established session are ignored.
    assert!(net.hi.on_handshake(net.lo_id, &[7; 16], T0).is_empty());
    assert!(net.hi.is_established(&net.lo_id));
    let next = net.lo.send(net.hi_id, b"\x01next".to_vec(), T0);
    net.pump(true, next, T0);
    assert_eq!(received(&net.hi_log), vec![b"\x01next".to_vec()]);
}

#[test]
fn ciphertext_without_a_session_starts_a_handshake() {
    let (mut a, _) = manager(0x11);
    let out = a.on_encrypted([3; 8], &[0; 40], T0);
    assert_eq!(handshake_message(&out).len(), 32);
}

#[test]
fn handshakes_are_rate_limited_per_peer() {
    let (mut target, target_id) = manager(0x11);
    let claimed: PeerId = [0xCD; 8];
    let mut answered = 0;
    for i in 0..HANDSHAKES_PER_MINUTE + 3 {
        let (mut sender, _) = manager(0x60 + i as u8);
        let msg1 = handshake_message(&sender.connect(target_id, T0));
        answered += target.on_handshake(claimed, &msg1, T0 + i as u64).len();
    }
    assert_eq!(answered, HANDSHAKES_PER_MINUTE);
    // A minute later the peer may try again.
    let (mut sender, _) = manager(0x7f);
    let msg1 = handshake_message(&sender.connect(target_id, T0));
    assert_eq!(target.on_handshake(claimed, &msg1, T0 + 61_000).len(), 1);
}
