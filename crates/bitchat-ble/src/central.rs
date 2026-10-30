//! BLE central: finds bitchat peripherals (phones), connects, subscribes to
//! the bitchat characteristic and turns each connection into a mesh link.
//! Mirrors iOS `BLEService+LinkLayerCentralRole`: whole frames are written
//! without response; notifications are reassembled into frames.

use crate::assembler::StreamAssembler;
use crate::{CHARACTERISTIC_UUID, SERVICE_UUID};
use btleplug::api::{
    Central as _, CentralEvent, CentralState, Characteristic, Manager as _, Peripheral as _,
    ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub type LinkId = u64;

/// Never reused, even across restarts, and shared with the peripheral.
static NEXT_LINK: AtomicU64 = AtomicU64::new(1);

pub fn next_link_id() -> LinkId {
    NEXT_LINK.fetch_add(1, Ordering::Relaxed)
}

/// Addresses of phones connected to our peripheral right now. Connecting
/// out to the same address while it is connected in confuses the stack
/// (seen with BlueZ and Android): both links drop.
static INCOMING: Mutex<Vec<[u8; 6]>> = Mutex::new(Vec::new());

pub(crate) fn note_incoming(address: [u8; 6], connected: bool) {
    let mut list = INCOMING.lock().expect("incoming");
    list.retain(|a| *a != address);
    if connected {
        list.push(address);
    }
}

fn is_incoming(address: [u8; 6]) -> bool {
    INCOMING.lock().expect("incoming").contains(&address)
}

/// iOS `bleMaxCentralLinks`.
pub const MAX_LINKS: usize = 6;
/// Ignore advertisers weaker than this (iOS `bleDynamicRSSIThresholdDefault`).
pub const RSSI_THRESHOLD: i16 = -90;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait this long after a failed attempt before trying the same device again.
const RETRY_AFTER: Duration = Duration::from_secs(10);
const DEFAULT_ATT_MTU: u16 = 23;
/// A phone restarts its advertising with a fresh address when we connect.
/// An address first seen this soon after a connection attempt is taken to be
/// that phone again and left alone while the link lives.
const READVERTISE_WINDOW: Duration = Duration::from_secs(10);
/// ...but checked again after this long, in case it was someone else.
const PRESUME_FOR: Duration = Duration::from_secs(300);
/// After a connection turns out to be a phone we already had, wait before
/// trying unknown addresses again: 30 s, doubling up to 5 minutes.
const DUPLICATE_BACKOFF_MIN: Duration = Duration::from_secs(30);
const DUPLICATE_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// The backoff starts over after this long without a duplicate.
const DUPLICATE_BACKOFF_RESET: Duration = Duration::from_secs(600);
/// Used when the stack never reports a real MTU; larger writes go with response.
const FALLBACK_MAX_WRITE: usize = 182;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BleStatus {
    /// No adapter, or the platform refused.
    Unavailable(String),
    Off,
    Scanning {
        links: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BleEvent {
    Status(BleStatus),
    LinkUp {
        link: LinkId,
        /// Largest frame to write in one go.
        max_write: usize,
        name: String,
        /// The phone connected to us (we are the peripheral).
        incoming: bool,
    },
    LinkDown {
        link: LinkId,
    },
    Frame {
        link: LinkId,
        frame: Vec<u8>,
    },
}

type Writers = Arc<Mutex<HashMap<LinkId, mpsc::UnboundedSender<Vec<u8>>>>>;
type Requests = mpsc::UnboundedSender<Request>;

/// Handle to the running central. Dropping it stops scanning and connections.
pub struct Central {
    writers: Writers,
    requests: Requests,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Central {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Central {
    /// Starts scanning on the first adapter; events go to `events`.
    pub fn start(events: mpsc::UnboundedSender<BleEvent>) -> Central {
        let writers: Writers = Arc::default();
        let (requests, requests_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(events, writers.clone(), requests_rx));
        Central {
            writers,
            requests,
            task,
        }
    }

    /// Queues a frame for a link; frames on one link go out in order.
    pub fn write(&self, link: LinkId, frame: Vec<u8>) {
        if frame.is_empty() {
            return;
        }
        if let Some(tx) = self.writers.lock().expect("writers").get(&link) {
            let _ = tx.send(frame);
        }
    }

    /// Disconnects a link the mesh found redundant (after its queued writes).
    pub fn close(&self, link: LinkId) {
        if let Some(tx) = self.writers.lock().expect("writers").get(&link) {
            let _ = tx.send(Vec::new());
        }
        let _ = self.requests.send(Request::Duplicate(link));
    }
}

enum Slot {
    Connecting(LinkId),
    Connected(LinkId),
}

struct State {
    adapter: Adapter,
    events: mpsc::UnboundedSender<BleEvent>,
    writers: Writers,
    slots: HashMap<PeripheralId, Slot>,
    retry_at: HashMap<PeripheralId, Instant>,
    /// Recent connection attempts: when, and the link they became.
    attempts: Vec<(Instant, LinkId)>,
    /// Addresses taken to be a connected phone's new advertisement.
    presumed: HashMap<PeripheralId, (LinkId, Instant)>,
    seen: HashSet<PeripheralId>,
    /// No connections to unknown addresses before this (duplicate backoff).
    explore_after: Option<Instant>,
    backoff: Duration,
    last_duplicate: Option<Instant>,
}

enum Request {
    /// The mesh found this link redundant (same peer on a newer one).
    Duplicate(LinkId),
}

enum Done {
    Connected(PeripheralId, LinkId),
    Failed(PeripheralId, String),
    Closed(PeripheralId, LinkId),
}

async fn run(
    events: mpsc::UnboundedSender<BleEvent>,
    writers: Writers,
    mut requests: mpsc::UnboundedReceiver<Request>,
) {
    let status = |s| {
        let _ = events.send(BleEvent::Status(s));
    };
    let manager = match Manager::new().await {
        Ok(m) => m,
        Err(e) => return status(BleStatus::Unavailable(e.to_string())),
    };
    let adapter = match manager.adapters().await.map(|a| a.into_iter().next()) {
        Ok(Some(a)) => a,
        Ok(None) => return status(BleStatus::Unavailable("no Bluetooth adapter".into())),
        Err(e) => return status(BleStatus::Unavailable(e.to_string())),
    };
    let mut central_events = match adapter.events().await {
        Ok(s) => s,
        Err(e) => return status(BleStatus::Unavailable(e.to_string())),
    };
    let powered = matches!(
        adapter.adapter_state().await,
        Ok(CentralState::PoweredOn | CentralState::Unknown)
    );
    if let Ok(info) = adapter.adapter_info().await {
        log::info!("ble: adapter {info}");
    }

    let mut st = State {
        adapter,
        events: events.clone(),
        writers,
        slots: HashMap::new(),
        retry_at: HashMap::new(),
        attempts: Vec::new(),
        presumed: HashMap::new(),
        seen: HashSet::new(),
        explore_after: None,
        backoff: DUPLICATE_BACKOFF_MIN,
        last_duplicate: None,
    };
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Done>();
    let mut scanning = false;
    if powered {
        scanning = st.start_scan().await;
    } else {
        status(BleStatus::Off);
    }
    let mut sweep = tokio::time::interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            event = central_events.next() => {
                let Some(event) = event else { break };
                match event {
                    CentralEvent::StateUpdate(CentralState::PoweredOff) => {
                        scanning = false;
                        st.drop_all();
                        status(BleStatus::Off);
                    }
                    CentralEvent::StateUpdate(_) if !scanning => {
                        scanning = st.start_scan().await;
                    }
                    CentralEvent::DeviceDiscovered(id)
                    | CentralEvent::DeviceUpdated(id)
                    | CentralEvent::ServicesAdvertisement { id, .. }
                    | CentralEvent::RssiUpdate { id, .. } => st.consider(id, &done_tx).await,
                    CentralEvent::DeviceDisconnected(id) => st.disconnected(&id),
                    _ => {}
                }
            }
            Some(done) = done_rx.recv() => st.finished(done),
            Some(Request::Duplicate(link)) = requests.recv() => st.duplicate(link),
            _ = sweep.tick() => {
                // Some stacks report a device only once: look again.
                if scanning && let Ok(list) = st.adapter.peripherals().await {
                    for p in list {
                        st.consider(p.id(), &done_tx).await;
                    }
                }
            }
        }
    }
}

impl State {
    async fn start_scan(&mut self) -> bool {
        let filter = ScanFilter {
            services: vec![SERVICE_UUID],
        };
        match self.adapter.start_scan(filter).await {
            Ok(()) => {
                self.status();
                true
            }
            Err(e) => {
                let _ = self
                    .events
                    .send(BleEvent::Status(BleStatus::Unavailable(e.to_string())));
                false
            }
        }
    }

    fn links(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s, Slot::Connected(_)))
            .count()
    }

    fn status(&self) {
        let _ = self.events.send(BleEvent::Status(BleStatus::Scanning {
            links: self.links(),
        }));
    }

    fn alive(&self, link: LinkId) -> bool {
        self.slots
            .values()
            .any(|s| matches!(s, Slot::Connecting(l) | Slot::Connected(l) if *l == link))
    }

    async fn consider(&mut self, id: PeripheralId, done: &mpsc::UnboundedSender<Done>) {
        let now = Instant::now();
        self.attempts
            .retain(|(at, _)| now.duration_since(*at) < READVERTISE_WINDOW);
        if self.seen.insert(id.clone())
            && let Some(&(_, link)) = self.attempts.last()
            && self.alive(link)
        {
            self.presumed.insert(id.clone(), (link, now));
        }
        if let Some(&(link, since)) = self.presumed.get(&id) {
            if self.alive(link) && now.duration_since(since) < PRESUME_FOR {
                return;
            }
            self.presumed.remove(&id);
        }
        // One attempt at a time: each one makes the target re-advertise.
        if self.slots.contains_key(&id)
            || self.slots.len() >= MAX_LINKS
            || self
                .slots
                .values()
                .any(|s| matches!(s, Slot::Connecting(_)))
        {
            return;
        }
        if self.retry_at.get(&id).is_some_and(|at| now < *at) {
            return;
        }
        // With nothing connected, any bitchat device is worth a try.
        if self.links() > 0 && self.explore_after.is_some_and(|at| now < at) {
            return;
        }
        let Ok(peripheral) = self.adapter.peripheral(&id).await else {
            return;
        };
        let Ok(Some(props)) = peripheral.properties().await else {
            return;
        };
        if !props.services.contains(&SERVICE_UUID) {
            return;
        }
        // No RSSI: a device the stack remembers but isn't hearing now
        // (BlueZ lists those); connecting would only time out.
        if props.rssi.is_none_or(|r| r < RSSI_THRESHOLD) {
            return;
        }
        if is_incoming(props.address.into_inner()) {
            return;
        }
        let name = props
            .local_name
            .or(props.advertisement_name)
            .unwrap_or_else(|| props.address.to_string());
        let link = next_link_id();
        self.slots.insert(id.clone(), Slot::Connecting(link));
        self.attempts.push((now, link));
        log::info!("ble: connecting to {name} (rssi {:?})", props.rssi);
        tokio::spawn(connect(
            peripheral,
            link,
            name,
            self.events.clone(),
            self.writers.clone(),
            done.clone(),
        ));
    }

    fn finished(&mut self, done: Done) {
        match done {
            Done::Connected(id, link) => {
                self.slots.insert(id, Slot::Connected(link));
            }
            Done::Failed(id, reason) => {
                log::info!("ble: connect failed: {reason}");
                self.slots.remove(&id);
                self.retry_at.insert(id, Instant::now() + RETRY_AFTER);
            }
            Done::Closed(id, link) => {
                if matches!(self.slots.get(&id), Some(Slot::Connected(l)) if *l == link) {
                    self.slots.remove(&id);
                    self.retry_at
                        .insert(id, Instant::now() + Duration::from_secs(2));
                }
                self.link_down(link);
            }
        }
        self.status();
    }

    fn duplicate(&mut self, link: LinkId) {
        let now = Instant::now();
        if self
            .last_duplicate
            .is_some_and(|at| now.duration_since(at) > DUPLICATE_BACKOFF_RESET)
        {
            self.backoff = DUPLICATE_BACKOFF_MIN;
        }
        self.last_duplicate = Some(now);
        self.explore_after = Some(now + self.backoff);
        log::info!(
            "ble: link {link} was a duplicate; next new device check in {}s",
            self.backoff.as_secs()
        );
        self.backoff = (self.backoff * 2).min(DUPLICATE_BACKOFF_MAX);
    }

    fn disconnected(&mut self, id: &PeripheralId) {
        if let Some(Slot::Connected(link)) = self.slots.remove(id) {
            self.retry_at
                .insert(id.clone(), Instant::now() + Duration::from_secs(2));
            self.link_down(link);
            self.status();
        }
    }

    fn link_down(&mut self, link: LinkId) {
        if self
            .writers
            .lock()
            .expect("writers")
            .remove(&link)
            .is_some()
        {
            let _ = self.events.send(BleEvent::LinkDown { link });
        }
    }

    fn drop_all(&mut self) {
        let links: Vec<LinkId> = self
            .slots
            .values()
            .filter_map(|s| match s {
                Slot::Connected(l) => Some(*l),
                Slot::Connecting(_) => None,
            })
            .collect();
        self.slots.clear();
        for link in links {
            self.link_down(link);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

async fn connect(
    peripheral: Peripheral,
    link: LinkId,
    name: String,
    events: mpsc::UnboundedSender<BleEvent>,
    writers: Writers,
    done: mpsc::UnboundedSender<Done>,
) {
    let id = peripheral.id();
    let characteristic = match setup(&peripheral).await {
        Ok(c) => c,
        Err(e) => {
            let _ = peripheral.disconnect().await;
            let _ = done.send(Done::Failed(id, e));
            return;
        }
    };
    let mut notifications = match peripheral.notifications().await {
        Ok(n) => n,
        Err(e) => {
            let _ = peripheral.disconnect().await;
            let _ = done.send(Done::Failed(id, e.to_string()));
            return;
        }
    };
    if let Err(e) = peripheral.subscribe(&characteristic).await {
        let _ = peripheral.disconnect().await;
        let _ = done.send(Done::Failed(id, format!("subscribe: {e}")));
        return;
    }

    // Windows drops a BLE connection after a few idle seconds unless a GATT
    // session asks to keep it; hold one for the life of the link.
    #[cfg(windows)]
    let _keep_alive = windows_keep_alive(&peripheral).await;

    // The MTU exchange usually lands right after connecting.
    for _ in 0..10 {
        if peripheral.mtu() > DEFAULT_ATT_MTU {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mtu = peripheral.mtu();
    let max_write = if mtu > DEFAULT_ATT_MTU {
        mtu as usize - 3
    } else {
        FALLBACK_MAX_WRITE
    };
    log::info!("ble: link {link} up to {name}, mtu {mtu}");

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    writers.lock().expect("writers").insert(link, tx);
    let _ = done.send(Done::Connected(id.clone(), link));
    let _ = events.send(BleEvent::LinkUp {
        link,
        max_write,
        name: name.clone(),
        incoming: false,
    });

    let mut writer = {
        let peripheral = peripheral.clone();
        let characteristic = characteristic.clone();
        tokio::spawn(async move {
            let mut failures = 0;
            while let Some(frame) = rx.recv().await {
                if frame.is_empty() {
                    log::info!("ble: closing redundant link {link}");
                    let _ = peripheral.disconnect().await;
                    break;
                }
                let limit = (peripheral.mtu().max(DEFAULT_ATT_MTU) - 3) as usize;
                // Oversized frames (unknown MTU) use a long write with response.
                let kind = if frame.len() <= limit {
                    WriteType::WithoutResponse
                } else {
                    WriteType::WithResponse
                };
                match peripheral.write(&characteristic, &frame, kind).await {
                    Ok(()) => failures = 0,
                    Err(e) => {
                        failures += 1;
                        log::warn!(
                            "ble: write on link {link} failed ({} bytes): {e}",
                            frame.len()
                        );
                        // RO_E_CLOSED: Windows already dropped the connection.
                        let closed = e.to_string().contains("0x80000013");
                        if closed || failures >= 5 {
                            let _ = peripheral.disconnect().await;
                            break;
                        }
                    }
                }
            }
        })
    };

    let mut assembler = StreamAssembler::default();
    loop {
        // The link ends when notifications stop or the writer gives up.
        let n = tokio::select! {
            n = notifications.next() => match n {
                Some(n) => n,
                None => break,
            },
            _ = &mut writer => break,
        };
        if n.uuid != CHARACTERISTIC_UUID {
            continue;
        }
        let out = assembler.append(&n.value, now_ms());
        if out.reset || !out.dropped.is_empty() {
            log::debug!(
                "ble: link {link} stream reset={} dropped={}",
                out.reset,
                out.dropped.len()
            );
        }
        for frame in out.frames {
            let _ = events.send(BleEvent::Frame { link, frame });
        }
    }
    writer.abort();
    let _ = peripheral.disconnect().await;
    log::info!("ble: link {link} to {name} closed");
    let _ = done.send(Done::Closed(id, link));
}

async fn setup(peripheral: &Peripheral) -> Result<Characteristic, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, peripheral.connect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("connect: {e}")),
        Err(_) => return Err("connect timed out".into()),
    }
    match tokio::time::timeout(DISCOVER_TIMEOUT, peripheral.discover_services()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("discover: {e}")),
        Err(_) => return Err("service discovery timed out".into()),
    }
    let found: HashSet<_> = peripheral.services().iter().map(|s| s.uuid).collect();
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == CHARACTERISTIC_UUID && c.service_uuid == SERVICE_UUID)
        .ok_or_else(|| format!("no bitchat characteristic (services {found:?})"))
}

#[cfg(windows)]
async fn windows_keep_alive(
    peripheral: &Peripheral,
) -> Option<windows::Devices::Bluetooth::GenericAttributeProfile::GattSession> {
    use windows::Devices::Bluetooth::BluetoothLEDevice;
    use windows::Devices::Bluetooth::GenericAttributeProfile::GattSession;
    let address: u64 = peripheral.address().into();
    let result = async {
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)?.await?;
        let session = GattSession::FromDeviceIdAsync(&device.BluetoothDeviceId()?)?.await?;
        session.SetMaintainConnection(true)?;
        Ok::<_, windows::core::Error>(session)
    }
    .await;
    match result {
        Ok(session) => Some(session),
        Err(e) => {
            log::warn!("ble: could not pin the connection: {e}");
            None
        }
    }
}
