//! Golomb-coded set filters and the REQUEST_SYNC payload
//! (iOS `GCSFilter`, `PacketIdUtil`, `RequestSyncPacket`, `SyncTypeFlags`).

use bitchat_proto::{Packet, message_type as t};
use sha2::{Digest, Sha256};

/// Highest Golomb-Rice parameter accepted from the wire.
pub const MAX_P: u8 = 32;
/// Largest filter (and fragment-ID list) accepted in a request.
pub const MAX_ACCEPT_BYTES: usize = 1024;
pub const MAX_FRAGMENT_IDS: usize = 60;

/// Sync membership ID: SHA-256(type ‖ sender ‖ BE64 timestamp ‖ payload)[0:16].
pub fn packet_id(packet: &Packet) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update([packet.packet_type]);
    h.update(packet.sender_id);
    h.update(packet.timestamp.to_be_bytes());
    h.update(&packet.payload);
    h.finalize()[..16].try_into().expect("16 bytes")
}

/// P = ceil(log2(1/fpr)), fpr clamped to [1e-6, 0.25].
pub fn derive_p(target_fpr: f64) -> u8 {
    let f = target_fpr.clamp(0.000001, 0.25);
    ((1.0 / f).log2().ceil() as u8).max(1)
}

pub fn estimate_max_elements(size_bytes: usize, p: u8) -> usize {
    let bits = (size_bytes * 8).max(8);
    let per = (p as usize + 2).max(3);
    (bits / per).max(1)
}

fn h64(id: &[u8]) -> u64 {
    let d = Sha256::digest(id);
    u64::from_be_bytes(d[..8].try_into().expect("8 bytes")) & 0x7fff_ffff_ffff_ffff
}

fn hash_range(count: usize, p: u8) -> u32 {
    if count == 0 {
        return 1;
    }
    match (count as u64).checked_mul(1u64 << p) {
        None => u32::MAX,
        Some(0) => 1,
        Some(v) => v.min(u32::MAX as u64) as u32,
    }
}

fn map_hash(hash: u64, modulo: u64) -> u64 {
    if modulo <= 1 {
        return 0;
    }
    match hash % modulo {
        0 => 1,
        v => v,
    }
}

/// The value a packet ID occupies in a filter with range `m`.
pub fn bucket(id: &[u8], m: u32) -> u64 {
    let modulo = m.max(1) as u64;
    if modulo <= 1 {
        return 0;
    }
    map_hash(h64(id), modulo)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
    /// How many of the input IDs (from the front) the filter covers.
    pub included: usize,
}

/// Builds a filter over `ids` (newest first). If it doesn't fit in
/// `max_bytes`, IDs are dropped from the tail, keeping `m` fixed.
pub fn build_filter(ids: &[[u8; 16]], max_bytes: usize, target_fpr: f64) -> Filter {
    let p = derive_p(target_fpr);
    if ids.is_empty() {
        return Filter {
            p,
            m: 1,
            data: Vec::new(),
            included: 0,
        };
    }
    let cap = estimate_max_elements(max_bytes, p);
    let range = hash_range(ids.len().min(cap), p).max(1);
    let modulo = range as u64;

    let encode_first = |count: usize| -> Vec<u8> {
        let mut mapped: Vec<u64> = ids[..count]
            .iter()
            .map(|id| map_hash(h64(id), modulo))
            .collect();
        mapped.sort_unstable();
        // Clamp into [1, m) and drop repeats.
        let mut values = Vec::with_capacity(mapped.len());
        if modulo > 1 {
            let mut last = 0;
            for v in mapped {
                let v = v.min(modulo - 1);
                if v > last {
                    values.push(v);
                    last = v;
                }
            }
        }
        if values.is_empty() {
            Vec::new()
        } else {
            encode(&values, p)
        }
    };

    let mut count = ids.len().min(cap);
    let mut data = encode_first(count);
    while data.len() > max_bytes && count > 1 {
        count = (count * 9 / 10).max(1);
        data = encode_first(count);
    }
    if data.len() > max_bytes {
        return Filter {
            p,
            m: range,
            data: Vec::new(),
            included: 0,
        };
    }
    let included = if data.is_empty() { 0 } else { count };
    Filter {
        p,
        m: range,
        data,
        included,
    }
}

struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    n: u8,
}

impl BitWriter {
    fn bit(&mut self, b: bool) {
        self.cur = (self.cur << 1) | b as u8;
        self.n += 1;
        if self.n == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.buf.push(self.cur << (8 - self.n));
        }
        self.buf
    }
}

/// Golomb-Rice over the deltas of ascending values; MSB first.
fn encode(sorted: &[u64], p: u8) -> Vec<u8> {
    let mut w = BitWriter {
        buf: Vec::new(),
        cur: 0,
        n: 0,
    };
    let mask = if p >= 64 { u64::MAX } else { (1u64 << p) - 1 };
    let mut prev = 0u64;
    for &v in sorted {
        let x = v.wrapping_sub(prev).wrapping_sub(1);
        prev = v;
        for _ in 0..(x >> p) {
            w.bit(true);
        }
        w.bit(false);
        let r = x & mask;
        for i in (0..p).rev() {
            w.bit((r >> i) & 1 == 1);
        }
    }
    w.finish()
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<bool> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1 == 1;
        self.pos += 1;
        Some(b)
    }
}

/// Values in the filter, ascending. Empty for out-of-range parameters.
pub fn decode(p: u8, m: u32, data: &[u8]) -> Vec<u64> {
    if !(1..=MAX_P).contains(&p) || m <= 1 {
        return Vec::new();
    }
    let mut r = BitReader { data, pos: 0 };
    let mut values = Vec::new();
    let mut acc: u64 = 0;
    'outer: loop {
        let mut q: u64 = 0;
        loop {
            match r.bit() {
                None => break 'outer,
                Some(true) => q += 1,
                Some(false) => break,
            }
        }
        let mut rem: u64 = 0;
        for _ in 0..p {
            match r.bit() {
                None => break 'outer,
                Some(b) => rem = (rem << 1) | b as u64,
            }
        }
        let x = (q << p).wrapping_add(rem).wrapping_add(1);
        acc = acc.wrapping_add(x);
        if acc >= m as u64 {
            break;
        }
        values.push(acc);
    }
    values
}

/// Packet types a sync request covers: bit per type, minimal LE bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncTypes(u64);

const TYPE_BITS: [(u8, u32); 11] = [
    (t::ANNOUNCE, 0),
    (t::MESSAGE, 1),
    (t::LEAVE, 2),
    (t::NOISE_HANDSHAKE, 3),
    (t::NOISE_ENCRYPTED, 4),
    (t::FRAGMENT, 5),
    (t::REQUEST_SYNC, 6),
    (t::FILE_TRANSFER, 7),
    (t::BOARD_POST, 8),
    (t::PREKEY_BUNDLE, 9),
    (t::GROUP_MESSAGE, 10),
];
const KNOWN_MASK: u64 = (1 << 11) - 1;

impl SyncTypes {
    pub const NONE: SyncTypes = SyncTypes(0);

    pub fn from_bits(bits: u64) -> SyncTypes {
        SyncTypes(bits & KNOWN_MASK)
    }

    pub fn of(types: &[u8]) -> SyncTypes {
        let mut bits = 0;
        for kind in types {
            if let Some((_, bit)) = TYPE_BITS.iter().find(|(k, _)| k == kind) {
                bits |= 1 << bit;
            }
        }
        SyncTypes(bits)
    }

    pub fn bits(self) -> u64 {
        self.0
    }

    pub fn contains(self, kind: u8) -> bool {
        TYPE_BITS
            .iter()
            .find(|(k, _)| *k == kind)
            .is_some_and(|(_, bit)| self.0 & (1 << bit) != 0)
    }

    pub fn union(self, other: SyncTypes) -> SyncTypes {
        SyncTypes(self.0 | other.0)
    }

