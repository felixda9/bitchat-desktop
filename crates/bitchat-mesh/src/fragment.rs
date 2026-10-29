//! Fragmentation (iOS `BLEOutboundFragmentPlanner`, `BLEFragmentAssemblyBuffer`).
//!
//! A packet's encoded frame is cut into chunks; each travels in a `fragment`
//! packet whose payload is `id(8) || index u16 || total u16 || type u8 || chunk`.

use bitchat_proto::limits;
use bitchat_proto::{Packet, PeerId, message_type as t};
use std::collections::{BTreeMap, HashMap};

pub const DEFAULT_CHUNK: usize = 469;
pub const MIN_CHUNK: usize = 64;
pub const MAX_FRAGMENTS: usize = 10_000;
/// Assemblies older than this are dropped (ms).
pub const ASSEMBLY_TIMEOUT_MS: u64 = 30_000;
pub const MAX_IN_FLIGHT: usize = 128;
const HEADER: usize = 13;

/// Splits `packet` (encoded with or without padding) into fragment packets.
/// `recipient` overrides the original recipient (directed sends).
pub fn split(
    packet: &Packet,
    pad: bool,
    chunk_size: usize,
    fragment_id: [u8; 8],
    recipient: Option<PeerId>,
) -> Option<Vec<Packet>> {
    let frame = packet.encode(pad)?;
    let chunk_size = chunk_size.max(MIN_CHUNK);
    let chunks: Vec<&[u8]> = frame.chunks(chunk_size).collect();
    if chunks.is_empty() || chunks.len() > MAX_FRAGMENTS {
        return None;
    }
    let total = chunks.len() as u16;
    let version = if packet.route.is_empty() { 1 } else { 2 };
    Some(
        chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let mut payload = Vec::with_capacity(HEADER + chunk.len());
                payload.extend_from_slice(&fragment_id);
                payload.extend_from_slice(&(i as u16).to_be_bytes());
                payload.extend_from_slice(&total.to_be_bytes());
                payload.push(packet.packet_type);
                payload.extend_from_slice(chunk);
                Packet {
                    version,
                    packet_type: t::FRAGMENT,
                    ttl: packet.ttl,
                    timestamp: packet.timestamp,
                    sender_id: packet.sender_id,
                    recipient_id: recipient.or(packet.recipient_id),
                    payload,
                    signature: None,
                    route: packet.route.clone(),
                    is_rsr: packet.is_rsr,
                }
            })
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentHeader {
    pub sender: PeerId,
    pub id: [u8; 8],
    pub index: u16,
    pub total: u16,
    pub original_type: u8,
    pub data: Vec<u8>,
}

impl FragmentHeader {
    pub fn parse(packet: &Packet) -> Option<FragmentHeader> {
        let p = &packet.payload;
        if packet.packet_type != t::FRAGMENT || p.len() < HEADER {
            return None;
        }
        let index = u16::from_be_bytes([p[8], p[9]]);
        let total = u16::from_be_bytes([p[10], p[11]]);
        if total == 0 || total as usize > MAX_FRAGMENTS || index >= total {
            return None;
        }
        Some(FragmentHeader {
            sender: packet.sender_id,
            id: p[..8].try_into().expect("8 bytes"),
            index,
            total,
            original_type: p[12],
            data: p[HEADER..].to_vec(),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    Stored,
    /// The whole packet, decoded. iOS processes it with TTL 0 (never relayed).
    Complete(Box<Packet>),
    /// Over the size limit for its type; the assembly was dropped.
    Oversized,
    /// Reassembled bytes did not decode to a packet of the announced type.
    Invalid,
}

struct Assembly {
    parts: BTreeMap<u16, Vec<u8>>,
    original_type: u8,
    started_ms: u64,
}

#[derive(Default)]
pub struct Reassembler {
    assemblies: HashMap<(PeerId, [u8; 8]), Assembly>,
}

impl Reassembler {
    pub fn add(&mut self, header: FragmentHeader, now_ms: u64) -> Added {
        let key = (header.sender, header.id);
        if !self.assemblies.contains_key(&key) && self.assemblies.len() >= MAX_IN_FLIGHT {
            let oldest = self
                .assemblies
                .iter()
                .min_by_key(|(_, a)| a.started_ms)
                .map(|(k, _)| *k);
            if let Some(k) = oldest {
                self.assemblies.remove(&k);
            }
        }
        let assembly = self.assemblies.entry(key).or_insert_with(|| Assembly {
            parts: BTreeMap::new(),
            original_type: header.original_type,
            started_ms: now_ms,
        });
        let held: usize = assembly.parts.values().map(Vec::len).sum();
        let replaced = assembly.parts.get(&header.index).map_or(0, Vec::len);
        let limit = limits::max_payload_bytes(header.original_type) + limits::MAX_FRAME_OVERHEAD;
        if held - replaced + header.data.len() > limit {
            self.assemblies.remove(&key);
            return Added::Oversized;
        }
        assembly.parts.insert(header.index, header.data);
        if assembly.parts.len() < header.total as usize {
            return Added::Stored;
        }
        let assembly = self.assemblies.remove(&key).expect("present");
        let frame: Vec<u8> = (0..header.total)
            .filter_map(|i| assembly.parts.get(&i))
            .flatten()
            .copied()
            .collect();
        match Packet::decode(&frame) {
            Some(packet) if packet.packet_type == assembly.original_type => {
                Added::Complete(Box::new(packet))
            }
            _ => Added::Invalid,
        }
    }

    /// Drops assemblies older than 30 seconds.
    pub fn expire(&mut self, now_ms: u64) {
        self.assemblies
            .retain(|_, a| now_ms.saturating_sub(a.started_ms) < ASSEMBLY_TIMEOUT_MS);
    }

    pub fn in_flight(&self) -> usize {
        self.assemblies.len()
    }
}
