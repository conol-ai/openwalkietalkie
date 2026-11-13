//! The macOS side of the seams: cpal for the streams, `objc2-core-audio` for
//! the three things cpal cannot answer.
//!
//! **V11 obsoletes §7.1's parenthesis.** "`objc2-core-audio` for the UID
//! lookup" is wrong for cpal 0.18.2: `Device::id()` reads
//! `kAudioDevicePropertyDeviceUID` itself, `Host::device_by_id` round-trips
//! it, and a `DeviceId` renders as `"coreaudio:<uid>"` — verified live
//! returning `coreaudio:C2-2D-81-AB-88-81:input`. That string goes straight
//! into `config.toml`, which is what §7 wants and what V8 makes mandatory
//! (the deck's `AudioDeviceID`s were 101/95 at research time and 99/93 when
//! the probes ran; they move on every connection).
//!
//! `objc2-core-audio` is still needed, for exactly three things:
//!
//! 1. **Property listeners while no stream is open.** `ScoExternal` is V7's
//!    routine path — the deck's input is this machine's default input
//!    device — and it happens when we have nothing open to be told about.
//! 2. **`kAudioDevicePropertyDeviceIsRunningSomewhere`**, which is how we
//!    know somebody else has the mic before the rate flips.
//! 3. **`kAudioDevicePropertyTransportType`**, because V12 found cpal's
//!    `description()` thin on macOS: *every* device, the deck included,
//!    reports `device_type: Unknown, interface_type: Unknown, manufacturer:
//!    None`. §7's "the first Bluetooth device whose UID ends in `:output`"
//!    cannot be resolved through cpal at all.
//!
//! Nothing here ever touches the system default device (§4).
use super::engine::OutputSink;
use super::lanes::Mixer;
use super::traits::{
    AudioError, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, DeviceSide, Fault,
    RouteEvent, StreamFaults,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, DeviceId, ErrorKind, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use deck_model::VoiceInput;
use objc2_core_audio::{
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceIsRunningSomewhere,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyTransportType, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectSystemObject, AudioObjectAddPropertyListener, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener,
};
use objc2_core_foundation::{CFRetained, CFString};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

/// `kAudioDeviceTransportTypeBluetooth` — `'blue'`, which is what the deck
/// reports (V12's only usable discriminator).
pub const TRANSPORT_BLUETOOTH: u32 = u32::from_be_bytes(*b"blue");
/// `kAudioDeviceTransportTypeBluetoothLE`.
pub const TRANSPORT_BLUETOOTH_LE: u32 = u32::from_be_bytes(*b"blea");

/// cpal's host prefix on macOS; a `DeviceId` prints as `"coreaudio:<uid>"`.
pub const HOST_PREFIX: &str = "coreaudio";

/// The `DeviceId` string for one side of a deck UID (V11).
///
/// `[voice] device_uid = "C2-2D-81-AB-88-81"` names the *pair*; CoreAudio
/// has two devices, suffixed `:input` and `:output`.
#[must_use]
pub fn device_id(uid: &str, side: DeviceSide) -> String {
    let suffix = match side {
        DeviceSide::Input => "input",
        DeviceSide::Output => "output",
    };
    if uid.ends_with(":input") || uid.ends_with(":output") {
        format!("{HOST_PREFIX}:{uid}")
    } else {
        format!("{HOST_PREFIX}:{uid}:{suffix}")
    }
}

/// Find a cpal device by UID, natively (V11). Never the system default.
#[must_use]
pub fn device_by_uid(uid: &str, side: DeviceSide) -> Option<Device> {
    let host = cpal::default_host();
    let id: DeviceId = device_id(uid, side).parse().ok()?;
    host.device_by_id(&id)
}

/// Translate one cpal error into the fault the route SM understands (V17,
/// V19).
#[must_use]
pub fn fault_of(kind: ErrorKind) -> Fault {
    match kind {
        ErrorKind::Xrun => Fault::Xrun,
        ErrorKind::StreamInvalidated => Fault::Invalidated,
        ErrorKind::DeviceNotAvailable | ErrorKind::DeviceChanged => Fault::Gone,
        ErrorKind::PermissionDenied => Fault::Denied,
        _ => Fault::Other,
    }
}

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

/// §7.1's microphone: cpal, by UID, at **exactly the device's own nominal
/// config**.
///
/// Never another rate: cpal answers a different rate by setting
/// `kAudioDevicePropertyNominalSampleRate` and waiting up to a second, and a
/// Bluetooth HFP endpoint cannot honour it. V8 measured the deck's input at
/// `AvailableNominalSampleRates == {16000}`, so there is nothing to choose.
pub struct CpalCapture {
    stream: Option<Stream>,
    opened: Option<CaptureOpened>,
}

impl Default for CpalCapture {
    fn default() -> Self {
        CpalCapture::new()
    }
}

impl CpalCapture {
    #[must_use]
    pub fn new() -> Self {
        CpalCapture {
            stream: None,
            opened: None,
        }
    }

    /// Which input an open asks for, decided without asking CoreAudio.
    ///
    /// The deck's input is opened by its UID or not at all: a press whose
    /// deck has no UID, or whose UID is not among the devices, is answered
    /// with silence and `no_device`, never with the Mac's own microphone,
    /// which would record the room instead of the headset (decision 11).
    /// Before this a deck press with no UID fell through to the system
    /// default input whatever `[voice] fallback_input` said. The Mac's input
    /// is for `voice.listen{input:"mac"}` alone.
    fn choose(req: &CaptureOpen) -> Pick {
        match (req.input, req.device_uid.as_deref()) {
            (VoiceInput::Mac, _) => Pick::SystemDefault,
            (VoiceInput::Deck, Some(uid)) => Pick::ByUid(uid.to_string()),
            (VoiceInput::Deck, None) => {
                Pick::NoDevice("the deck's input has no device UID: set [voice] device_uid".into())
            }
        }
    }

    fn pick(req: &CaptureOpen) -> Result<Device, AudioError> {
        match CpalCapture::choose(req) {
            Pick::ByUid(uid) => {
                device_by_uid(&uid, DeviceSide::Input).ok_or(AudioError::NoDevice(uid))
            }
            Pick::SystemDefault => cpal::default_host()
                .default_input_device()
                .ok_or_else(|| AudioError::NoDevice("system default input".into())),
            Pick::NoDevice(why) => Err(AudioError::NoDevice(why)),
        }
    }
}

/// [`CpalCapture::choose`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pick {
    /// The deck's input, by its UID pair (`:input` appended).
    ByUid(String),
    /// The Mac's default input device.
    SystemDefault,
    /// Nothing to open, and why.
    NoDevice(String),
}

