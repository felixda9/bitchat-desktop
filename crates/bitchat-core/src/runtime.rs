//! Runs the engine: relay pool, Tor, storage, proof-of-work mining, receipt
//! pacing, presence heartbeats and the daily relay-list refresh.

use crate::engine::{Effect, Engine, MineRequest};
use crate::model::UiEvent;
use crate::state::State;
use crate::storage::{Storage, StorageError};
use bitchat_nostr::event::kind;
use bitchat_nostr::net::{Dialer, PoolEvent, RelayPool, Tor, TorStatus, http};
use bitchat_nostr::pow;
use bitchat_nostr::relay_directory::{self, Directory};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

/// Receipts go out one at a time (`nostrReadAckInterval`).
pub const ACK_INTERVAL: Duration = Duration::from_millis(350);
pub const PEOPLE_REFRESH: Duration = Duration::from_secs(30);
pub const DIRECTORY_REFRESH: Duration = Duration::from_secs(24 * 3600);
const DIRECTORY_CACHE: &str = "relays.csv";

#[derive(Clone, Debug)]
pub enum Command {
    Join { geohash: String, manual: bool },
    Leave,
    Submit { text: String },
    OpenDm { peer: Option<String> },
    SetNickname { nickname: String },
    SetTor { enabled: bool },
    AddRelay { url: String },
    RemoveRelay { url: String },
    SetLocation { fix: Option<(f64, f64)> },
    Wipe,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

pub type Sink = Arc<dyn Fn(UiEvent) + Send + Sync>;

#[derive(Clone)]
pub struct App {
    commands: mpsc::UnboundedSender<Command>,
}

impl App {
    /// Loads (or creates) the identity in `dir` and starts everything on the
    /// current tokio runtime. UI events go to `sink`.
    pub fn start(dir: PathBuf, sink: Sink) -> Result<App, StartError> {
        App::start_with_storage(Storage::open(dir)?, sink)
    }

