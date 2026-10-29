//! secp256k1 identities for Nostr (iOS `NostrIdentity`).

use bech32::{Bech32, Hrp};
use secp256k1::{Keypair, SecretKey, XOnlyPublicKey};

#[derive(Clone)]
pub struct Keys {
    keypair: Keypair,
    secret: [u8; 32],
    public: [u8; 32],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("public", &self.public_key_hex())
            .finish_non_exhaustive()
    }
}

impl Keys {
    /// `None` when the bytes are not a valid secp256k1 scalar.
    pub fn from_secret_bytes(secret: [u8; 32]) -> Option<Self> {
        let keypair = Keypair::from_secret_bytes(secret).ok()?;
        let public = keypair.x_only_public_key().0.to_byte_array();
        Some(Keys {
            keypair,
            secret,
            public,
        })
    }

    pub fn generate() -> Self {
        loop {
            if let Some(keys) = Self::from_secret_bytes(random_bytes()) {
                return keys;
            }
        }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret
    }

    /// Raw scalar as stored, used for ECDH (not BIP-340 normalized).
    pub fn secret_key(&self) -> SecretKey {
        SecretKey::from_secret_bytes(self.secret).expect("validated on construction")
    }

    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }

    /// 32-byte x-only public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public)
    }

    pub fn npub(&self) -> String {
        npub_encode(&self.public)
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("OS random number generator failed");
    out
}

pub fn npub_encode(public_key: &[u8; 32]) -> String {
    let hrp = Hrp::parse("npub").expect("valid hrp");
    bech32::encode::<Bech32>(hrp, public_key).expect("32 bytes always encode")
}

/// Decodes an `npub1…` string to its 32-byte key.
pub fn npub_decode(npub: &str) -> Option<[u8; 32]> {
    let (hrp, data) = bech32_decode(npub)?;
    if hrp != "npub" {
        return None;
    }
    data.try_into().ok()
}

/// Strict Bech32 (not Bech32m) decode returning the lowercase HRP and payload.
pub fn bech32_decode(s: &str) -> Option<(String, Vec<u8>)> {
    let checked = bech32::primitives::decode::CheckedHrpstring::new::<Bech32>(s).ok()?;
    let hrp = checked.hrp().to_lowercase();
    if hrp.is_empty() {
        return None;
    }
    let data: Vec<u8> = checked.byte_iter().collect();
    // Re-encoding must reproduce the input: rejects non-zero padding bits.
    let reencoded = bech32::encode::<Bech32>(Hrp::parse(&hrp).ok()?, &data).ok()?;
    if !reencoded.eq_ignore_ascii_case(s) {
        return None;
    }
    Some((hrp, data))
}

/// Parses a 64-char hex x-only key.
pub fn xonly_from_hex(hex_key: &str) -> Option<XOnlyPublicKey> {
    let bytes: [u8; 32] = hex::decode(hex_key).ok()?.try_into().ok()?;
    XOnlyPublicKey::from_byte_array(bytes).ok()
}
