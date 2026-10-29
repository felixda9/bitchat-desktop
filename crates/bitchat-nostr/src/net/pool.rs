//! Relay pool: routes subscriptions and publishes to relays, deduplicates
//! verified events per subscription, and reports end of stored events.

use super::relay::{self, Command, RelayStatus, Report};
use super::transport::{self, Dialer};
use crate::Event;
use crate::filter::Filter;
use crate::relay_message::{self, RelayMessage};
use crate::relay_url;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;

/// Dedup set size and trim target (`nostrInboundEventDedupCap`).
pub const DEDUP_CAP: usize = 4096;
pub const DEDUP_TRIM_TO: usize = 3072;
/// End-of-stored-events fallback (`nostrSubscriptionEOSEFallbackSeconds`).
pub const EOSE_FALLBACK: Duration = Duration::from_secs(10);
/// Relays nobody uses are closed after this long.
pub const IDLE_RELAY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq)]
pub enum PoolEvent {
    Event {
        subscription: String,
        relay: String,
        event: Event,
    },
    /// Stored events finished on every relay of the subscription (or the fallback fired).
    Eose {
        subscription: String,
    },
    Ok {
        relay: String,
        event_id: String,
        accepted: bool,
        message: String,
    },
    Status {
        relay: String,
        status: RelayStatus,
    },
}

enum PoolCommand {
    Subscribe {
        id: String,
        filter: Filter,
        relays: Vec<String>,
    },
    Unsubscribe {
        id: String,
    },
    Publish {
        event: Event,
        relays: Vec<String>,
    },
    SetDialer(Dialer),
    CloseAll,
}

/// Cheap to clone; all clones talk to the same pool task.
#[derive(Clone)]
pub struct RelayPool {
    commands: mpsc::UnboundedSender<PoolCommand>,
}

impl RelayPool {
    /// Starts the pool on the current tokio runtime.
    pub fn spawn(dialer: Dialer) -> (RelayPool, mpsc::UnboundedReceiver<PoolEvent>) {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        tokio::spawn(run(dialer, transport::tls_connector(), command_rx, events));
        (RelayPool { commands }, event_rx)
    }

    /// Opens (or replaces) subscription `id` on `relays`. Unusable relay
    /// addresses are skipped.
    pub fn subscribe(&self, id: impl Into<String>, filter: Filter, relays: &[String]) {
        let _ = self.commands.send(PoolCommand::Subscribe {
            id: id.into(),
            filter,
            relays: normalize_all(relays),
        });
    }

    pub fn unsubscribe(&self, id: impl Into<String>) {
        let _ = self
            .commands
            .send(PoolCommand::Unsubscribe { id: id.into() });
    }

    /// Fire-and-forget publish; relays that are offline get it when they reconnect.
    pub fn publish(&self, event: Event, relays: &[String]) {
        let _ = self.commands.send(PoolCommand::Publish {
            event,
            relays: normalize_all(relays),
        });
    }

    /// Switches every relay to a new dialer (e.g. Tor on or off) and reconnects.
    pub fn set_dialer(&self, dialer: Dialer) {
        let _ = self.commands.send(PoolCommand::SetDialer(dialer));
    }

    /// Drops every subscription and connection (panic wipe, shutdown).
    pub fn close_all(&self) {
        let _ = self.commands.send(PoolCommand::CloseAll);
    }
}

fn normalize_all(relays: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in relays {
        if let Some(n) = relay_url::normalize(r, Some("wss"))
            && !out.contains(&n)
        {
            out.push(n);
        }
    }
    out
}

struct Subscription {
    relays: Vec<String>,
    eose: HashSet<String>,
    started: Instant,
    done: bool,
}

struct RelayEntry {
    commands: mpsc::UnboundedSender<Command>,
    last_used: Instant,
}

#[derive(Default)]
struct Dedup {
    seen: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
}

impl Dedup {
    /// True the first time a (subscription, event id) pair is seen.
    fn insert(&mut self, key: (String, String)) -> bool {
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > DEDUP_CAP {
            while self.order.len() > DEDUP_TRIM_TO {
                if let Some(old) = self.order.pop_front() {
                    self.seen.remove(&old);
                }
            }
        }
        true
    }
}

struct Pool {
    dialer: Dialer,
    tls: TlsConnector,
    relays: HashMap<String, RelayEntry>,
    subs: HashMap<String, Subscription>,
    dedup: Dedup,
    reports: mpsc::UnboundedSender<Report>,
    events: mpsc::UnboundedSender<PoolEvent>,
}

impl Pool {
    fn relay(&mut self, url: &str) -> &mut RelayEntry {
        if !self.relays.contains_key(url) {
            let commands = relay::spawn(
                url.to_string(),
                self.dialer.clone(),
                self.tls.clone(),
                self.reports.clone(),
            );
            self.relays.insert(
                url.to_string(),
                RelayEntry {
                    commands,
                    last_used: Instant::now(),
                },
            );
        }
        let entry = self.relays.get_mut(url).expect("inserted above");
        entry.last_used = Instant::now();
        entry
    }

