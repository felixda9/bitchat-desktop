//! Two app engines talking over their mesh nodes, wired back to back.

use bitchat_core::engine::{
    ACCOUNT_SUB, BRIDGE_SUB, Effect, Engine, MESH_CHANNEL, MeshPeer, geo_sub, mesh_key,
};
use bitchat_core::model::{DmStatus, UiEvent};
use bitchat_core::state::State;
use bitchat_mesh::MeshIdentity;
use bitchat_mesh::announce::caps;
use bitchat_mesh::node::NodeEvent;
use bitchat_mesh::node::{MeshNode, Output};
use bitchat_nostr::Event;
use bitchat_nostr::bridge as wire;
use bitchat_nostr::net::{PoolEvent, RelayStatus};
use std::collections::VecDeque;

const T0: u64 = 1_790_000_000_000;

struct Side {
    engine: Engine,
    node: MeshNode,
    ui: Vec<UiEvent>,
    /// Events published to relays while settling.
    relayed: Vec<Event>,
    subscriptions: Vec<(String, bitchat_nostr::Filter)>,
    /// Carriers sent by a side with no mesh links (drain).
    carriers: Vec<(Option<[u8; 8]>, Vec<u8>)>,
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
            relayed: Vec::new(),
            subscriptions: Vec::new(),
            carriers: Vec::new(),
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
                let side = &mut sides[i];
                match effect {
                    Effect::MeshPublic { text, timestamp } => {
                        outputs.extend(side.node.send_public(&text, timestamp))
                    }
                    Effect::MeshPrivate { peer, payload } => {
                        outputs.extend(side.node.send_private(peer, payload, now))
                    }
                    Effect::MeshCarrier { to, payload } => {
                        outputs.extend(side.node.send_carrier(to, payload, now))
                    }
                    Effect::MeshAdvertise { capabilities, cell } => {
                        outputs.extend(side.node.set_advertisement(capabilities, cell, now))
                    }
                    Effect::Publish { event, .. } => side.relayed.push(event),
                    Effect::Subscribe { id, filter, .. } => side.subscriptions.push((id, filter)),
                    Effect::Ui(e) => side.ui.push(e),
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
        for side in sides.iter_mut() {
            let peers = side
                .node
                .peers()
                .map(|p| MeshPeer {
                    id: p.peer_id,
                    noise_public: p.noise_public,
                    nickname: p.nickname.clone(),
                    direct: p.direct,
                    capabilities: p.capabilities,
                    bridge_cell: p.bridge_geohash.clone(),
                })
                .collect();
            side.engine.set_mesh_peers(peers, now);
        }
        if !busy {
            break;
        }
    }
}

