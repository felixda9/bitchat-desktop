//! PKCS#7-style bucket padding (BitFoundation `MessagePadding`).

const BLOCK_SIZES: [usize; 4] = [256, 512, 1024, 2048];

/// Smallest bucket that fits `len + 16`, or `len` itself when none does.
pub fn optimal_block_size(len: usize) -> usize {
    let total = len + 16;
    BLOCK_SIZES
        .iter()
        .copied()
        .find(|&size| total <= size)
        .unwrap_or(len)
}

/// Pads to `target` with bytes equal to the pad length. Leaves the data
/// unchanged when it is already at least `target` or would need more than 255 bytes.
pub fn pad(mut data: Vec<u8>, target: usize) -> Vec<u8> {
    if data.len() >= target {
        return data;
    }
    let needed = target - data.len();
    if needed > 255 {
        return data;
    }
    data.resize(target, needed as u8);
    data
}

/// Strips valid PKCS#7 padding. Returns `None` when the tail is not valid padding.
pub fn unpad(data: &[u8]) -> Option<&[u8]> {
    let &last = data.last()?;
    let len = last as usize;
    if len == 0 || len > data.len() {
        return None;
    }
    let start = data.len() - len;
    if data[start..].iter().all(|&b| b == last) {
        Some(&data[..start])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        assert_eq!(optimal_block_size(0), 256);
        assert_eq!(optimal_block_size(240), 256);
        assert_eq!(optimal_block_size(241), 512);
        assert_eq!(optimal_block_size(2032), 2048);
        assert_eq!(optimal_block_size(2033), 2033);
    }

    #[test]
    fn pad_limits() {
        assert_eq!(pad(vec![1; 240], 256).len(), 256);
        assert_eq!(pad(vec![1; 240], 256)[255], 16);
        // 241 bytes go to the 512 bucket, which needs 271 pad bytes: left unpadded.
        assert_eq!(pad(vec![1; 241], 512).len(), 241);
        assert_eq!(pad(vec![1; 300], 256).len(), 300);
    }

    #[test]
    fn unpad_checks_every_byte() {
        assert_eq!(unpad(&[9, 9, 2, 2]), Some(&[9u8, 9][..]));
        assert_eq!(unpad(&[9, 3, 2, 3]), None);
        assert_eq!(unpad(&[1, 0]), None);
        assert_eq!(unpad(&[5, 5]), None);
        assert_eq!(unpad(&[]), None);
    }
}