impl CaptureSource for CpalCapture {
    fn open(
        &mut self,
        req: &CaptureOpen,
        mut sink: CaptureSink,
    ) -> Result<CaptureOpened, AudioError> {
        self.close();
        let device = CpalCapture::pick(req)?;
        let uid = device.id().ok().map(|id| id.id().to_string());
        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        let rate = supported.sample_rate();
        let channels = supported.channels();
        let config: StreamConfig = supported.into();
        let faults = sink.faults();
        let channel_count = usize::from(channels).max(1);
        // Allocated **here**, at build time, never in the callback: a
        // multi-channel input (the Mac's own mic, for
        // `voice.listen{input:"mac"}`) has to be
        // de-interleaved to the mono §7.1 promises, and the copy needs
        // somewhere to go. Sized for 8192 frames, which is sixteen of V18's
        // quanta; a longer buffer is truncated rather than reallocated.
        let mut scratch = vec![0.0f32; 8192];
        let stream = match supported.sample_format() {
            SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
                config,
                move |data, _| {
                    // Real-time: no allocation, no lock, no logging.
                    if channel_count == 1 {
                        sink.deliver(rate, data);
                        return;
                    }
                    let frames = (data.len() / channel_count).min(scratch.len());
                    for (i, slot) in scratch[..frames].iter_mut().enumerate() {
                        // Channel 0, not an average: an average of a stereo
                        // mic is not the same signal and the recogniser was
                        // tuned on one channel.
                        *slot = data[i * channel_count];
                    }
                    sink.deliver(rate, &scratch[..frames]);
                },
                move |e| faults.raise(fault_of(e.kind())),
                None,
            ),
            other => {
                return Err(AudioError::Refused(format!(
                    "the device offers {other:?}, not f32"
                )))
            }
        }
        .map_err(|e| match e.kind() {
            ErrorKind::PermissionDenied => AudioError::Refused("microphone not allowed".into()),
            _ => AudioError::Refused(e.to_string()),
        })?;
        stream
            .play()
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        let opened = CaptureOpened {
            rate,
            channels,
            uid,
        };
        self.stream = Some(stream);
        self.opened = Some(opened.clone());
        Ok(opened)
    }

    fn close(&mut self) {
        // Dropping the stream stops IO, which is what ends the SCO link and
        // starts the 2307 ms A2DP resume timer (V1).
        self.stream = None;
        self.opened = None;
    }

    fn is_open(&self) -> bool {
        self.stream.is_some()
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// §7.2's output stream, pinned to the deck's `:output` UID.
pub struct CpalOutput {
    stream: Option<Stream>,
    uid: Option<String>,
}

impl Default for CpalOutput {
    fn default() -> Self {
        CpalOutput::new(None)
    }
}

impl CpalOutput {
    #[must_use]
    pub fn new(uid: Option<String>) -> Self {
        CpalOutput { stream: None, uid }
    }

    pub fn set_uid(&mut self, uid: Option<String>) {
        self.uid = uid;
    }
}

impl OutputSink for CpalOutput {
    fn open(
        &mut self,
        rate: u32,
        mut mixer: Mixer,
        faults: Arc<StreamFaults>,
    ) -> Result<(), AudioError> {
        self.close();
        let device = match self.uid.as_deref() {
            Some(uid) => device_by_uid(uid, DeviceSide::Output)
                .ok_or_else(|| AudioError::NoDevice(uid.to_string()))?,
            None => cpal::default_host()
                .default_output_device()
                .ok_or_else(|| AudioError::NoDevice("system default output".into()))?,
        };
        let supported = device
            .default_output_config()
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        let mut config: StreamConfig = supported.into();
        // V3: whatever the device is at **now**. The caller already decided
        // that; this only sanity-checks it against what cpal reports.
        config.sample_rate = rate;
        config.channels = 2;
        let stream = device
            .build_output_stream::<f32, _, _>(
                config,
                move |out, _| mixer.render(out),
                // V19: this runs on the real-time thread. One `fetch_or`.
                move |e| faults.raise(fault_of(e.kind())),
                None,
            )
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        stream
            .play()
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn close(&mut self) {
        // Dropping it drops the render closure, which drops the `Mixer`,
        // whose `Drop` puts the lane consumers back (§7.2).
        self.stream = None;
    }

    fn is_open(&self) -> bool {
        self.stream.is_some()
    }

    fn set_device(&mut self, uid: Option<String>) {
        if self.uid != uid {
            self.uid = uid;
            // The next open uses it; closing here would cut an announcement
            // in half because somebody saved `config.toml`.
        }
    }
}

// ---------------------------------------------------------------------------
// The property listeners (V11's remaining three jobs)
// ---------------------------------------------------------------------------

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read a fixed-size property off an audio object.
fn get<T: Copy>(object: AudioObjectID, selector: u32, empty: T) -> Option<T> {
    let mut addr = address(selector);
    let mut value = empty;
    #[allow(clippy::cast_possible_truncation)]
    let mut size = std::mem::size_of::<T>() as u32;
    // SAFETY: `addr` and `value` are live locals of the right sizes, and
    // `size` is `value`'s own size. CoreAudio writes at most `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    (status == 0).then_some(value)
}

/// Every audio object on the system.
#[must_use]
pub fn devices() -> Vec<AudioObjectID> {
    let mut addr = address(kAudioHardwarePropertyDevices);
    let mut size = 0u32;
    #[allow(clippy::cast_sign_loss)]
    let system = kAudioObjectSystemObject as AudioObjectID;
    // SAFETY: `addr` and `size` are live locals; this call only reports a size.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut ids = vec![0 as AudioObjectID; count];
    // SAFETY: `ids` holds exactly `size` bytes, which is what `size` says.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr().cast::<c_void>()).expect("a live vec"),
        )
    };
    if status == 0 {
        ids
    } else {
        Vec::new()
    }
}

