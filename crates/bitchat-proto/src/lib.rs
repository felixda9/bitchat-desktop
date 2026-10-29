//! bitchat wire format: packets, payloads, ids, geohash.
//!
//! Ported from the iOS BitFoundation package, which is the spec of record.

pub mod compression;
pub mod ids;
pub mod limits;
pub mod message_type;
pub mod packet;
pub mod padding;

pub use packet::{Packet, PeerId};
