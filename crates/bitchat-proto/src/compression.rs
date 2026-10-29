//! Raw DEFLATE payload compression (BitFoundation `CompressionUtil`).
//!
//! iOS uses Apple's `COMPRESSION_ZLIB`, which emits raw DEFLATE (no zlib
//! header or checksum). Byte-identical output matters only for signed mesh
//! packets, whose signature covers a re-compressed copy; it is checked against
//! Swift vectors.

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

/// Payloads shorter than this are never compressed.
pub const COMPRESSION_THRESHOLD: usize = 100;

const LEVEL: u32 = 5;

/// True when the payload is long enough and not already high-entropy.
pub fn should_compress(data: &[u8]) -> bool {
    if data.len() < COMPRESSION_THRESHOLD {
        return false;
    }
    let mut seen = [false; 256];
    let mut unique = 0usize;
    for &b in data {
        if !seen[b as usize] {
            seen[b as usize] = true;
            unique += 1;
        }
    }
    let sample = data.len().min(256);
    (unique as f64) / (sample as f64) < 0.9
}

/// Raw-deflates `data`; `None` unless the result is non-empty and strictly shorter.
pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
    let capacity = data.len() + data.len() / 255 + 16;
    let mut out = Vec::with_capacity(capacity);
    let mut encoder = Compress::new(Compression::new(LEVEL), false);
    match encoder.compress_vec(data, &mut out, FlushCompress::Finish) {
        Ok(Status::StreamEnd) => {}
        _ => return None,
    }
    if out.is_empty() || out.len() >= data.len() {
        return None;
    }
    Some(out)
}

/// Inflates raw DEFLATE into exactly `original_size` bytes, like Apple's
/// `compression_decode_buffer`: output beyond `original_size` is ignored and a
/// stream that ends short is rejected.
pub fn decompress(data: &[u8], original_size: usize) -> Option<Vec<u8>> {
    if original_size == 0 {
        return None;
    }
    let mut out = vec![0u8; original_size];
    let mut decoder = Decompress::new(false);
    decoder
        .decompress(data, &mut out, FlushDecompress::Finish)
        .ok()?;
    (decoder.total_out() as usize == original_size).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_and_entropy() {
        assert!(!should_compress(&[b'a'; 99]));
        assert!(should_compress(&[b'a'; 100]));
        let all: Vec<u8> = (0..=255).collect();
        assert!(!should_compress(&all));
    }

    #[test]
    fn round_trip() {
        let text = "hello world ".repeat(25).into_bytes();
        let packed = compress(&text).unwrap();
        assert!(packed.len() < text.len());
        assert_eq!(decompress(&packed, text.len()).unwrap(), text);
    }

    #[test]
    fn short_stream_is_rejected() {
        let text = "hello world ".repeat(25).into_bytes();
        let packed = compress(&text).unwrap();
        assert!(decompress(&packed, text.len() + 1).is_none());
        assert_eq!(decompress(&packed, 10).unwrap(), &text[..10]);
        assert!(decompress(&packed, 0).is_none());
        assert!(decompress(&[0xff, 0xff, 0xff], 10).is_none());
    }
}