/// A `CFStringRef`-valued property, as a `String`.
fn get_string(device: AudioObjectID, selector: u32) -> Option<String> {
    let raw: *const CFString = get(device, selector, std::ptr::null())?;
    let ptr = NonNull::new(raw.cast_mut())?;
    // SAFETY: both selectors used here return a +1 `CFStringRef` the caller
    // owns, which is exactly `CFRetained::from_raw`'s contract.
    let string = unsafe { CFRetained::<CFString>::from_raw(ptr) };
    Some(string.to_string())
}

/// One device's CoreAudio UID.
#[must_use]
pub fn uid_of(device: AudioObjectID) -> Option<String> {
    get_string(device, kAudioDevicePropertyDeviceUID)
}

/// One device's name. §7's default for `[voice] device_uid` needs it, and
/// V12 says cpal cannot supply it in a usable form on macOS.
#[must_use]
pub fn name_of(device: AudioObjectID) -> Option<String> {
    get_string(device, kAudioObjectPropertyName)
}

/// The `AudioObjectID` behind a UID, now. It changes on every connection
/// (V8: 101/95 at research time, 99/93 when the probes ran), which is why
/// nothing caches it across a disconnect.
#[must_use]
pub fn object_for_uid(uid: &str) -> Option<AudioObjectID> {
    devices()
        .into_iter()
        .find(|d| uid_of(*d).as_deref() == Some(uid))
}

