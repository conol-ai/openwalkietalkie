//! The deck's BLE audio service on Linux: BlueZ over D-Bus (`bluer`).
//!
//! The same job `central` does with CoreBluetooth, behind the same seam
//! ([`super::link::Shared`]): find the deck the system already holds
//! connected for HID, discover the audio service, read its info, subscribe to
//! the mic and drain the speaker queue. BlueZ keeps the HID side (bonding,
//! encryption, the link itself), exactly as macOS does, so this never pairs,
//! unpairs or scans: a deck that is not connected is "not found", and the
//! retry loop looks again.
//!
//! One thread with its own single-threaded runtime owns the attach; the
//! daemon's handle only ever calls [`Backend::kick`] (frames were queued) and
//! drops the backend (stop).
use super::link::{LinkState, Peer, Shared, Transport};
use bluer::gatt::remote::Characteristic;
use bluer::{Adapter, Address, Device, DeviceEvent, DeviceProperty};
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

const SERVICE: uuid::Uuid = uuid::Uuid::from_u128(0xe9895507_bb72_45e5_a802_ab38fe1e4bc1);
const MIC: uuid::Uuid = uuid::Uuid::from_u128(0x6e25df5b_b48a_4203_bb04_f3d4caeb1b7a);
const SPEAKER: uuid::Uuid = uuid::Uuid::from_u128(0x84de7298_ab42_46e4_9c02_f6a65d990807);
const INFO: uuid::Uuid = uuid::Uuid::from_u128(0xa2259f3a_8d16_4c1e_a243_78f7206e35f5);

/// How long a link that is not ready waits before looking again.
const RETRY: Duration = Duration::from_secs(2);
/// The longest wait between attempts at a deck that keeps failing to attach.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// One attach step may take this long (service resolution on a fresh
/// connection is the slow one).
const STEP: Duration = Duration::from_secs(15);

pub(crate) struct Backend {
    wake: Arc<Notify>,
    stop: Arc<AtomicBool>,
}

impl Backend {
    pub(crate) fn spawn(shared: Arc<Shared>, name: &str) -> Backend {
        let wake = Arc::new(Notify::new());
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_wake, thread_stop, name) = (wake.clone(), stop.clone(), name.to_string());
        let started = std::thread::Builder::new()
            .name("deckd-ble-bluez".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        shared.set_problem(Transport::Ble, Some(format!("cannot start the BlueZ link: {e}")));
                        return;
                    }
                };
                runtime.block_on(run(shared, name, thread_wake, thread_stop));
            });
        if let Err(e) = started {
            tracing::warn!(%e, "cannot start the BlueZ link thread");
        }
        Backend { wake, stop }
    }

    /// Frames were queued for the speaker.
    pub(crate) fn kick(&self) {
        self.wake.notify_one();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

async fn run(shared: Arc<Shared>, name: String, wake: Arc<Notify>, stop: Arc<AtomicBool>) {
    let mut failures = 0u32;
    while !stop.load(Ordering::Acquire) {
        match attach(&shared, &name, &wake, &stop).await {
            Ok(()) => failures = 0, // it was ready, and has ended: start again promptly
            Err(why) => {
                failures = failures.saturating_add(1);
                tracing::info!(%why, "deck audio link not ready");
                shared.set_problem(Transport::Ble, Some(why));
            }
        }
        shared.set_peer(Transport::Ble, None);
        shared.set_state(Transport::Ble, LinkState::Detached);
        let wait = (RETRY * 2u32.saturating_pow(failures.saturating_sub(1).min(5))).min(MAX_BACKOFF);
        tokio::time::sleep(if failures == 0 { RETRY } else { wait }).await;
    }
}

async fn find(adapter: &Adapter, name: &str) -> Result<Option<Device>, String> {
    let addresses = adapter.device_addresses().await.map_err(|e| e.to_string())?;
    for address in addresses {
        let Ok(device) = adapter.device(address) else { continue };
        let named = device.name().await.ok().flatten().is_some_and(|got| got == name);
        if named && device.is_connected().await.unwrap_or(false) {
            return Ok(Some(device));
        }
    }
    Ok(None)
}

async fn step<T, E: std::fmt::Display>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(STEP, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(format!("{what}: {e}")),
        Err(_) => Err(format!("{what}: no answer in {}s", STEP.as_secs())),
    }
}

