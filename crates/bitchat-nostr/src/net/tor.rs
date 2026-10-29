//! In-process Tor (Arti), the same engine the iOS app uses.
//!
//! Relay sockets wait for Tor instead of falling back to a direct
//! connection, so nothing leaks while Tor is starting or blocked.

use arti_client::config::TorClientConfigBuilder;
use arti_client::{DataStream, TorClient};
use futures_util::StreamExt;
use futures_util::task::SpawnExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tor_rtcompat::PreferredRuntime;

/// After this long without finishing, the UI reports a stall (iOS: 75 s).
pub const BOOTSTRAP_STALL_AFTER: Duration = Duration::from_secs(75);

#[derive(Clone, Debug, PartialEq)]
pub enum TorStatus {
    Starting {
        progress: f32,
        summary: String,
    },
    Ready,
    /// Still trying, but taking longer than it should (network may block Tor).
    Stalled {
        progress: f32,
        summary: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TorError {
    #[error("tor config: {0}")]
    Config(String),
    #[error("tor: {0}")]
    Client(#[from] arti_client::Error),
}

#[derive(Clone)]
pub struct Tor {
    client: Arc<TorClient<PreferredRuntime>>,
    status: watch::Receiver<TorStatus>,
}

impl Tor {
    /// Creates the client and starts bootstrapping in the background.
    /// Tor runs on its own runtime and threads, so a busy or stuck Tor can
    /// never starve the chat engine. Call once per process; must be called
    /// inside a tokio runtime (for the status watcher).
    pub fn start(state_dir: &Path, cache_dir: &Path) -> Result<Tor, TorError> {
        let config = TorClientConfigBuilder::from_directories(state_dir, cache_dir)
            .build()
            .map_err(|e| TorError::Config(e.to_string()))?;
        let runtime = PreferredRuntime::create().map_err(|e| TorError::Config(e.to_string()))?;
        let client = TorClient::with_runtime(runtime.clone())
            .config(config)
            .create_unbootstrapped()?;

        let (tx, rx) = watch::channel(TorStatus::Starting {
            progress: 0.0,
            summary: "starting tor".into(),
        });

        let boot = client.clone();
        let boot_tx = tx.clone();
        runtime
            .spawn(async move {
                if let Err(e) = boot.bootstrap().await {
                    log::warn!("tor bootstrap: {e}");
                    let _ = boot_tx.send(TorStatus::Stalled {
                        progress: 0.0,
                        summary: e.to_string(),
                    });
                }
            })
            .map_err(|e| TorError::Config(e.to_string()))?;

        let events_client = client.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let mut events = events_client.bootstrap_events();
            loop {
                let next = tokio::time::timeout(Duration::from_secs(5), events.next()).await;
                let status = events_client.bootstrap_status();
                let progress = status.as_frac();
                let summary = status.to_string();
                let new = if status.ready_for_traffic() {
                    TorStatus::Ready
                } else if started.elapsed() >= BOOTSTRAP_STALL_AFTER {
                    TorStatus::Stalled { progress, summary }
                } else {
                    TorStatus::Starting { progress, summary }
                };
                if *tx.borrow() != new {
                    let _ = tx.send(new);
                }
                if matches!(next, Ok(None)) {
                    break;
                }
            }
        });

        Ok(Tor { client, status: rx })
    }

    pub fn status(&self) -> watch::Receiver<TorStatus> {
        self.status.clone()
    }

    /// Waits until Tor can carry traffic. Never times out on its own.
    pub async fn wait_ready(&self) {
        let mut rx = self.status.clone();
        let _ = rx.wait_for(|s| *s == TorStatus::Ready).await;
    }

    /// Opens a stream to `host:port` on its own circuit.
    pub async fn connect(&self, host: &str, port: u16) -> Result<DataStream, arti_client::Error> {
        self.client.isolated_client().connect((host, port)).await
    }
}