/// V12: cpal's `description()` cannot tell a Bluetooth device from a
/// built-in one on macOS — every device reports `Unknown` — so §7's "the
/// first **Bluetooth** device whose UID ends in `:output`" needs this.
#[must_use]
pub fn is_bluetooth(device: AudioObjectID) -> bool {
    matches!(
        get(device, kAudioDevicePropertyTransportType, 0u32),
        Some(TRANSPORT_BLUETOOTH | TRANSPORT_BLUETOOTH_LE)
    )
}

/// The deck's advertised CoreAudio name (`list_audio`, both probe runs).
pub const DECK_NAME: &str = "OpenWalkieTalkie";

/// §7's default for `[voice] device_uid`: the first **Bluetooth** device
/// whose UID ends in `:output` **and whose name is `OpenWalkieTalkie`**.
///
/// The name is not decoration, and a re-run of `list_audio.c` on
/// 2026-09-21 shows why: this machine currently has *two* Bluetooth
/// `:output` devices — `20-F4-D4-69-62-F6:output` ("Tony's AirPods Pro 3",
/// id 119, and the system default output) and `C2-2D-81-AB-88-81:output`
/// ("OpenWalkieTalkie", id 93). CoreAudio returned the AirPods **first**. A rule
/// that stopped at "Bluetooth, ends in `:output`" would therefore have
/// configured the daemon to speak into the operator's earbuds, and V12's
/// summary of the rule — which omits the name — is the half that bites.
#[must_use]
pub fn discover_deck() -> Option<String> {
    devices()
        .into_iter()
        .filter(|d| is_bluetooth(*d))
        .filter(|d| name_of(*d).as_deref() == Some(DECK_NAME))
        .filter_map(uid_of)
        .find(|uid| uid.ends_with(":output"))
        .map(|uid| uid.trim_end_matches(":output").to_string())
}

