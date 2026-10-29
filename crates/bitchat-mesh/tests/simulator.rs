//! Loopback mesh: several nodes wired together in memory, on a virtual clock.

use bitchat_mesh::MeshIdentity;
use bitchat_mesh::node::{LinkId, MeshNode, NodeEvent, Output};
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
