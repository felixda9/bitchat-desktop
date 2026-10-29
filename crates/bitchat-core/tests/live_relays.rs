//! Two app instances talking over real public relays (Tor off).
//! Run with `cargo test -p bitchat-core --test live_relays -- --ignored --nocapture`.

use bitchat_core::model::{DmStatus, UiEvent};
use bitchat_core::runtime::{App, Command};
use bitchat_core::state::State;
use bitchat_core::storage::Storage;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn instance(name: &str, nick: &str) -> (App, mpsc::UnboundedReceiver<UiEvent>) {
    let dir = std::env::temp_dir().join(format!("bitchat-live-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let storage = Storage::with_fixed_key(dir, [name.len() as u8; 32]).unwrap();
    let mut state = State::fresh();
    state.nickname = nick.into();
    state.tor_enabled = false;
    storage.save(&state).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::start_with_storage(
        storage,
        Arc::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    (app, rx)
}

async fn wait_for<T>(
    rx: &mut mpsc::UnboundedReceiver<UiEvent>,
    secs: u64,
    mut f: impl FnMut(&UiEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            let e = rx.recv().await.expect("app running");
            if let Some(v) = f(&e) {
                return v;
            }
        }
    })
    .await
    .expect("expected event in time")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "uses public relays"]
async fn two_clients_chat_over_public_relays() {
    // An empty patch of ocean so nobody real sees the test traffic.
    let geohash = bitchat_proto::geohash::encode(-0.5, -25.5, 8);
    let (alice, mut arx) = instance("alice", "alice");
    let (bob, mut brx) = instance("bobby", "bob");
    alice.send(Command::Join {
        geohash: geohash.clone(),
        manual: true,
    });
    bob.send(Command::Join {
        geohash: geohash.clone(),
        manual: true,
    });
    tokio::time::sleep(Duration::from_secs(8)).await;

    let text = format!("live test {}", bitchat_core::runtime::now_ms());
    alice.send(Command::Submit { text: text.clone() });
    let alice_pk = wait_for(&mut brx, 60, |e| match e {
        UiEvent::Public { message } if message.content == text => Some(message.pubkey.clone()),
        _ => None,
    })
    .await;
    println!("bob saw alice's public message");

    bob.send(Command::OpenDm {
        peer: Some(alice_pk.clone()),
    });
    bob.send(Command::Submit {
        text: "hi alice, this is bob".into(),
    });
    wait_for(&mut arx, 60, |e| match e {
        UiEvent::Dm { message, .. } if message.content == "hi alice, this is bob" => Some(()),
        _ => None,
    })
    .await;
    println!("alice got bob's DM");

    wait_for(&mut brx, 60, |e| match e {
        UiEvent::DmStatus {
            status: DmStatus::Delivered,
            ..
        } => Some(()),
        _ => None,
    })
    .await;
    println!("bob saw the delivered receipt");
}
