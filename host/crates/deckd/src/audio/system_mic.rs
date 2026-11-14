//! The computer's own microphone on Windows, for `voice.listen{input:"mac"}`
//! (the API's word for "this computer's input", from the macOS original):
//! cpal over WASAPI, the system default input at its own shared-mode format,
//! channel 0 of it, as `coreaudio::CpalCapture` does on a Mac.
//!
//! The deck's microphone is never looked for here: it arrives over the
//! deck's link (`audio::ble`), and a deck press is never answered with the
//! room (decision 11).
use super::traits::{AudioError, CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, Fault};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, Stream, StreamConfig};
use deck_model::VoiceInput;

/// One cpal error as the route machine's fault (`coreaudio::fault_of`'s
/// table, which is macOS-only there).
fn fault_of(kind: ErrorKind) -> Fault {
    match kind {
        ErrorKind::Xrun => Fault::Xrun,
        ErrorKind::StreamInvalidated => Fault::Invalidated,
        ErrorKind::DeviceNotAvailable | ErrorKind::DeviceChanged => Fault::Gone,
        ErrorKind::PermissionDenied => Fault::Denied,
        _ => Fault::Other,
    }
}

#[derive(Default)]
pub struct SystemMic {
    stream: Option<Stream>,
}

impl CaptureSource for SystemMic {
    fn open(&mut self, req: &CaptureOpen, mut sink: CaptureSink) -> Result<CaptureOpened, AudioError> {
        self.close();
        if req.input != VoiceInput::Mac {
            return Err(AudioError::NoDevice("the deck's microphone comes over its link".into()));
        }
        let device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| AudioError::NoDevice("system default input".into()))?;
        let uid = device.id().ok().map(|id| id.id().to_string());
        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::Refused(e.to_string()))?;
        let rate = supported.sample_rate();
        let channels = supported.channels();
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let faults = sink.faults();
        let channel_count = usize::from(channels).max(1);
        // Allocated here, never in the callback (see `CpalCapture::open`).
        let mut scratch = vec![0.0f32; 8192];
        let stream = match format {
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
                        *slot = data[i * channel_count];
                    }
                    sink.deliver(rate, &scratch[..frames]);
                },
                move |e| faults.raise(fault_of(e.kind())),
                None,
            ),
            other => return Err(AudioError::Refused(format!("the device offers {other:?}, not f32"))),
        }
        .map_err(|e| AudioError::Refused(e.to_string()))?;
        stream.play().map_err(|e| AudioError::Refused(e.to_string()))?;
        self.stream = Some(stream);
        Ok(CaptureOpened { rate, channels, uid })
    }

    fn close(&mut self) {
        self.stream = None;
    }

    fn is_open(&self) -> bool {
        self.stream.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::traits::StreamFaults;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn open(input: VoiceInput) -> CaptureOpen {
        CaptureOpen {
            input,
            device_uid: None,
            ask_deck: None,
        }
    }

    /// The computer's own input opens and delivers mono samples at its own
    /// rate; a machine without one says so, and nothing is asserted of it.
    /// The deck's input is never answered from here.
    #[test]
    fn the_computers_input_delivers_and_the_decks_is_not_opened_here() {
        let mut mic = SystemMic::default();
        let sink = || CaptureSink::new(Arc::new(StreamFaults::new()), |_, _| {});
        assert!(matches!(mic.open(&open(VoiceInput::Deck), sink()), Err(AudioError::NoDevice(_))));
        assert!(!mic.is_open());
        let heard = Arc::new(AtomicUsize::new(0));
        let counted = heard.clone();
        let opened = mic.open(
            &open(VoiceInput::Mac),
            CaptureSink::new(Arc::new(StreamFaults::new()), move |rate, samples| {
                assert!(rate > 0);
                counted.fetch_add(samples.len(), Ordering::Relaxed);
            }),
        );
        let opened = match opened {
            Ok(opened) => opened,
            Err(AudioError::NoDevice(_)) => return, // no microphone here
            Err(e) => panic!("the default input would not open: {e:?}"),
        };
        assert!(mic.is_open());
        assert!(opened.rate > 0 && opened.channels > 0, "{opened:?}");
        std::thread::sleep(std::time::Duration::from_millis(600));
        mic.close();
        assert!(!mic.is_open());
        assert!(heard.load(Ordering::Relaxed) > 0, "no samples in 600 ms");
    }
}