    /// As [`App::start`] with a prepared storage (tests use a fixed key).
    pub fn start_with_storage(storage: Storage, sink: Sink) -> Result<App, StartError> {
        let state = match storage.load() {
            Ok(Some(s)) => s,
            Ok(None) => fresh_state(&storage)?,
            Err(StorageError::Corrupt) => {
                log::warn!("saved state unreadable; starting with a new identity");
                fresh_state(&storage)?
            }
            Err(e) => return Err(e.into()),
        };
        let (commands, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(storage, state, rx, sink));
        Ok(App { commands })
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

fn fresh_state(storage: &Storage) -> Result<State, StorageError> {
    let state = State::fresh();
    storage.save(&state)?;
    Ok(state)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("OS random number generator failed");
    u64::from_le_bytes(b)
}

/// Uniform duration in `[lo, hi]` seconds.
fn jitter(lo: u64, hi: u64) -> Duration {
    Duration::from_millis(lo * 1000 + random_u64() % ((hi - lo) * 1000 + 1))
}

struct Runner {
    storage: Storage,
    engine: Engine,
    pool: RelayPool,
    tor: Option<Tor>,
    tor_status: Option<watch::Receiver<TorStatus>>,
    sink: Sink,
    mine_queue: VecDeque<MineRequest>,
    mining: bool,
    mined_tx: mpsc::UnboundedSender<(u64, Vec<String>)>,
    dir_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl Runner {
    fn dialer(&self) -> Dialer {
        match &self.tor {
            Some(t) if self.engine.state.tor_enabled => Dialer::Tor(t.clone()),
            _ => Dialer::Direct,
        }
    }

    fn start_tor(&mut self) {
        if self.tor.is_some() {
            return;
        }
        let (state_dir, cache_dir) = self.storage.tor_dirs();
        match Tor::start(&state_dir, &cache_dir) {
            Ok(t) => {
                self.tor_status = Some(t.status());
                self.tor = Some(t);
            }
            Err(e) => {
                log::error!("tor failed to start: {e}");
                (self.sink)(UiEvent::Tor {
                    state: "error".into(),
                    progress: 0.0,
                    summary: e.to_string(),
                });
            }
        }
    }

    fn emit_tor(&self) {
        let event = match (&self.tor_status, self.engine.state.tor_enabled) {
            (_, false) => UiEvent::Tor {
                state: "off".into(),
                progress: 0.0,
                summary: "tor is off".into(),
            },
            (None, true) => UiEvent::Tor {
                state: "error".into(),
                progress: 0.0,
                summary: "tor did not start".into(),
            },
            (Some(rx), true) => match rx.borrow().clone() {
                TorStatus::Ready => UiEvent::Tor {
                    state: "ready".into(),
                    progress: 1.0,
                    summary: "tor connected".into(),
                },
                TorStatus::Starting { progress, summary } => UiEvent::Tor {
                    state: "starting".into(),
                    progress,
                    summary,
                },
                TorStatus::Stalled { progress, summary } => UiEvent::Tor {
                    state: "stalled".into(),
                    progress,
                    summary,
                },
            },
        };
        (self.sink)(event);
    }

    fn apply(&mut self) {
        for effect in self.engine.take_effects() {
            match effect {
                Effect::Subscribe { id, filter, relays } => {
                    self.pool.subscribe(id, filter, &relays)
                }
                Effect::Unsubscribe { id } => self.pool.unsubscribe(id),
                Effect::Publish { event, relays } => self.pool.publish(event, &relays),
                Effect::Mine(req) => self.mine_queue.push_back(req),
                Effect::Ui(event) => (self.sink)(event),
                Effect::Save => {
                    if let Err(e) = self.storage.save(&self.engine.state) {
                        log::error!("saving state failed: {e}");
                    }
                }
            }
        }
        self.mine_next();
    }

    /// One mining job at a time keeps sends in order (as on iOS).
    fn mine_next(&mut self) {
        if self.mining {
            return;
        }
        let Some(req) = self.mine_queue.pop_front() else {
            return;
        };
        self.mining = true;
        let tx = self.mined_tx.clone();
        tokio::task::spawn_blocking(move || {
            let tag = pow::mine_nonce_tag(
                &req.pubkey,
                req.created_at,
                kind::EPHEMERAL,
                &req.tags,
                &req.content,
                pow::TARGET_BITS,
            );
            let _ = tx.send((req.id, tag));
        });
    }

    fn command(&mut self, command: Command) {
        let now = now_ms();
        match command {
            Command::Join { geohash, manual } => {
                self.engine.join_channel(&geohash, manual, now);
            }
            Command::Leave => self.engine.leave_channel(),
            Command::Submit { text } => self.engine.submit(&text, now),
            Command::OpenDm { peer } => self.engine.open_dm(peer.as_deref()),
            Command::SetNickname { nickname } => self.engine.set_nickname(&nickname),
            Command::SetTor { enabled } => {
                self.engine.set_tor_enabled(enabled);
                if enabled {
                    self.start_tor();
                }
                self.pool.set_dialer(self.dialer());
                self.emit_tor();
            }
            Command::AddRelay { url } => self.engine.add_relay(&url, now),
            Command::RemoveRelay { url } => self.engine.remove_relay(&url, now),
            Command::SetLocation { fix } => self.engine.set_location(fix, now),
            Command::Wipe => self.wipe(),
        }
        self.apply();
    }

    /// Panic wipe: drop every connection and secret, start over as a new person.
    fn wipe(&mut self) {
        self.pool.close_all();
        self.mine_queue.clear();
        let _ = self.engine.take_effects();
        if let Err(e) = self.storage.wipe() {
            log::error!("wipe incomplete: {e}");
        }
        let state = State::fresh();
        if let Err(e) = self.storage.save(&state) {
            log::error!("saving fresh state failed: {e}");
        }
        let tor_enabled = state.tor_enabled;
        self.engine = Engine::new(state, self.storage.key_in_keychain());
        if tor_enabled {
            self.start_tor();
        }
        self.pool.set_dialer(self.dialer());
        (self.sink)(UiEvent::Wiped);
        self.engine.announce();
        self.emit_tor();
    }

    fn refresh_directory(&self) {
        let dialer = self.dialer();
        let tx = self.dir_tx.clone();
        tokio::spawn(async move {
            match http::get(
                relay_directory::REMOTE_URL,
                &dialer,
                relay_directory::MAX_BYTES,
            )
            .await
            {
                Ok(body) => {
                    let _ = tx.send(body);
                }
                Err(e) => log::info!("relay list refresh failed: {e}"),
            }
        });
    }

    fn accept_directory(&mut self, csv: Vec<u8>) {
        match self.engine.directory().accept_update(&csv) {
            Some(dir) => {
                self.engine.set_directory(dir);
                let _ = std::fs::write(self.storage.dir().join(DIRECTORY_CACHE), &csv);
            }
            None => log::warn!("downloaded relay list rejected"),
        }
    }

    fn publish_presence(&self) {
        let events = self.engine.presence_events(now_ms());
        if events.is_empty() {
            return;
        }
        let pool = self.pool.clone();
        tokio::spawn(async move {
            for (i, (event, relays)) in events.into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(jitter(2, 5)).await;
                }
                pool.publish(event, &relays);
            }
        });
    }
}

async fn run(
    storage: Storage,
    state: State,
    mut commands: mpsc::UnboundedReceiver<Command>,
    sink: Sink,
) {
    let (mined_tx, mut mined_rx) = mpsc::unbounded_channel();
    let (dir_tx, mut dir_rx) = mpsc::unbounded_channel();
    let key_in_keychain = storage.key_in_keychain();
    let mut engine = Engine::new(state, key_in_keychain);

    // A cached copy of the relay list must still extend the bundled one.
    if let Ok(csv) = std::fs::read(storage.dir().join(DIRECTORY_CACHE))
        && let Some(dir) = Directory::bundled().accept_update(&csv)
    {
        engine.set_directory(dir);
    }

    let (pool, mut pool_rx): (RelayPool, mpsc::UnboundedReceiver<PoolEvent>) =
        RelayPool::spawn(Dialer::Direct);
    let mut runner = Runner {
        storage,
        engine,
        pool,
        tor: None,
        tor_status: None,
        sink,
        mine_queue: VecDeque::new(),
        mining: false,
        mined_tx,
        dir_tx,
    };
    if runner.engine.state.tor_enabled {
        runner.start_tor();
    }
    runner.pool.set_dialer(runner.dialer());
    runner.engine.announce();
    runner.emit_tor();
    runner.apply();
    runner.refresh_directory();

    let mut acks = tokio::time::interval(ACK_INTERVAL);
    let mut people = tokio::time::interval(PEOPLE_REFRESH);
    let mut directory = tokio::time::interval(DIRECTORY_REFRESH);
    directory.tick().await;
    let presence = tokio::time::sleep(jitter(40, 80));
    tokio::pin!(presence);

    loop {
        let mut tor_rx = runner.tor_status.clone();
        let tor_changed = async {
            match tor_rx.as_mut() {
                Some(rx) => rx.changed().await.is_ok(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            cmd = commands.recv() => match cmd {
                Some(c) => runner.command(c),
                None => break,
            },
            Some(event) = pool_rx.recv() => {
                runner.engine.on_pool_event(event, now_ms());
                runner.apply();
            }
            Some((id, tag)) = mined_rx.recv() => {
                runner.mining = false;
                runner.engine.finish_public(id, tag);
                runner.apply();
            }
            Some(csv) = dir_rx.recv() => runner.accept_directory(csv),
            _ = acks.tick() => {
                if runner.engine.pending_acks() > 0 {
                    runner.engine.next_ack(now_ms());
                    runner.apply();
                }
            }
            _ = people.tick() => {
                runner.engine.emit_people((now_ms() / 1000) as i64);
                runner.apply();
            }
            _ = directory.tick() => runner.refresh_directory(),
            _ = &mut presence => {
                runner.publish_presence();
                presence.as_mut().reset(tokio::time::Instant::now() + jitter(40, 80));
            }
            changed = tor_changed => {
                if changed {
                    runner.emit_tor();
                } else {
                    runner.tor_status = None;
                }
            }
        }
    }
    runner.pool.close_all();
}
