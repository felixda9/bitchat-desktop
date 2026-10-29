//! Size limits (BitFoundation `FileTransferLimits`, `PacketPayloadLimits`).

use crate::message_type as t;

pub const V1_HEADER_SIZE: usize = 14;
pub const V2_HEADER_SIZE: usize = 16;
pub const ID_SIZE: usize = 8;
pub const SIGNATURE_SIZE: usize = 64;

/// Largest file payload (1 MiB).
pub const MAX_FILE_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Largest framed file: payload + TLV metadata + v2 packet envelope (1,179,760 bytes).
pub const MAX_FRAMED_FILE_BYTES: usize = MAX_FILE_PAYLOAD_BYTES
    + 18
    + (u16::MAX as usize) * 2
    + V2_HEADER_SIZE
    + ID_SIZE
    + ID_SIZE
    + SIGNATURE_SIZE;

/// Highest ratio raw deflate can reach; claims above it are forged.
pub const MAX_DEFLATE_RATIO: usize = 1032;

const FRAGMENT_BYTES: usize = 1_280;
const CONTROL_BYTES: usize = 4 * 1024;
const REQUEST_SYNC_BYTES: usize = 8 * 1024;
const ENVELOPE_BYTES: usize = 64 * 1024;
const TWICE_V1_FRAME_BYTES: usize = 128 * 1024;

/// Worst-case frame bytes around a payload: v2 header, both IDs, a full
/// route, the original-size field, a signature and maximum padding (2396).
pub const MAX_FRAME_OVERHEAD: usize =
    V2_HEADER_SIZE + ID_SIZE + ID_SIZE + 1 + 255 * ID_SIZE + 4 + SIGNATURE_SIZE + 255;

/// Ceiling on the decoded payload of a packet type, compressed or not.
pub fn max_payload_bytes(packet_type: u8) -> usize {
    match packet_type {
        t::FILE_TRANSFER | t::NOISE_ENCRYPTED => MAX_FRAMED_FILE_BYTES,
        t::MESSAGE | t::GROUP_MESSAGE => TWICE_V1_FRAME_BYTES,
        t::COURIER_ENVELOPE | t::NOSTR_CARRIER => ENVELOPE_BYTES,
        t::REQUEST_SYNC => REQUEST_SYNC_BYTES,
        t::FRAGMENT => FRAGMENT_BYTES,
        t::ANNOUNCE
        | t::ANNOUNCE_V2
        | t::LEAVE
        | t::BOARD_POST
        | t::PREKEY_BUNDLE
        | t::VOICE_FRAME
        | t::NOISE_HANDSHAKE
        | t::PING
        | t::PONG => CONTROL_BYTES,
        _ => TWICE_V1_FRAME_BYTES,
    }
}
