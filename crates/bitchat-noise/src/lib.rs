//! Noise sessions for bitchat private messages (iOS `NoiseProtocol`).
//!
//! `Noise_XX_25519_ChaChaPoly_SHA256`, empty prologue, empty handshake
//! payloads (messages of 32, 96 and 64 bytes). Built on `snow`, with the
//! bitchat specifics around it:
//!
//! - transport frames are `BE32(nonce) || ciphertext || tag`; the receiver
//!   decrypts with the carried nonce (AEAD nonce = 4 zero bytes || LE64)
//! - a 1024-nonce sliding replay window, committed only after a successful open
//! - senders stop at nonce 2^32 - 2 (the prefix is 32 bits)
//! - received ephemeral and static keys are checked against the same list of
//!   weak X25519 points as iOS

use sha2::{Digest, Sha256};

pub const PROTOCOL: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
pub const MAX_HANDSHAKE_MESSAGE: usize = 2048;
/// Plaintext cap for normal payloads (private files may be larger).
pub const MAX_PLAINTEXT: usize = 65_535;
pub const REPLAY_WINDOW: u64 = 1024;
/// Highest nonce a sender may use.
pub const MAX_NONCE: u64 = u32::MAX as u64 - 1;
const TAG_LEN: usize = 16;
const NONCE_PREFIX: usize = 4;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NoiseError {
    #[error("noise: {0}")]
    Snow(String),
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("handshake message too large")]
    MessageTooLarge,
    #[error("handshake not finished")]
    NotFinished,
    #[error("ciphertext too short")]
    InvalidCiphertext,
    #[error("replayed or too old")]
    ReplayDetected,
    #[error("nonce space used up; rekey")]
    NonceExceeded,
}

impl From<snow::Error> for NoiseError {
    fn from(e: snow::Error) -> Self {
        NoiseError::Snow(e.to_string())
    }
}

/// Weak X25519 points rejected by iOS (`validatePublicKey`).
const WEAK_POINTS: [[u8; 32]; 8] = {
    let mut ones = [0u8; 32];
    ones[0] = 0x01;
    let mut last_one = [0u8; 32];
    last_one[31] = 0x01;
    let mut da = [0xffu8; 32];
    da[0] = 0xda;
    let mut db = [0xffu8; 32];
    db[0] = 0xdb;
    [
        [0u8; 32],
        ones,
        last_one,
        [
            0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
            0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
            0x5f, 0x49, 0xb8, 0x00,
        ],
        [
            0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83,
            0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd,
            0xd0, 0x9f, 0x11, 0x57,
        ],
        [0xffu8; 32],
        da,
        db,
    ]
};

/// Constant-time over the list, like iOS.
pub fn is_weak_public_key(key: &[u8]) -> bool {
    if key.len() != 32 {
        return true;
    }
    let mut found = 0u8;
    for weak in &WEAK_POINTS {
        let mut diff = 0u8;
        for (a, b) in key.iter().zip(weak) {
            diff |= a ^ b;
        }
        found |= (diff == 0) as u8;
    }
    found != 0
}

/// 8-byte mesh peer ID of a Noise static public key.
pub fn peer_id(static_public: &[u8; 32]) -> [u8; 8] {
    let digest = Sha256::digest(static_public);
    digest[..8].try_into().expect("8 bytes")
}

/// X25519 public key for a static secret.
pub fn public_key(static_secret: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*static_secret)).to_bytes()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

pub struct Handshake {
    state: snow::HandshakeState,
    role: Role,
    /// Messages processed so far (written or read), 0..=3.
    step: u8,
}

impl Handshake {
    pub fn new(role: Role, static_secret: &[u8; 32]) -> Result<Handshake, NoiseError> {
        Handshake::with_options(role, static_secret, &[], None)
    }

