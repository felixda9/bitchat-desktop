//! BitChat private envelopes over Nostr (iOS `NostrProtocol`).
//!
//! Three layers: an unsigned kind-14 rumor, encrypted into a kind-13 seal
//! signed by the sender, encrypted into a kind-1059 gift wrap signed by a
//! one-time key. Reuses NIP-17/NIP-59 kind numbers but is NOT NIP-44:
//!
//! - shared secret: secp256k1 ECDH point in 33-byte compressed form; the
//!   sender lifts the recipient's x-only key with even Y, the receiver tries
//!   the sender's key with even then odd Y
//! - key: HKDF-SHA256(ikm = point, salt = empty, info = "nip44-v2", 32 bytes)
//! - cipher: XChaCha20-Poly1305, random 24-byte nonce, no AAD
//! - content: `"v2:" + base64url_nopad(nonce || ciphertext || tag)`

use crate::event::{Event, kind};
use crate::keys::{self, Keys};
use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use secp256k1::{PublicKey, SecretKey, ecdh};
use sha2::Sha256;

/// Upper bound for each encrypted layer and each decrypted JSON layer.
pub const MAX_CIPHERTEXT_BYTES: usize = 64 * 1024;

/// Seal and wrap timestamps are shifted by up to this many seconds either way.
pub const TIMESTAMP_JITTER_SECONDS: i64 = 900;

/// Inner rumor timestamps outside this window are dropped: lookback plus skew.
pub const DM_LOOKBACK_SECONDS: i64 = 86_400;
pub const DM_MAX_CLOCK_SKEW_SECONDS: i64 = 900;

const HKDF_INFO: &[u8] = b"nip44-v2";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid event")]
    InvalidEvent,
    #[error("invalid ciphertext")]
    InvalidCiphertext,
}

/// Base64 decoding as iOS `Base64URLCoding.decode`: URL alphabet mapped to
/// standard, padding optional.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let standard: String = s
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    LENIENT.decode(standard).ok()
}

pub fn base64url_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

pub fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    base64_decode(s)
}

/// ECDH point `scalar · P` in 33-byte compressed form, where P is `xonly`
/// lifted with even (`odd = false`) or odd Y.
pub fn shared_point(scalar: &SecretKey, xonly: &[u8; 32], odd: bool) -> Option<[u8; 33]> {
    let mut compressed = [0u8; 33];
    compressed[0] = if odd { 0x03 } else { 0x02 };
    compressed[1..].copy_from_slice(xonly);
    let point = PublicKey::from_byte_array_compressed(compressed).ok()?;
    Some(compress_point(&ecdh::shared_secret_point(&point, scalar)))
}

fn compress_point(xy: &[u8; 64]) -> [u8; 33] {
    let mut out = [0u8; 33];
    out[0] = 0x02 | (xy[63] & 1);
    out[1..].copy_from_slice(&xy[..32]);
    out
}

pub fn derive_key(shared: &[u8; 33]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&[]), shared);
    let mut key = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key)
        .expect("32 bytes is a valid HKDF length");
    key
}

/// Encrypts with an explicit nonce. Use [`encrypt`] outside tests.
pub fn encrypt_with_nonce(
    plaintext: &str,
    sender: &Keys,
    recipient_xonly: &[u8; 32],
    nonce: &[u8; 24],
) -> Result<String, EnvelopeError> {
    let shared = shared_point(&sender.secret_key(), recipient_xonly, false)
        .ok_or(EnvelopeError::InvalidPublicKey)?;
    let cipher = XChaCha20Poly1305::new(&derive_key(&shared).into());
    let sealed = cipher
        .encrypt(&XNonce::from(*nonce), Payload::from(plaintext.as_bytes()))
        .map_err(|_| EnvelopeError::InvalidCiphertext)?;
    let mut combined = Vec::with_capacity(24 + sealed.len());
    combined.extend_from_slice(nonce);
    combined.extend_from_slice(&sealed);
    Ok(format!("v2:{}", base64url_encode(&combined)))
}

pub fn encrypt(
    plaintext: &str,
    sender: &Keys,
    recipient_xonly: &[u8; 32],
) -> Result<String, EnvelopeError> {
    encrypt_with_nonce(plaintext, sender, recipient_xonly, &keys::random_bytes())
}

pub fn decrypt(
    content: &str,
    sender_pubkey_hex: &str,
    recipient: &Keys,
) -> Result<String, EnvelopeError> {
    if content.len() > MAX_CIPHERTEXT_BYTES {
        return Err(EnvelopeError::InvalidCiphertext);
    }
    let encoded = content
        .strip_prefix("v2:")
        .ok_or(EnvelopeError::InvalidCiphertext)?;
    let data = base64_decode(encoded).ok_or(EnvelopeError::InvalidCiphertext)?;
    if data.len() <= 24 + 16 {
        return Err(EnvelopeError::InvalidCiphertext);
    }
    let sender = hex::decode(sender_pubkey_hex).map_err(|_| EnvelopeError::InvalidCiphertext)?;
    let (nonce, sealed) = data.split_at(24);
    let nonce = XNonce::try_from(nonce).map_err(|_| EnvelopeError::InvalidCiphertext)?;
    let scalar = recipient.secret_key();

    let attempt = |shared: [u8; 33]| -> Option<Vec<u8>> {
        XChaCha20Poly1305::new(&derive_key(&shared).into())
            .decrypt(&nonce, Payload::from(sealed))
            .ok()
    };

    let plaintext = match sender.len() {
        32 => {
            let xonly: [u8; 32] = sender.as_slice().try_into().unwrap();
            [false, true]
                .into_iter()
                .filter_map(|odd| shared_point(&scalar, &xonly, odd))
                .find_map(attempt)
        }
        33 => {
            let compressed: [u8; 33] = sender.as_slice().try_into().unwrap();
            PublicKey::from_byte_array_compressed(compressed)
                .ok()
                .map(|point| compress_point(&ecdh::shared_secret_point(&point, &scalar)))
                .and_then(attempt)
        }
        _ => return Err(EnvelopeError::InvalidCiphertext),
    }
    .ok_or(EnvelopeError::InvalidCiphertext)?;

    String::from_utf8(plaintext).map_err(|_| EnvelopeError::InvalidCiphertext)
}