async fn characteristics(device: &Device) -> Result<[Option<Characteristic>; 3], String> {
    let mut found: [Option<Characteristic>; 3] = [None, None, None];
    for service in device.services().await.map_err(|e| e.to_string())? {
        if service.uuid().await.map_err(|e| e.to_string())? != SERVICE {
            continue;
        }
        for characteristic in service.characteristics().await.map_err(|e| e.to_string())? {
            match characteristic.uuid().await.map_err(|e| e.to_string())? {
                MIC => found[0] = Some(characteristic),
                SPEAKER => found[1] = Some(characteristic),
                INFO => found[2] = Some(characteristic),
                _ => {}
            }
        }
    }
    Ok(found)
}

/// One attach, from finding the deck to losing it. `Ok` when it ended after
/// having been ready; `Err(why)` when it never got there.
async fn attach(
    shared: &Arc<Shared>,
    name: &str,
    wake: &Arc<Notify>,
    stop: &Arc<AtomicBool>,
) -> Result<(), String> {
    let session = bluer::Session::new().await.map_err(|e| format!("BlueZ: {e}"))?;
    let adapter = session.default_adapter().await.map_err(|e| format!("no Bluetooth adapter: {e}"))?;
    if !adapter.is_powered().await.unwrap_or(false) {
        return Err("the Bluetooth adapter is off".into());
    }
    let Some(device) = find(&adapter, name).await? else {
        return Err(format!("no connected {name}"));
    };
    let address: Address = device.address();
    shared.set_peer(
        Transport::Ble,
        Some(Peer { name: Some(name.to_string()), id: address.to_string(), transport: Transport::Ble }),
    );
    shared.set_state(Transport::Ble, LinkState::Attaching);

    let mut events = device.events().await.map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now() + STEP;
    while !device.is_services_resolved().await.unwrap_or(false) {
        if tokio::time::Instant::now() > deadline {
            return Err("service discovery: no answer".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let [mic, speaker, info] = characteristics(&device).await?;
    let (Some(mic), Some(speaker)) = (mic, speaker) else {
        return Err("the deck has no audio service (firmware without it?)".into());
    };

    let info = match info {
        Some(info) => step("info", info.read()).await.ok().and_then(|v| {
            (v.len() >= 5).then(|| [v[0], v[1], v[2], v[3], v[4]])
        }),
        None => None,
    };
    let mut writer = step("speaker", speaker.write_io()).await?;
    // BlueZ reports the ATT MTU; a write carries three bytes less. Under-claiming
    // costs samples per frame, never the layout (`codec::samples_per_frame`).
    let max_write = writer.mtu().saturating_sub(3);
    let mut notifications = Box::pin(step("mic subscription", mic.notify()).await?);

    tracing::info!(peer = %address, max_write, ?info, "deck audio link ready");
    shared.set_state(Transport::Ble, LinkState::Ready { max_write, info });

    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        tokio::select! {
            value = notifications.next() => match value {
                Some(value) => shared.notify(Transport::Ble, &value),
                None => return Ok(()),
            },
            event = events.next() => match event {
                Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) | None => return Ok(()),
                Some(DeviceEvent::PropertyChanged(DeviceProperty::ServicesResolved(false))) => return Ok(()),
                Some(_) => {}
            },
            () = wake.notified() => {
                let mut frames: Vec<Vec<u8>> = Vec::new();
                shared.drain_for(Transport::Ble, || true, |frame| frames.push(frame.to_vec()));
                for frame in frames {
                    if let Err(e) = writer.write_all(&frame).await {
                        tracing::warn!(%e, "deck speaker write failed");
                        return Ok(());
                    }
                }
            }
        }
    }
}
