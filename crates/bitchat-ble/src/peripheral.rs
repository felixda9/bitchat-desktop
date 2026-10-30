//! BLE peripheral: hosts the bitchat GATT service and advertises it, so
//! phones can connect to us (iPhones with bitchat in the background can
//! only find devices that advertise). Each subscribed phone becomes a link:
//! its writes are reassembled into frames, and we send with notifications.
//! Windows uses `GattServiceProvider`, Linux BlueZ (through bluer).

use crate::central::{BleEvent, LinkId};
use tokio::sync::mpsc;

#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
enum Cmd {
    Notify(LinkId, Vec<u8>),
}

/// Handle to the running peripheral. Dropping it stops advertising.
pub struct Peripheral {
    commands: Option<mpsc::UnboundedSender<Cmd>>,
}

impl Peripheral {
    pub fn start(events: mpsc::UnboundedSender<BleEvent>) -> Peripheral {
        #[cfg(windows)]
        {
            let (tx, rx) = mpsc::unbounded_channel();
            std::thread::Builder::new()
                .name("ble-peripheral".into())
                .spawn(move || winrt::run(events, rx))
                .expect("spawn peripheral thread");
            Peripheral { commands: Some(tx) }
        }
        #[cfg(target_os = "linux")]
        {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(bluez::run(events, rx));
            Peripheral { commands: Some(tx) }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = events;
            log::info!("ble: peripheral role not available on this platform yet");
            Peripheral { commands: None }
        }
    }

    /// Sends a frame to the phone on `link` as a notification.
    pub fn write(&self, link: LinkId, frame: Vec<u8>) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(Cmd::Notify(link, frame));
        }
    }
}

