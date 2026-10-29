//! Authenticated peer state (iOS `AuthenticatedPeerStatePacket`), sent as
//! Noise payload `0x21` right after a session is established:
//! `[version 0x01] [01 len caps] [02 32 ed25519 key]`, unknown TLVs skipped.

use crate::announce::{decode_capabilities, encode_capabilities};

const VERSION: u8 = 1;
const CAPS: u8 = 0x01;
const SIGNING_KEY: u8 = 0x02;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerState {
    pub capabilities: u64,
    pub signing_public: [u8; 32],
}

impl PeerState {
    pub fn encode(&self) -> Vec<u8> {
        let caps = encode_capabilities(self.capabilities);
        let mut out = vec![VERSION, CAPS, caps.len() as u8];
        out.extend_from_slice(&caps);
        out.push(SIGNING_KEY);
        out.push(32);
        out.extend_from_slice(&self.signing_public);
        out
    }

    /// Rejects unknown versions, duplicates, non-canonical capability bytes
    /// and malformed lengths.
    pub fn decode(data: &[u8]) -> Option<PeerState> {
        if data.first() != Some(&VERSION) {
            return None;
        }
        let mut offset = 1;
        let mut capabilities = None;
        let mut signing_public = None;
        while offset < data.len() {
            if offset + 2 > data.len() {
                return None;
            }
            let kind = data[offset];
            let len = data[offset + 1] as usize;
            offset += 2;
            let value = data.get(offset..offset + len)?;
            offset += len;
            match kind {
                CAPS => {
                    if capabilities.is_some() || value.is_empty() || value.len() > 8 {
                        return None;
                    }
                    let c = decode_capabilities(value);
                    if encode_capabilities(c) != value {
                        return None;
                    }
                    capabilities = Some(c);
                }
                SIGNING_KEY => {
                    if signing_public.is_some() || len != 32 {
                        return None;
                    }
                    signing_public = Some(value.try_into().ok()?);
                }
                _ => {}
            }
        }
        Some(PeerState {
            capabilities: capabilities?,
            signing_public: signing_public?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejections() {
        let s = PeerState {
            capabilities: 0x0329,
            signing_public: [7; 32],
        };
        let bytes = s.encode();
        assert_eq!(&bytes[..5], &[1, 1, 2, 0x29, 0x03]);
        assert_eq!(PeerState::decode(&bytes), Some(s));
        let mut padded_caps = vec![1, 1, 3, 0x29, 0x03, 0x00, 2, 32];
        padded_caps.extend_from_slice(&[7; 32]);
        assert_eq!(
            PeerState::decode(&padded_caps),
            None,
            "non-canonical capabilities"
        );
        assert_eq!(PeerState::decode(&[2, 1, 1, 0]), None, "unknown version");
    }
}
