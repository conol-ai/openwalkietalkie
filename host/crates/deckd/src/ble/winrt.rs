//! The deck's BLE audio service on Windows: WinRT's GATT client
//! (`Windows.Devices.Bluetooth`).
//!
//! The same job `bluez` does on Linux, behind the same seam
//! ([`super::link::Shared`]): find the deck Windows already holds connected
//! for HID, discover the audio service, read its info, subscribe to the mic
//! and drain the speaker queue. Windows keeps the HID side (bonding,
//! encryption, the link itself), so this never pairs, unpairs or scans: a
//! deck that is not connected is "not found", and the retry loop looks again.
//!
//! One thread owns the attach and waits on WinRT's operations synchronously
//! (`.get()`); notifications arrive on WinRT's own thread pool and go
//! straight to [`Shared::notify`]. The daemon's handle only ever calls
//! [`Backend::kick`] (frames were queued) and drops the backend (stop).
use super::link::{LinkState, Peer, Shared, Transport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use windows::core::{GUID, HSTRING};
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristic, GattClientCharacteristicConfigurationDescriptorValue,
    GattCommunicationStatus, GattSession, GattValueChangedEventArgs, GattWriteOption,
};
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};

const SERVICE: GUID = GUID::from_u128(0xe9895507_bb72_45e5_a802_ab38fe1e4bc1);
const MIC: GUID = GUID::from_u128(0x6e25df5b_b48a_4203_bb04_f3d4caeb1b7a);
const SPEAKER: GUID = GUID::from_u128(0x84de7298_ab42_46e4_9c02_f6a65d990807);
const INFO: GUID = GUID::from_u128(0xa2259f3a_8d16_4c1e_a243_78f7206e35f5);

/// How long a link that is not ready waits before looking again.
const RETRY: Duration = Duration::from_secs(2);
/// The longest wait between attempts at a deck that keeps failing to attach.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often a ready link checks that the deck is still connected when no
/// frames wake it.
const TICK: Duration = Duration::from_millis(500);

/// The thread's doorbell: frames were queued, or the backend was dropped.
#[derive(Default)]
struct Bell {
    rung: Mutex<bool>,
    cond: Condvar,
}

impl Bell {
    fn ring(&self) {
        *self.rung.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.cond.notify_one();
    }

    /// Wait for a ring or `timeout`, whichever comes first, and clear it.
    fn wait(&self, timeout: Duration) {
        let rung = self.rung.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut rung, _) = self
            .cond
            .wait_timeout_while(rung, timeout, |rung| !*rung)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *rung = false;
    }
}

pub(crate) struct Backend {
    bell: Arc<Bell>,
    stop: Arc<AtomicBool>,
}

impl Backend {
    pub(crate) fn spawn(shared: Arc<Shared>, name: &str) -> Backend {
        let bell = Arc::new(Bell::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_bell, thread_stop, name) = (bell.clone(), stop.clone(), name.to_string());
        let started = std::thread::Builder::new()
            .name("deckd-ble-winrt".into())
            .spawn(move || run(&shared, &name, &thread_bell, &thread_stop));
        if let Err(e) = started {
            tracing::warn!(%e, "cannot start the Bluetooth link thread");
        }
        Backend { bell, stop }
    }

    /// Frames were queued for the speaker.
    pub(crate) fn kick(&self) {
        self.bell.ring();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.bell.ring();
    }
}

fn run(shared: &Arc<Shared>, name: &str, bell: &Bell, stop: &AtomicBool) {
    let mut failures = 0u32;
    while !stop.load(Ordering::Acquire) {
        match attach(shared, name, bell, stop) {
            Ok(()) => failures = 0, // it was ready, and has ended: start again promptly
            Err(why) => {
                failures = failures.saturating_add(1);
                // The same reason every two seconds is noise after the first.
                if failures == 1 {
                    tracing::info!(%why, "deck audio link not ready");
                }
                shared.set_problem(Transport::Ble, Some(why));
            }
        }
        shared.set_peer(Transport::Ble, None);
        shared.set_state(Transport::Ble, LinkState::Detached);
        let wait = (RETRY * 2u32.saturating_pow(failures.saturating_sub(1).min(5))).min(MAX_BACKOFF);
        bell.wait(if failures == 0 { RETRY } else { wait });
    }
}