    /// Full constructor; tests use a fixed ephemeral key and a prologue.
    pub fn with_options(
        role: Role,
        static_secret: &[u8; 32],
        prologue: &[u8],
        ephemeral: Option<&[u8; 32]>,
    ) -> Result<Handshake, NoiseError> {
        let params: snow::params::NoiseParams = PROTOCOL.parse()?;
        let mut builder = snow::Builder::new(params)
            .local_private_key(static_secret)?
            .prologue(prologue)?;
        if let Some(e) = ephemeral {
            builder = builder.fixed_ephemeral_key_for_testing_only(e);
        }
        let state = match role {
            Role::Initiator => builder.build_initiator()?,
            Role::Responder => builder.build_responder()?,
        };
        Ok(Handshake {
            state,
            role,
            step: 0,
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Next handshake message with an empty payload (bitchat never sends payloads).
    pub fn write_message(&mut self) -> Result<Vec<u8>, NoiseError> {
        self.write_payload(&[])
    }

    pub fn write_payload(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        let n = self.state.write_message(payload, &mut buf)?;
        buf.truncate(n);
        self.step += 1;
        Ok(buf)
    }

    /// Reads a handshake message, rejecting weak keys from the peer.
    /// Returns the (normally empty) payload.
    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if message.len() > MAX_HANDSHAKE_MESSAGE {
            return Err(NoiseError::MessageTooLarge);
        }
        // XX: message 1 starts with the initiator's `e`, message 2 with the
        // responder's `e`; both are cleartext, so check them before any DH.
        let carries_ephemeral = (self.role == Role::Responder && self.step == 0)
            || (self.role == Role::Initiator && self.step == 1);
        if carries_ephemeral && (message.len() < 32 || is_weak_public_key(&message[..32])) {
            return Err(NoiseError::InvalidPublicKey);
        }
        let mut buf = vec![0u8; MAX_HANDSHAKE_MESSAGE];
        let n = self.state.read_message(message, &mut buf)?;
        buf.truncate(n);
        self.step += 1;
        if let Some(rs) = self.state.get_remote_static()
            && is_weak_public_key(rs)
        {
            return Err(NoiseError::InvalidPublicKey);
        }
        Ok(buf)
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    pub fn remote_static(&self) -> Option<[u8; 32]> {
        self.state
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
    }

    pub fn handshake_hash(&self) -> Vec<u8> {
        self.state.get_handshake_hash().to_vec()
    }

    pub fn into_session(self) -> Result<Session, NoiseError> {
        if !self.is_finished() {
            return Err(NoiseError::NotFinished);
        }
        let remote_static = self.remote_static().ok_or(NoiseError::NotFinished)?;
        let handshake_hash = self.handshake_hash();
        let transport = self.state.into_stateless_transport_mode()?;
        Ok(Session {
            transport,
            send_nonce: 0,
            replay: ReplayWindow::default(),
            remote_static,
            handshake_hash,
            role: self.role,
        })
    }
}

/// Sliding window over received nonces. Bit `i` set = nonce `highest - i` seen.
#[derive(Clone, Debug, Default)]
pub struct ReplayWindow {
    highest: u64,
    bits: [u64; (REPLAY_WINDOW / 64) as usize],
}

impl ReplayWindow {
    fn bit(&self, offset: u64) -> bool {
        self.bits[(offset / 64) as usize] >> (offset % 64) & 1 == 1
    }

    fn set(&mut self, offset: u64) {
        self.bits[(offset / 64) as usize] |= 1 << (offset % 64);
    }

    /// Same acceptance boundary as iOS: anything at or below
    /// `highest - 1024` is too old, anything above `highest` is new.
    pub fn check(&self, nonce: u64) -> bool {
        if self.highest >= REPLAY_WINDOW && nonce <= self.highest - REPLAY_WINDOW {
            return false;
        }
        if nonce > self.highest {
            return true;
        }
        !self.bit(self.highest - nonce)
    }

    pub fn mark(&mut self, nonce: u64) {
        if nonce > self.highest {
            let shift = nonce - self.highest;
            if shift >= REPLAY_WINDOW {
                self.bits = Default::default();
            } else {
                // Move every bit `shift` places towards older offsets.
                let words = (shift / 64) as usize;
                let rem = shift % 64;
                let len = self.bits.len();
                for i in (0..len).rev() {
                    let src = i.checked_sub(words);
                    let mut v = src.map_or(0, |s| self.bits[s] << rem);
                    if rem != 0
                        && let Some(s) = src.and_then(|s| s.checked_sub(1))
                    {
                        v |= self.bits[s] >> (64 - rem);
                    }
                    self.bits[i] = v;
                }
            }
            self.highest = nonce;
            self.set(0);
        } else {
            self.set(self.highest - nonce);
        }
    }
}

pub struct Session {
    transport: snow::StatelessTransportState,
    send_nonce: u64,
    replay: ReplayWindow,
    remote_static: [u8; 32],
    handshake_hash: Vec<u8>,
    role: Role,
}

impl Session {
    pub fn remote_static(&self) -> [u8; 32] {
        self.remote_static
    }

    pub fn remote_peer_id(&self) -> [u8; 8] {
        peer_id(&self.remote_static)
    }

    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn messages_sent(&self) -> u64 {
        self.send_nonce
    }

    /// `BE32(nonce) || ciphertext || tag`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if self.send_nonce > MAX_NONCE {
            return Err(NoiseError::NonceExceeded);
        }
        let nonce = self.send_nonce;
        let mut out = vec![0u8; NONCE_PREFIX + plaintext.len() + TAG_LEN];
        out[..NONCE_PREFIX].copy_from_slice(&(nonce as u32).to_be_bytes());
        let n = self
            .transport
            .write_message(nonce, plaintext, &mut out[NONCE_PREFIX..])?;
        out.truncate(NONCE_PREFIX + n);
        self.send_nonce += 1;
        Ok(out)
    }

    /// Decrypts a frame with its carried nonce; the replay window only
    /// advances after the tag verifies.
    pub fn decrypt(&mut self, frame: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if frame.len() < NONCE_PREFIX + TAG_LEN {
            return Err(NoiseError::InvalidCiphertext);
        }
        let nonce = u32::from_be_bytes(frame[..NONCE_PREFIX].try_into().expect("4 bytes")) as u64;
        if !self.replay.check(nonce) {
            return Err(NoiseError::ReplayDetected);
        }
        let body = &frame[NONCE_PREFIX..];
        let mut out = vec![0u8; body.len()];
        let n = self.transport.read_message(nonce, body, &mut out)?;
        out.truncate(n);
        self.replay.mark(nonce);
        Ok(out)
    }

    #[doc(hidden)]
    pub fn set_send_nonce_for_testing(&mut self, nonce: u64) {
        self.send_nonce = nonce;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_window() {
        let mut w = ReplayWindow::default();
        assert!(w.check(0));
        w.mark(0);
        assert!(!w.check(0));
        w.mark(2);
        assert!(w.check(1), "never-seen nonce inside the window");
        assert!(!w.check(0));
        assert!(!w.check(2));
        w.mark(13);
        w.mark(15);
        assert!(!w.check(13));
        assert!(w.check(14));
        w.mark(2000);
        assert!(!w.check(2000 - REPLAY_WINDOW), "too old");
        assert!(w.check(2000 - REPLAY_WINDOW + 1));
        assert!(!w.check(15));
        w.mark(1500);
        assert!(!w.check(1500));
    }

    #[test]
    fn weak_keys() {
        assert!(is_weak_public_key(&[0u8; 32]));
        assert!(is_weak_public_key(&[0xff; 32]));
        assert!(is_weak_public_key(&[1u8; 31]));
        assert!(!is_weak_public_key(&public_key(&[7u8; 32])));
    }
}