/// The device's nominal rate right now — what V3's `Reopening` opens at.
#[must_use]
pub fn nominal_rate(device: AudioObjectID) -> Option<u32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    get(device, kAudioDevicePropertyNominalSampleRate, 0f64).map(|r| r.round() as u32)
}

/// V11's second job: is somebody else running IO on this device?
#[must_use]
pub fn is_running_somewhere(device: AudioObjectID) -> bool {
    get(device, kAudioDevicePropertyDeviceIsRunningSomewhere, 0u32).is_some_and(|v| v != 0)
}

#[must_use]
pub fn is_alive(device: AudioObjectID) -> bool {
    get(device, kAudioDevicePropertyDeviceIsAlive, 0u32).is_some_and(|v| v != 0)
}

/// What a listener registration carries to the C callback.
struct Watch {
    side: DeviceSide,
    events: Sender<RouteEvent>,
}

/// V11's first job: a listener that fires **while no stream is open**.
///
/// Without it, `ScoExternal` — V7's routine path, because the deck is this
/// machine's default input device — is invisible: we have nothing open for
/// cpal to invalidate, so the only thing that could tell us is the property
/// itself.
///
/// The registered listener is never called on the real-time thread (it is a
/// HAL notification, not an IO callback), so a channel send is allowed here
/// in a way it is not in the error callback (V19).
pub struct DeviceWatcher {
    watches: Vec<(AudioObjectID, *mut Watch)>,
}

// SAFETY: the only non-`Send` field is a raw pointer to a heap `Watch`,
// which is owned solely by this struct and freed only in `Drop`.
unsafe impl Send for DeviceWatcher {}

impl std::fmt::Debug for DeviceWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceWatcher")
            .field("watching", &self.watches.len())
            .finish()
    }
}

/// The selectors a watch registers, and what each one becomes.
const WATCHED: [u32; 3] = [
    kAudioDevicePropertyNominalSampleRate,
    kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyDeviceIsRunningSomewhere,
];

unsafe extern "C-unwind" fn on_property(
    object: AudioObjectID,
    count: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    data: *mut c_void,
) -> i32 {
    // SAFETY: `data` is the `Box<Watch>` this listener was registered with,
    // and CoreAudio removes the listener before `Drop` frees it.
    let Some(watch) = (unsafe { data.cast::<Watch>().as_ref() }) else {
        return 0;
    };
    for i in 0..count as usize {
        // SAFETY: CoreAudio promises `count` contiguous addresses.
        let addr = unsafe { *addresses.as_ptr().add(i) };
        let event = match addr.mSelector {
            s if s == kAudioDevicePropertyNominalSampleRate && watch.side == DeviceSide::Output => {
                nominal_rate(object).map(|rate| RouteEvent::NominalRate { rate })
            }
            s if s == kAudioDevicePropertyDeviceIsAlive => Some(RouteEvent::DevicePresent {
                side: watch.side,
                present: is_alive(object),
            }),
            s if s == kAudioDevicePropertyDeviceIsRunningSomewhere
                && watch.side == DeviceSide::Input =>
            {
                Some(RouteEvent::ExternalCapture {
                    running: is_running_somewhere(object),
                })
            }
            _ => None,
        };
        if let Some(event) = event {
            let _ = watch.events.send(event);
        }
    }
    0
}

impl DeviceWatcher {
    #[must_use]
    pub fn new() -> Self {
        DeviceWatcher {
            watches: Vec::new(),
        }
    }

