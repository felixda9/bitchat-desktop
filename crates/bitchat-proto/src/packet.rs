//! Binary packet frame (BitFoundation `BinaryProtocol` / `BitchatPacket`).
//!
//! v1 header (14 bytes): version, type, ttl, timestamp u64 ms, flags, payload length u16.
//! v2 header (16 bytes): same with a u32 payload length.
//! Body: sender 8 | recipient 8 (flag 0x01) | route (v2, flag 0x08) |
//! original size (flag 0x04) | payload | signature 64 (flag 0x02).
//! All integers are big-endian.

use crate::compression;
use crate::limits::{self, ID_SIZE, SIGNATURE_SIZE, V1_HEADER_SIZE};
use crate::padding;

pub const FLAG_HAS_RECIPIENT: u8 = 0x01;
pub const FLAG_HAS_SIGNATURE: u8 = 0x02;
pub const FLAG_IS_COMPRESSED: u8 = 0x04;
pub const FLAG_HAS_ROUTE: u8 = 0x08;
pub const FLAG_IS_RSR: u8 = 0x10;

pub type PeerId = [u8; ID_SIZE];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub version: u8,
    pub packet_type: u8,
    pub ttl: u8,
    pub timestamp: u64,
    pub sender_id: PeerId,
    pub recipient_id: Option<PeerId>,
    /// Decoded (uncompressed) payload.
    pub payload: Vec<u8>,
    pub signature: Option<[u8; SIGNATURE_SIZE]>,
    /// Intermediate hops (v2 only). Empty means no route.
    pub route: Vec<PeerId>,
    /// Request-sync response marker; not covered by the signature.
    pub is_rsr: bool,
}

impl Packet {
    /// A v1 packet with no recipient, signature, or route.
    pub fn new(
        packet_type: u8,
        sender_id: PeerId,
        timestamp: u64,
        payload: Vec<u8>,
        ttl: u8,
    ) -> Self {
        Packet {
            version: 1,
            packet_type,
            ttl,
            timestamp,
            sender_id,
            recipient_id: None,
            payload,
            signature: None,
            route: Vec::new(),
            is_rsr: false,
        }
    }

    /// Encodes the frame, compressing the payload when it qualifies and
    /// optionally applying bucket padding. `None` for unsupported versions or
    /// payloads too large for the length field.
    pub fn encode(&self, pad: bool) -> Option<Vec<u8>> {
        let version = self.version;
        if version != 1 && version != 2 {
            return None;
        }
        let length_field = length_field_size(version);
        let max_representable = if version == 2 {
            u32::MAX as usize
        } else {
            u16::MAX as usize
        };

        let mut payload: &[u8] = &self.payload;
        let compressed;
        let mut original_size = None;
        if compression::should_compress(payload)
            && payload.len() <= max_representable
            && let Some(c) = compression::compress(payload)
        {
            original_size = Some(payload.len());
            compressed = c;
            payload = &compressed;
        }

        let route: &[PeerId] = if version >= 2 { &self.route } else { &[] };
        if route.len() > 255 {
            return None;
        }

        let payload_data_size = payload.len()
            + if original_size.is_some() {
                length_field
            } else {
                0
            };
        if payload_data_size > max_representable {
            return None;
        }

        let mut out = Vec::with_capacity(
            limits::V2_HEADER_SIZE
                + 2 * ID_SIZE
                + 1
                + route.len() * ID_SIZE
                + payload_data_size
                + SIGNATURE_SIZE
                + 255,
        );
        out.push(version);
        out.push(self.packet_type);
        out.push(self.ttl);
        out.extend_from_slice(&self.timestamp.to_be_bytes());

        let mut flags = 0u8;
        if self.recipient_id.is_some() {
            flags |= FLAG_HAS_RECIPIENT;
        }
        if self.signature.is_some() {
            flags |= FLAG_HAS_SIGNATURE;
        }
        if original_size.is_some() {
            flags |= FLAG_IS_COMPRESSED;
        }
        if !route.is_empty() {
            flags |= FLAG_HAS_ROUTE;
        }
        if self.is_rsr {
            flags |= FLAG_IS_RSR;
        }
        out.push(flags);

        push_length(&mut out, version, payload_data_size);
        out.extend_from_slice(&self.sender_id);
        if let Some(recipient) = &self.recipient_id {
            out.extend_from_slice(recipient);
        }
        if !route.is_empty() {
            out.push(route.len() as u8);
            for hop in route {
                out.extend_from_slice(hop);
            }
        }
        if let Some(size) = original_size {
            push_length(&mut out, version, size);
        }
        out.extend_from_slice(payload);
        if let Some(signature) = &self.signature {
            out.extend_from_slice(signature);
        }

        if pad {
            let target = padding::optimal_block_size(out.len());
            out = padding::pad(out, target);
        }
        Some(out)
    }

