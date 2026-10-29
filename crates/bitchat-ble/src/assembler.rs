//! Rebuilds whole frames from a stream of BLE notifications (iOS
//! `NotificationStreamAssembler`). A peer may split one frame across several
//! notifications, put several frames in one, or pad between them.

/// A partial frame larger than this resets the stream.
pub const HARD_CAP_BYTES: usize = 8 * 1024 * 1024;
/// A partial frame that has not grown for this long is dropped.
pub const STALL_RESET_MS: u64 = 250;

const V1_HEADER: usize = 14;
const V2_HEADER: usize = 16;
const SENDER: usize = 8;
const RECIPIENT: usize = 8;
const SIGNATURE: usize = 64;
const FLAGS_OFFSET: usize = 11;
const LENGTH_OFFSET: usize = 12;
const HAS_RECIPIENT: u8 = 0x01;
const HAS_SIGNATURE: u8 = 0x02;
const IS_COMPRESSED: u8 = 0x04;
const HAS_ROUTE: u8 = 0x08;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Assembled {
    pub frames: Vec<Vec<u8>>,
    /// Stray bytes skipped before a frame start.
    pub dropped: Vec<u8>,
    /// The stream was reset (bad length, overflow or stall).
    pub reset: bool,
}

#[derive(Debug, Default)]
pub struct StreamAssembler {
    buffer: Vec<u8>,
    pending_since_ms: Option<u64>,
    pending_len: usize,
}

impl StreamAssembler {
    fn reset(&mut self) {
        self.buffer = Vec::new();
        self.pending_since_ms = None;
        self.pending_len = 0;
    }

    /// PKCS#7 padding left after a frame: n bytes of value n (never 1 or 2,
    /// which would be a version byte).
    fn discard_padding(&mut self) -> bool {
        let Some(&first) = self.buffer.first() else {
            return false;
        };
        if first == 1 || first == 2 {
            return false;
        }
        let n = first as usize;
        if n == 0 || n > self.buffer.len() || !self.buffer[..n].iter().all(|&b| b == first) {
            return false;
        }
        self.buffer.drain(..n);
        self.pending_since_ms = None;
        self.pending_len = 0;
        true
    }

    pub fn append(&mut self, chunk: &[u8], now_ms: u64) -> Assembled {
        let mut out = Assembled::default();
        if chunk.is_empty() {
            return out;
        }
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > HARD_CAP_BYTES {
            self.reset();
            out.reset = true;
            return out;
        }

        while self.buffer.len() >= V1_HEADER + SENDER {
            let version = self.buffer[0];
            if version != 1 && version != 2 {
                if self.discard_padding() {
                    continue;
                }
                out.dropped.push(self.buffer.remove(0));
                self.pending_since_ms = None;
                self.pending_len = 0;
                continue;
            }
            let header = if version == 1 { V1_HEADER } else { V2_HEADER };
            let prefix = header + SENDER;
            if self.buffer.len() < prefix {
                break;
            }
            let flags = self.buffer[FLAGS_OFFSET];
            let has_recipient = flags & HAS_RECIPIENT != 0;
            let has_signature = flags & HAS_SIGNATURE != 0;
            let compressed = flags & IS_COMPRESSED != 0;
            let has_route = version >= 2 && flags & HAS_ROUTE != 0;
            let b = &self.buffer[LENGTH_OFFSET..];
            let payload_len = if version == 2 {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
            } else {
                u16::from_be_bytes([b[0], b[1]]) as usize
            };

            let mut frame_len = prefix + payload_len;
            if has_recipient {
                frame_len += RECIPIENT;
            }
            if has_signature {
                frame_len += SIGNATURE;
            }
            if has_route {
                let count_at = prefix + if has_recipient { RECIPIENT } else { 0 };
                if self.buffer.len() <= count_at {
                    break;
                }
                frame_len += 1 + self.buffer[count_at] as usize * SENDER;
            }
            let raw_length_bytes = if version == 2 { 4 } else { 2 };
            if (compressed && payload_len < raw_length_bytes)
                || frame_len == 0
                || frame_len > HARD_CAP_BYTES
            {
                self.reset();
                out.reset = true;
                break;
            }

            if self.buffer.len() < frame_len {
                match self.pending_since_ms {
                    Some(since) if self.pending_len == frame_len => {
                        if now_ms.saturating_sub(since) >= STALL_RESET_MS {
                            self.reset();
                            out.reset = true;
                        }
                    }
                    _ => {
                        self.pending_since_ms = Some(now_ms);
                        self.pending_len = frame_len;
                    }
                }
                break;
            }

            self.pending_since_ms = None;
            self.pending_len = 0;
            out.frames.push(self.buffer.drain(..frame_len).collect());
            self.discard_padding();
        }

        if self.discard_padding() {
            return out;
        }
        if !self.buffer.is_empty() && self.buffer.iter().all(|&b| b == 0) {
            self.reset();
        }
        out
    }
}
