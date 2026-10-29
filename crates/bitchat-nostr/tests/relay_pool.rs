#![cfg(feature = "net")]
//! Relay pool against in-process mock relays (plain ws://127.0.0.1).

use bitchat_nostr::event::kind;
use bitchat_nostr::net::{Dialer, PoolEvent, RelayPool};
use bitchat_nostr::{Event, Filter, Keys};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Default)]
struct MockRelay {
    stored: Arc<Mutex<Vec<Event>>>,
    /// Connections accepted so far.
    connections: Arc<Mutex<u32>>,
    /// When set, the next connection is dropped right after the first REQ.
    drop_once: Arc<Mutex<bool>>,
    /// Extra raw frames sent after EOSE (e.g. forged events).
    extra: Arc<Mutex<Vec<String>>>,
}

impl MockRelay {
    async fn start(self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let relay = self.clone();
                tokio::spawn(async move { relay.serve(tcp).await });
            }
        });
        url
    }

    async fn serve(self, tcp: tokio::net::TcpStream) {
        *self.connections.lock().unwrap() += 1;
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            let Message::Text(text) = msg else { continue };
            let v: Value = serde_json::from_str(text.as_str()).unwrap();
            match v[0].as_str().unwrap() {
                "REQ" => {
                    if std::mem::take(&mut *self.drop_once.lock().unwrap()) {
                        return;
                    }
                    let sub = v[1].as_str().unwrap().to_string();
                    let filter = filter_from_json(&v[2]);
                    let events: Vec<Event> = self.stored.lock().unwrap().clone();
                    for e in events.iter().filter(|e| filter.matches(e)) {
                        ws.send(Message::text(json!(["EVENT", sub, e]).to_string()))
                            .await
                            .unwrap();
                    }
                    ws.send(Message::text(json!(["EOSE", sub]).to_string()))
                        .await
                        .unwrap();
                    let extra: Vec<String> = self.extra.lock().unwrap().clone();
                    for frame in extra {
                        ws.send(Message::text(frame.replace("$SUB", &sub)))
                            .await
                            .unwrap();
                    }
                }
                "EVENT" => {
                    let e: Event = serde_json::from_value(v[1].clone()).unwrap();
                    let id = e.id.clone();
                    self.stored.lock().unwrap().push(e);
                    ws.send(Message::text(json!(["OK", id, true, ""]).to_string()))
                        .await
                        .unwrap();
                }
                _ => {}
            }
        }
    }
}

fn filter_from_json(v: &Value) -> Filter {
    let mut f = Filter {
        kinds: v
            .get("kinds")
            .map(|k| serde_json::from_value(k.clone()).unwrap()),
        since: v.get("since").and_then(Value::as_i64),
        ..Default::default()
    };
    for (k, vals) in v.as_object().unwrap() {
        if let Some(tag) = k.strip_prefix('#') {
            f.tags.insert(
                tag.to_string(),
                serde_json::from_value(vals.clone()).unwrap(),
            );
        }
    }
    f
}

fn geo_event(keys: &Keys, content: &str) -> Event {
    Event::new(
        keys.public_key_hex(),
        1_790_000_000,
        kind::EPHEMERAL,
        vec![vec!["g".into(), "u4pruy".into()]],
        content.into(),
    )
    .sign(keys)
}

async fn next(
    rx: &mut mpsc::UnboundedReceiver<PoolEvent>,
    want: impl Fn(&PoolEvent) -> bool,
) -> PoolEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let e = rx.recv().await.expect("pool alive");
            if want(&e) {
                return e;
            }
        }
    })
    .await
    .expect("expected pool event in time")
}