    /// `None` when empty.
    pub fn encode(self) -> Option<Vec<u8>> {
        if self.0 == 0 {
            return None;
        }
        let bytes = self.0.to_le_bytes();
        let len = 8 - self.0.leading_zeros() as usize / 8;
        Some(bytes[..len].to_vec())
    }

    pub fn decode(data: &[u8]) -> Option<SyncTypes> {
        if !(1..=8).contains(&data.len()) {
            return None;
        }
        let mut raw = [0u8; 8];
        raw[..data.len()].copy_from_slice(data);
        Some(SyncTypes::from_bits(u64::from_le_bytes(raw)))
    }
}

/// REQUEST_SYNC payload: TLVs of (u8 type, BE16 length, value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSync {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
    pub types: Option<SyncTypes>,
    /// The filter only covers packets at or after this time.
    pub since: Option<u64>,
    /// Comma-separated 16-hex fragment stream IDs.
    pub fragment_ids: Option<String>,
}

impl RequestSync {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut put = |kind: u8, v: &[u8]| {
            out.push(kind);
            out.extend_from_slice(&(v.len() as u16).to_be_bytes());
            out.extend_from_slice(v);
        };
        put(0x01, &[self.p]);
        put(0x02, &self.m.to_be_bytes());
        put(0x03, &self.data);
        if let Some(types) = self.types.and_then(SyncTypes::encode) {
            put(0x04, &types);
        }
        if let Some(since) = self.since {
            put(0x05, &since.to_be_bytes());
        }
        if let Some(ids) = &self.fragment_ids {
            put(0x06, ids.as_bytes());
        }
        out
    }

    pub fn decode(data: &[u8]) -> Option<RequestSync> {
        let mut off = 0;
        let (mut p, mut m, mut filter) = (None, None, None);
        let (mut types, mut since, mut fragment_ids) = (None, None, None);
        while off + 3 <= data.len() {
            let kind = data[off];
            let len = u16::from_be_bytes([data[off + 1], data[off + 2]]) as usize;
            off += 3;
            let v = data.get(off..off + len)?;
            off += len;
            match kind {
                0x01 if len == 1 => p = Some(v[0]),
                0x02 if len == 4 => m = Some(u32::from_be_bytes(v.try_into().ok()?)),
                0x03 => {
                    if len > MAX_ACCEPT_BYTES {
                        return None;
                    }
                    filter = Some(v.to_vec());
                }
                0x04 => {
                    if let Some(decoded) = SyncTypes::decode(v) {
                        types = Some(decoded);
                    }
                }
                0x05 if len == 8 => since = Some(u64::from_be_bytes(v.try_into().ok()?)),
                0x06 if len <= MAX_ACCEPT_BYTES => {
                    if let Ok(s) = std::str::from_utf8(v) {
                        fragment_ids = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }
        let (p, m, data) = (p?, m?, filter?);
        if !(1..=MAX_P).contains(&p) || m == 0 {
            return None;
        }
        Some(RequestSync {
            p,
            m,
            data,
            types,
            since,
            fragment_ids,
        })
    }
}

/// `None` if no valid 8-byte IDs; at most 60.
pub fn encode_fragment_ids(ids: &[[u8; 8]]) -> Option<String> {
    let tokens: Vec<String> = ids.iter().take(MAX_FRAGMENT_IDS).map(hex::encode).collect();
    (!tokens.is_empty()).then(|| tokens.join(","))
}

pub fn decode_fragment_ids(filter: &str) -> Option<Vec<[u8; 8]>> {
    let mut ids: Vec<[u8; 8]> = Vec::new();
    for token in filter
        .split(',')
        .filter(|s| !s.is_empty())
        .take(MAX_FRAGMENT_IDS)
    {
        if token.len() != 16 {
            continue;
        }
        if let Ok(bytes) = hex::decode(token)
            && let Ok(id) = <[u8; 8]>::try_from(bytes.as_slice())
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    (!ids.is_empty()).then_some(ids)
}
