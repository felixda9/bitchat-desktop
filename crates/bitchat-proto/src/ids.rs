//! Identity fingerprints and mesh peer IDs.

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of a Noise static public key: the stable identity.
pub fn fingerprint(noise_public_key: &[u8]) -> String {
    hex::encode(Sha256::digest(noise_public_key))
}

/// 8-byte mesh peer ID: the first 8 bytes of the fingerprint hash.
pub fn peer_id(noise_public_key: &[u8]) -> [u8; 8] {
    let digest = Sha256::digest(noise_public_key);
    let mut id = [0u8; 8];
    id.copy_from_slice(&digest[..8]);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_is_fingerprint_prefix() {
        let key = [0x11u8; 32];
        assert_eq!(hex::encode(peer_id(&key)), fingerprint(&key)[..16]);
    }
}