#[cfg(windows)]
mod winrt {
    use super::Cmd;
    use crate::central::{BleEvent, next_link_id};
    use crate::writes::{Assembled, WriteAssembler};
    use crate::{CHARACTERISTIC_UUID, SERVICE_UUID};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use uuid::Uuid;
    use windows::Devices::Bluetooth::BluetoothError;
    use windows::Devices::Bluetooth::GenericAttributeProfile::{
        GattCharacteristicProperties, GattLocalCharacteristic, GattLocalCharacteristicParameters,
        GattProtectionLevel, GattReadRequestedEventArgs, GattServiceProvider,
        GattServiceProviderAdvertisementStatusChangedEventArgs,
        GattServiceProviderAdvertisingParameters, GattSubscribedClient, GattWriteOption,
        GattWriteRequestedEventArgs,
    };
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};
    use windows::core::{GUID, Result};

    fn guid(u: Uuid) -> GUID {
        GUID::from_u128(u.as_u128())
    }

    fn to_buffer(bytes: &[u8]) -> Result<IBuffer> {
        let w = DataWriter::new()?;
        w.WriteBytes(bytes)?;
        w.DetachBuffer()
    }

    fn from_buffer(buf: &IBuffer) -> Result<Vec<u8>> {
        let r = DataReader::FromBuffer(buf)?;
        let mut v = vec![0u8; buf.Length()? as usize];
        r.ReadBytes(&mut v)?;
        Ok(v)
    }

    struct Client {
        link: u64,
        client: GattSubscribedClient,
        writes: WriteAssembler,
    }

    #[derive(Default)]
    struct Clients {
        /// By Bluetooth device ID.
        by_device: HashMap<String, Client>,
    }

    fn device_of(client: &GattSubscribedClient) -> Result<String> {
        Ok(client.Session()?.DeviceId()?.Id()?.to_string())
    }

    pub(super) fn run(
        events: mpsc::UnboundedSender<BleEvent>,
        mut commands: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let clients: Arc<Mutex<Clients>> = Arc::default();
        let (provider, characteristic) = match setup(&events, &clients) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("ble: could not start the peripheral: {e}");
                return;
            }
        };
        log::info!("ble: advertising the bitchat service");
        while let Some(cmd) = commands.blocking_recv() {
            match cmd {
                Cmd::Notify(link, frame) => {
                    let client = clients
                        .lock()
                        .expect("clients")
                        .by_device
                        .values()
                        .find(|c| c.link == link)
                        .map(|c| c.client.clone());
                    let Some(client) = client else { continue };
                    let sent = to_buffer(&frame).and_then(|buf| {
                        characteristic
                            .NotifyValueForSubscribedClientAsync(&buf, &client)?
                            .join()
                    });
                    if let Err(e) = sent {
                        log::warn!("ble: notify on link {link} failed: {e}");
                    }
                }
            }
        }
        let _ = provider.StopAdvertising();
    }

    fn setup(
        events: &mpsc::UnboundedSender<BleEvent>,
        clients: &Arc<Mutex<Clients>>,
    ) -> Result<(GattServiceProvider, GattLocalCharacteristic)> {
        let created = GattServiceProvider::CreateAsync(guid(SERVICE_UUID))?.join()?;
        if created.Error()? != BluetoothError::Success {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(-1),
                format!("service provider: {:?}", created.Error()?),
            ));
        }
        let provider = created.ServiceProvider()?;

        let params = GattLocalCharacteristicParameters::new()?;
        params.SetCharacteristicProperties(
            GattCharacteristicProperties::Read
                | GattCharacteristicProperties::Write
                | GattCharacteristicProperties::WriteWithoutResponse
                | GattCharacteristicProperties::Notify,
        )?;
        params.SetReadProtectionLevel(GattProtectionLevel::Plain)?;
        params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;
        let made = provider
            .Service()?
            .CreateCharacteristicAsync(guid(CHARACTERISTIC_UUID), &params)?
            .join()?;
        if made.Error()? != BluetoothError::Success {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(-1),
                format!("characteristic: {:?}", made.Error()?),
            ));
        }
        let characteristic = made.Characteristic()?;

        // Phones connect, subscribe, then write. A subscription is a link.
        {
            let events = events.clone();
            let clients = clients.clone();
            characteristic.SubscribedClientsChanged(&TypedEventHandler::<
                GattLocalCharacteristic,
                windows::core::IInspectable,
            >::new(move |sender, _| {
                let Some(ch) = sender.as_ref() else {
                    return Ok(());
                };
                let list = ch.SubscribedClients()?;
                let mut current: HashMap<String, GattSubscribedClient> = HashMap::new();
                for i in 0..list.Size()? {
                    let c = list.GetAt(i)?;
                    if let Ok(id) = device_of(&c) {
                        current.insert(id, c);
                    }
                }
                let mut state = clients.lock().expect("clients");
                let gone: Vec<String> = state
                    .by_device
                    .keys()
                    .filter(|k| !current.contains_key(*k))
                    .cloned()
                    .collect();
                for id in gone {
                    if let Some(c) = state.by_device.remove(&id) {
                        log::info!("ble: incoming link {} closed", c.link);
                        let _ = events.send(BleEvent::LinkDown { link: c.link });
                    }
                }
                for (id, client) in current {
                    if state.by_device.contains_key(&id) {
                        continue;
                    }
                    let link = next_link_id();
                    let max_write = client.MaxNotificationSize().unwrap_or(20) as usize;
                    log::info!(
                        "ble: incoming link {link} from {id}, notifications up to {max_write} bytes"
                    );
                    state.by_device.insert(
                        id.clone(),
                        Client {
                            link,
                            client,
                            writes: WriteAssembler::default(),
                        },
                    );
                    let _ = events.send(BleEvent::LinkUp {
                        link,
                        max_write,
                        name: id,
                        incoming: true,
                    });
                }
                Ok(())
            }))?;
        }

        {
            let events = events.clone();
            let clients = clients.clone();
            characteristic.WriteRequested(&TypedEventHandler::<
                GattLocalCharacteristic,
                GattWriteRequestedEventArgs,
            >::new(move |_, args| {
                let Some(args) = args.as_ref() else {
                    return Ok(());
                };
                let deferral = args.GetDeferral()?;
                let device = args.Session()?.DeviceId()?.Id()?.to_string();
                let request = args.GetRequestAsync()?.join()?;
                let data = from_buffer(&request.Value()?)?;
                let offset = request.Offset()? as usize;
                if request.Option()? == GattWriteOption::WriteWithResponse {
                    request.Respond()?;
                }
                deferral.Complete()?;
                let mut state = clients.lock().expect("clients");
                let Some(client) = state.by_device.get_mut(&device) else {
                    log::debug!("ble: write from {device} before it subscribed; dropped");
                    return Ok(());
                };
                match client.writes.append(offset, &data) {
                    Assembled::Frame(frame) => {
                        let _ = events.send(BleEvent::Frame {
                            link: client.link,
                            frame,
                        });
                    }
                    Assembled::Oversized => {
                        log::warn!("ble: oversized write on link {}", client.link)
                    }
                    Assembled::Waiting => {}
                }
                Ok(())
            }))?;
        }

        // Reads return nothing (iOS does the same).
        characteristic.ReadRequested(&TypedEventHandler::<
            GattLocalCharacteristic,
            GattReadRequestedEventArgs,
        >::new(|_, args| {
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            let deferral = args.GetDeferral()?;
            let request = args.GetRequestAsync()?.join()?;
            request.RespondWithValue(&to_buffer(&[])?)?;
            deferral.Complete()
        }))?;

        provider.AdvertisementStatusChanged(&TypedEventHandler::<
            GattServiceProvider,
            GattServiceProviderAdvertisementStatusChangedEventArgs,
        >::new(|_, args| {
            if let Some(args) = args.as_ref() {
                log::info!(
                    "ble: advertising status {:?} ({:?})",
                    args.Status()?,
                    args.Error()?
                );
            }
            Ok(())
        }))?;

        let adv = GattServiceProviderAdvertisingParameters::new()?;
        adv.SetIsConnectable(true)?;
        adv.SetIsDiscoverable(true)?;
        provider.StartAdvertisingWithParameters(&adv)?;
        Ok((provider, characteristic))
    }
}

