use bitchat_proto::Packet;
use serde::Deserialize;

fn vectors(path: &str) -> String {
    let full = format!("{}/../../vectors/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{full}: {e}"))
}

fn id(s: &str) -> [u8; 8] {
    hex::decode(s).unwrap().try_into().unwrap()
}

#[derive(Deserialize)]
struct WirePacket {
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
    hex: String,
}

#[derive(Deserialize)]
struct AndroidWire {
    packets: Vec<WirePacket>,
}

#[test]
fn android_golden_packets() {
    let wire: AndroidWire = serde_json::from_str(&vectors("imported/android-wire.json")).unwrap();
    for v in wire.packets {
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
            is_rsr: false,
        };
        let expected = hex::decode(&v.hex).unwrap();
        assert_eq!(packet.encode(false).unwrap(), expected, "{}", v.name);
        assert_eq!(Packet::decode(&expected).unwrap(), packet, "{}", v.name);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CourierInputs {
    sender_i_d: String,
    recipient_i_d: String,
    timestamp_millis: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CourierSigning {
    unsigned_unpadded: String,
    signing_preimage: String,
    signing_preimage_pad_byte: String,
    signing_preimage_pad_count: usize,
    signed_wire_length: usize,
}

#[derive(Deserialize)]
struct CourierEnvelope {
    encoded: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Courier {
    inputs: CourierInputs,
    #[serde(rename = "envelopeTLV")]
    envelope_tlv: CourierEnvelope,
    packet_signing: CourierSigning,
}

#[test]
fn courier_signing_preimage() {
    let c: Courier =
        serde_json::from_str(&vectors("imported/upstream/courier-test-vectors.json")).unwrap();
    let mut packet = Packet::new(
        0x04,
        id(&c.inputs.sender_i_d),
        c.inputs.timestamp_millis,
        hex::decode(&c.envelope_tlv.encoded).unwrap(),
        7,
    );
    packet.recipient_id = Some(id(&c.inputs.recipient_i_d));

    let s = &c.packet_signing;
    assert_eq!(
        hex::encode(packet.encode(false).unwrap()),
        s.unsigned_unpadded
    );

    let preimage = packet.signing_bytes().unwrap();
    assert_eq!(hex::encode(&preimage), s.signing_preimage);
    let pad = u8::from_str_radix(s.signing_preimage_pad_byte.trim_start_matches("0x"), 16).unwrap();
    assert!(
        preimage[preimage.len() - s.signing_preimage_pad_count..]
            .iter()
            .all(|&b| b == pad)
    );

    packet.signature = Some([0x5a; 64]);
    let wire = packet.encode(false).unwrap();
    assert_eq!(wire.len(), s.signed_wire_length);
    assert_eq!(wire[11], 0x03);
    // Relays change TTL; the signed bytes must not change with it.
    packet.ttl = 3;
    assert_eq!(packet.signing_bytes().unwrap(), preimage);
}

fn header(flags: u8, length: [u8; 2]) -> Vec<u8> {
    let mut d = vec![1, 1, 10];
    d.extend_from_slice(&[0; 8]);
    d.push(flags);
    d.extend_from_slice(&length);
    d.extend_from_slice(&[0x01; 8]);
    d
}

// Cases from BitFoundationTests/BinaryProtocolTests.swift.
#[test]
fn malformed_frames_are_rejected() {
    let mut claims_more = header(0, [0x00, 0xc1]);
    claims_more.extend_from_slice(&[0x02; 8]);
    assert_eq!(claims_more.len(), 30);
    assert!(Packet::decode(&claims_more).is_none());

    let mut short_compressed = header(0x04, [0x00, 0x01]);
    short_compressed.push(0x99);
    assert!(Packet::decode(&short_compressed).is_none());

    let mut huge = header(0, [0xff, 0xff]);
    huge.extend_from_slice(&[1, 2, 3]);
    assert!(Packet::decode(&huge).is_none());

    let mut bad_original = header(0x04, [0x00, 0x10]);
    bad_original.extend_from_slice(&[0x20, 0x00, 1, 2, 3, 4]);
    bad_original.resize(21 + 16, 0);
    assert!(Packet::decode(&bad_original).is_none());

    let mut ratio = header(0x04, [0x00, 0x03]);
    ratio.extend_from_slice(&[0xff, 0xff, 0x99]);
    assert!(Packet::decode(&ratio).is_none());

    let mut overflow = header(0x03, [0xff, 0xfe]);
    overflow.extend_from_slice(&[0x02; 8]);
    overflow.extend_from_slice(&[1, 2]);
    assert!(Packet::decode(&overflow).is_none());

    for size in [0, 1, 5, 10, 12] {
        assert!(Packet::decode(&vec![0x01; size]).is_none());
    }

    let packet = Packet::new(0x02, [7; 8], 1, b"hello".to_vec(), 7);
    let encoded = packet.encode(true).unwrap();
    for cut in [0, 5, 10, 15, 20, 25] {
        assert!(Packet::decode(&encoded[..cut]).is_none(), "cut {cut}");
    }
    let unpadded = packet.encode(false).unwrap();
    assert!(Packet::decode(&unpadded[..unpadded.len() - 3]).is_none());

    // Minimal frame with an empty payload decodes.
    let minimal = header(0, [0, 0]);
    assert_eq!(Packet::decode(&minimal).unwrap().payload, Vec::<u8>::new());
}

#[test]
fn padded_frames_round_trip() {
    let payloads = [
        "Short".to_string(),
        "Medium length message content ".repeat(10),
        "Long message content that should exceed the 512 byte limit ".repeat(20),
        "Very long message content that should definitely exceed the 2048 byte limit for sure "
            .repeat(30),
    ];
    for text in payloads {
        let mut packet = Packet::new(
            0x11,
            [9; 8],
            1_790_000_000_000,
            text.clone().into_bytes(),
            7,
        );
        packet.recipient_id = Some([8; 8]);
        let encoded = packet.encode(true).unwrap();
        if encoded.len() <= 2048 {
            assert!(
                [256, 512, 1024, 2048].contains(&encoded.len()),
                "{}",
                encoded.len()
            );
        }
        let decoded = Packet::decode(&encoded).unwrap();
        assert_eq!(decoded.payload, text.as_bytes());
        assert_eq!(decoded, packet);
    }
}

#[test]
fn v1_ignores_route_and_v2_keeps_it() {
    let mut packet = Packet::new(0x11, [1; 8], 42, vec![0xde, 0xad], 6);
    packet.recipient_id = Some([2; 8]);
    packet.route = vec![[3; 8], [4; 8]];

    let v1 = packet.encode(false).unwrap();
    assert_eq!(v1[11] & 0x08, 0);
    assert!(Packet::decode(&v1).unwrap().route.is_empty());

    packet.version = 2;
    let v2 = packet.encode(false).unwrap();
    assert_eq!(v2[11] & 0x08, 0x08);
    assert_eq!(Packet::decode(&v2).unwrap(), packet);
}

#[test]
fn compressed_payload_round_trips() {
    let text = "hello world ".repeat(25).into_bytes();
    let packet = Packet::new(0x02, [5; 8], 1_790_000_000_123, text, 7);
    let encoded = packet.encode(false).unwrap();
    assert_eq!(encoded[11] & 0x04, 0x04);
    assert!(encoded.len() < 14 + 8 + packet.payload.len());
    assert_eq!(Packet::decode(&encoded).unwrap(), packet);
}
