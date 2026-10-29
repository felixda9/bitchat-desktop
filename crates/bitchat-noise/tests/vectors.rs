use bitchat_noise::{Handshake, NoiseError, Role, Session};
use serde::Deserialize;

#[derive(Deserialize)]
struct Message {
    payload: String,
    ciphertext: String,
}

#[derive(Deserialize)]
struct Vector {
    protocol_name: String,
    init_prologue: String,
    init_static: String,
    init_ephemeral: String,
    resp_static: String,
    resp_ephemeral: String,
    handshake_hash: Option<String>,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct File {
    vectors: Vec<Vector>,
}

fn key(hex_str: &str) -> [u8; 32] {
    hex::decode(hex_str).unwrap().try_into().unwrap()
}

fn vectors() -> Vec<Vector> {
    let path = format!(
        "{}/../../vectors/imported/upstream/NoiseTestVectors.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).unwrap();
    // The iOS file is either a bare list or {"vectors": [...]}.
    serde_json::from_str::<File>(&text)
        .map(|f| f.vectors)
        .or_else(|_| serde_json::from_str::<Vec<Vector>>(&text))
        .unwrap()
}

/// The XX vectors iOS checks (cacophony/snow): handshake bytes, hash, and
/// transport ciphertext once the 4-byte nonce prefix is stripped.
#[test]
fn ios_noise_vectors() {
    let vs = vectors();
    assert!(!vs.is_empty());
    for v in vs {
        assert_eq!(v.protocol_name, bitchat_noise::PROTOCOL);
        let prologue = hex::decode(&v.init_prologue).unwrap();
        let mut init = Handshake::with_options(
            Role::Initiator,
            &key(&v.init_static),
            &prologue,
            Some(&key(&v.init_ephemeral)),
        )
        .unwrap();
        let mut resp = Handshake::with_options(
            Role::Responder,
            &key(&v.resp_static),
            &prologue,
            Some(&key(&v.resp_ephemeral)),
        )
        .unwrap();

        for (i, m) in v.messages.iter().take(3).enumerate() {
            let payload = hex::decode(&m.payload).unwrap();
            let (sender, receiver) = if i % 2 == 0 {
                (&mut init, &mut resp)
            } else {
                (&mut resp, &mut init)
            };
            let wire = sender.write_payload(&payload).unwrap();
            assert_eq!(hex::encode(&wire), m.ciphertext, "handshake message {i}");
            assert_eq!(receiver.read_message(&wire).unwrap(), payload);
        }
        assert!(init.is_finished() && resp.is_finished());
        if let Some(h) = &v.handshake_hash {
            assert_eq!(hex::encode(init.handshake_hash()), *h);
        }

        let mut init: Session = init.into_session().unwrap();
        let mut resp: Session = resp.into_session().unwrap();
        for (i, m) in v.messages.iter().enumerate().skip(3) {
            let payload = hex::decode(&m.payload).unwrap();
            let (sender, receiver) = if i % 2 == 0 {
                (&mut init, &mut resp)
            } else {
                (&mut resp, &mut init)
            };
            let frame = sender.encrypt(&payload).unwrap();
            assert_eq!(
                hex::encode(&frame[4..]),
                m.ciphertext,
                "transport message {i}"
            );
            assert_eq!(receiver.decrypt(&frame).unwrap(), payload);
        }
    }
}

fn pair() -> (Session, Session) {
    let mut a = Handshake::new(Role::Initiator, &[1u8; 32]).unwrap();
    let mut b = Handshake::new(Role::Responder, &[2u8; 32]).unwrap();
    let m1 = a.write_message().unwrap();
    assert_eq!(m1.len(), 32);
    b.read_message(&m1).unwrap();
    let m2 = b.write_message().unwrap();
    assert_eq!(m2.len(), 96);
    a.read_message(&m2).unwrap();
    let m3 = a.write_message().unwrap();
    assert_eq!(m3.len(), 64);
    b.read_message(&m3).unwrap();
    (a.into_session().unwrap(), b.into_session().unwrap())
}

#[test]
fn bitchat_frames() {
    let (mut a, mut b) = pair();
    assert_eq!(
        a.remote_peer_id(),
        bitchat_noise::peer_id(&bitchat_noise::public_key(&[2u8; 32]))
    );

    let f0 = a.encrypt(b"zero").unwrap();
    let f1 = a.encrypt(b"one").unwrap();
    let f2 = a.encrypt(b"two").unwrap();
    assert_eq!(&f0[..4], &[0, 0, 0, 0]);
    assert_eq!(&f2[..4], &[0, 0, 0, 2]);
    assert_eq!(f1.len(), 4 + 3 + 16);

    // Out of order is fine, replays are not.
    assert_eq!(b.decrypt(&f2).unwrap(), b"two");
    assert_eq!(b.decrypt(&f0).unwrap(), b"zero");
    assert_eq!(b.decrypt(&f0), Err(NoiseError::ReplayDetected));
    assert_eq!(b.decrypt(&f1).unwrap(), b"one");

    // A forged frame fails without moving the window.
    let mut bad = a.encrypt(b"three").unwrap();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(matches!(b.decrypt(&bad), Err(NoiseError::Snow(_))));
    bad[last] ^= 1;
    assert_eq!(b.decrypt(&bad).unwrap(), b"three");

    assert_eq!(b.decrypt(&[0u8; 19]), Err(NoiseError::InvalidCiphertext));

    a.set_send_nonce_for_testing(bitchat_noise::MAX_NONCE + 1);
    assert_eq!(a.encrypt(b"x"), Err(NoiseError::NonceExceeded));
}

#[test]
fn weak_ephemeral_rejected() {
    let mut b = Handshake::new(Role::Responder, &[2u8; 32]).unwrap();
    assert_eq!(
        b.read_message(&[0u8; 32]),
        Err(NoiseError::InvalidPublicKey)
    );
}

/// Handshake and extracted-nonce frames exported from the iOS code.
#[test]
fn ios_bitchat_frames() {
    let path = format!(
        "{}/../../vectors/swift/export.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let export: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let Some(n) = export.get("noise") else {
        eprintln!("no noise section in vectors/swift/export.json yet; run the swift-vectors job");
        return;
    };
    let s = |k: &str| n[k].as_str().unwrap().to_string();
    let mut init = Handshake::with_options(
        Role::Initiator,
        &key(&s("init_static")),
        &[],
        Some(&key(&s("init_ephemeral"))),
    )
    .unwrap();
    let mut resp = Handshake::with_options(
        Role::Responder,
        &key(&s("resp_static")),
        &[],
        Some(&key(&s("resp_ephemeral"))),
    )
    .unwrap();
    let expected: Vec<String> = n["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect();
    let m1 = init.write_message().unwrap();
    assert_eq!(hex::encode(&m1), expected[0]);
    resp.read_message(&m1).unwrap();
    let m2 = resp.write_message().unwrap();
    assert_eq!(hex::encode(&m2), expected[1]);
    init.read_message(&m2).unwrap();
    let m3 = init.write_message().unwrap();
    assert_eq!(hex::encode(&m3), expected[2]);
    resp.read_message(&m3).unwrap();
    assert_eq!(hex::encode(init.handshake_hash()), s("handshake_hash"));

    let mut init = init.into_session().unwrap();
    let mut resp = resp.into_session().unwrap();
    for f in n["frames"].as_array().unwrap() {
        let plaintext = hex::decode(f["plaintext"].as_str().unwrap()).unwrap();
        let from_initiator = f["from_initiator"].as_bool().unwrap();
        let (sender, receiver) = if from_initiator {
            (&mut init, &mut resp)
        } else {
            (&mut resp, &mut init)
        };
        let frame = sender.encrypt(&plaintext).unwrap();
        assert_eq!(hex::encode(&frame), f["frame"].as_str().unwrap());
        assert_eq!(receiver.decrypt(&frame).unwrap(), plaintext);
    }
}
