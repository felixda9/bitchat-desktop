//! `cargo xtask mesh [nickname] [--inbox FILE]`: runs a mesh node over the
//! BLE central and prints what happens. Lines appended to the inbox file are
//! sent: plain text goes to the mesh, `/dm <name> text` is a private message.

use bitchat_ble::central::{BleEvent, Central};
use bitchat_mesh::MeshIdentity;
use bitchat_mesh::node::{MeshNode, NodeEvent, Output};
use bitchat_proto::PeerId;
use bitchat_proto::payload::{NoisePayload, PayloadType, PrivateMessage};
use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn stamp() -> String {
    let s = now_ms() / 1000 % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Same identity across runs, so the phone sees one peer.
fn identity() -> MeshIdentity {
    let path = super::root().join("target").join("mesh-cli-identity");
    let bytes = std::fs::read(&path)
        .ok()
        .filter(|b| b.len() == 64)
        .unwrap_or_else(|| {
            let mut b = vec![0u8; 64];
            getrandom::fill(&mut b).unwrap();
            std::fs::write(&path, &b).unwrap();
            b
        });
    MeshIdentity::new(
        bytes[..32].try_into().unwrap(),
        bytes[32..].try_into().unwrap(),
    )
}

fn message_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).unwrap();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode_upper(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

struct Cli {
    node: MeshNode,
    central: Central,
    names: HashMap<PeerId, String>,
}

impl Cli {
    fn name(&self, id: &PeerId) -> String {
        let nick = self.names.get(id).cloned().unwrap_or_else(|| "?".into());
        format!("{nick}#{}", &hex::encode(id)[..4])
    }

    fn apply(&mut self, outputs: Vec<Output>) {
        for output in outputs {
            match output {
                Output::Write { link, frame } => {
                    if let Some(p) = bitchat_proto::Packet::decode(&frame) {
                        let detail = match bitchat_mesh::gcs::RequestSync::decode(&p.payload) {
                            Some(r) if p.packet_type == 0x21 => format!(
                                " sync types {:?} m {} filter {}B since {:?}",
                                r.types.map(|t| t.bits()),
                                r.m,
                                r.data.len(),
                                r.since
                            ),
                            _ => String::new(),
                        };
                        log::debug!(
                            "tx link {link}: type {:#04x} ttl {} from {} rsr {} ({} bytes){detail}",
                            p.packet_type,
                            p.ttl,
                            &hex::encode(p.sender_id)[..4],
                            p.is_rsr,
                            frame.len()
                        );
                    }
                    self.central.write(link, frame)
                }
                Output::Close { link } => self.central.close(link),
                Output::Event(event) => self.event(event),
            }
        }
    }

    fn event(&mut self, event: NodeEvent) {
        match event {
            NodeEvent::PeerConnected(p) | NodeEvent::PeerSeen(p) => {
                let direct = if p.direct { "direct" } else { "via mesh" };
                self.names.insert(p.peer_id, p.nickname.clone());
                println!(
                    "{} [peer] {} ({direct}, {})",
                    stamp(),
                    self.name(&p.peer_id),
                    hex::encode(p.peer_id)
                );
            }
            NodeEvent::PeerLeft(id) => println!("{} [left] {}", stamp(), self.name(&id)),
            NodeEvent::PublicMessage {
                sender,
                nickname,
                text,
                ..
            } => {
                self.names.insert(sender, nickname);
                println!("{} <{}> {text}", stamp(), self.name(&sender));
            }
            NodeEvent::SessionUp(id) => {
                println!("{} [noise] session with {}", stamp(), self.name(&id))
            }
            NodeEvent::PeerState { peer, state } => {
                println!(
                    "{} [noise] {} capabilities {:#x}",
                    stamp(),
                    self.name(&peer),
                    state.capabilities
                )
            }
            NodeEvent::Private { from, plaintext } => self.private(from, plaintext),
            NodeEvent::Unsent { peer, payloads } => {
                println!(
                    "{} [dm] {} unreachable, {} not sent",
                    stamp(),
                    self.name(&peer),
                    payloads.len()
                )
            }
        }
    }

    fn private(&mut self, from: PeerId, plaintext: Vec<u8>) {
        let Some(payload) = NoisePayload::decode(&plaintext) else {
            println!(
                "{} [dm] {} sent unknown payload {:02x?}",
                stamp(),
                self.name(&from),
                plaintext.first()
            );
            return;
        };
        match payload.kind {
            PayloadType::PrivateMessage => {
                let Some(m) = PrivateMessage::decode(&payload.data) else {
                    return;
                };
                println!("{} [dm] {} → me: {}", stamp(), self.name(&from), m.content);
                // Tell the sender it arrived, then that it was read.
                for kind in [PayloadType::Delivered, PayloadType::ReadReceipt] {
                    let ack = NoisePayload::ack(kind, &m.message_id).encode();
                    let out = self.node.send_private(from, ack, now_ms());
                    self.apply(out);
                }
            }
            PayloadType::Delivered | PayloadType::ReadReceipt => {
                let id = String::from_utf8_lossy(&payload.data);
                let what = if payload.kind == PayloadType::Delivered {
                    "delivered"
                } else {
                    "read"
                };
                println!("{} [dm] {} {what} {id}", stamp(), self.name(&from));
            }
            other => println!("{} [dm] {} sent {other:?}", stamp(), self.name(&from)),
        }
    }

    fn command(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(rest) = line.strip_prefix("/dm ") {
            let (who, text) = rest.split_once(' ').unwrap_or((rest, ""));
            let who = who.trim_start_matches('@').to_lowercase();
            let target = self.node.peers().find(|p| {
                p.nickname.to_lowercase() == who || hex::encode(p.peer_id).starts_with(&who)
            });
            let Some(peer) = target.map(|p| p.peer_id) else {
                println!("{} [dm] no peer {who}", stamp());
                return;
            };
            let id = message_id();
            let Some(body) = PrivateMessage::new(id.clone(), text).encode() else {
                return;
            };
            let payload = NoisePayload::new(PayloadType::PrivateMessage, body).encode();
            println!("{} [dm] me → {}: {text} ({id})", stamp(), self.name(&peer));
            let out = self.node.send_private(peer, payload, now_ms());
            self.apply(out);
        } else if line == "/who" {
            for p in self.node.peers() {
                println!(
                    "  {}#{} {}",
                    p.nickname,
                    &hex::encode(p.peer_id)[..4],
                    if p.direct { "direct" } else { "" }
                );
            }
        } else {
            println!("{} <me> {line}", stamp());
            let out = self.node.send_public(line, now_ms());
            self.apply(out);
        }
    }
}

pub fn run(args: &[String]) {
    let mut nickname = "desktop".to_string();
    let mut inbox: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--inbox" {
            inbox = it.next().map(PathBuf::from);
        } else {
            nickname = a.clone();
        }
    }
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,btleplug=warn"),
    )
    .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let central = Central::start(tx);
        let identity = identity();
        println!("{} me: {nickname}#{} ({})", stamp(), &hex::encode(identity.peer_id())[..4], hex::encode(identity.peer_id()));
        let mut cli = Cli { node: MeshNode::new(identity, &nickname, now_ms()), central, names: HashMap::new() };

        // Read stdin lines (when interactive) and the inbox file.
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        {
            let line_tx = line_tx.clone();
            std::thread::spawn(move || {
                for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(path) = inbox {
            std::thread::spawn(move || {
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path);
                let mut pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                loop {
                    std::thread::sleep(Duration::from_millis(300));
                    let Ok(mut f) = std::fs::File::open(&path) else { continue };
                    if f.seek(SeekFrom::Start(pos)).is_err() {
                        continue;
                    }
                    let mut reader = std::io::BufReader::new(f);
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        pos += line.len() as u64;
                        let _ = line_tx.send(line.trim_end().to_string());
                        line.clear();
                    }
                }
            });
        }

        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                Some(event) = rx.recv() => match event {
                    BleEvent::Status(s) => println!("{} [ble] {s:?}", stamp()),
                    BleEvent::LinkUp { link, max_write, name } => {
                        println!("{} [ble] link {link} up: {name} (writes up to {max_write} bytes)", stamp());
                        let out = cli.node.link_up(link, max_write, now_ms());
                        cli.apply(out);
                    }
                    BleEvent::LinkDown { link } => {
                        println!("{} [ble] link {link} down", stamp());
                        let out = cli.node.link_down(link);
                        cli.apply(out);
                    }
                    BleEvent::Frame { link, frame } => {
                        if let Some(p) = bitchat_proto::Packet::decode(&frame) {
                            let detail = match bitchat_mesh::gcs::RequestSync::decode(&p.payload) {
                                Some(r) if p.packet_type == 0x21 => format!(" sync types {:?} m {} filter {}B since {:?}", r.types.map(|t| t.bits()), r.m, r.data.len(), r.since),
                                _ => String::new(),
                            };
                            log::debug!("rx link {link}: type {:#04x} ttl {} from {} rsr {} ts {} ({} bytes){detail}", p.packet_type, p.ttl, &hex::encode(p.sender_id)[..4], p.is_rsr, p.timestamp, frame.len());
                        }
                        let out = cli.node.on_frame(link, &frame, now_ms());
                        cli.apply(out);
                    }
                },
                Some(line) = line_rx.recv() => cli.command(&line),
                _ = tick.tick() => {
                    let out = cli.node.tick(now_ms());
                    cli.apply(out);
                }
            }
        }
    });
}
