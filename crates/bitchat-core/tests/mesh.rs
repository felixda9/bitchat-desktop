//! Two app engines talking over their mesh nodes, wired back to back.

use bitchat_core::engine::{Effect, Engine, MESH_CHANNEL, MeshPeer, mesh_key};
use bitchat_core::model::{DmStatus, UiEvent};
use bitchat_core::state::State;
use bitchat_mesh::MeshIdentity;
use bitchat_mesh::node::{MeshNode, Output};
use std::collections::VecDeque;

const T0: u64 = 1_790_000_000_000;

struct Side {
    engine: Engine,
    node: MeshNode,
    ui: Vec<UiEvent>,
}

impl Side {
    fn new(nick: &str) -> Side {
        let mut state = State::fresh();
        state.nickname = nick.to_string();
        let id = MeshIdentity::new(state.identity.noise_static, state.identity.signing_seed);
        Side {
            node: MeshNode::new(id, nick, T0),
            engine: Engine::new(state, true),
            ui: Vec::new(),
        }
    }

    fn key(&self) -> String {
        mesh_key(&self.node.peer_id())
    }
}

/// Moves effects into nodes, frames across the wire and node events back
/// into engines until both sides are quiet.
#[allow(clippy::needless_range_loop)] // side `i` writes to side `1 - i`
fn settle(sides: &mut [Side; 2], now: u64) {
    let mut wire: VecDeque<(usize, Vec<u8>)> = VecDeque::new();
    loop {
        let mut busy = false;
        for i in 0..2 {
            let mut outputs = Vec::new();
            for effect in sides[i].engine.take_effects() {
                busy = true;
                match effect {
                    Effect::MeshPublic { text } => {
                        outputs.extend(sides[i].node.send_public(&text, now))
                    }
                    Effect::MeshPrivate { peer, payload } => {
                        outputs.extend(sides[i].node.send_private(peer, payload, now))
                    }
                    Effect::Ui(e) => sides[i].ui.push(e),
                    _ => {}
                }
            }
            for output in outputs {
                busy = true;
                match output {
                    Output::Write { frame, .. } => wire.push_back((1 - i, frame)),
                    Output::Event(e) => sides[i].engine.on_mesh_event(e, now),
                    Output::Close { .. } => {}
                }
            }
        }
        while let Some((to, frame)) = wire.pop_front() {
            busy = true;
            let out = sides[to].node.on_frame(1, &frame, now);
            for output in out {
                match output {
                    Output::Write { frame, .. } => wire.push_back((1 - to, frame)),
                    Output::Event(e) => sides[to].engine.on_mesh_event(e, now),
                    Output::Close { .. } => {}
                }
            }
        }
        if !busy {
            break;
        }
    }
    for side in sides.iter_mut() {
        let peers = side
            .node
            .peers()
            .map(|p| MeshPeer {
                id: p.peer_id,
                nickname: p.nickname.clone(),
                direct: p.direct,
            })
            .collect();
        side.engine.set_mesh_peers(peers, now);
        for effect in side.engine.take_effects() {
            if let Effect::Ui(e) = effect {
                side.ui.push(e);
            }
        }
    }
}

fn run(sides: &mut [Side; 2], from: u64, ms: u64) -> u64 {
    let mut now = from;
    while now < from + ms {
        now += 50;
        let mut wire = Vec::new();
        for (i, side) in sides.iter_mut().enumerate() {
            for output in side.node.tick(now) {
                match output {
                    Output::Write { frame, .. } => wire.push((1 - i, frame)),
                    Output::Event(e) => side.engine.on_mesh_event(e, now),
                    Output::Close { .. } => {}
                }
            }
        }
        for (to, frame) in wire {
            for output in sides[to].node.on_frame(1, &frame, now) {
                if let Output::Event(e) = output {
                    sides[to].engine.on_mesh_event(e, now);
                }
            }
        }
        settle(sides, now);
    }
    now
}

#[test]
fn mesh_channel_people_messages_and_receipts() {
    let mut sides = [Side::new("alice"), Side::new("bob")];
    let (alice_key, bob_key) = (sides[0].key(), sides[1].key());
    let mut wire: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, side) in sides.iter_mut().enumerate() {
        side.engine.join_channel(MESH_CHANNEL, false, T0);
        for output in side.node.link_up(1, 512, T0) {
            if let Output::Write { frame, .. } = output {
                wire.push((1 - i, frame));
            }
        }
    }
    // Deliver the link-up announces.
    for (to, frame) in wire {
        for output in sides[to].node.on_frame(1, &frame, T0) {
            if let Output::Event(e) = output {
                sides[to].engine.on_mesh_event(e, T0);
            }
        }
    }
    let mut now = run(&mut sides, T0, 500);

    assert!(sides[0].ui.iter().any(|e| matches!(e,
        UiEvent::Channel { geohash: Some(g), .. } if g == MESH_CHANNEL)));
    let bob_listed = sides[0].ui.iter().rev().find_map(|e| match e {
        UiEvent::People {
            geohash, people, ..
        } if geohash == MESH_CHANNEL => Some(people.clone()),
        _ => None,
    });
    let people = bob_listed.expect("people for #mesh");
    assert!(
        people
            .iter()
            .any(|p| p.pubkey == bob_key && p.display.starts_with("bob#"))
    );
    assert!(people.iter().any(|p| p.me && p.pubkey == alice_key));

    // Public message: local echo for alice, delivered to bob.
    sides[0].engine.submit("hello mesh", now);
    settle(&mut sides, now);
    now = run(&mut sides, now, 300);
    let public = |s: &Side| -> Vec<(String, bool)> {
        s.ui.iter()
            .filter_map(|e| match e {
                UiEvent::Public { message } => Some((message.content.clone(), message.own)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(public(&sides[0]), vec![("hello mesh".to_string(), true)]);
    assert_eq!(public(&sides[1]), vec![("hello mesh".to_string(), false)]);

    // /msg by nickname: Noise handshake, then delivered + read once bob opens it.
    sides[0].engine.submit("/msg bob secret plans", now);
    settle(&mut sides, now);
    now = run(&mut sides, now, 1_000);
    let got = sides[1].ui.iter().find_map(|e| match e {
        UiEvent::Dm { peer, message } if !message.from_me => {
            Some((peer.clone(), message.content.clone()))
        }
        _ => None,
    });
    assert_eq!(got, Some((alice_key.clone(), "secret plans".to_string())));
    let status = |s: &Side| {
        s.ui.iter()
            .filter_map(|e| match e {
                UiEvent::DmStatus { status, .. } => Some(*status),
                _ => None,
            })
            .next_back()
    };
    assert_eq!(status(&sides[0]), Some(DmStatus::Delivered));
    sides[1].engine.open_dm(Some(&alice_key));
    settle(&mut sides, now);
    run(&mut sides, now, 300);
    assert_eq!(status(&sides[0]), Some(DmStatus::Read));
}