    /// The bytes an Ed25519 signature covers: this packet re-encoded with no
    /// signature, TTL 0, RSR cleared, padding on (and compression re-applied).
    pub fn signing_bytes(&self) -> Option<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        unsigned.ttl = 0;
        unsigned.is_rsr = false;
        unsigned.encode(true)
    }

    /// Decodes a frame, retrying once without PKCS#7 padding. Trailing bytes are ignored.
    pub fn decode(data: &[u8]) -> Option<Packet> {
        if let Some(packet) = decode_core(data) {
            return Some(packet);
        }
        decode_core(padding::unpad(data)?)
    }
}

fn length_field_size(version: u8) -> usize {
    if version == 2 { 4 } else { 2 }
}

fn push_length(out: &mut Vec<u8>, version: u8, value: usize) {
    if version == 2 {
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.extend_from_slice(&(value as u16).to_be_bytes());
    }
}

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(n)?;
        let slice = self.data.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn id(&mut self) -> Option<PeerId> {
        self.take(ID_SIZE)?.try_into().ok()
    }

    fn length(&mut self, version: u8) -> Option<usize> {
        if version == 2 {
            Some(self.u32()? as usize)
        } else {
            Some(self.u16()? as usize)
        }
    }
}

fn decode_core(raw: &[u8]) -> Option<Packet> {
    if raw.len() < V1_HEADER_SIZE + ID_SIZE {
        return None;
    }
    let mut r = Reader {
        data: raw,
        offset: 0,
    };

    let version = r.u8()?;
    if version != 1 && version != 2 {
        return None;
    }
    let header_size = if version == 2 {
        limits::V2_HEADER_SIZE
    } else {
        V1_HEADER_SIZE
    };
    if raw.len() < header_size + ID_SIZE {
        return None;
    }
    let length_field = length_field_size(version);

    let packet_type = r.u8()?;
    let ttl = r.u8()?;
    let timestamp = r.u64()?;
    let flags = r.u8()?;
    let has_recipient = flags & FLAG_HAS_RECIPIENT != 0;
    let has_signature = flags & FLAG_HAS_SIGNATURE != 0;
    let is_compressed = flags & FLAG_IS_COMPRESSED != 0;
    let has_route = version >= 2 && flags & FLAG_HAS_ROUTE != 0;
    let is_rsr = flags & FLAG_IS_RSR != 0;

    let payload_length = r.length(version)?;
    if payload_length > limits::MAX_FRAMED_FILE_BYTES {
        return None;
    }

    let sender_id = r.id()?;
    let recipient_id = if has_recipient { Some(r.id()?) } else { None };

    let mut route = Vec::new();
    if has_route {
        let count = r.u8()?;
        for _ in 0..count {
            route.push(r.id()?);
        }
    }

    let max_payload = limits::max_payload_bytes(packet_type);
    let payload = if is_compressed {
        if payload_length < length_field {
            return None;
        }
        let original_size = r.length(version)?;
        if original_size > max_payload {
            return None;
        }
        let compressed_size = payload_length - length_field;
        if compressed_size == 0 {
            return None;
        }
        let compressed = r.take(compressed_size)?;
        if original_size > compressed_size * limits::MAX_DEFLATE_RATIO {
            return None;
        }
        compression::decompress(compressed, original_size)?
    } else {
        if payload_length > max_payload {
            return None;
        }
        r.take(payload_length)?.to_vec()
    };

    let signature = if has_signature {
        Some(r.take(SIGNATURE_SIZE)?.try_into().ok()?)
    } else {
        None
    };

    Some(Packet {
        version,
        packet_type,
        ttl,
        timestamp,
        sender_id,
        recipient_id,
        payload,
        signature,
        route,
        is_rsr,
    })
}
