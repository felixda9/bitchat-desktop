use bitchat_nostr::envelope::{self, EnvelopeError};
use bitchat_nostr::event::kind;
use bitchat_nostr::{Event, Keys};
use serde::Deserialize;
use serde_json::Value;

fn read(path: &str) -> String {
    let full = format!("{}/../../vectors/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{full}: {e}"))
}

fn keys(hex_secret: &str) -> Keys {
    Keys::from_secret_bytes(hex::decode(hex_secret).unwrap().try_into().unwrap()).unwrap()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Deserialize)]
struct Captured {
    name: String,
    event_file: String,
    recipient_private_key: String,
    expected_content: String,
    expected_sender: String,
}

#[test]
fn captured_ios_and_android_envelopes_open() {
    let cases: Vec<Captured> = serde_json::from_str(&read("imported/envelopes.json")).unwrap();
    for c in cases {
        let wrap: Event =
            serde_json::from_str(&read(&format!("imported/{}", c.event_file))).unwrap();
        let opened = envelope::open_private_message(&wrap, &keys(&c.recipient_private_key))
            .unwrap_or_else(|e| panic!("{}: {e}", c.name));
        assert_eq!(opened.content, c.expected_content, "{}", c.name);
        assert_eq!(opened.sender, c.expected_sender, "{}", c.name);
    }
}

#[derive(Deserialize)]
struct Chain {
    sender_private_key: String,
    recipient_private_key: String,
    sender_shared_point: String,
    recipient_shared_point_even: String,
    recipient_shared_point_odd: String,
    key: String,
    nonce: String,
    plaintext: String,
    content: String,
}

#[test]
fn key_chain_matches_ios() {
    let export: Value = serde_json::from_str(&read("swift/export.json")).unwrap();
    let rows: Vec<Chain> = serde_json::from_value(export["envelope_chain"].clone()).unwrap();
    for r in rows {
        let sender = keys(&r.sender_private_key);
        let recipient = keys(&r.recipient_private_key);
        let rx = recipient.public_key();
        let sx = sender.public_key();

        let shared = envelope::shared_point(&sender.secret_key(), &rx, false).unwrap();
        assert_eq!(hex::encode(shared), r.sender_shared_point);
        let even = envelope::shared_point(&recipient.secret_key(), &sx, false).unwrap();
        let odd = envelope::shared_point(&recipient.secret_key(), &sx, true).unwrap();
        assert_eq!(hex::encode(even), r.recipient_shared_point_even);
        assert_eq!(hex::encode(odd), r.recipient_shared_point_odd);
        assert_eq!(hex::encode(envelope::derive_key(&shared)), r.key);

        let nonce: [u8; 24] = hex::decode(&r.nonce).unwrap().try_into().unwrap();
        let ours = envelope::encrypt_with_nonce(&r.plaintext, &sender, &rx, &nonce).unwrap();
        assert_eq!(ours, r.content);
        let plain = envelope::decrypt(&r.content, &sender.public_key_hex(), &recipient).unwrap();
        assert_eq!(plain, r.plaintext);
    }
}

#[derive(Deserialize)]
struct SwiftEnvelope {
    event: Event,
    recipient_private_key: String,
    expected_content: String,
    expected_sender: String,
}

#[test]
fn ios_made_envelopes_open() {
    let export: Value = serde_json::from_str(&read("swift/export.json")).unwrap();
    let rows: Vec<SwiftEnvelope> = serde_json::from_value(export["envelopes"].clone()).unwrap();
    assert_eq!(rows.len(), 3);
    for r in rows {
        let opened =
            envelope::open_private_message(&r.event, &keys(&r.recipient_private_key)).unwrap();
        assert_eq!(opened.content, r.expected_content);
        assert_eq!(opened.sender, r.expected_sender);
    }
}

#[test]
fn round_trip_keeps_real_time_and_jitters_layers() {
    let sender = Keys::generate();
    let recipient = Keys::generate();
    let t = now();
    let wrap =
        envelope::create_private_message("hello", &recipient.public_key_hex(), &sender, t).unwrap();
    assert_eq!(wrap.kind, kind::GIFT_WRAP);
    assert_ne!(wrap.pubkey, sender.public_key_hex());
    assert!((wrap.created_at - t).abs() <= 900);
    let opened = envelope::open_private_message(&wrap, &recipient).unwrap();
    assert_eq!(opened.content, "hello");
    assert_eq!(opened.sender, sender.public_key_hex());
    assert_eq!(opened.timestamp, t);

    let other = Keys::generate();
    assert!(envelope::open_private_message(&wrap, &other).is_err());
}

#[test]
fn android_inner_p_tag_is_accepted_other_shapes_are_not() {
    let sender = Keys::generate();
    let recipient = Keys::generate();
    let me = recipient.public_key_hex();
    let other = Keys::generate().public_key_hex();
    let ok = envelope::create_private_message_with_tags(
        "a",
        &me,
        &sender,
        now(),
        vec![vec!["p".into(), me.clone()]],
    )
    .unwrap();
    assert!(envelope::open_private_message(&ok, &recipient).is_ok());

    let bad: Vec<Vec<Vec<String>>> = vec![
        vec![vec!["p".into(), other]],
        vec![vec!["p".into(), me.clone()], vec!["p".into(), me.clone()]],
        vec![vec!["p".into(), me.clone(), "unexpected".into()]],
        vec![vec!["x".into(), me.clone()]],
    ];
    for tags in bad {
        let wrap =
            envelope::create_private_message_with_tags("a", &me, &sender, now(), tags).unwrap();
        assert_eq!(
            envelope::open_private_message(&wrap, &recipient),
            Err(EnvelopeError::InvalidEvent)
        );
    }
}