    fn send(&mut self, url: &str, command: Command) {
        let _ = self.relay(url).commands.send(command);
    }

    fn unsubscribe(&mut self, id: &str) {
        if let Some(sub) = self.subs.remove(id) {
            for url in sub.relays {
                if let Some(entry) = self.relays.get(&url) {
                    let _ = entry
                        .commands
                        .send(Command::Unsubscribe { id: id.to_string() });
                }
            }
        }
    }

    fn handle_command(&mut self, command: PoolCommand) {
        match command {
            PoolCommand::Subscribe { id, filter, relays } => {
                self.unsubscribe(&id);
                let frame = relay_message::req(&id, &filter);
                for url in &relays {
                    self.send(
                        url,
                        Command::Subscribe {
                            id: id.clone(),
                            frame: frame.clone(),
                        },
                    );
                }
                self.subs.insert(
                    id,
                    Subscription {
                        relays,
                        eose: HashSet::new(),
                        started: Instant::now(),
                        done: false,
                    },
                );
            }
            PoolCommand::Unsubscribe { id } => self.unsubscribe(&id),
            PoolCommand::Publish { event, relays } => {
                let frame = relay_message::event(&event);
                for url in &relays {
                    self.send(
                        url,
                        Command::Publish {
                            frame: frame.clone(),
                        },
                    );
                }
            }
            PoolCommand::SetDialer(dialer) => {
                self.dialer = dialer.clone();
                for entry in self.relays.values() {
                    let _ = entry.commands.send(Command::SetDialer(dialer.clone()));
                }
            }
            PoolCommand::CloseAll => {
                for (_, entry) in self.relays.drain() {
                    let _ = entry.commands.send(Command::Shutdown);
                }
                self.subs.clear();
                self.dedup = Dedup::default();
            }
        }
    }

    fn handle_report(&mut self, report: Report) {
        match report {
            Report::Status { relay, status } => {
                let _ = self.events.send(PoolEvent::Status { relay, status });
            }
            Report::Message { relay, message } => match message {
                RelayMessage::Event {
                    subscription,
                    event,
                } => {
                    if self.subs.contains_key(&subscription)
                        && self.dedup.insert((subscription.clone(), event.id.clone()))
                    {
                        let _ = self.events.send(PoolEvent::Event {
                            subscription,
                            relay,
                            event,
                        });
                    }
                }
                RelayMessage::Eose { subscription } => {
                    if let Some(sub) = self.subs.get_mut(&subscription) {
                        sub.eose.insert(relay);
                        if !sub.done && sub.relays.iter().all(|r| sub.eose.contains(r)) {
                            sub.done = true;
                            let _ = self.events.send(PoolEvent::Eose { subscription });
                        }
                    }
                }
                RelayMessage::Ok {
                    event_id,
                    accepted,
                    message,
                } => {
                    let _ = self.events.send(PoolEvent::Ok {
                        relay,
                        event_id,
                        accepted,
                        message,
                    });
                }
                _ => {}
            },
        }
    }

    fn tick(&mut self) {
        for (id, sub) in self.subs.iter_mut() {
            if !sub.done && sub.started.elapsed() >= EOSE_FALLBACK {
                sub.done = true;
                let _ = self.events.send(PoolEvent::Eose {
                    subscription: id.clone(),
                });
            }
        }
        let in_use: HashSet<&String> = self.subs.values().flat_map(|s| s.relays.iter()).collect();
        let idle: Vec<String> = self
            .relays
            .iter()
            .filter(|(url, e)| !in_use.contains(url) && e.last_used.elapsed() >= IDLE_RELAY_TIMEOUT)
            .map(|(url, _)| url.clone())
            .collect();
        for url in idle {
            if let Some(entry) = self.relays.remove(&url) {
                let _ = entry.commands.send(Command::Shutdown);
            }
        }
    }
}

async fn run(
    dialer: Dialer,
    tls: TlsConnector,
    mut commands: mpsc::UnboundedReceiver<PoolCommand>,
    events: mpsc::UnboundedSender<PoolEvent>,
) {
    let (reports, mut report_rx) = mpsc::unbounded_channel();
    let mut pool = Pool {
        dialer,
        tls,
        relays: HashMap::new(),
        subs: HashMap::new(),
        dedup: Dedup::default(),
        reports,
        events,
    };
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                Some(c) => pool.handle_command(c),
                None => break,
            },
            Some(report) = report_rx.recv() => pool.handle_report(report),
            _ = ticker.tick() => pool.tick(),
        }
    }
    pool.handle_command(PoolCommand::CloseAll);
}
