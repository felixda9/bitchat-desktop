//! File transfers: images, voice notes, files (iOS `BitchatFilePacket`,
//! `MimeType`, `BLEIncomingFileValidator`).
//!
//! TLVs: 01 file name, 02 file size (4 bytes; 8 accepted), 03 MIME type, each
//! with a 2-byte big-endian length; 04 content with a 4-byte length (2 bytes
//! accepted for old senders). Unknown tags are skipped.

use crate::limits::MAX_FILE_PAYLOAD_BYTES;

/// Voice notes and images stay small (`FileTransferLimits`).
pub const MAX_VOICE_NOTE_BYTES: usize = 512 * 1024;
pub const MAX_IMAGE_BYTES: usize = 512 * 1024;

const FILE_NAME: u8 = 0x01;
const FILE_SIZE: u8 = 0x02;
const MIME_TYPE: u8 = 0x03;
const CONTENT: u8 = 0x04;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePacket {
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub mime_type: Option<String>,
    pub content: Vec<u8>,
}

impl FilePacket {
    pub fn encode(&self) -> Option<Vec<u8>> {
        let size = self.file_size.unwrap_or(self.content.len() as u64);
        if size > MAX_FILE_PAYLOAD_BYTES as u64 || self.content.len() > MAX_FILE_PAYLOAD_BYTES {
            return None;
        }
        let mut out = Vec::with_capacity(self.content.len() + 64);
        if let Some(name) = &self.file_name
            && name.len() <= u16::MAX as usize
        {
            out.push(FILE_NAME);
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out.push(FILE_SIZE);
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&(size as u32).to_be_bytes());
        if let Some(mime) = &self.mime_type
            && mime.len() <= u16::MAX as usize
        {
            out.push(MIME_TYPE);
            out.extend_from_slice(&(mime.len() as u16).to_be_bytes());
            out.extend_from_slice(mime.as_bytes());
        }
        out.push(CONTENT);
        out.extend_from_slice(&(self.content.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.content);
        Some(out)
    }

    pub fn decode(data: &[u8]) -> Option<FilePacket> {
        let mut at = 0;
        let (mut file_name, mut file_size, mut mime_type) = (None, None, None);
        let mut content = Vec::new();
        let read_len = |at: &mut usize, bytes: usize| -> Option<usize> {
            let v = data.get(*at..*at + bytes)?;
            *at += bytes;
            Some(v.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize))
        };
        while at < data.len() {
            let tag = data[at];
            at += 1;
            let len = if tag == CONTENT {
                // 4-byte length when it fits, else the old 2-byte form.
                let snapshot = at;
                match read_len(&mut at, 4) {
                    Some(n) if n <= data.len() - at => n,
                    _ => {
                        at = snapshot;
                        read_len(&mut at, 2)?
                    }
                }
            } else {
                read_len(&mut at, 2)?
            };
            let value = data.get(at..at + len)?;
            at += len;
            match tag {
                FILE_NAME => file_name = String::from_utf8(value.to_vec()).ok(),
                FILE_SIZE if len == 4 || len == 8 => {
                    let size = value.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64);
                    if size > MAX_FILE_PAYLOAD_BYTES as u64 {
                        return None;
                    }
                    file_size = Some(size);
                }
                MIME_TYPE => mime_type = String::from_utf8(value.to_vec()).ok(),
                CONTENT => {
                    if content.len() + value.len() > MAX_FILE_PAYLOAD_BYTES {
                        return None;
                    }
                    content.extend_from_slice(value);
                }
                _ => {}
            }
        }
        if content.is_empty() {
            return None;
        }
        Some(FilePacket {
            file_name,
            file_size: Some(file_size.unwrap_or(content.len() as u64)),
            mime_type,
            content,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Image,
    Audio,
    File,
}

impl Category {
    /// iOS message text for a received file.
    pub fn prefix(self) -> &'static str {
        match self {
            Category::Audio => "[voice]",
            Category::Image => "[image]",
            Category::File => "[file]",
        }
    }
}

/// An accepted file: its canonical MIME type and category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub packet: FilePacket,
    pub mime: &'static str,
    pub category: Category,
}

/// (canonical MIME, category, aliases)
const TYPES: &[(&str, Category, &[&str])] = &[
    ("image/jpeg", Category::Image, &["image/jpg", "image/pjpeg"]),
    ("image/png", Category::Image, &[]),
    ("image/gif", Category::Image, &[]),
    ("image/webp", Category::Image, &[]),
    (
        "audio/mp4",
        Category::Audio,
        &["audio/x-m4a", "audio/mp4a-latm"],
    ),
    ("audio/m4a", Category::Audio, &[]),
    ("audio/aac", Category::Audio, &["audio/x-aac"]),
    (
        "audio/mpeg",
        Category::Audio,
        &["audio/mpeg3", "audio/x-mp3", "audio/x-mpeg"],
    ),
    ("audio/mp3", Category::Audio, &[]),
    (
        "audio/wav",
        Category::Audio,
        &["audio/wave", "audio/vnd.wave"],
    ),
    ("audio/x-wav", Category::Audio, &[]),
    ("audio/ogg", Category::Audio, &[]),
    ("application/pdf", Category::File, &[]),
    ("application/octet-stream", Category::File, &[]),
];