fn err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {}", e.message())
}

/// The connected Bluetooth LE device that goes by `name`. Windows lists the
/// paired devices it holds connected; one that is only advertising is not
/// in the list, by design (see the module comment).
fn find(name: &str) -> Result<Option<BluetoothLEDevice>, String> {
    let selector = BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)
        .map_err(err("Bluetooth"))?;
    let found = DeviceInformation::FindAllAsyncAqsFilter(&selector)
        .and_then(|op| op.get())
        .map_err(|e| match e.code().0 as u32 {
            // HRESULT_FROM_WIN32(ERROR_DEVICE_NOT_AVAILABLE) and friends: no radio.
            0x8007_10DF | 0x8007_048F => "no Bluetooth adapter".to_string(),
            _ => format!("Bluetooth: {}", e.message()),
        })?;
    for info in found {
        if info.Name().is_ok_and(|got| got == HSTRING::from(name)) {
            let device = BluetoothLEDevice::FromIdAsync(&info.Id().map_err(err("device id"))?)
                .and_then(|op| op.get())
                .map_err(err("opening the deck"))?;
            return Ok(Some(device));
        }
    }
    Ok(None)
}

fn bytes(buffer: &IBuffer) -> Result<Vec<u8>, windows::core::Error> {
    let reader = DataReader::FromBuffer(buffer)?;
    let mut out = vec![0u8; reader.UnconsumedBufferLength()? as usize];
    reader.ReadBytes(&mut out)?;
    Ok(out)
}

fn buffer(frame: &[u8]) -> Result<IBuffer, windows::core::Error> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(frame)?;
    writer.DetachBuffer()
}

/// The audio service's characteristics: mic, speaker, info.
fn characteristics(
    device: &BluetoothLEDevice,
) -> Result<[Option<GattCharacteristic>; 3], String> {
    let services = device
        .GetGattServicesForUuidWithCacheModeAsync(SERVICE, BluetoothCacheMode::Uncached)
        .and_then(|op| op.get())
        .map_err(err("service discovery"))?;
    match services.Status().map_err(err("service discovery"))? {
        GattCommunicationStatus::Success => {}
        GattCommunicationStatus::Unreachable => return Err("service discovery: the deck is unreachable".into()),
        GattCommunicationStatus::AccessDenied => return Err("service discovery: access denied".into()),
        status => return Err(format!("service discovery: status {}", status.0)),
    }
    let mut found: [Option<GattCharacteristic>; 3] = [None, None, None];
    for service in services.Services().map_err(err("service discovery"))? {
        let list = service
            .GetCharacteristicsWithCacheModeAsync(BluetoothCacheMode::Uncached)
            .and_then(|op| op.get())
            .map_err(err("characteristic discovery"))?;
        if list.Status().map_err(err("characteristic discovery"))? != GattCommunicationStatus::Success {
            continue;
        }
        for characteristic in list.Characteristics().map_err(err("characteristic discovery"))? {
            match characteristic.Uuid().map_err(err("characteristic discovery"))? {
                MIC => found[0] = Some(characteristic),
                SPEAKER => found[1] = Some(characteristic),
                INFO => found[2] = Some(characteristic),
                _ => {}
            }
        }
    }
    Ok(found)
}

fn read_info(info: &GattCharacteristic) -> Option<[u8; 5]> {
    let result = info.ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached).ok()?.get().ok()?;
    if result.Status().ok()? != GattCommunicationStatus::Success {
        return None;
    }
    let v = bytes(&result.Value().ok()?).ok()?;
    (v.len() >= 5).then(|| [v[0], v[1], v[2], v[3], v[4]])
}

fn connected(device: &BluetoothLEDevice) -> bool {
    device.ConnectionStatus().is_ok_and(|status| status == BluetoothConnectionStatus::Connected)
}