fn run(sides: &mut [Side; 2], from: u64, ms: u64) -> u64 {
    let mut now = from;
    while now < from + ms {
        now += 50;
        let mut wire = Vec::new();
        for (i, side) in sides.iter_mut().enumerate() {
            side.engine.bridge_tick(now);
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

/// Alice and Bob in #mesh, linked and introduced.
fn meet() -> ([Side; 2], u64) {
    let mut sides = [Side::new("alice"), Side::new("bob")];
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
    let now = run(&mut sides, T0, 500);
    (sides, now)
}

#[test]
fn mesh_channel_people_messages_and_receipts() {
    let (mut sides, mut now) = meet();
    let (alice_key, bob_key) = (sides[0].key(), sides[1].key());

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

/// Events an engine published to relays.
fn published(side: &mut Side) -> Vec<Event> {
    let mut out = Vec::new();
    for effect in side.engine.take_effects() {
        match effect {
            Effect::Publish { event, .. } => out.push(event),
            Effect::Ui(e) => side.ui.push(e),
            _ => {}
        }
    }
    out
}

fn deliver_account(to: &mut Side, events: Vec<Event>, now: u64) {
    for event in events {
        to.engine.on_pool_event(
            PoolEvent::Event {
                subscription: ACCOUNT_SUB.into(),
                relay: "wss://r".into(),
                event,
            },
            now,
        );
    }
}

#[test]
fn favorites_keep_talking_over_nostr_when_apart() {
    let (mut sides, mut now) = meet();
    let (alice_key, bob_key) = (sides[0].key(), sides[1].key());

    // Favorite each other over Bluetooth: each learns the other's npub.
    sides[0].engine.submit("/fav bob", now);
    sides[1].engine.submit("/fav alice", now);
    settle(&mut sides, now);
    now = run(&mut sides, now, 1_000);
    for (side, other) in [(0, &bob_key), (1, &alice_key)] {
        let favs = sides[side].ui.iter().rev().find_map(|e| match e {
            UiEvent::Favorites { items } => Some(items.clone()),
            _ => None,
        });
        let f = favs
            .unwrap()
            .into_iter()
            .find(|f| &f.peer == other)
            .expect("favorite");
        assert!(f.ours && f.theirs && f.reachable, "{f:?}");
    }
    assert!(
        sides[1]
            .ui
            .iter()
            .any(|e| matches!(e, UiEvent::System { text } if text.contains("favorited you")))
    );
    assert!(
        !sides[1].ui.iter().any(|e| matches!(e, UiEvent::Dm { message, .. } if message.content.starts_with("[FAVORITED]"))),
        "the marker is not shown as a message"
    );

    // Bob walks out of Bluetooth range.
    sides[0].engine.set_mesh_peers(Vec::new(), now);
    sides[1].engine.set_mesh_peers(Vec::new(), now);
    sides[0].engine.send_dm(&bob_key, "are you home yet?", now);
    let wraps = published(&mut sides[0]);
    assert_eq!(wraps.len(), 1, "sent over Nostr");
    assert_eq!(wraps[0].kind, 1059);
    deliver_account(&mut sides[1], wraps, now);
    for e in sides[1].engine.take_effects() {
        if let Effect::Ui(e) = e {
            sides[1].ui.push(e);
        }
    }
    let got = sides[1].ui.iter().find_map(|e| match e {
        UiEvent::Dm { peer, message } if !message.from_me => {
            Some((peer.clone(), message.content.clone()))
        }
        _ => None,
    });
    assert_eq!(
        got,
        Some((alice_key.clone(), "are you home yet?".to_string())),
        "filed under alice"
    );

    // Bob's delivered receipt goes back over Nostr too.
    sides[1].engine.next_ack(now);
    let acks = published(&mut sides[1]);
    assert_eq!(acks.len(), 1);
    deliver_account(&mut sides[0], acks, now);
    for e in sides[0].engine.take_effects() {
        if let Effect::Ui(e) = e {
            sides[0].ui.push(e);
        }
    }
    assert!(sides[0].ui.iter().any(|e| matches!(e,
        UiEvent::DmStatus { peer, status: DmStatus::Delivered, .. } if *peer == bob_key)));
}

// ---- mesh bridge and gateway ----

/// Somewhere in Montréal.
const HERE: (f64, f64) = (45.5017, -73.5673);

fn relays_up(side: &mut Side, now: u64) {
    side.engine.on_pool_event(
        PoolEvent::Status {
            relay: "wss://relay.example".into(),
            status: RelayStatus::Connected,
        },
        now,
    );
}

/// Carries out a lone side's effects (no mesh links).
fn drain(side: &mut Side, now: u64) {
    for effect in side.engine.take_effects() {
        match effect {
            Effect::Publish { event, .. } => side.relayed.push(event),
            Effect::Subscribe { id, filter, .. } => side.subscriptions.push((id, filter)),
            Effect::Ui(e) => side.ui.push(e),
            Effect::MeshPublic { text, timestamp } => {
                side.node.send_public(&text, timestamp);
            }
            Effect::MeshCarrier { to, payload } => side.carriers.push((to, payload)),
            Effect::MeshAdvertise { capabilities, cell } => {
                side.node.set_advertisement(capabilities, cell, now);
            }
            _ => {}
        }
    }
}

/// On its own island, with internet and a location, bridge on, in #mesh.
fn bridge_side(nick: &str, now: u64) -> Side {
    let mut side = Side::new(nick);
    side.engine.set_location(Some(HERE), now);
    relays_up(&mut side, now);
    side.engine.set_bridge(true, now);
    side.engine.join_channel(MESH_CHANNEL, false, now);
    drain(&mut side, now);
    side
}

fn deliver(side: &mut Side, subscription: &str, event: Event, now: u64) {
    side.engine.on_pool_event(
        PoolEvent::Event {
            subscription: subscription.into(),
            relay: "wss://relay.example".into(),
            event,
        },
        now,
    );
}

fn rows(side: &Side, bridged: bool) -> Vec<bitchat_core::model::PublicMessage> {
    side.ui
        .iter()
        .filter_map(|e| match e {
            UiEvent::Public { message } if message.bridged == bridged => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn last_settings_cell(side: &Side) -> Option<String> {
    side.ui.iter().rev().find_map(|e| match e {
        UiEvent::Settings { bridge_cell, .. } => Some(bridge_cell.clone()),
        _ => None,
    })?
}

#[test]
fn the_bridge_joins_islands_and_the_radio_copy_wins() {
    let now = T0;
    let cell = bitchat_proto::geohash::encode(HERE.0, HERE.1, 6);
    let mut alice = bridge_side("alice", now);
    let mut carol = bridge_side("carol", now);

    // Own cell and its 8 neighbours; a presence heartbeat; the cell shown.
    let (_, filter) = carol
        .subscriptions
        .iter()
        .find(|(id, _)| id == BRIDGE_SUB)
        .unwrap();
    assert_eq!(filter.tags["r"].len(), 9);
    assert_eq!(filter.tags["r"][0], cell);
    assert!(alice.relayed.iter().any(|e| e.kind == 20001));
    assert_eq!(last_settings_cell(&alice), Some(cell.clone()));

    let sent_at = now + 1000;
    alice.engine.submit("hello across town", sent_at);
    drain(&mut alice, sent_at);
    let event = alice
        .relayed
        .iter()
        .find(|e| e.kind == 20000)
        .unwrap()
        .clone();
    assert!(event.verify());
    assert_eq!(event.tag_value("r"), Some(cell.as_str()));
    let alice_hex = hex::encode(alice.node.peer_id());
    let radio_id = wire::stable_id(&alice_hex, sent_at, "hello across town");
    let m = event.tags.iter().find(|t| t[0] == "m").unwrap();
    assert_eq!(
        m[1..],
        [radio_id.clone(), alice_hex.clone(), sent_at.to_string()]
    );

    // Her own copy coming back from the relay is not shown twice.
    deliver(&mut alice, BRIDGE_SUB, event.clone(), now + 1500);
    drain(&mut alice, now + 1500);
    assert_eq!(rows(&alice, false).len(), 1);
    assert!(rows(&alice, true).is_empty());

    deliver(&mut carol, BRIDGE_SUB, event.clone(), now + 1500);
    drain(&mut carol, now + 1500);
    let row = rows(&carol, true).pop().expect("bridged row");
    assert_eq!(row.id, event.id);
    assert_eq!(row.content, "hello across town");
    assert!(row.display.starts_with("alice#"));
    let people = carol
        .ui
        .iter()
        .rev()
        .find_map(|e| match e {
            UiEvent::People { people, .. } => Some(people.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        people
            .iter()
            .any(|p| p.bridged && p.display.starts_with("alice#"))
    );

    // Carol serves her island: after a short jitter it goes out on her radio.
    carol.engine.bridge_tick(now + 3100);
    drain(&mut carol, now + 3100);
    let (to, payload) = carol.carriers.last().expect("downlink");
    assert!(to.is_none());
    let carrier = wire::Carrier::decode(payload).unwrap();
    assert_eq!(carrier.direction, wire::Direction::FromBridge);
    assert_eq!(carrier.event().unwrap().id, event.id);

    // The islands meet: the radio copy replaces the bridged row.
    carol.engine.on_mesh_event(
        NodeEvent::PublicMessage {
            sender: alice.node.peer_id(),
            nickname: "alice".into(),
            text: "hello across town".into(),
            timestamp: sent_at,
        },
        now + 4000,
    );
    drain(&mut carol, now + 4000);
    assert!(
        carol
            .ui
            .iter()
            .any(|e| matches!(e, UiEvent::RemovePublic { id } if *id == event.id))
    );
    assert_eq!(rows(&carol, false).pop().unwrap().id, radio_id);
}

#[test]
fn a_phone_without_internet_bridges_through_a_neighbour() {
    let (mut sides, mut now) = meet();
    let cell = bitchat_proto::geohash::encode(HERE.0, HERE.1, 6);
    sides[0].engine.set_location(Some(HERE), now);
    relays_up(&mut sides[0], now);
    sides[0].engine.set_bridge(true, now);
    sides[1].engine.set_bridge(true, now);
    settle(&mut sides, now);
    now = run(&mut sides, now, 1000);
    // Bob has no location: he meets on the cell alice announces.
    assert_eq!(last_settings_cell(&sides[1]), Some(cell.clone()));
    let alice_seen_by_bob = sides[1].node.peer(&sides[0].node.peer_id()).unwrap();
    assert_ne!(alice_seen_by_bob.capabilities & caps::BRIDGE, 0);
    assert_ne!(alice_seen_by_bob.capabilities & caps::GATEWAY, 0);

    sides[1].engine.submit("from a phone with no internet", now);
    settle(&mut sides, now);
    let deposited = sides[0]
        .relayed
        .iter()
        .find(|e| e.kind == 20000 && e.content == "from a phone with no internet")
        .expect("alice published the deposit")
        .clone();
    assert!(deposited.verify());
    assert_eq!(deposited.tag_value("r"), Some(cell.as_str()));
    let m = deposited.tags.iter().find(|t| t[0] == "m").unwrap();
    assert_eq!(m[2], hex::encode(sides[1].node.peer_id()));
    // Alice heard it once, over the radio.
    let heard = rows(&sides[0], false)
        .into_iter()
        .filter(|r| r.content == "from a phone with no internet")
        .count();
    assert_eq!(heard, 1);
}

#[test]
fn the_gateway_carries_a_location_channel_both_ways() {
    let (mut sides, mut now) = meet();
    relays_up(&mut sides[0], now);
    sides[0].engine.set_bridge(true, now);
    for side in sides.iter_mut() {
        side.engine.join_channel("u4pruy", true, now);
    }
    settle(&mut sides, now);
    now = run(&mut sides, now, 1000);

    // Bob has no relay: his signed channel message goes to alice.
    sides[1].engine.submit("via the gateway", now);
    let mine = sides[1]
        .engine
        .take_effects()
        .into_iter()
        .find_map(|e| match e {
            Effect::Mine(req) => Some(req),
            _ => None,
        })
        .unwrap();
    sides[1]
        .engine
        .finish_public(mine.id, vec!["nonce".into(), "0".into(), "8".into()]);
    settle(&mut sides, now);
    let uplinked = sides[0]
        .relayed
        .iter()
        .find(|e| e.content == "via the gateway")
        .expect("alice published it");
    assert_eq!(uplinked.tag_value("g"), Some("u4pruy"));
    assert!(
        rows(&sides[0], false)
            .iter()
            .any(|r| r.content == "via the gateway")
    );
    assert!(
        sides[1]
            .ui
            .iter()
            .any(|e| matches!(e, UiEvent::System { text } if text == "sent via mesh gateway"))
    );

    // And the channel from the internet reaches bob over the radio.
    let dave = bitchat_nostr::Keys::generate();
    let event = Event::new(
        dave.public_key_hex(),
        (now / 1000) as i64,
        20000,
        vec![
            vec!["g".into(), "u4pruy".into()],
            vec!["n".into(), "dave".into()],
        ],
        "hello from the internet".into(),
    )
    .sign(&dave);
    deliver(&mut sides[0], &geo_sub("u4pruy"), event, now);
    settle(&mut sides, now);
    let row = rows(&sides[1], false)
        .into_iter()
        .find(|r| r.content == "hello from the internet")
        .expect("bob got it over the mesh");
    assert!(row.display.starts_with("dave#"));
}
