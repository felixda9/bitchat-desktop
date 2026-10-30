//! Mesh identity: Noise static key (peer ID) and the separate Ed25519 key
//! that signs packets.

use crate::announce::Announce;
use bitchat_proto::{Packet, message_type as t};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Packets start with TTL 7.
pub const DEFAULT_TTL: u8 = 7;

pub struct MeshIdentity {
    noise_secret: [u8; 32],
    noise_public: [u8; 32],
    signing: SigningKey,
    peer_id: [u8; 8],
}

impl MeshIdentity {
    pub fn new(noise_secret: [u8; 32], signing_seed: [u8; 32]) -> MeshIdentity {
        let noise_public = bitchat_noise::public_key(&noise_secret);
        MeshIdentity {
            noise_secret,
            noise_public,
            signing: SigningKey::from_bytes(&signing_seed),
            peer_id: bitchat_noise::peer_id(&noise_public),
        }
    }

    pub fn peer_id(&self) -> [u8; 8] {
        self.peer_id
    }

    pub fn noise_secret(&self) -> &[u8; 32] {
        &self.noise_secret
    }

    pub fn noise_public(&self) -> [u8; 32] {
        self.noise_public
    }

    pub fn signing_public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Lowercase hex SHA-256 of the Noise public key.
    pub fn fingerprint(&self) -> String {
        bitchat_proto::ids::fingerprint(&self.noise_public)
    }

    /// Signs the re-encoded pre-image (TTL 0, no signature, RSR off, padded).
    pub fn sign(&self, packet: &mut Packet) -> bool {
        let Some(bytes) = packet.signing_bytes() else {
            return false;
        };
        packet.signature = Some(self.signing.sign(&bytes).to_bytes());
        true
    }

    fn signed(&self, packet_type: u8, payload: Vec<u8>, timestamp_ms: u64) -> Option<Packet> {
        let mut packet = Packet::new(
            packet_type,
            self.peer_id,
            timestamp_ms,
            payload,
            DEFAULT_TTL,
        );
        self.sign(&mut packet).then_some(packet)
    }

    /// Signed v1 announce with our keys, neighbours and capabilities.
    pub fn announce(
        &self,
        nickname: &str,
        neighbors: &[[u8; 8]],
        capabilities: u64,
        timestamp_ms: u64,
    ) -> Option<Packet> {
        self.announce_with_cell(nickname, neighbors, capabilities, None, timestamp_ms)
    }

    /// Announce that also advertises our mesh-bridge rendezvous cell.
    pub fn announce_with_cell(
        &self,
        nickname: &str,
        neighbors: &[[u8; 8]],
        capabilities: u64,
        bridge_geohash: Option<&str>,
        timestamp_ms: u64,
    ) -> Option<Packet> {
        let payload = Announce {
            nickname: nickname.to_string(),
            noise_public: self.noise_public.to_vec(),
            signing_public: self.signing_public().to_vec(),
            neighbors: (!neighbors.is_empty()).then(|| neighbors.to_vec()),
            capabilities: Some(capabilities),
            bridge_geohash: bridge_geohash.map(str::to_string),
        }
        .encode()?;
        self.signed(t::ANNOUNCE, payload, timestamp_ms)
    }

    /// Signed public chat message; the payload is the UTF-8 text.
    pub fn public_message(&self, text: &str, timestamp_ms: u64) -> Option<Packet> {
        self.signed(t::MESSAGE, text.as_bytes().to_vec(), timestamp_ms)
    }

    pub fn leave(&self, timestamp_ms: u64) -> Option<Packet> {
        self.signed(t::LEAVE, Vec::new(), timestamp_ms)
    }
}

/// Checks a packet signature against a 32-byte Ed25519 public key.
pub fn verify(packet: &Packet, signing_public: &[u8]) -> bool {
    let (Some(sig), Ok(key)) = (packet.signature, <[u8; 32]>::try_from(signing_public)) else {
        return false;
    };
    let (Ok(key), Some(bytes)) = (VerifyingKey::from_bytes(&key), packet.signing_bytes()) else {
        return false;
    };
    key.verify(&bytes, &Signature::from_bytes(&sig)).is_ok()
}
