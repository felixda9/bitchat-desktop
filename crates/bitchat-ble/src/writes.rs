//! Rebuilds frames from GATT writes on our characteristic (iOS
//! `BLEInboundWriteBuffer`). Phones write a whole frame per write without
//! response; long writes arrive as chunks with offsets. Bytes collect until
//! they decode as a packet.

use bitchat_proto::Packet;

/// A partial write larger than this is dropped (iOS caps at the largest frame).
pub const WRITE_CAP_BYTES: usize = bitchat_proto::limits::MAX_FRAMED_FILE_BYTES + 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Assembled {
    /// A whole frame (the buffer starts over).
    Frame(Vec<u8>),
    Waiting,
    /// Over the cap without decoding; the buffer was dropped.
    Oversized,
}

#[derive(Debug, Default)]
pub struct WriteAssembler {
    buffer: Vec<u8>,
}

impl WriteAssembler {
    pub fn append(&mut self, offset: usize, data: &[u8]) -> Assembled {
        if !data.is_empty() {
            let end = offset + data.len();
            if self.buffer.len() < end {
                self.buffer.resize(end, 0);
            }
            self.buffer[offset..end].copy_from_slice(data);
        }
        if Packet::decode(&self.buffer).is_some() {
            return Assembled::Frame(std::mem::take(&mut self.buffer));
        }
        if self.buffer.len() > WRITE_CAP_BYTES {
            self.buffer = Vec::new();
            return Assembled::Oversized;
        }
        Assembled::Waiting
    }
}
