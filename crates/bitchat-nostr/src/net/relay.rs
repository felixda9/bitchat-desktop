//! One task per relay: keeps the socket up, replays subscriptions after a
//! reconnect, buffers publishes while offline, and verifies inbound events.

use super::transport::{self, ConnectError, Dialer};
use crate::relay_message::{self, RelayMessage};
use futures_util::{SinkExt, StreamExt};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;

/// Reconnect backoff (`TransportConfig.nostrRelay…`).
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
pub const BACKOFF_JITTER: f64 = 0.2;
pub const MAX_RECONNECT_ATTEMPTS: u32 = 10;
/// Pause after permanent failures or too many attempts.
pub const FAILURE_COOLDOWN: Duration = Duration::from_secs(600);
/// Publishes held while disconnected; the oldest are dropped first.
pub const PENDING_SEND_CAP: usize = 200;
const PING_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayStatus {
    Connecting,
    Connected,
    Disconnected,
    /// Gave up for now (cooldown before the next try).
    Failed,
}

pub(crate) enum Command {
    Subscribe { id: String, frame: String },
    Unsubscribe { id: String },
    Publish { frame: String },
    SetDialer(Dialer),
    Shutdown,
}

pub(crate) enum Report {
    Message {
        relay: String,
        message: RelayMessage,
    },
    Status {
        relay: String,
        status: RelayStatus,
    },
}

pub(crate) fn spawn(
    url: String,
    dialer: Dialer,
    tls: TlsConnector,
    reports: mpsc::UnboundedSender<Report>,
) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(url, dialer, tls, rx, reports));
    tx
}

#[derive(Default)]
struct State {
    /// Subscription id → REQ frame, replayed on every connect.
    subs: BTreeMap<String, String>,
    pending: VecDeque<String>,
}

impl State {
    fn queue(&mut self, frame: String) {
        if self.pending.len() >= PENDING_SEND_CAP {
            self.pending.pop_front();
        }
        self.pending.push_back(frame);
    }
}

enum End {
    Shutdown,
    Disconnected,
    Redial,
}

pub fn backoff(attempt: u32) -> Duration {
    let base = INITIAL_BACKOFF.as_secs_f64() * 2f64.powi(attempt.saturating_sub(1) as i32);
    let base = base.min(MAX_BACKOFF.as_secs_f64());
    let r = u32::from_le_bytes(crate::keys::random_bytes()) as f64 / u32::MAX as f64;
    Duration::from_secs_f64(base * (1.0 + BACKOFF_JITTER * (2.0 * r - 1.0)))
}

async fn run(
    url: String,
    mut dialer: Dialer,
    tls: TlsConnector,
    mut commands: mpsc::UnboundedReceiver<Command>,
    reports: mpsc::UnboundedSender<Report>,
) {
    let status = |s| {
        let _ = reports.send(Report::Status {
            relay: url.clone(),
            status: s,
        });
    };
    let mut state = State::default();
    let mut attempt = 0u32;

    'outer: loop {
        status(RelayStatus::Connecting);
        let delay = match transport::connect(&url, &dialer, &tls).await {
            Ok(socket) => {
                attempt = 0;
                status(RelayStatus::Connected);
                match session(
                    socket,
                    &url,
                    &mut state,
                    &mut commands,
                    &reports,
                    &mut dialer,
                )
                .await
                {
                    End::Shutdown => break 'outer,
                    End::Redial => continue 'outer,
                    End::Disconnected => {
                        status(RelayStatus::Disconnected);
                        attempt += 1;
                        backoff(attempt)
                    }
                }
            }
            Err(e) => {
                log::debug!("relay {url}: {e}");
                attempt += 1;
                if failed_for_now(&e, attempt) {
                    status(RelayStatus::Failed);
                    attempt = 0;
                    FAILURE_COOLDOWN
                } else {
                    status(RelayStatus::Disconnected);
                    backoff(attempt)
                }
            }
        };

        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                cmd = commands.recv() => match cmd {
                    None | Some(Command::Shutdown) => break 'outer,
                    Some(Command::SetDialer(d)) => {
                        dialer = d;
                        attempt = 0;
                        break;
                    }
                    Some(Command::Subscribe { id, frame }) => {
                        state.subs.insert(id, frame);
                    }
                    Some(Command::Unsubscribe { id }) => {
                        state.subs.remove(&id);
                    }
                    Some(Command::Publish { frame }) => state.queue(frame),
                },
            }
        }
    }
    status(RelayStatus::Disconnected);
}

fn failed_for_now(e: &ConnectError, attempt: u32) -> bool {
    e.is_permanent() || attempt >= MAX_RECONNECT_ATTEMPTS
}

async fn session(
    socket: transport::Socket,
    url: &str,
    state: &mut State,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    reports: &mpsc::UnboundedSender<Report>,
    dialer: &mut Dialer,
) -> End {
    let (mut sink, mut stream) = socket.split();

    for frame in state.subs.values() {
        if sink.send(Message::text(frame.clone())).await.is_err() {
            return End::Disconnected;
        }
    }
    while let Some(frame) = state.pending.pop_front() {
        if sink.send(Message::text(frame.clone())).await.is_err() {
            state.pending.push_front(frame);
            return End::Disconnected;
        }
    }

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        tokio::select! {
            cmd = commands.recv() => {
                let frame = match cmd {
                    None | Some(Command::Shutdown) => {
                        let _ = sink.close().await;
                        return End::Shutdown;
                    }
                    Some(Command::SetDialer(d)) => {
                        *dialer = d;
                        let _ = sink.close().await;
                        return End::Redial;
                    }
                    Some(Command::Subscribe { id, frame }) => {
                        state.subs.insert(id, frame.clone());
                        frame
                    }
                    Some(Command::Unsubscribe { id }) => {
                        if state.subs.remove(&id).is_none() {
                            continue;
                        }
                        relay_message::close(&id)
                    }
                    Some(Command::Publish { frame }) => {
                        if sink.send(Message::text(frame.clone())).await.is_err() {
                            state.queue(frame);
                            return End::Disconnected;
                        }
                        continue;
                    }
                };
                if sink.send(Message::text(frame)).await.is_err() {
                    return End::Disconnected;
                }
            }
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if let Some(message) = accept(text.as_str(), state, url) {
                        let _ = reports.send(Report::Message { relay: url.to_string(), message });
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return End::Disconnected,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Default::default())).await.is_err() {
                    return End::Disconnected;
                }
            }
        }
    }
}

/// Parses a relay frame. Events must belong to an active subscription, stay
/// within tag limits and carry a valid id and signature.
fn accept(text: &str, state: &State, url: &str) -> Option<RelayMessage> {
    let message = RelayMessage::parse(text)?;
    match &message {
        RelayMessage::Event {
            subscription,
            event,
        } => {
            if !state.subs.contains_key(subscription)
                || !event.within_tag_limits()
                || !event.verify()
            {
                return None;
            }
        }
        RelayMessage::Notice { message } => log::debug!("relay {url} notice: {message}"),
        RelayMessage::Closed {
            subscription,
            message,
        } => {
            log::debug!("relay {url} closed {subscription}: {message}")
        }
        RelayMessage::Auth { .. } => log::debug!("relay {url} asked for AUTH; not supported"),
        _ => {}
    }
    Some(message)
}
