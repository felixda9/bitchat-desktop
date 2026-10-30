use bitchat_ble::writes::{Assembled, WriteAssembler};
use bitchat_proto::{Packet, message_type as t};

fn frame(ts: u64) -> Vec<u8> {
    Packet::new(t::MESSAGE, [3; 8], ts, (0..120u8).collect(), 7)
        .encode(false)
        .unwrap()
}

#[test]
fn whole_frames_come_straight_through() {
    let mut w = WriteAssembler::default();
    let f = frame(1);
    assert_eq!(w.append(0, &f), Assembled::Frame(f.clone()));
    let g = frame(2);
    assert_eq!(w.append(0, &g), Assembled::Frame(g));
}

#[test]
fn long_writes_assemble_by_offset() {
    let mut w = WriteAssembler::default();
    let f = frame(3);
    assert_eq!(w.append(0, &f[..60]), Assembled::Waiting);
    assert_eq!(w.append(60, &f[60..100]), Assembled::Waiting);
    assert_eq!(w.append(100, &f[100..]), Assembled::Frame(f));
}

#[test]
fn chunks_out_of_order_still_assemble() {
    let mut w = WriteAssembler::default();
    let f = frame(4);
    assert_eq!(w.append(80, &f[80..]), Assembled::Waiting);
    assert_eq!(w.append(0, &f[..80]), Assembled::Frame(f));
}
