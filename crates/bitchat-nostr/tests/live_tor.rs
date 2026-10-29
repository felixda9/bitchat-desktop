#![cfg(feature = "net")]
//! Live network checks. Run with `cargo test -p bitchat-nostr --features net -- --ignored`.

use bitchat_nostr::Filter;
use bitchat_nostr::event::kind;
use bitchat_nostr::net::{Dialer, PoolEvent, RelayPool, Tor, TorStatus};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs internet and Tor"]
async fn subscribe_through_tor() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let dir = std::env::temp_dir().join(format!("bitchat-tor-test-{}", std::process::id()));
    let tor = Tor::start(&dir.join("state"), &dir.join("cache")).unwrap();
    let mut status = tor.status();
    let ready = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let current = status.borrow_and_update().clone();
            eprintln!("tor: {current:?}");
            if current == TorStatus::Ready {
                return true;
            }
            if status.changed().await.is_err() {
                return false;
            }
        }
    })
    .await
    .expect("tor bootstrapped in time");
    assert!(ready, "tor status channel closed before ready");

    let (pool, mut rx) = RelayPool::spawn(Dialer::Tor(tor));
    let filter = Filter {
        kinds: Some(vec![kind::TEXT_NOTE]),
        limit: Some(1),
        ..Default::default()
    };
    pool.subscribe("live", filter, &["wss://nos.lol".to_string()]);
    let got = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match rx.recv().await.unwrap() {
                PoolEvent::Event { event, .. } => return Some(event),
                PoolEvent::Eose { .. } => return None,
                _ => {}
            }
        }
    })
    .await
    .expect("relay answered through tor");
    if let Some(event) = got {
        assert!(event.verify());
    }
    let _ = std::fs::remove_dir_all(dir);
}