    /// Watch one side of the deck, by UID. Returns false when the UID is
    /// not present right now — which is the BLE-HID-only case and not an
    /// error.
    pub fn watch(&mut self, uid: &str, side: DeviceSide, events: Sender<RouteEvent>) -> bool {
        let full = match side {
            DeviceSide::Input => format!("{uid}:input"),
            DeviceSide::Output => format!("{uid}:output"),
        };
        let Some(device) = object_for_uid(&full).or_else(|| object_for_uid(uid)) else {
            return false;
        };
        let watch = Box::into_raw(Box::new(Watch {
            side,
            events: events.clone(),
        }));
        for selector in WATCHED {
            let mut addr = address(selector);
            // SAFETY: `addr` is a live local for the duration of the call,
            // `on_property` matches the expected signature, and `watch` stays
            // alive until `Drop` removes every listener that uses it.
            unsafe {
                AudioObjectAddPropertyListener(
                    device,
                    NonNull::from(&mut addr),
                    Some(on_property),
                    watch.cast::<c_void>(),
                );
            }
        }
        self.watches.push((device, watch));
        // Seed the state: a listener only reports *changes*, and the route
        // SM has to start from where the device actually is.
        let _ = events.send(RouteEvent::DevicePresent {
            side,
            present: is_alive(device),
        });
        if side == DeviceSide::Output {
            if let Some(rate) = nominal_rate(device) {
                let _ = events.send(RouteEvent::NominalRate { rate });
            }
        } else {
            let _ = events.send(RouteEvent::ExternalCapture {
                running: is_running_somewhere(device),
            });
        }
        true
    }

    #[must_use]
    pub fn watching(&self) -> usize {
        self.watches.len()
    }
}

