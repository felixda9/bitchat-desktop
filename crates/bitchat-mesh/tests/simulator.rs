//! Loopback mesh: several nodes wired together in memory, on a virtual clock.

use bitchat_mesh::MeshIdentity;
use bitchat_mesh::node::{LinkId, LinkRole, MeshNode, NodeEvent, Output};
use bitchat_proto::PeerId;
use std::collections::{HashMap, VecDeque};

const START: u64 = 1_790_000_000_000;
const STEP_MS: u64 = 10;

struct Sim {
    nodes: Vec<MeshNode>,
    /// (node, link) → (other node, its link)
    wires: HashMap<(usize, LinkId), (usize, LinkId)>,
    queue: VecDeque<(usize, LinkId, Vec<u8>)>,
    events: Vec<Vec<NodeEvent>>,
    /// Largest frame written per node, and the link limit it had to fit.
    largest_frame: usize,
    mtu: usize,
    now: u64,
    next_link: LinkId,
}

impl Sim {
    fn new(names: &[&str], mtu: usize) -> Sim {
        let nodes = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let id = MeshIdentity::new([i as u8 + 1; 32], [i as u8 + 101; 32]);
                MeshNode::new(id, name, START)
            })
            .collect();
        Sim {
            nodes,
            wires: HashMap::new(),
            queue: VecDeque::new(),
            events: vec![Vec::new(); names.len()],
            largest_frame: 0,
            mtu,
            now: START,
            next_link: 1,
        }
    }

    fn id(&self, node: usize) -> PeerId {
        self.nodes[node].peer_id()
    }

    fn handle(&mut self, node: usize, outputs: Vec<Output>) {
        for output in outputs {
            match output {
                Output::Write { link, frame } => {
                    self.largest_frame = self.largest_frame.max(frame.len());
                    if let Some(&(other, other_link)) = self.wires.get(&(node, link)) {
                        self.queue.push_back((other, other_link, frame));
                    }
                }
                Output::Close { link } => {
                    if let Some(other) = self.wires.remove(&(node, link)) {
                        self.wires.remove(&other);
                        let out = self.nodes[other.0].link_down(other.1);
                        self.handle(other.0, out);
                    }
                }
                Output::Event(e) => self.events[node].push(e),
            }
        }
    }

    fn settle(&mut self) {
        while let Some((node, link, frame)) = self.queue.pop_front() {
            let out = self.nodes[node].on_frame(link, &frame, self.now);
            self.handle(node, out);
        }
    }

    fn connect(&mut self, a: usize, b: usize) {
        let (la, lb) = (self.next_link, self.next_link + 1);
        self.next_link += 2;
        self.wires.insert((a, la), (b, lb));
        self.wires.insert((b, lb), (a, la));
        let out = self.nodes[a].link_up(la, self.mtu, self.now);
        self.handle(a, out);
        let out = self.nodes[b].link_up(lb, self.mtu, self.now);
        self.handle(b, out);
        self.settle();
    }

    fn disconnect(&mut self, a: usize, b: usize) {
        let links: Vec<_> = self
            .wires
            .iter()
            .filter(|((n, _), (m, _))| (*n == a && *m == b) || (*n == b && *m == a))
            .map(|(k, _)| *k)
            .collect();
        for (node, link) in links {
            self.wires.remove(&(node, link));
            let out = self.nodes[node].link_down(link);
            self.handle(node, out);
        }
    }

    fn run(&mut self, ms: u64) {
        let end = self.now + ms;
        while self.now < end {
            self.now += STEP_MS;
            for i in 0..self.nodes.len() {
                let out = self.nodes[i].tick(self.now);
                self.handle(i, out);
            }
            self.settle();
        }
    }

    fn act(&mut self, node: usize, f: impl FnOnce(&mut MeshNode, u64) -> Vec<Output>) {
        let now = self.now;
        let out = f(&mut self.nodes[node], now);
        self.handle(node, out);
        self.settle();
    }

    fn public_texts(&self, node: usize) -> Vec<(PeerId, String, String)> {
        self.events[node]
            .iter()
            .filter_map(|e| match e {
                NodeEvent::PublicMessage {
                    sender,
                    nickname,
                    text,
                    ..
                } => Some((*sender, nickname.clone(), text.clone())),
                _ => None,
            })
            .collect()
    }

    fn private(&self, node: usize) -> Vec<(PeerId, Vec<u8>)> {
        self.events[node]
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Private { from, plaintext } => Some((*from, plaintext.clone())),
                _ => None,
            })
            .collect()
    }

    fn knows(&self, node: usize, other: usize) -> bool {
        self.nodes[node].peer(&self.id(other)).is_some()
    }
}

/// A - B - C
fn line() -> Sim {
    let mut sim = Sim::new(&["alice", "bob", "carol"], 512);
    sim.connect(0, 1);
    sim.connect(1, 2);
    sim.run(2_000);
    sim
}