/// Linux: BlueZ GATT server through bluer. BlueZ sends a notification to
/// every subscribed phone at once (no per-device notify), so a frame for one
/// link reaches them all; each drops what isn't for it. Phones become links
/// on their first write; a link ends when the phone disconnects.
#[cfg(target_os = "linux")]
mod bluez {
    use super::Cmd;
    use crate::central::{BleEvent, next_link_id};
    use crate::writes::{Assembled, WriteAssembler};
    use crate::{CHARACTERISTIC_UUID, SERVICE_UUID};
    use bluer::Address;
    use bluer::adv::{Advertisement, Type};
    use bluer::gatt::local::{
        Application, Characteristic, CharacteristicNotifier, CharacteristicNotify,
        CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite,
        CharacteristicWriteMethod, Service,
    };
    use futures::FutureExt;
    use std::collections::HashMap;
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    struct Client {
        link: u64,
        writes: WriteAssembler,
    }

    type Clients = Arc<Mutex<HashMap<Address, Client>>>;
    type Notifier = Arc<tokio::sync::Mutex<Option<CharacteristicNotifier>>>;

    /// The same frame for several links goes out once.
    const SAME_FRAME_WINDOW: Duration = Duration::from_secs(1);

    pub(super) async fn run(
        events: mpsc::UnboundedSender<BleEvent>,
        mut commands: mpsc::UnboundedReceiver<Cmd>,
    ) {
        if let Err(e) = serve(&events, &mut commands).await {
            log::warn!("ble: could not start the peripheral: {e}");
        }
    }