impl Default for DeviceWatcher {
    fn default() -> Self {
        DeviceWatcher::new()
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        for (device, watch) in self.watches.drain(..) {
            for selector in WATCHED {
                let mut addr = address(selector);
                // SAFETY: removing exactly what `watch` registered, before
                // the `Box` below frees the client data it points at.
                unsafe {
                    AudioObjectRemovePropertyListener(
                        device,
                        NonNull::from(&mut addr),
                        Some(on_property),
                        watch.cast::<c_void>(),
                    );
                }
            }
            // SAFETY: the pointer came from `Box::into_raw` in `watch` and
            // every listener that could dereference it has been removed.
            drop(unsafe { Box::from_raw(watch) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V11's string, which is what `config.toml` stores. Getting this wrong
    /// means the daemon opens *a* microphone rather than *the* microphone,
    /// and V8 says the ids it might otherwise use move on every connection.
    #[test]
    fn a_device_id_is_the_uid_with_a_side_and_cpals_host_prefix() {
        assert_eq!(
            device_id("C2-2D-81-AB-88-81", DeviceSide::Input),
            "coreaudio:C2-2D-81-AB-88-81:input"
        );
        assert_eq!(
            device_id("C2-2D-81-AB-88-81", DeviceSide::Output),
            "coreaudio:C2-2D-81-AB-88-81:output"
        );
        // A config that already names a side is taken at its word.
        assert_eq!(
            device_id("C2-2D-81-AB-88-81:output", DeviceSide::Input),
            "coreaudio:C2-2D-81-AB-88-81:output"
        );
        // And it parses back, which is the half that makes `device_by_id`
        // work at all.
        let id: DeviceId = device_id("C2-2D-81-AB-88-81", DeviceSide::Input)
            .parse()
            .expect("cpal parses its own format");
        assert_eq!(id.id(), "C2-2D-81-AB-88-81:input");
    }

    /// Decision 11 on the Classic transport, decided without CoreAudio: a
    /// press opens the deck by its UID or answers `no_device`, and only
    /// `voice.listen{input:"mac"}` reaches the Mac's default input. (A UID
    /// that is not among the devices is `NoDevice(uid)` in `pick`, from the
    /// lookup this choice hands it to.)
    #[test]
    fn a_press_opens_the_deck_by_uid_or_nothing_and_never_the_macs_input() {
        let open = |input: VoiceInput, device_uid: Option<&str>| CaptureOpen {
            input,
            device_uid: device_uid.map(String::from),
            ask_deck: None,
        };
        assert_eq!(
            CpalCapture::choose(&open(VoiceInput::Deck, Some("C2-2D-81-AB-88-81"))),
            Pick::ByUid("C2-2D-81-AB-88-81".into())
        );
        let Pick::NoDevice(why) = CpalCapture::choose(&open(VoiceInput::Deck, None)) else {
            panic!("a deck press with no UID must not open anything");
        };
        assert_eq!(
            why,
            "the deck's input has no device UID: set [voice] device_uid"
        );
        assert_eq!(
            AudioError::NoDevice(why).code(),
            "no_device",
            "silence and a code"
        );
        for uid in [None, Some("C2-2D-81-AB-88-81")] {
            assert_eq!(
                CpalCapture::choose(&open(VoiceInput::Mac, uid)),
                Pick::SystemDefault,
                "voice.listen{{input:\"mac\"}} with {uid:?}"
            );
        }
    }

    /// V17/V19's translation table, which is the whole of what the route SM
    /// is allowed to learn from cpal.
    #[test]
    fn every_cpal_error_maps_to_a_fault_the_route_machine_knows() {
        assert_eq!(fault_of(ErrorKind::Xrun), Fault::Xrun);
        assert_eq!(fault_of(ErrorKind::StreamInvalidated), Fault::Invalidated);
        assert_eq!(fault_of(ErrorKind::DeviceNotAvailable), Fault::Gone);
        assert_eq!(fault_of(ErrorKind::DeviceChanged), Fault::Gone);
        assert_eq!(fault_of(ErrorKind::PermissionDenied), Fault::Denied);
        assert_eq!(fault_of(ErrorKind::BackendError), Fault::Other);
    }

    /// V12's four-character code, spelled out so a future reader does not
    /// have to trust a magic number.
    #[test]
    fn the_bluetooth_transport_type_is_the_four_character_code_blue() {
        assert_eq!(TRANSPORT_BLUETOOTH, 0x626c_7565);
        assert_eq!(TRANSPORT_BLUETOOTH_LE, 0x626c_6561);
    }

    /// Enumerating is read-only and must be safe to do with no deck, no
    /// grant and no stream — the one thing every CI run does.
    ///
    /// It asserts nothing about *which* devices exist, because a CI machine
    /// has whatever it has; it asserts that asking does not crash and that
    /// the UID of anything found round-trips to the same object.
    #[test]
    fn enumerating_devices_is_safe_with_no_deck_and_no_grant() {
        let all = devices();
        for device in all.iter().take(8) {
            if let Some(uid) = uid_of(*device) {
                assert!(!uid.is_empty());
                assert_eq!(
                    object_for_uid(&uid),
                    Some(*device),
                    "a UID must find its way back to the same object"
                );
            }
            // These are reads; none of them changes anything.
            let _ = is_bluetooth(*device);
            let _ = is_running_somewhere(*device);
            let _ = is_alive(*device);
            let _ = nominal_rate(*device);
        }
        // `discover_deck` is §7's `[voice] device_uid` default. With no deck
        // connected it answers `None` rather than guessing.
        if let Some(uid) = discover_deck() {
            assert!(!uid.ends_with(":output"), "the pair, not one side: {uid}");
        }
    }

    /// A watcher with nothing to watch is a no-op, and dropping it removes
    /// what it registered. The listener's client data is a raw `Box`, so a
    /// leak or a double free here would be a crash somewhere else entirely.
    #[test]
    fn a_watcher_for_an_absent_uid_registers_nothing_and_drops_clean() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut watcher = DeviceWatcher::new();
        assert!(!watcher.watch("00-00-00-00-00-00", DeviceSide::Output, tx));
        assert_eq!(watcher.watching(), 0);
        drop(watcher);
        assert!(rx.try_recv().is_err(), "and it said nothing");
    }
}