/// One attach, from finding the deck to losing it. `Ok` when it ended after
/// having been ready; `Err(why)` when it never got there.
fn attach(shared: &Arc<Shared>, name: &str, bell: &Bell, stop: &AtomicBool) -> Result<(), String> {
    let Some(device) = find(name)? else {
        return Err(format!("no connected {name}"));
    };
    let id = device
        .BluetoothAddress()
        .map(|address| {
            let b = address.to_be_bytes();
            format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", b[2], b[3], b[4], b[5], b[6], b[7])
        })
        .map_err(err("the deck's address"))?;
    shared.set_peer(
        Transport::Ble,
        Some(Peer { name: Some(name.to_string()), id: id.clone(), transport: Transport::Ble }),
    );
    shared.set_state(Transport::Ble, LinkState::Attaching);

    let [mic, speaker, info] = characteristics(&device)?;
    let (Some(mic), Some(speaker)) = (mic, speaker) else {
        return Err("the deck has no audio service (firmware without it?)".into());
    };
    let info = info.as_ref().and_then(read_info);

    // The ATT MTU of the session the HID side keeps up; a write carries three
    // bytes less. Under-claiming costs samples per frame, never the layout
    // (`codec::samples_per_frame`).
    let session = device
        .BluetoothDeviceId()
        .and_then(|id| GattSession::FromDeviceIdAsync(&id))
        .and_then(|op| op.get())
        .map_err(err("the GATT session"))?;
    let max_write = usize::from(session.MaxPduSize().map_err(err("the GATT session"))?).saturating_sub(3);

    let sink = shared.clone();
    let token = mic
        .ValueChanged(&TypedEventHandler::<GattCharacteristic, GattValueChangedEventArgs>::new(
            move |_, args| {
                if let Some(args) = args.as_ref() {
                    if let Ok(value) = args.CharacteristicValue().and_then(|buffer| bytes(&buffer)) {
                        sink.notify(Transport::Ble, &value);
                    }
                }
                Ok(())
            },
        ))
        .map_err(err("mic subscription"))?;
    let subscribed = mic
        .WriteClientCharacteristicConfigurationDescriptorAsync(
            GattClientCharacteristicConfigurationDescriptorValue::Notify,
        )
        .and_then(|op| op.get())
        .map_err(err("mic subscription"));
    let result = match subscribed {
        Ok(GattCommunicationStatus::Success) => {
            tracing::info!(peer = %id, max_write, ?info, "deck audio link ready");
            shared.set_state(Transport::Ble, LinkState::Ready { max_write, info });
            serve(shared, &device, &speaker, bell, stop);
            Ok(())
        }
        Ok(status) => Err(format!("mic subscription: status {}", status.0)),
        Err(why) => Err(why),
    };
    let _ = mic.RemoveValueChanged(token);
    // Best effort: the deck stops notifying a host that has gone.
    if let Ok(op) = mic.WriteClientCharacteristicConfigurationDescriptorAsync(
        GattClientCharacteristicConfigurationDescriptorValue::None,
    ) {
        let _ = op.get();
    }
    result
}

/// The ready link: drain the speaker queue whenever frames arrive, until the
/// deck disconnects, a write fails, or the backend stops.
fn serve(shared: &Shared, device: &BluetoothLEDevice, speaker: &GattCharacteristic, bell: &Bell, stop: &AtomicBool) {
    loop {
        if stop.load(Ordering::Acquire) || !connected(device) {
            return;
        }
        let mut frames: Vec<Vec<u8>> = Vec::new();
        shared.drain_for(Transport::Ble, || true, |frame| frames.push(frame.to_vec()));
        for frame in frames {
            let written = buffer(&frame)
                .and_then(|data| speaker.WriteValueWithOptionAsync(&data, GattWriteOption::WriteWithoutResponse))
                .and_then(|op| op.get());
            match written {
                Ok(GattCommunicationStatus::Success) => {}
                Ok(status) => {
                    tracing::warn!(status = status.0, "deck speaker write failed");
                    return;
                }
                Err(e) => {
                    tracing::warn!(e = %e.message(), "deck speaker write failed");
                    return;
                }
            }
        }
        bell.wait(TICK);
    }
}
