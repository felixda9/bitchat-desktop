//! Two engines talking through their effects, no network.

use bitchat_core::engine::{Effect, Engine, dm_sub, geo_sub};
use bitchat_core::model::{DmStatus, UiEvent};
use bitchat_core::state::State;
use bitchat_nostr::event::kind;
use bitchat_nostr::net::PoolEvent;
use bitchat_nostr::{Event, pow};

const GH: &str = "u4pruy";
const T0: u64 = 1_790_000_000_000;

fn user(nick: &str) -> Engine {
    let mut state = State::fresh();
    state.nickname = nick.to_string();
    Engine::new(state, true)
}

/// Runs mining synchronously and returns the remaining effects.
fn settle(engine: &mut Engine) -> Vec<Effect> {
    let mut out = Vec::new();
    loop {
        let effects = engine.take_effects();
        if effects.is_empty() {
            return out;
        }
        for e in effects {
            if let Effect::Mine(req) = e {
                let tag = pow::mine_nonce_tag(
                    &req.pubkey,
                    req.created_at,
                    kind::EPHEMERAL,
                    &req.tags,
                    &req.content,
                    pow::TARGET_BITS,
                );
                engine.finish_public(req.id, tag);
            } else {
                out.push(e);
            }
        }
    }
}

fn published(effects: &[Effect]) -> Vec<Event> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Publish { event, .. } => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn ui(effects: &[Effect]) -> Vec<UiEvent> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Ui(u) => Some(u.clone()),
            _ => None,
        })
        .collect()
}

fn system_lines(effects: &[Effect]) -> Vec<String> {
    ui(effects)
        .into_iter()
        .filter_map(|u| match u {
            UiEvent::System { text } => Some(text),
            _ => None,
        })
        .collect()
}

/// Delivers events to `to` the way the relay pool would.
fn deliver(to: &mut Engine, events: &[Event], now: u64) -> Vec<Effect> {
    for e in events {
        let subscription = if e.kind == kind::GIFT_WRAP {
            dm_sub(GH)
        } else {
            geo_sub(GH)
        };
        to.on_pool_event(
            PoolEvent::Event {
                subscription,
                relay: "wss://r".into(),
                event: e.clone(),
            },
            now,
        );
    }
    settle(to)
}

