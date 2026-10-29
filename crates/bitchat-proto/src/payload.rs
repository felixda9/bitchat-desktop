//! Typed private payloads carried in `noiseEncrypted` packets
//! (`NoisePayloadType`, `NoisePayload`, `PrivateMessagePacket`).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PayloadType {
    PrivateMessage,
    ReadReceipt,
    Delivered,
    GroupInvite,
    GroupKeyUpdate,
    VoiceFrame,
    VerifyChallenge,
    VerifyResponse,
    Vouch,
    PrivateFile,
    AuthenticatedPeerState,
}

/// Pre-release code for `PrivateFile`: accepted on decode, never sent.
pub const PRIVATE_FILE_PRERELEASE: u8 = 0x09;

impl PayloadType {
    pub fn code(self) -> u8 {
        match self {
            PayloadType::PrivateMessage => 0x01,
            PayloadType::ReadReceipt => 0x02,
            PayloadType::Delivered => 0x03,
            PayloadType::GroupInvite => 0x06,
            PayloadType::GroupKeyUpdate => 0x07,
            PayloadType::VoiceFrame => 0x08,
            PayloadType::VerifyChallenge => 0x10,
            PayloadType::VerifyResponse => 0x11,
            PayloadType::Vouch => 0x12,
            PayloadType::PrivateFile => 0x20,
            PayloadType::AuthenticatedPeerState => 0x21,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x01 => PayloadType::PrivateMessage,
            0x02 => PayloadType::ReadReceipt,
            0x03 => PayloadType::Delivered,
            0x06 => PayloadType::GroupInvite,
            0x07 => PayloadType::GroupKeyUpdate,
            0x08 => PayloadType::VoiceFrame,
            0x10 => PayloadType::VerifyChallenge,
            0x11 => PayloadType::VerifyResponse,
            0x12 => PayloadType::Vouch,
            0x20 | PRIVATE_FILE_PRERELEASE => PayloadType::PrivateFile,
            0x21 => PayloadType::AuthenticatedPeerState,
            _ => return None,
        })
    }
}

/// `type (1 byte) || data`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoisePayload {
    pub kind: PayloadType,
    pub data: Vec<u8>,
}

impl NoisePayload {
    pub fn new(kind: PayloadType, data: Vec<u8>) -> Self {
        NoisePayload { kind, data }
    }

    /// Delivery or read ack: the body is the UTF-8 message ID.
    pub fn ack(kind: PayloadType, message_id: &str) -> Self {
        NoisePayload::new(kind, message_id.as_bytes().to_vec())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.data.len());
        out.push(self.kind.code());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&code, rest) = data.split_first()?;
        Some(NoisePayload::new(
            PayloadType::from_code(code)?,
            rest.to_vec(),
        ))
    }
}

/// Largest UTF-8 length of a private message ID or content.
pub const PRIVATE_MESSAGE_FIELD_MAX: usize = 255;

/// TLV `00 len messageID` `01 len content`, one-byte lengths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateMessage {
    pub message_id: String,
    pub content: String,
}

impl PrivateMessage {
    pub fn new(message_id: impl Into<String>, content: impl Into<String>) -> Self {
        PrivateMessage {
            message_id: message_id.into(),
            content: content.into(),
        }
    }

    /// `None` when either field is longer than 255 UTF-8 bytes (iOS drops such sends).
    pub fn encode(&self) -> Option<Vec<u8>> {
        let id = self.message_id.as_bytes();
        let content = self.content.as_bytes();
        if id.len() > PRIVATE_MESSAGE_FIELD_MAX || content.len() > PRIVATE_MESSAGE_FIELD_MAX {
            return None;
        }
        let mut out = Vec::with_capacity(4 + id.len() + content.len());
        out.push(0x00);
        out.push(id.len() as u8);
        out.extend_from_slice(id);
        out.push(0x01);
        out.push(content.len() as u8);
        out.extend_from_slice(content);
        Some(out)
    }

    /// Unknown TLV types reject the whole packet; later duplicates win; a
    /// trailing lone byte is ignored.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut message_id: Option<String> = None;
        let mut content: Option<String> = None;
        while offset + 2 <= data.len() {
            let kind = data[offset];
            if kind > 0x01 {
                return None;
            }
            let len = data[offset + 1] as usize;
            offset += 2;
            let value = data.get(offset..offset + len)?;
            offset += len;
            let text = String::from_utf8(value.to_vec()).ok();
            if kind == 0x00 {
                message_id = text;
            } else {
                content = text;
            }
        }
        Some(PrivateMessage {
            message_id: message_id?,
            content: content?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_file_code_is_canonicalized() {
        let p = NoisePayload::decode(&[0x09, 0xca, 0xfe]).unwrap();
        assert_eq!(p.kind, PayloadType::PrivateFile);
        assert_eq!(p.encode(), vec![0x20, 0xca, 0xfe]);
        assert!(NoisePayload::decode(&[]).is_none());
        assert!(NoisePayload::decode(&[0x04]).is_none());
    }

    #[test]
    fn field_limit() {
        assert!(
            PrivateMessage::new("id", "x".repeat(255))
                .encode()
                .is_some()
        );
        assert!(
            PrivateMessage::new("id", "x".repeat(256))
                .encode()
                .is_none()
        );
        assert!(
            PrivateMessage::new("id", "é".repeat(128))
                .encode()
                .is_none()
        );
    }

    #[test]
    fn decode_rules() {
        assert!(PrivateMessage::decode(&[0x02, 0x00]).is_none());
        assert!(PrivateMessage::decode(&[0x00, 0x01, b'a']).is_none());
        let dup = [0x00, 1, b'a', 0x01, 1, b'x', 0x00, 1, b'b', 0x7f];
        assert_eq!(
            PrivateMessage::decode(&dup).unwrap(),
            PrivateMessage::new("b", "x")
        );
        assert!(PrivateMessage::decode(&[0x00, 1, 0xff, 0x01, 0]).is_none());
    }
}
