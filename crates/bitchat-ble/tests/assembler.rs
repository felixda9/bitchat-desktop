//! Cases from iOS `NotificationStreamAssemblerTests`, plus stalls and routes.

use bitchat_ble::assembler::{STALL_RESET_MS, StreamAssembler};
use bitchat_proto::{Packet, message_type as t};

fn packet(timestamp: u64) -> Packet {
    Packet::new(
        t::MESSAGE,
        [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
        timestamp,
        vec![0xDE, 0xAD, 0xBE, 0xEF],
        3,
    )
}

#[test]
fn one_frame_across_chunks() {
    let frame = packet(0x0102030405).encode(false).unwrap();
    let mut a = StreamAssembler::default();
    let split = 20.min(frame.len() / 2);
    let first = a.append(&frame[..split], 0);
    assert!(first.frames.is_empty() && first.dropped.is_empty() && !first.reset);
    let second = a.append(&frame[split..], 1);
    assert_eq!(second.frames, vec![frame.clone()]);
    assert_eq!(
        Packet::decode(&second.frames[0]).unwrap(),
        packet(0x0102030405)
    );

    let mut direct = StreamAssembler::default();
    assert_eq!(direct.append(&frame, 0).frames, vec![frame]);
}

#[test]
fn several_frames_in_a_row() {
    let f1 = packet(0xABC).encode(false).unwrap();
    let f2 = packet(0xDEF).encode(false).unwrap();
    let combined = [f1.clone(), f2.clone()].concat();
    let mut a = StreamAssembler::default();
    assert!(a.append(&combined[..20], 0).frames.is_empty());
    assert_eq!(a.append(&combined[20..], 1).frames, vec![f1, f2]);
}

#[test]
fn stray_prefix_byte_is_dropped() {
    let frame = packet(0xF00).encode(false).unwrap();
    let mut a = StreamAssembler::default();
    let out = a.append(&[[0x00].as_slice(), &frame].concat(), 0);
    assert_eq!(out.dropped, vec![0x00]);
    assert_eq!(out.frames, vec![frame]);
    assert!(!out.reset);
}

#[test]
fn padding_between_frames_is_skipped() {
    let p1 = packet(0x111).encode(true).unwrap();
    let p2 = packet(0x222).encode(true).unwrap();
    let mut a = StreamAssembler::default();
    let out = a.append(&p1, 0);
    assert_eq!(out.frames.len(), 1);
    assert!(out.dropped.is_empty());
    let out = a.append(&p2, 1);
    assert_eq!(out.frames.len(), 1);
    assert!(out.dropped.is_empty() && !out.reset);
    assert_eq!(Packet::decode(&out.frames[0]).unwrap().timestamp, 0x222);
}

#[test]
fn large_compressed_v2_frame() {
    let mut p = Packet::new(
        t::FILE_TRANSFER,
        [0xAA; 8],
        0x010203040506,
        vec![0x41; 600_000],
        3,
    );
    p.version = 2;
    let frame = p.encode(false).unwrap();
    assert!(frame[11] & 0x04 != 0, "compressed");
    let mut a = StreamAssembler::default();
    let split = 4096.min(frame.len() / 2);
    assert!(a.append(&frame[..split], 0).frames.is_empty());
    let out = a.append(&frame[split..], 1);
    assert_eq!(out.frames, vec![frame]);
    assert_eq!(
        Packet::decode(&out.frames[0]).unwrap().payload.len(),
        600_000
    );
}

#[test]
fn routed_frames_count_their_hops() {
    let mut p = packet(5);
    p.version = 2;
    p.recipient_id = Some([9; 8]);
    p.route = vec![[1; 8], [2; 8]];
    let frame = p.encode(false).unwrap();
    let mut a = StreamAssembler::default();
    assert!(a.append(&frame[..frame.len() - 1], 0).frames.is_empty());
    assert_eq!(a.append(&frame[frame.len() - 1..], 1).frames, vec![frame]);
}

#[test]
fn a_stalled_partial_frame_is_dropped() {
    let mut long = packet(1);
    long.payload = (0..100u8).collect();
    let frame = long.encode(false).unwrap();
    let mut a = StreamAssembler::default();
    assert!(a.append(&frame[..30], 0).frames.is_empty());
    // Still incomplete, but within the window: keep waiting.
    let out = a.append(&frame[30..40], STALL_RESET_MS - 1);
    assert!(!out.reset);
    let mut a = StreamAssembler::default();
    a.append(&frame[..30], 0);
    // More bytes arrive too late and the frame is still not whole: reset.
    let out = a.append(&frame[30..40], STALL_RESET_MS);
    assert!(out.reset && out.frames.is_empty());
    // A fresh frame afterwards assembles normally.
    assert_eq!(a.append(&frame, STALL_RESET_MS + 1).frames, vec![frame]);
}

#[test]
fn trailing_zero_bytes_are_cleared() {
    let frame = packet(2).encode(false).unwrap();
    let mut a = StreamAssembler::default();
    let out = a.append(&[frame.as_slice(), &[0, 0, 0]].concat(), 0);
    assert_eq!(out.frames.len(), 1);
    assert_eq!(
        a.append(&frame, 1).frames,
        vec![frame],
        "no leftover zeros in front"
    );
}