#[test]
fn announces_spread_across_hops() {
    let sim = line();
    assert!(
        sim.knows(0, 1) && sim.knows(0, 2),
        "alice sees bob and carol"
    );
    assert!(
        sim.knows(2, 0) && sim.knows(2, 1),
        "carol sees alice and bob"
    );
    assert_eq!(sim.nodes[2].peer(&sim.id(0)).unwrap().nickname, "alice");
    assert!(sim.nodes[0].peer(&sim.id(1)).unwrap().direct);
    assert!(!sim.nodes[0].peer(&sim.id(2)).unwrap().direct);
    assert!(
        sim.events[2]
            .iter()
            .any(|e| matches!(e, NodeEvent::PeerSeen(p) if p.peer_id == sim.id(0)))
    );
    assert!(
        sim.events[2]
            .iter()
            .any(|e| matches!(e, NodeEvent::PeerConnected(p) if p.peer_id == sim.id(1)))
    );
}

#[test]
fn public_messages_are_relayed_once() {
    let mut sim = line();
    sim.act(0, |n, now| n.send_public("hello mesh", now));
    sim.run(1_000);
    let alice = sim.id(0);
    for node in [1, 2] {
        assert_eq!(
            sim.public_texts(node),
            vec![(alice, "alice".into(), "hello mesh".into())]
        );
    }
    assert!(sim.public_texts(0).is_empty(), "no echo of our own message");
}

#[test]
fn a_triangle_delivers_each_message_once() {
    let mut sim = Sim::new(&["alice", "bob", "carol"], 512);
    sim.connect(0, 1);
    sim.connect(1, 2);
    sim.connect(0, 2);
    sim.run(2_000);
    sim.act(0, |n, now| n.send_public("once", now));
    sim.run(1_000);
    assert_eq!(sim.public_texts(1).len(), 1);
    assert_eq!(sim.public_texts(2).len(), 1);
}

#[test]
fn private_messages_cross_a_relay_over_noise() {
    let mut sim = line();
    let (alice, carol) = (sim.id(0), sim.id(2));
    sim.act(0, |n, now| {
        n.send_private(carol, b"\x01secret".to_vec(), now)
    });
    sim.run(2_000);
    assert_eq!(sim.private(2), vec![(alice, b"\x01secret".to_vec())]);
    assert!(sim.nodes[0].has_session(&carol) && sim.nodes[2].has_session(&alice));
    assert!(sim.private(1).is_empty(), "bob only relays ciphertext");
    assert!(
        sim.events[0]
            .iter()
            .any(|e| matches!(e, NodeEvent::PeerState { peer, .. } if *peer == carol))
    );

    sim.act(2, |n, now| n.send_private(alice, b"\x03ack".to_vec(), now));
    sim.run(500);
    assert_eq!(sim.private(0), vec![(carol, b"\x03ack".to_vec())]);
}

#[test]
fn long_messages_are_fragmented_to_fit_links() {
    let mut sim = Sim::new(&["alice", "bob", "carol"], 185);
    sim.connect(0, 1);
    sim.connect(1, 2);
    sim.run(2_000);
    let long: String = (0..60).map(|i| format!("w{i:03} ")).collect();
    sim.act(0, |n, now| n.send_public(&long, now));
    sim.run(1_000);
    assert_eq!(sim.public_texts(2).len(), 1);
    assert_eq!(sim.public_texts(2)[0].2, long);
    assert!(
        sim.largest_frame <= 185,
        "largest frame {}",
        sim.largest_frame
    );

    let carol = sim.id(2);
    let big = [vec![0x01], vec![b'x'; 900]].concat();
    sim.act(0, |n, now| n.send_private(carol, big.clone(), now));
    sim.run(2_000);
    assert_eq!(sim.private(2).last().map(|(_, p)| p.clone()), Some(big));
    assert!(
        sim.largest_frame <= 185,
        "largest frame {}",
        sim.largest_frame
    );
}

#[test]
fn a_late_joiner_catches_up_by_sync() {
    let mut sim = Sim::new(&["alice", "bob", "dave"], 512);
    sim.connect(0, 1);
    sim.run(2_000);
    for text in ["one", "two", "three"] {
        sim.act(0, |n, now| n.send_public(text, now));
        sim.run(100);
    }
    // Alice leaves the area; Dave arrives next to Bob only.
    sim.disconnect(0, 1);
    sim.run(500);
    sim.connect(1, 2);
    sim.run(3_000);
    let texts: Vec<String> = sim.public_texts(2).into_iter().map(|(_, _, t)| t).collect();
    let mut sorted = texts.clone();
    sorted.sort();
    assert_eq!(sorted, ["one", "three", "two"], "got {texts:?}");
    assert!(sim.knows(2, 0), "alice's announce came with them");
}

#[test]
fn leaving_and_silent_peers_drop_out() {
    let mut sim = line();
    let (alice, carol) = (sim.id(0), sim.id(2));
    sim.act(2, |n, now| n.leave(now));
    sim.run(500);
    assert!(sim.events[0].contains(&NodeEvent::PeerLeft(carol)));
    assert!(!sim.knows(0, 2));

    // Alice goes out of range without saying goodbye.
    sim.disconnect(0, 1);
    sim.run(65_000);
    assert!(sim.events[1].contains(&NodeEvent::PeerLeft(alice)));
}