    async fn serve(
        events: &mpsc::UnboundedSender<BleEvent>,
        commands: &mut mpsc::UnboundedReceiver<Cmd>,
    ) -> bluer::Result<()> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        let clients: Clients = Arc::default();
        let notifier: Notifier = Arc::default();

        let write = {
            let events = events.clone();
            let clients = clients.clone();
            CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                let events = events.clone();
                let clients = clients.clone();
                async move {
                    let mut map = clients.lock().expect("clients");
                    let client = map.entry(req.device_address).or_insert_with(|| {
                        let link = next_link_id();
                        let max_write = (req.mtu as usize).saturating_sub(3).max(20);
                        log::info!(
                            "ble: incoming link {link} from {}, writes up to {max_write} bytes",
                            req.device_address
                        );
                        let _ = events.send(BleEvent::LinkUp {
                            link,
                            max_write,
                            name: req.device_address.to_string(),
                            incoming: true,
                        });
                        Client {
                            link,
                            writes: WriteAssembler::default(),
                        }
                    });
                    match client.writes.append(req.offset as usize, &value) {
                        Assembled::Frame(frame) => {
                            let _ = events.send(BleEvent::Frame {
                                link: client.link,
                                frame,
                            });
                        }
                        Assembled::Oversized => {
                            log::warn!("ble: oversized write on link {}", client.link)
                        }
                        Assembled::Waiting => {}
                    }
                    Ok(())
                }
                .boxed()
            }))
        };
        let notify = {
            let notifier = notifier.clone();
            CharacteristicNotifyMethod::Fun(Box::new(move |n| {
                let notifier = notifier.clone();
                async move {
                    log::info!("ble: a phone subscribed to notifications");
                    *notifier.lock().await = Some(n);
                }
                .boxed()
            }))
        };
        let app = Application {
            services: vec![Service {
                uuid: SERVICE_UUID,
                primary: true,
                characteristics: vec![Characteristic {
                    uuid: CHARACTERISTIC_UUID,
                    // Reads return nothing (iOS does the same).
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(|_| async move { Ok(Vec::new()) }.boxed()),
                        ..Default::default()
                    }),
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: write,
                        ..Default::default()
                    }),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: notify,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let _app = adapter.serve_gatt_application(app).await?;
        // Service UUID only: no local name, like the phones.
        let _adv = adapter
            .advertise(Advertisement {
                advertisement_type: Type::Peripheral,
                service_uuids: [SERVICE_UUID].into_iter().collect(),
                discoverable: Some(true),
                ..Default::default()
            })
            .await?;
        log::info!("ble: advertising the bitchat service");

        let mut recent: HashMap<u64, Instant> = HashMap::new();
        let mut check = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                cmd = commands.recv() => {
                    let Some(Cmd::Notify(_link, frame)) = cmd else { break };
                    let now = Instant::now();
                    recent.retain(|_, at| now.duration_since(*at) < SAME_FRAME_WINDOW);
                    let mut h = DefaultHasher::new();
                    frame.hash(&mut h);
                    if recent.insert(h.finish(), now).is_some() {
                        continue;
                    }
                    let mut guard = notifier.lock().await;
                    if let Some(n) = guard.as_mut()
                        && let Err(e) = n.notify(frame).await
                    {
                        log::info!("ble: notifications stopped: {e}");
                        *guard = None;
                    }
                }
                _ = check.tick() => {
                    // Links end when their phone disconnects.
                    let known: Vec<(Address, u64)> = clients
                        .lock()
                        .expect("clients")
                        .iter()
                        .map(|(a, c)| (*a, c.link))
                        .collect();
                    for (address, link) in known {
                        let connected = match adapter.device(address) {
                            Ok(d) => d.is_connected().await.unwrap_or(false),
                            Err(_) => false,
                        };
                        if !connected {
                            clients.lock().expect("clients").remove(&address);
                            log::info!("ble: incoming link {link} closed");
                            let _ = events.send(BleEvent::LinkDown { link });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