fn canonical(mime: &str) -> Option<(&'static str, Category)> {
    let m = mime.to_lowercase();
    TYPES
        .iter()
        .find(|(c, _, aliases)| *c == m || aliases.contains(&m.as_str()))
        .map(|(c, cat, _)| (*c, *cat))
}

/// The content starts the way its MIME type says (iOS `MimeType.matches`).
fn magic_ok(mime: &str, d: &[u8]) -> bool {
    if d.is_empty() {
        return false;
    }
    match mime {
        "application/octet-stream" => true,
        "image/jpeg" => d.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => d.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/gif" => {
            d.len() >= 6 && d.starts_with(b"GIF8") && (d[4] == b'7' || d[4] == b'9') && d[5] == b'a'
        }
        "image/webp" => d.len() >= 12 && d.starts_with(b"RIFF") && &d[8..12] == b"WEBP",
        // Recorder output varies; size is capped anyway.
        "audio/mp4" | "audio/m4a" | "audio/aac" => d.len() > 100,
        "audio/mpeg" | "audio/mp3" => {
            d.starts_with(b"ID3") || (d.len() >= 2 && d[0] == 0xFF && d[1] & 0xE0 == 0xE0)
        }
        "audio/wav" | "audio/x-wav" => {
            d.len() >= 12 && d.starts_with(b"RIFF") && &d[8..12] == b"WAVE"
        }
        "audio/ogg" => d.starts_with(b"OggS"),
        "application/pdf" => d.starts_with(b"%PDF"),
        _ => false,
    }
}

/// Decodes and checks a received file: size, allowed type, magic bytes.
pub fn validate(payload: &[u8]) -> Option<Accepted> {
    let packet = FilePacket::decode(payload)?;
    let (mime, category) = canonical(packet.mime_type.as_deref()?)?;
    if packet.content.len() > MAX_FILE_PAYLOAD_BYTES || !magic_ok(mime, &packet.content) {
        return None;
    }
    Some(Accepted {
        packet,
        mime,
        category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg() -> Vec<u8> {
        let mut d = vec![0xFF, 0xD8, 0xFF, 0xE0];
        d.extend(std::iter::repeat_n(7u8, 300));
        d
    }

    #[test]
    fn round_trip_and_layout() {
        let p = FilePacket {
            file_name: Some("img_x.jpg".into()),
            file_size: None,
            mime_type: Some("image/jpeg".into()),
            content: jpeg(),
        };
        let bytes = p.encode().unwrap();
        assert_eq!(&bytes[..3], &[0x01, 0x00, 0x09]);
        assert_eq!(&bytes[12..19], &[0x02, 0x00, 0x04, 0x00, 0x00, 0x01, 0x30]);
        let back = FilePacket::decode(&bytes).unwrap();
        assert_eq!(back.file_size, Some(304));
        assert_eq!(back.content, p.content);
        let ok = validate(&bytes).unwrap();
        assert_eq!((ok.mime, ok.category), ("image/jpeg", Category::Image));
    }

    #[test]
    fn legacy_and_bad_input() {
        // Old 2-byte content length and 8-byte size.
        let mut old = vec![0x02, 0x00, 0x08, 0, 0, 0, 0, 0, 0, 0, 3];
        old.extend_from_slice(&[0x04, 0x00, 0x03, 1, 2, 3]);
        let p = FilePacket::decode(&old).unwrap();
        assert_eq!((p.file_size, p.content), (Some(3), vec![1, 2, 3]));
        assert!(FilePacket::decode(&[0x04, 0x00, 0x00, 0x00, 0x00]).is_none());
        assert!(FilePacket::decode(&[0x01, 0x00, 0x05, b'a']).is_none());
        // A PNG claiming to be a JPEG, and an unknown type, are refused.
        let mut fake = FilePacket {
            file_name: None,
            file_size: None,
            mime_type: Some("image/jpeg".into()),
            content: b"\x89PNG\r\n\x1a\nxxxx".to_vec(),
        };
        assert!(validate(&fake.encode().unwrap()).is_none());
        fake.mime_type = Some("text/html".into());
        assert!(validate(&fake.encode().unwrap()).is_none());
        fake.mime_type = Some("IMAGE/PNG".into());
        assert_eq!(validate(&fake.encode().unwrap()).unwrap().mime, "image/png");
    }
}