#[tokio::test]
async fn stored_events_dedup_eose_and_forgery() {
    let keys = Keys::generate();
    let shared = geo_event(&keys, "seen on both relays");

    let a = MockRelay::default();
    let b = MockRelay::default();
    a.stored.lock().unwrap().push(shared.clone());
    b.stored.lock().unwrap().push(shared.clone());
    let mut forged = geo_event(&keys, "forged");
    forged.content = "tampered".into();
    b.extra
        .lock()
        .unwrap()
        .push(json!(["EVENT", "$SUB", forged]).to_string());
    let (ua, ub) = (a.start().await, b.start().await);

    let (pool, mut rx) = RelayPool::spawn(Dialer::Direct);
    pool.subscribe(
        "geo-u4pruy",
        Filter::geohash_ephemeral("u4pruy", None, 200),
        &[ua, ub],
    );

    let first = next(&mut rx, |e| matches!(e, PoolEvent::Event { .. })).await;
    let PoolEvent::Event {
        event,
        subscription,
        ..
    } = first
    else {
        unreachable!()
    };
    assert_eq!(subscription, "geo-u4pruy");
    assert_eq!(event, shared);

    next(
        &mut rx,
        |e| matches!(e, PoolEvent::Eose { subscription } if subscription == "geo-u4pruy"),
    )
    .await;

    // Neither the duplicate nor the forged event may arrive.
    let extra = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            if let Some(PoolEvent::Event { .. }) = rx.recv().await {
                return true;
            }
        }
    })
    .await;
    assert!(extra.is_err(), "unexpected extra event");
}

#[tokio::test]
async fn publish_reaches_live_subscription_and_gets_ok() {
    let relay = MockRelay::default();
    let url = relay.clone().start().await;
    let (pool, mut rx) = RelayPool::spawn(Dialer::Direct);

    let keys = Keys::generate();
    let event = geo_event(&keys, "hello relay");
    pool.publish(event.clone(), std::slice::from_ref(&url));
    let ok = next(&mut rx, |e| matches!(e, PoolEvent::Ok { .. })).await;
    assert_eq!(
        ok,
        PoolEvent::Ok {
            relay: url.clone(),
            event_id: event.id.clone(),
            accepted: true,
            message: String::new()
        }
    );
    assert_eq!(relay.stored.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn resubscribes_after_reconnect() {
    let keys = Keys::generate();
    let relay = MockRelay::default();
    relay
        .stored
        .lock()
        .unwrap()
        .push(geo_event(&keys, "after reconnect"));
    *relay.drop_once.lock().unwrap() = true;
    let url = relay.clone().start().await;

    let (pool, mut rx) = RelayPool::spawn(Dialer::Direct);
    pool.subscribe(
        "geo-u4pruy",
        Filter::geohash_ephemeral("u4pruy", None, 200),
        &[url],
    );
    let got = next(&mut rx, |e| matches!(e, PoolEvent::Event { .. })).await;
    let PoolEvent::Event { event, .. } = got else {
        unreachable!()
    };
    assert_eq!(event.content, "after reconnect");
    assert!(*relay.connections.lock().unwrap() >= 2);
}

/// A relay stuck waiting for its dialer (Tor bootstrapping) must still
/// switch when the dialer changes, keeping its subscriptions.
#[tokio::test]
async fn switching_dialer_unsticks_waiting_relays() {
    let keys = Keys::generate();
    let relay = MockRelay::default();
    relay
        .stored
        .lock()
        .unwrap()
        .push(geo_event(&keys, "after switch"));
    let url = relay.clone().start().await;

    let (pool, mut rx) = RelayPool::spawn(Dialer::Offline);
    pool.subscribe(
        "geo-u4pruy",
        Filter::geohash_ephemeral("u4pruy", None, 200),
        &[url],
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(*relay.connections.lock().unwrap(), 0);

    pool.set_dialer(Dialer::Direct);
    let got = next(&mut rx, |e| matches!(e, PoolEvent::Event { .. })).await;
    let PoolEvent::Event { event, .. } = got else {
        unreachable!()
    };
    assert_eq!(event.content, "after switch");
}
