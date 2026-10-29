//! `bitchat1:` payloads: private messages and acks carried inside Nostr DMs
//! (iOS `NostrEmbeddedBitChat`).
//!
//! The content is `"bitchat1:" + base64url_nopad(packet)`, where the packet is
//! a padded v1 `noiseEncrypted` frame whose payload is a plaintext typed
//! payload (the envelope already encrypts it).

use crate::envelope::{base64url_decode, base64url_encode};
use bitchat_proto::limits::MAX_FRAMED_FILE_BYTES;
use bitchat_proto::message_type::NOISE_ENCRYPTED;
use bitchat_proto::payload::{NoisePayload, PayloadType, PrivateMessage};
use bitchat_proto::{Packet, PeerId};

pub const PREFIX: &str = "bitchat1:";

/// Who the embedded packet names. Geohash DMs use a fresh random sender and
/// no recipient so personas cannot be linked to the mesh peer ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressing {
    Geohash,
    Account { sender: PeerId, recipient: PeerId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Embedded {
    PrivateMessage { message_id: String, content: String },
    Delivered { message_id: String },
    ReadReceipt { message_id: String },
}

fn wrap(payload: NoisePayload, addressing: Addressing, timestamp_ms: u64) -> Option<String> {
    let (sender, recipient) = match addressing {
        Addressing::Geohash => (crate::keys::random_bytes::<8>(), None),
        Addressing::Account { sender, recipient } => (sender, Some(recipient)),
    };
    let mut packet = Packet::new(NOISE_ENCRYPTED, sender, timestamp_ms, payload.encode(), 7);
    packet.recipient_id = recipient;
    Some(format!(
        "{PREFIX}{}",
        base64url_encode(&packet.encode(true)?)
    ))
}

/// `None` when the message ID or content exceeds 255 UTF-8 bytes.
pub fn encode(item: &Embedded, addressing: Addressing, timestamp_ms: u64) -> Option<String> {
    let payload = match item {
        Embedded::PrivateMessage {
            message_id,
            content,
        } => NoisePayload::new(
            PayloadType::PrivateMessage,
            PrivateMessage::new(message_id.clone(), content.clone()).encode()?,
        ),
        Embedded::Delivered { message_id } => NoisePayload::ack(PayloadType::Delivered, message_id),
        Embedded::ReadReceipt { message_id } => {
            NoisePayload::ack(PayloadType::ReadReceipt, message_id)
        }
    };
    wrap(payload, addressing, timestamp_ms)
}

/// Decodes the packet behind a `bitchat1:` string, bounded like iOS.
pub fn decode_packet(content: &str) -> Option<Packet> {
    let encoded = content.strip_prefix(PREFIX)?;
    let max_encoded = MAX_FRAMED_FILE_BYTES.div_ceil(3) * 4;
    if encoded.len() > max_encoded {
        return None;
    }
    let bytes = base64url_decode(encoded)?;
    if bytes.len() > MAX_FRAMED_FILE_BYTES {
        return None;
    }
    Packet::decode(&bytes)
}

/// The private message or ack inside a `bitchat1:` string. Other payload
/// types are not used over Nostr and yield `None`.
pub fn decode(content: &str) -> Option<(Packet, Embedded)> {
    let packet = decode_packet(content)?;
    if packet.packet_type != NOISE_ENCRYPTED {
        return None;
    }
    let payload = NoisePayload::decode(&packet.payload)?;
    let item = match payload.kind {
        PayloadType::PrivateMessage => {
            let pm = PrivateMessage::decode(&payload.data)?;
            Embedded::PrivateMessage {
                message_id: pm.message_id,
                content: pm.content,
            }
        }
        PayloadType::Delivered => Embedded::Delivered {
            message_id: String::from_utf8(payload.data).ok()?,
        },
        PayloadType::ReadReceipt => Embedded::ReadReceipt {
            message_id: String::from_utf8(payload.data).ok()?,
        },
        _ => return None,
    };
    Some((packet, item))
}
