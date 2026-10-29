//! Checks against vectors exported from the iOS code (vectors/swift/export.json).

use bitchat_proto::payload::PrivateMessage;
use bitchat_proto::{Packet, compression, geohash};
use serde::Deserialize;

#[derive(Deserialize)]
struct Export {
    packets: Vec<SwiftPacket>,
    compression: Vec<SwiftCompression>,
    private_message: Vec<SwiftPrivateMessage>,
    geohash: SwiftGeohash,
}

#[derive(Deserialize)]
struct SwiftPacket {
    name: String,
    version: u8,
    #[serde(rename = "type")]
    packet_type: u8,
    ttl: u8,
    timestamp: u64,
    sender: String,
    recipient: Option<String>,
    route: Option<Vec<String>>,
    payload: String,
    signature: Option<String>,
    is_rsr: bool,
    padded: String,
    unpadded: String,
    signing: String,
}

#[derive(Deserialize)]
struct SwiftCompression {
    input: String,
    should_compress: bool,
    compressed: Option<String>,
}

#[derive(Deserialize)]
struct SwiftPrivateMessage {
    message_id: String,
    content: String,
    tlv: Option<String>,
}

#[derive(Deserialize)]
struct SwiftGeohash {
    encode: Vec<GeoEncode>,
    decode: Vec<GeoDecode>,
    validity: Vec<GeoValidity>,
}

#[derive(Deserialize)]
struct GeoEncode {
    lat: f64,
    lon: f64,
    precision: usize,
    geohash: String,
}

#[derive(Deserialize)]
struct GeoDecode {
    geohash: String,
    center_lat: f64,
    center_lon: f64,
    neighbors: Vec<String>,
    valid: bool,
}

#[derive(Deserialize)]
struct GeoValidity {
    geohash: String,
    valid: bool,
}

fn export() -> Export {
    let path = format!(
        "{}/../../vectors/swift/export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn id(s: &str) -> [u8; 8] {
    hex::decode(s).unwrap().try_into().unwrap()
}

#[test]
fn packets_match_swift_bytes() {
    for v in export().packets {
        let packet = Packet {
            version: v.version,
            packet_type: v.packet_type,
            ttl: v.ttl,
            timestamp: v.timestamp,
            sender_id: id(&v.sender),
            recipient_id: v.recipient.as_deref().map(id),
            payload: hex::decode(&v.payload).unwrap(),
            signature: v
                .signature
                .as_deref()
                .map(|s| hex::decode(s).unwrap().try_into().unwrap()),
            route: v.route.unwrap_or_default().iter().map(|h| id(h)).collect(),
            is_rsr: v.is_rsr,
        };
        assert_eq!(
            hex::encode(packet.encode(true).unwrap()),
            v.padded,
            "{} padded",
            v.name
        );
        assert_eq!(
            hex::encode(packet.encode(false).unwrap()),
            v.unpadded,
            "{} unpadded",
            v.name
        );
        assert_eq!(
            hex::encode(packet.signing_bytes().unwrap()),
            v.signing,
            "{} signing",
            v.name
        );
        assert_eq!(
            Packet::decode(&hex::decode(&v.padded).unwrap()).unwrap(),
            packet,
            "{} decode",
            v.name
        );
    }
}

#[test]
fn compression_matches_apple() {
    for v in export().compression {
        let input = hex::decode(&v.input).unwrap();
        assert_eq!(
            compression::should_compress(&input),
            v.should_compress,
            "{}",
            v.input
        );
        if let Some(expected) = &v.compressed {
            let expected = hex::decode(expected).unwrap();
            assert_eq!(
                compression::decompress(&expected, input.len()).unwrap(),
                input
            );
            let ours = compression::compress(&input).unwrap();
            assert_eq!(
                hex::encode(&ours),
                hex::encode(&expected),
                "deflate bytes for {}",
                v.input
            );
        }
    }
}

#[test]
fn private_messages_match_swift() {
    for v in export().private_message {
        let ours = PrivateMessage::new(v.message_id.clone(), v.content.clone()).encode();
        assert_eq!(ours.as_deref().map(hex::encode), v.tlv, "{}", v.content);
        if let Some(tlv) = v.tlv {
            let decoded = PrivateMessage::decode(&hex::decode(tlv).unwrap()).unwrap();
            assert_eq!(decoded, PrivateMessage::new(v.message_id, v.content));
        }
    }
}

#[test]
fn geohash_matches_swift() {
    let g = export().geohash;
    for v in g.encode {
        assert_eq!(
            geohash::encode(v.lat, v.lon, v.precision),
            v.geohash,
            "{} {}",
            v.lat,
            v.lon
        );
    }
    for v in g.decode {
        let (lat, lon) = geohash::decode_center(&v.geohash);
        assert_eq!((lat, lon), (v.center_lat, v.center_lon), "{}", v.geohash);
        assert_eq!(geohash::neighbors(&v.geohash), v.neighbors, "{}", v.geohash);
        assert_eq!(geohash::is_valid(&v.geohash), v.valid, "{}", v.geohash);
    }
    for v in g.validity {
        assert_eq!(geohash::is_valid(&v.geohash), v.valid, "{:?}", v.geohash);
    }
}