#[test]
fn a_peer_on_two_links_keeps_the_newest() {
    let mut sim = Sim::new(&["alice", "bob"], 512);
    sim.connect(0, 1);
    sim.run(500);
    // Bob shows up again under a new address while the old link lives on.
    sim.connect(0, 1);
    sim.run(500);
    let alice_links: Vec<_> = sim
        .wires
        .keys()
        .filter(|(n, _)| *n == 0)
        .map(|(_, l)| *l)
        .collect();
    assert_eq!(alice_links.len(), 1, "one link left: {alice_links:?}");
    assert!(alice_links[0] >= 3, "the newer one");
    sim.act(1, |n, now| n.send_public("still here", now));
    sim.run(200);
    assert_eq!(sim.public_texts(0).len(), 1);
}

#[test]
fn connected_neighbours_stay_without_announces() {
    let mut sim = Sim::new(&["alice", "bob"], 512);
    sim.connect(0, 1);
    sim.run(200);
    // Long quiet stretch on a live link: no PeerLeft.
    sim.run(90_000);
    assert!(sim.knows(0, 1) && sim.knows(1, 0));
    assert!(
        !sim.events[0]
            .iter()
            .any(|e| matches!(e, NodeEvent::PeerLeft(_)))
    );
}

#[test]
fn a_phone_linked_both_ways_keeps_both_links() {
    let mut sim = Sim::new(&["alice", "bob"], 512);
    // Alice connected to Bob, and Bob connected to Alice: one link each way.
    let (la, lb) = (sim.next_link, sim.next_link + 1);
    sim.next_link += 2;
    sim.wires.insert((0, la), (1, lb));
    sim.wires.insert((1, lb), (0, la));
    let out = sim.nodes[0].link_up(la, 512, sim.now);
    sim.handle(0, out);
    let out = sim.nodes[1].link_up_as(lb, 512, LinkRole::Incoming, sim.now);
    sim.handle(1, out);
    sim.settle();
    let (lc, ld) = (sim.next_link, sim.next_link + 1);
    sim.next_link += 2;
    sim.wires.insert((1, lc), (0, ld));
    sim.wires.insert((0, ld), (1, lc));
    let out = sim.nodes[1].link_up(lc, 512, sim.now);
    sim.handle(1, out);
    let out = sim.nodes[0].link_up_as(ld, 512, LinkRole::Incoming, sim.now);
    sim.handle(0, out);
    sim.settle();
    sim.run(1_000);
    assert_eq!(sim.wires.len(), 4, "no link was closed");
    sim.act(0, |n, now| n.send_public("both ways", now));
    sim.run(300);
    assert_eq!(sim.public_texts(1).len(), 1, "delivered once");
}

fn carriers(sim: &Sim, node: usize) -> Vec<(PeerId, Vec<u8>, bool)> {
    sim.events[node]
        .iter()
        .filter_map(|e| match e {
            NodeEvent::Carrier {
                from,
                payload,
                directed,
            } => Some((*from, payload.clone(), *directed)),
            _ => None,
        })
        .collect()
}

#[test]
fn carriers_reach_their_peer_or_everyone() {
    let mut sim = line();
    let (alice, carol) = (sim.id(0), sim.id(2));
    // A large payload is fragmented on the way.
    let deposit = vec![7u8; 900];
    sim.act(0, |n, now| {
        n.send_carrier(Some(carol), deposit.clone(), now)
    });
    sim.run(1_000);
    assert_eq!(carriers(&sim, 2), vec![(alice, deposit, true)]);
    assert!(
        carriers(&sim, 1).is_empty(),
        "bob only relays a directed carrier"
    );

    sim.act(2, |n, now| n.send_carrier(None, b"downlink".to_vec(), now));
    sim.run(1_000);
    for node in [0, 1] {
        assert_eq!(
            carriers(&sim, node),
            vec![(carol, b"downlink".to_vec(), false)]
        );
    }
}

#[test]
fn the_bridge_cell_rides_the_announce() {
    let mut sim = line();
    sim.act(1, |n, now| {
        n.set_advertisement(
            bitchat_mesh::announce::caps::BRIDGE | bitchat_mesh::announce::caps::GATEWAY,
            Some("u4pruy".into()),
            now,
        )
    });
    sim.run(1_000);
    let bob = sim.nodes[0].peer(&sim.id(1)).unwrap();
    assert_eq!(bob.bridge_geohash.as_deref(), Some("u4pruy"));
    assert_ne!(bob.capabilities & bitchat_mesh::announce::caps::BRIDGE, 0);
    let bob = sim.nodes[2].peer(&sim.id(1)).unwrap();
    assert_eq!(bob.bridge_geohash.as_deref(), Some("u4pruy"));
}
