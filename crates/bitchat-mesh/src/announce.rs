//! Announce payload (iOS `AnnouncementPacket`): TLVs with one-byte type and
//! length. Unknown TLVs are skipped; nickname and both keys are required.

/// Feature bits (BitFoundation `PeerCapabilities`). Only advertise what is
/// actually implemented.
pub mod caps {
    pub const PREKEYS: u64 = 1 << 0;
    pub const WIFI_BULK: u64 = 1 << 1;
    pub const GATEWAY: u64 = 1 << 2;
    pub const GROUPS: u64 = 1 << 3;
    pub const BOARD: u64 = 1 << 4;
    pub const VOUCH: u64 = 1 << 5;
    pub const MESH_DIAGNOSTICS: u64 = 1 << 6;
    pub const BRIDGE: u64 = 1 << 7;
    pub const PRIVATE_MEDIA: u64 = 1 << 8;
    pub const PRIVATE_MEDIA_RECEIPTS: u64 = 1 << 9;
    pub const NON_DESTRUCTIVE_NOISE_REPLACEMENT: u64 = 1 << 10;
}

/// Most neighbour IDs carried in one announce.
pub const MAX_NEIGHBORS: usize = 10;

const NICKNAME: u8 = 0x01;
const NOISE_KEY: u8 = 0x02;
const SIGNING_KEY: u8 = 0x03;
const NEIGHBORS: u8 = 0x04;
const CAPABILITIES: u8 = 0x05;
const BRIDGE_GEOHASH: u8 = 0x06;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub nickname: String,
    pub noise_public: Vec<u8>,
    pub signing_public: Vec<u8>,
    pub neighbors: Option<Vec<[u8; 8]>>,
    pub capabilities: Option<u64>,
    pub bridge_geohash: Option<String>,
}

/// Little-endian, shortest form, at least one byte.
pub fn encode_capabilities(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value;
    loop {
        out.push(v as u8);
        v >>= 8;
        if v == 0 {
            return out;
        }
    }
}

pub fn decode_capabilities(data: &[u8]) -> u64 {
    data.iter()
        .take(8)
        .enumerate()
        .fold(0, |acc, (i, b)| acc | (*b as u64) << (8 * i))
}

fn push(out: &mut Vec<u8>, kind: u8, value: &[u8]) -> Option<()> {
    if value.len() > 255 {
        return None;
    }
    out.push(kind);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
    Some(())
}

impl Announce {
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        push(&mut out, NICKNAME, self.nickname.as_bytes())?;
        push(&mut out, NOISE_KEY, &self.noise_public)?;
        push(&mut out, SIGNING_KEY, &self.signing_public)?;
        if let Some(neighbors) = &self.neighbors
            && !neighbors.is_empty()
        {
            let bytes: Vec<u8> = neighbors
                .iter()
                .take(MAX_NEIGHBORS)
                .flatten()
                .copied()
                .collect();
            push(&mut out, NEIGHBORS, &bytes)?;
        }
        if let Some(c) = self.capabilities {
            push(&mut out, CAPABILITIES, &encode_capabilities(c))?;
        }
        if let Some(cell) = &self.bridge_geohash
            && !cell.is_empty()
            && cell.len() <= 12
        {
            push(&mut out, BRIDGE_GEOHASH, cell.as_bytes())?;
        }
        Some(out)
    }

    pub fn decode(data: &[u8]) -> Option<Announce> {
        let mut offset = 0;
        let mut nickname = None;
        let mut noise_public = None;
        let mut signing_public = None;
        let mut neighbors = None;
        let mut capabilities = None;
        let mut bridge_geohash = None;
        while offset + 2 <= data.len() {
            let kind = data[offset];
            let len = data[offset + 1] as usize;
            offset += 2;
            let value = data.get(offset..offset + len)?;
            offset += len;
            match kind {
                NICKNAME => nickname = String::from_utf8(value.to_vec()).ok(),
                NOISE_KEY => noise_public = Some(value.to_vec()),
                SIGNING_KEY => signing_public = Some(value.to_vec()),
                NEIGHBORS if len > 0 && len.is_multiple_of(8) => {
                    neighbors = Some(value.as_chunks::<8>().0.to_vec());
                }
                CAPABILITIES => capabilities = Some(decode_capabilities(value)),
                BRIDGE_GEOHASH if len <= 12 => {
                    bridge_geohash = String::from_utf8(value.to_vec()).ok()
                }
                _ => {}
            }
        }
        Some(Announce {
            nickname: nickname?,
            noise_public: noise_public?,
            signing_public: signing_public?,
            neighbors,
            capabilities,
            bridge_geohash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bytes() {
        assert_eq!(encode_capabilities(0), vec![0]);
        assert_eq!(encode_capabilities(0x0329), vec![0x29, 0x03]);
        assert_eq!(decode_capabilities(&[0x29, 0x03]), 0x0329);
    }
}