fn wrap_raw(content: String, tags: Vec<Vec<String>>, kind_: u32) -> Event {
    let wrapper = Keys::generate();
    Event::new(wrapper.public_key_hex(), now(), kind_, tags, content).sign(&wrapper)
}

#[test]
fn outer_layer_checks() {
    let recipient = Keys::generate();
    let me = recipient.public_key_hex();
    let p = vec![vec!["p".to_string(), me.clone()]];

    let oversized = wrap_raw(
        format!("v2:{}", "A".repeat(64 * 1024)),
        p.clone(),
        kind::GIFT_WRAP,
    );
    assert_eq!(
        envelope::open_private_message(&oversized, &recipient),
        Err(EnvelopeError::InvalidCiphertext)
    );

    // A real NIP-44 v2 payload must not be read as BitChat's format.
    let nip44 = "AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABee0G5VSK0/9YypIObAtDKfYEAjD35uVkHyB0F4DwrcNaCXlCWZKaArsGrY6M9wnuTMxWfp1RTN9Xga8no+kF5Vsb";
    let standard = wrap_raw(nip44.to_string(), p.clone(), kind::GIFT_WRAP);
    assert_eq!(
        envelope::open_private_message(&standard, &recipient),
        Err(EnvelopeError::InvalidCiphertext)
    );

    let sender = Keys::generate();
    let real = envelope::create_private_message("x", &me, &sender, now()).unwrap();

    let mut wrong_kind = real.clone();
    wrong_kind.kind = kind::TEXT_NOTE;
    assert_eq!(
        envelope::open_private_message(&wrong_kind, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );

    let mut extra_tag = real.clone();
    extra_tag.tags.push(vec!["p".into(), me.clone()]);
    assert_eq!(
        envelope::open_private_message(&extra_tag, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );

    let mut bad_sig = real;
    bad_sig.sig = Some("0".repeat(128));
    assert_eq!(
        envelope::open_private_message(&bad_sig, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );
}

/// Builds a wrap by hand so the seal and rumor can be tampered with.
fn custom_wrap(recipient: &Keys, seal: Event) -> Event {
    let wrapper = Keys::generate();
    let content = envelope::encrypt(
        &serde_json::to_string(&seal).unwrap(),
        &wrapper,
        &recipient.public_key(),
    )
    .unwrap();
    Event::new(
        wrapper.public_key_hex(),
        now(),
        kind::GIFT_WRAP,
        vec![vec!["p".into(), recipient.public_key_hex()]],
        content,
    )
    .sign(&wrapper)
}

fn seal_for(recipient: &Keys, signer: &Keys, rumor: &Event) -> Event {
    let content = envelope::encrypt(
        &serde_json::to_string(rumor).unwrap(),
        signer,
        &recipient.public_key(),
    )
    .unwrap();
    Event::new(signer.public_key_hex(), now(), kind::SEAL, vec![], content).sign(signer)
}

#[test]
fn seal_and_rumor_checks() {
    let recipient = Keys::generate();
    let sender = Keys::generate();
    let rumor = Event::new(
        sender.public_key_hex(),
        now(),
        kind::DM,
        vec![],
        "hi".into(),
    );

    let good = custom_wrap(&recipient, seal_for(&recipient, &sender, &rumor));
    assert!(envelope::open_private_message(&good, &recipient).is_ok());

    let mut bad_seal = seal_for(&recipient, &sender, &rumor);
    bad_seal.sig = Some("0".repeat(128));
    let w = custom_wrap(&recipient, bad_seal);
    assert_eq!(
        envelope::open_private_message(&w, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );

    let impostor = Keys::generate();
    let w = custom_wrap(&recipient, seal_for(&recipient, &impostor, &rumor));
    assert_eq!(
        envelope::open_private_message(&w, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );

    let signed_rumor = rumor.clone().sign(&sender);
    let w = custom_wrap(&recipient, seal_for(&recipient, &sender, &signed_rumor));
    assert_eq!(
        envelope::open_private_message(&w, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );

    let mut tagged_seal = seal_for(&recipient, &sender, &rumor);
    tagged_seal.tags = vec![vec!["p".into(), recipient.public_key_hex()]];
    let tagged_seal = Event::new(
        tagged_seal.pubkey,
        tagged_seal.created_at,
        kind::SEAL,
        tagged_seal.tags,
        tagged_seal.content,
    )
    .sign(&sender);
    let w = custom_wrap(&recipient, tagged_seal);
    assert_eq!(
        envelope::open_private_message(&w, &recipient),
        Err(EnvelopeError::InvalidEvent)
    );
}

#[test]
fn rumor_time_window() {
    let t = 1_790_000_000;
    assert!(envelope::is_plausible_rumor_timestamp(t, t));
    assert!(envelope::is_plausible_rumor_timestamp(t + 900, t));
    assert!(!envelope::is_plausible_rumor_timestamp(t + 901, t));
    assert!(envelope::is_plausible_rumor_timestamp(t - 86_400 - 900, t));
    assert!(!envelope::is_plausible_rumor_timestamp(t - 86_400 - 901, t));
}