fn joined(nick: &str) -> Engine {
    let mut e = user(nick);
    assert!(e.join_channel(GH, true, T0));
    let effects = settle(&mut e);
    e.on_pool_event(
        PoolEvent::Eose {
            subscription: geo_sub(GH),
        },
        T0,
    );
    let subs: Vec<_> = effects
        .iter()
        .filter_map(|x| match x {
            Effect::Subscribe { id, filter, relays } => {
                Some((id.clone(), filter.clone(), relays.len()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].0, geo_sub(GH));
    assert_eq!(subs[0].1.kinds, Some(vec![20000, 20001]));
    assert_eq!(subs[0].1.since, Some((T0 / 1000) as i64 - 3600));
    assert_eq!(subs[0].2, 5);
    assert_eq!(subs[1].0, dm_sub(GH));
    assert_eq!(subs[1].1.kinds, Some(vec![1059]));
    e
}

#[test]
fn public_chat_dm_and_receipts() {
    let mut alice = joined("alice");
    let mut bob = joined("bob");

    alice.submit("hello geohash", T0 + 1000);
    let out = settle(&mut alice);
    let sent = published(&out);
    assert_eq!(sent.len(), 1);
    let msg = &sent[0];
    assert_eq!(msg.kind, kind::EPHEMERAL);
    assert_eq!(msg.tag_value("g"), Some(GH));
    assert_eq!(msg.tag_value("n"), Some("alice"));
    assert_eq!(msg.tag_value("t"), Some("teleport"));
    assert!(pow::validated_difficulty(&msg.id, &msg.tags) >= 8);
    assert!(msg.verify());
    // Local echo, then the relay copy must not show twice.
    assert_eq!(
        ui(&out)
            .iter()
            .filter(|u| matches!(u, UiEvent::Public { .. }))
            .count(),
        1
    );
    let echo = deliver(&mut alice, &sent, T0 + 2000);
    assert!(
        ui(&echo)
            .iter()
            .all(|u| !matches!(u, UiEvent::Public { .. }))
    );

    let got = deliver(&mut bob, &sent, T0 + 2000);
    let public: Vec<_> = ui(&got)
        .into_iter()
        .filter_map(|u| match u {
            UiEvent::Public { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(public.len(), 1);
    let alice_name = public[0].display.clone();
    assert!(alice_name.starts_with("alice#") && alice_name.len() == 10);
    assert!(public[0].teleported);

    // bob → alice over a geohash DM
    bob.submit("/msg alice hi there", T0 + 3000);
    let out = settle(&mut bob);
    assert!(system_lines(&out).contains(&"started private chat with alice".to_string()));
    let wraps = published(&out);
    assert_eq!(wraps.len(), 1);
    assert_eq!(wraps[0].kind, kind::GIFT_WRAP);

    let got = deliver(&mut alice, &wraps, T0 + 4000);
    let dm = ui(&got).into_iter().find_map(|u| match u {
        UiEvent::Dm { message, .. } => Some(message),
        _ => None,
    });
    let dm = dm.expect("alice got the DM");
    assert_eq!(dm.content, "hi there");
    assert!(!dm.from_me);

    // Delivered ack goes back to bob.
    assert_eq!(alice.pending_acks(), 1);
    alice.next_ack(T0 + 4500);
    let ack = published(&settle(&mut alice));
    let status = ui(&deliver(&mut bob, &ack, T0 + 5000))
        .into_iter()
        .find_map(|u| match u {
            UiEvent::DmStatus { status, .. } => Some(status),
            _ => None,
        });
    assert_eq!(status, Some(DmStatus::Delivered));

    // Opening the chat sends a read receipt.
    let bob_pk = bob.resolve_nickname("bob", T0 + 5000);
    assert!(
        bob_pk.is_none(),
        "alice has not seen bob's nickname in public yet"
    );
    let peer = ui(&got)
        .into_iter()
        .find_map(|u| match u {
            UiEvent::Dm { peer, .. } => Some(peer),
            _ => None,
        })
        .unwrap();
    alice.open_dm(Some(&peer));
    settle(&mut alice);
    alice.next_ack(T0 + 6000);
    let read = published(&settle(&mut alice));
    let status = ui(&deliver(&mut bob, &read, T0 + 6500))
        .into_iter()
        .find_map(|u| match u {
            UiEvent::DmStatus { status, .. } => Some(status),
            _ => None,
        });
    assert_eq!(status, Some(DmStatus::Read));
}

#[test]
fn who_block_and_limits() {
    let mut alice = joined("alice");
    let mut bob = joined("bob");

    alice.submit("first", T0 + 1000);
    let first = published(&settle(&mut alice));
    deliver(&mut bob, &first, T0 + 1500);

    bob.submit("/who", T0 + 2000);
    let lines = system_lines(&settle(&mut bob));
    assert!(lines[0].starts_with("online: alice#"), "{lines:?}");

    // Too long for a private message.
    let alice_pk = bob.resolve_nickname("alice", T0 + 2000).unwrap();
    bob.open_dm(Some(&alice_pk));
    bob.submit(&"x".repeat(256), T0 + 2100);
    let lines = system_lines(&settle(&mut bob));
    assert!(lines[0].starts_with("message too long"), "{lines:?}");
    bob.open_dm(None);
    settle(&mut bob);

    // Block hides further messages.
    let name = bob.display_of(&alice_pk);
    bob.submit(&format!("/block {name}"), T0 + 2200);
    assert_eq!(
        system_lines(&settle(&mut bob)),
        vec![format!("blocked {name} in geohash chats")]
    );
    alice.submit("after block", T0 + 3000);
    let later = published(&settle(&mut alice));
    assert!(
        ui(&deliver(&mut bob, &later, T0 + 3500))
            .iter()
            .all(|u| !matches!(u, UiEvent::Public { .. }))
    );
    bob.submit(&format!("/unblock {name}"), T0 + 3600);
    assert_eq!(
        system_lines(&settle(&mut bob)),
        vec![format!("unblocked {name} in geohash chats")]
    );

    // Unmined bursts are rate limited per sender (5 burst).
    let spammer = bitchat_nostr::Keys::generate();
    let burst: Vec<Event> = (0..7)
        .map(|i| {
            Event::new(
                spammer.public_key_hex(),
                (T0 / 1000) as i64 + 4,
                kind::EPHEMERAL,
                vec![vec!["g".into(), GH.into()]],
                format!("spam {i}"),
            )
            .sign(&spammer)
        })
        .collect();
    let shown = ui(&deliver(&mut bob, &burst, T0 + 4000))
        .iter()
        .filter(|u| matches!(u, UiEvent::Public { .. }))
        .count();
    assert_eq!(shown, 5);

    bob.submit("/fav alice", T0 + 5000);
    assert_eq!(
        system_lines(&settle(&mut bob)),
        vec!["favorites are only for mesh peers in #mesh".to_string()]
    );
    bob.submit("/nope", T0 + 5000);
    assert_eq!(
        system_lines(&settle(&mut bob)),
        vec!["unknown command: /nope — type /help for commands".to_string()]
    );
}

#[test]
fn hug_goes_out_as_public_emote() {
    let mut alice = joined("alice");
    let mut bob = joined("bob");
    alice.submit("hi", T0 + 1000);
    deliver(&mut bob, &published(&settle(&mut alice)), T0 + 1500);

    bob.submit("/hug alice", T0 + 2000);
    let out = settle(&mut bob);
    let sent = published(&out);
    assert_eq!(sent[0].content, "* 🫂 bob hugs alice *");
    assert_eq!(system_lines(&out), vec!["🫂 bob hugs alice".to_string()]);
    let got = ui(&deliver(&mut alice, &sent, T0 + 2500));
    let emote = got.iter().find_map(|u| match u {
        UiEvent::Public { message } => Some(message.emote),
        _ => None,
    });
    assert_eq!(emote, Some(true));
}

#[test]
fn presence_only_with_location() {
    let mut e = user("carol");
    assert!(e.presence_events(T0).is_empty());
    e.set_location(Some((48.8566, 2.3522)), T0);
    settle(&mut e);
    let presence = e.presence_events(T0);
    assert_eq!(presence.len(), 3);
    for (event, relays) in &presence {
        assert_eq!(event.kind, kind::GEOHASH_PRESENCE);
        assert!(event.content.is_empty());
        assert_eq!(event.tags.len(), 1);
        assert!([2, 4, 5].contains(&event.tag_value("g").unwrap().len()));
        assert_eq!(relays.len(), 5);
        assert!(event.verify());
    }
    // A channel from the location list is not teleported.
    let city = e
        .regional_channels()
        .into_iter()
        .find(|(_, g)| g.len() == 5)
        .unwrap()
        .1;
    e.join_channel(&city, false, T0);
    let out = settle(&mut e);
    let teleported = ui(&out).into_iter().find_map(|u| match u {
        UiEvent::Channel { teleported, .. } => Some(teleported),
        _ => None,
    });
    assert_eq!(teleported, Some(false));
}

#[test]
fn drop_needs_location() {
    let mut e = joined("dave");
    e.submit("/drop hello place", T0);
    assert_eq!(
        system_lines(&settle(&mut e)),
        vec!["leaving a note needs location — turn it on in settings".to_string()]
    );
    e.set_location(Some((48.8566, 2.3522)), T0);
    settle(&mut e);
    e.submit("/drop hello place", T0);
    let out = settle(&mut e);
    let note = published(&out).into_iter().next().unwrap();
    assert_eq!(note.kind, kind::TEXT_NOTE);
    assert_eq!(note.tag_value("g").unwrap().len(), 8);
    assert_eq!(
        note.tag_value("expiration"),
        Some(((T0 / 1000) as i64 + 86_400).to_string().as_str())
    );
    assert!(system_lines(&out).contains(&"📍 note left here — it fades in 24h".to_string()));
}

#[test]
fn senders_show_up_in_people() {
    let mut alice = joined("alice");
    let mut bob = joined("bob");
    alice.submit("hello", T0 + 1000);
    deliver(&mut bob, &published(&settle(&mut alice)), T0 + 1500);
    bob.emit_people(((T0 + 2000) / 1000) as i64);
    let people = ui(&settle(&mut bob))
        .into_iter()
        .find_map(|u| match u {
            UiEvent::People {
                people, unknown, ..
            } => Some((people, unknown)),
            _ => None,
        })
        .unwrap();
    let names: Vec<_> = people.0.iter().map(|p| (p.display.clone(), p.me)).collect();
    assert!(!people.1, "{names:?}");
    assert!(
        names.iter().any(|(n, me)| n.starts_with("alice#") && !me),
        "{names:?}"
    );
}