fn xonly_bytes(hex_key: &str) -> Result<[u8; 32], EnvelopeError> {
    hex::decode(hex_key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(EnvelopeError::InvalidPublicKey)
}

fn jittered(now: i64) -> i64 {
    let r = u32::from_le_bytes(keys::random_bytes());
    let span = (2 * TIMESTAMP_JITTER_SECONDS + 1) as u32;
    now - TIMESTAMP_JITTER_SECONDS + i64::from(r % span)
}

fn to_json(event: &Event) -> String {
    serde_json::to_string(event).expect("events always serialize")
}

/// Builds a gift wrap for `recipient_hex`. `now` is unix seconds; the rumor
/// carries it exactly, seal and wrap are shifted by up to ±15 minutes.
pub fn create_private_message(
    content: &str,
    recipient_hex: &str,
    sender: &Keys,
    now: i64,
) -> Result<Event, EnvelopeError> {
    create_private_message_with_tags(content, recipient_hex, sender, now, Vec::new())
}

/// As [`create_private_message`] with explicit rumor tags. iOS sends none;
/// Android sends `[["p", recipient]]`.
pub fn create_private_message_with_tags(
    content: &str,
    recipient_hex: &str,
    sender: &Keys,
    now: i64,
    rumor_tags: Vec<Vec<String>>,
) -> Result<Event, EnvelopeError> {
    let recipient = xonly_bytes(recipient_hex)?;

    let rumor = Event::new(
        sender.public_key_hex(),
        now,
        kind::DM,
        rumor_tags,
        content.to_string(),
    );

    let seal = Event::new(
        sender.public_key_hex(),
        jittered(now),
        kind::SEAL,
        Vec::new(),
        encrypt(&to_json(&rumor), sender, &recipient)?,
    )
    .sign(sender);

    let wrap_keys = Keys::generate();
    Ok(Event::new(
        wrap_keys.public_key_hex(),
        jittered(now),
        kind::GIFT_WRAP,
        vec![vec!["p".to_string(), recipient_hex.to_string()]],
        encrypt(&to_json(&seal), &wrap_keys, &recipient)?,
    )
    .sign(&wrap_keys))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub content: String,
    /// Authenticated sender: the seal signer.
    pub sender: String,
    /// Real send time from the inner rumor (unix seconds).
    pub timestamp: i64,
}

fn parse_layer(json: &str) -> Result<Event, EnvelopeError> {
    if json.len() > MAX_CIPHERTEXT_BYTES {
        return Err(EnvelopeError::InvalidCiphertext);
    }
    let event: Event = serde_json::from_str(json).map_err(|_| EnvelopeError::InvalidEvent)?;
    if !event.within_tag_limits() {
        return Err(EnvelopeError::InvalidEvent);
    }
    Ok(event)
}

fn p_tag(pubkey: &str) -> Vec<Vec<String>> {
    vec![vec!["p".to_string(), pubkey.to_string()]]
}

/// Opens a gift wrap addressed to `recipient`, enforcing the shapes BitChat
/// clients emit: exactly one outer `p` tag, a tagless signed seal, and an
/// unsigned rumor with no tags (iOS) or only our `p` tag (Android) whose
/// author is the seal signer.
pub fn open_private_message(wrap: &Event, recipient: &Keys) -> Result<Opened, EnvelopeError> {
    let me = recipient.public_key_hex();
    if wrap.content.len() > MAX_CIPHERTEXT_BYTES {
        return Err(EnvelopeError::InvalidCiphertext);
    }
    if wrap.kind != kind::GIFT_WRAP || wrap.tags != p_tag(&me) || !wrap.verify() {
        return Err(EnvelopeError::InvalidEvent);
    }

    let seal = parse_layer(&decrypt(&wrap.content, &wrap.pubkey, recipient)?)?;
    if seal.kind != kind::SEAL || !seal.tags.is_empty() || !seal.verify() {
        return Err(EnvelopeError::InvalidEvent);
    }

    let rumor = parse_layer(&decrypt(&seal.content, &seal.pubkey, recipient)?)?;
    let tags_ok = rumor.tags.is_empty() || rumor.tags == p_tag(&me);
    if rumor.kind != kind::DM || !tags_ok || rumor.sig.is_some() || rumor.pubkey != seal.pubkey {
        return Err(EnvelopeError::InvalidEvent);
    }

    Ok(Opened {
        content: rumor.content,
        sender: seal.pubkey,
        timestamp: rumor.created_at,
    })
}

/// Rumor times older than the 24 h lookback (plus skew) or more than 15
/// minutes in the future are rejected.
pub fn is_plausible_rumor_timestamp(timestamp: i64, now: i64) -> bool {
    let age = now - timestamp;
    (-DM_MAX_CLOCK_SKEW_SECONDS..=DM_LOOKBACK_SECONDS + DM_MAX_CLOCK_SKEW_SECONDS).contains(&age)
}
