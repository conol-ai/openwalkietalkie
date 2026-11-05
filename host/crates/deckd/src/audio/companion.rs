//! A native companion's own recogniser (`caps.transcribe: "push"`).
//!
//! Conol's chats are answered by a cloud model, and Conol has its own,
//! much larger speech model behind its web composer. A press addressed to
//! one of its agents is therefore recorded here, offered to the connection
//! that owns the agent (`agent.transcribe`), fetched by it in pages
//! (`agent.transcribe.audio`) and answered with text
//! (`agent.transcribe.result`). That text becomes the press's final
//! transcript, so everything after it — the deck's transcript window, the
//! order, a dictated answer to a question card — is the path every other
//! press takes.
//!
//! The local recogniser is never switched off for such a press: it is fed
//! the same samples, its partials are what the deck shows while the
//! operator talks, and its final is the answer whenever the companion
//! fails, refuses, sends nothing, disconnects or is later than
//! [`COMPANION_BUDGET`]. A companion can only ever make a press better.
//!
//! [`CompanionHub`] is the one piece of state the audio thread and the core
//! share: the audio thread files an offer and polls for its answer on the
//! engine's tick; the core binds the offer to the owning session, pages the
//! WAV out and files the answer.

use super::traits::{AudioError, BoxFuture, SttEvent, SttSession};
use deck_model::{AgentId, SessionId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// The rate of the WAV a companion receives. Speech models want 16 kHz,
/// and the deck's own microphone already is; the Mac-microphone fallback
/// (44.1/48 kHz) is resampled down, which also keeps a full minute under
/// the 4.5 MB request bodies hosted functions accept.
pub const WAV_RATE: u32 = 16_000;
/// How long the companion has, from the release, before the local final
/// is used instead. Conol's hosted model answers a minute of speech in a
/// few seconds; its own fallback chain may take a few more.
pub const COMPANION_BUDGET: Duration = Duration::from_secs(20);
/// A press shorter than this is a slip of the thumb: not worth a round
/// trip, and the local recogniser's empty answer is the right one.
const MIN_AUDIO: Duration = Duration::from_millis(300);
/// The deck caps a press at 60 s; a little headroom for the tail.
const MAX_AUDIO: Duration = Duration::from_secs(65);
/// The page size of `agent.transcribe.audio`, in bytes of WAV (base64 on
/// the wire, a third larger: well under the API's 256 KiB line).
pub const PAGE: usize = 96 * 1024;

/// Why the hub refused a companion's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubError {
    /// No such offer: never made, already answered, or given up on.
    NotFound,
    /// The offer belongs to another connection.
    NotOwner,
}

struct Offer {
    agent: AgentId,
    wav: Arc<Vec<u8>>,
    ms: u32,
    owner: Option<SessionId>,
    result: Option<Result<String, String>>,
}

/// The offers in flight, shared by the audio thread and the core.
#[derive(Default)]
pub struct CompanionHub {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    offers: HashMap<u64, Offer>,
}

/// The wire id of an offer.
#[must_use]
pub fn offer_id(id: u64) -> String {
    format!("tr_{id}")
}

/// An offer's number from its wire id.
#[must_use]
pub fn parse_offer_id(s: &str) -> Option<u64> {
    s.strip_prefix("tr_")?.parse().ok()
}

/// What the core needs to announce an offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferInfo {
    pub agent: AgentId,
    pub ms: u32,
    pub bytes: usize,
}

impl CompanionHub {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// File a recording for `agent`'s owner; the core announces it.
    pub fn offer(&self, agent: AgentId, wav: Vec<u8>, ms: u32) -> u64 {
        let mut inner = self.lock();
        inner.next += 1;
        let id = inner.next;
        inner.offers.insert(id, Offer { agent, wav: Arc::new(wav), ms, owner: None, result: None });
        id
    }

    #[must_use]
    pub fn info(&self, id: u64) -> Option<OfferInfo> {
        self.lock().offers.get(&id).map(|o| OfferInfo { agent: o.agent.clone(), ms: o.ms, bytes: o.wav.len() })
    }

    /// The core announced the offer to `session`: only it may read or
    /// answer it from here on.
    pub fn bind(&self, id: u64, session: SessionId) -> bool {
        match self.lock().offers.get_mut(&id) {
            Some(offer) if offer.result.is_none() => {
                offer.owner = Some(session);
                true
            }
            _ => false,
        }
    }

    fn owned(inner: &mut Inner, id: u64, session: SessionId) -> Result<&mut Offer, HubError> {
        let offer = inner.offers.get_mut(&id).filter(|o| o.result.is_none()).ok_or(HubError::NotFound)?;
        if offer.owner == Some(session) { Ok(offer) } else { Err(HubError::NotOwner) }
    }

    /// The recording, for `agent.transcribe.audio`.
    pub fn audio(&self, id: u64, session: SessionId) -> Result<Arc<Vec<u8>>, HubError> {
        Self::owned(&mut self.lock(), id, session).map(|o| o.wav.clone())
    }

    /// `agent.transcribe.result`: the first answer stands.
    pub fn resolve(&self, id: u64, session: SessionId, result: Result<String, String>) -> Result<(), HubError> {
        Self::owned(&mut self.lock(), id, session)?.result = Some(result);
        Ok(())
    }

    /// The core could not hand the offer to anybody.
    pub fn fail(&self, id: u64, why: &str) {
        if let Some(offer) = self.lock().offers.get_mut(&id) {
            offer.result.get_or_insert_with(|| Err(why.to_string()));
        }
    }

    /// `session` is gone: everything it was asked fails now, rather than
    /// at the budget, so the local final is used at once.
    pub fn fail_session(&self, session: SessionId) {
        for offer in self.lock().offers.values_mut() {
            if offer.owner == Some(session) {
                offer.result.get_or_insert_with(|| Err("the companion disconnected".into()));
            }
        }
    }

    fn poll(&self, id: u64) -> Option<Result<String, String>> {
        self.lock().offers.get(&id).and_then(|o| o.result.clone())
    }

    /// The press is decided; forget the recording.
    pub fn withdraw(&self, id: u64) {
        self.lock().offers.remove(&id);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().offers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Tells the core an offer is waiting (`FromAudio::Transcribe`).
pub type Announce = Box<dyn Fn(u64) + Send>;

/// A press for a transcribing companion: the local recogniser, tee'd.
pub struct CompanionSession {
    local: Option<Box<dyn SttSession>>,
    events: crossbeam_channel::Receiver<SttEvent>,
    agent: AgentId,
    hub: Arc<CompanionHub>,
    announce: Announce,
    pcm: Vec<f32>,
    rate: u32,
    /// The rate changed mid-press: the recording is not one signal any
    /// more, so only the local recogniser answers.
    broken: bool,
}

impl CompanionSession {
    #[must_use]
    pub fn new(
        local: Option<Box<dyn SttSession>>,
        agent: AgentId,
        hub: Arc<CompanionHub>,
        announce: Announce,
    ) -> Self {
        let events = local.as_ref().map_or_else(crossbeam_channel::never, |l| l.events());
        CompanionSession { local, events, agent, hub, announce, pcm: Vec::new(), rate: 0, broken: false }
    }

    fn recorded(&self) -> Duration {
        if self.rate == 0 { Duration::ZERO } else { Duration::from_secs_f64(self.pcm.len() as f64 / f64::from(self.rate)) }
    }
}

impl SttSession for CompanionSession {
    fn feed(&mut self, mono_f32: &[f32], rate: u32) {
        if let Some(local) = self.local.as_mut() {
            local.feed(mono_f32, rate);
        }
        if self.broken || rate == 0 {
            return;
        }
        if self.rate == 0 {
            self.rate = rate;
        } else if self.rate != rate {
            tracing::warn!(from = self.rate, to = rate, "companion transcription: the capture rate changed; local only");
            self.broken = true;
            self.pcm = Vec::new();
            return;
        }
        let room = (MAX_AUDIO.as_secs() as usize * rate as usize).saturating_sub(self.pcm.len());
        self.pcm.extend_from_slice(&mono_f32[..mono_f32.len().min(room)]);
    }

    fn events(&self) -> crossbeam_channel::Receiver<SttEvent> {
        self.events.clone()
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>> {
        let this = *self;
        let recorded = this.recorded();
        let local = this.local.map(SttSession::finish);
        if this.broken || recorded < MIN_AUDIO {
            return local.unwrap_or_else(|| Box::pin(async { Ok(String::new()) }));
        }
        let wav = wav16(&resample(&this.pcm, this.rate, WAV_RATE), WAV_RATE);
        let ms = u32::try_from(recorded.as_millis()).unwrap_or(u32::MAX);
        let id = this.hub.offer(this.agent.clone(), wav, ms);
        (this.announce)(id);
        Box::pin(CompanionFinal {
            id,
            agent: this.agent,
            hub: this.hub,
            local,
            local_result: None,
            deadline: Instant::now() + COMPANION_BUDGET,
            companion_done: false,
            started: Instant::now(),
        })
    }

    fn finish_budget(&self) -> Duration {
        let local = self.local.as_ref().map_or(Duration::ZERO, |l| l.finish_budget());
        local.max(COMPANION_BUDGET + Duration::from_secs(5))
    }

    fn cancel(self: Box<Self>) {
        if let Some(local) = self.local {
            local.cancel();
        }
    }
}

/// The release's answer: the companion's text, else the local final.
struct CompanionFinal {
    id: u64,
    agent: AgentId,
    hub: Arc<CompanionHub>,
    local: Option<BoxFuture<'static, Result<String, AudioError>>>,
    local_result: Option<Result<String, AudioError>>,
    deadline: Instant,
    companion_done: bool,
    started: Instant,
}

impl Future for CompanionFinal {
    type Output = Result<String, AudioError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        if let Some(local) = this.local.as_mut() {
            if let Poll::Ready(result) = local.as_mut().poll(cx) {
                this.local_result = Some(result);
                this.local = None;
            }
        }
        if !this.companion_done {
            let ms = this.started.elapsed().as_millis();
            match this.hub.poll(this.id) {
                Some(Ok(text)) if !text.trim().is_empty() => {
                    tracing::info!(agent = %this.agent.0, ms, "companion transcription: answered");
                    this.hub.withdraw(this.id);
                    this.companion_done = true;
                    this.local = None;
                    return Poll::Ready(Ok(text));
                }
                Some(Ok(_)) => {
                    tracing::info!(agent = %this.agent.0, ms, "companion transcription: empty; using the local final");
                    this.companion_done = true;
                }
                Some(Err(why)) => {
                    tracing::warn!(agent = %this.agent.0, ms, why, "companion transcription failed; using the local final");
                    this.companion_done = true;
                }
                None if Instant::now() >= this.deadline => {
                    tracing::warn!(agent = %this.agent.0, ms, "companion transcription timed out; using the local final");
                    this.companion_done = true;
                }
                None => {}
            }
            if this.companion_done {
                this.hub.withdraw(this.id);
            }
        }
        if !this.companion_done {
            return Poll::Pending;
        }
        match this.local_result.take() {
            Some(result) => Poll::Ready(result),
            None if this.local.is_none() => Poll::Ready(Err(AudioError::Engine(
                "the companion did not transcribe this and there is no local recogniser".into(),
            ))),
            None => Poll::Pending,
        }
    }
}

impl Drop for CompanionFinal {
    fn drop(&mut self) {
        self.hub.withdraw(self.id);
    }
}

/// Linear resampling of a whole recording. Speech at 16 kHz loses nothing
/// a recogniser uses, and a press is at most a minute long.
#[must_use]
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let len = ((input.len() as f64) / ratio).floor() as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let at = pos.floor() as usize;
            let frac = (pos - at as f64) as f32;
            let a = input[at.min(input.len() - 1)];
            let b = input[(at + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// 16-bit mono PCM WAV.
#[must_use]
pub fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::Waker;

    /// A local recogniser that answers `text` once `ready` is set.
    struct Local {
        text: Result<String, ()>,
        ready: Arc<std::sync::atomic::AtomicBool>,
        fed: Arc<AtomicU64>,
        tx: crossbeam_channel::Sender<SttEvent>,
        rx: crossbeam_channel::Receiver<SttEvent>,
    }

    impl Local {
        fn boxed(text: Result<&str, ()>, ready: bool) -> (Box<dyn SttSession>, Arc<std::sync::atomic::AtomicBool>, Arc<AtomicU64>) {
            let (tx, rx) = crossbeam_channel::unbounded();
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(ready));
            let fed = Arc::new(AtomicU64::new(0));
            (Box::new(Local { text: text.map(String::from), ready: flag.clone(), fed: fed.clone(), tx, rx }), flag, fed)
        }
    }

    impl SttSession for Local {
        fn feed(&mut self, mono: &[f32], _rate: u32) {
            self.fed.fetch_add(mono.len() as u64, Ordering::SeqCst);
            let _ = self.tx.send(SttEvent::Partial("local partial".into()));
        }
        fn events(&self) -> crossbeam_channel::Receiver<SttEvent> {
            self.rx.clone()
        }
        fn finish(self: Box<Self>) -> BoxFuture<'static, Result<String, AudioError>> {
            let ready = self.ready.clone();
            let text = self.text.clone();
            Box::pin(std::future::poll_fn(move |_| {
                if ready.load(Ordering::SeqCst) {
                    Poll::Ready(text.clone().map_err(|()| AudioError::Engine("local failed".into())))
                } else {
                    Poll::Pending
                }
            }))
        }
        fn cancel(self: Box<Self>) {}
    }

    fn poll(f: &mut BoxFuture<'static, Result<String, AudioError>>) -> Poll<Result<String, AudioError>> {
        f.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    }

    fn session(local: Option<Box<dyn SttSession>>, hub: &Arc<CompanionHub>) -> (Box<CompanionSession>, Arc<Mutex<Vec<u64>>>) {
        let announced = Arc::new(Mutex::new(Vec::new()));
        let log = announced.clone();
        let s = CompanionSession::new(local, AgentId("ag_conol".into()), hub.clone(), Box::new(move |id| log.lock().unwrap().push(id)));
        (Box::new(s), announced)
    }

    const SESSION: SessionId = SessionId(7);

    #[test]
    fn the_companion_text_wins_and_the_recording_is_forgotten() {
        let hub = CompanionHub::new();
        let (local, _, fed) = Local::boxed(Ok("apple text"), true);
        let (mut s, announced) = session(Some(local), &hub);
        s.feed(&vec![0.25; 16_000], 16_000);
        assert_eq!(fed.load(Ordering::SeqCst), 16_000, "the local recogniser hears every sample");
        assert!(s.events().try_recv().is_ok(), "local partials still reach the deck");
        let mut f = s.finish();
        let id = announced.lock().unwrap()[0];
        let info = hub.info(id).unwrap();
        assert_eq!((info.agent.0.as_str(), info.ms, info.bytes), ("ag_conol", 1000, 44 + 32_000));
        assert!(poll(&mut f).is_pending(), "the local final waits for the companion");
        assert!(hub.bind(id, SESSION));
        assert_eq!(hub.audio(id, SessionId(8)).unwrap_err(), HubError::NotOwner);
        let wav = hub.audio(id, SESSION).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        hub.resolve(id, SESSION, Ok("conol text".into())).unwrap();
        assert!(matches!(poll(&mut f), Poll::Ready(Ok(t)) if t == "conol text"));
        assert!(hub.is_empty());
    }

    #[test]
    fn a_failure_or_a_disconnect_falls_back_to_the_local_final() {
        for disconnect in [false, true] {
            let hub = CompanionHub::new();
            let (local, ready, _) = Local::boxed(Ok("apple text"), false);
            let (mut s, announced) = session(Some(local), &hub);
            s.feed(&vec![0.1; 8_000], 16_000);
            let mut f = s.finish();
            let id = announced.lock().unwrap()[0];
            hub.bind(id, SESSION);
            if disconnect { hub.fail_session(SESSION) } else { hub.resolve(id, SESSION, Err("503".into())).unwrap() }
            assert!(poll(&mut f).is_pending(), "the local final is still decoding");
            ready.store(true, Ordering::SeqCst);
            assert!(matches!(poll(&mut f), Poll::Ready(Ok(t)) if t == "apple text"));
            assert!(hub.is_empty());
        }
    }

    #[test]
    fn an_empty_answer_uses_the_local_final_and_no_local_is_an_error() {
        let hub = CompanionHub::new();
        let (local, _, _) = Local::boxed(Ok("apple text"), true);
        let (mut s, announced) = session(Some(local), &hub);
        s.feed(&vec![0.1; 8_000], 16_000);
        let mut f = s.finish();
        let id = announced.lock().unwrap()[0];
        hub.bind(id, SESSION);
        hub.resolve(id, SESSION, Ok("  ".into())).unwrap();
        assert!(matches!(poll(&mut f), Poll::Ready(Ok(t)) if t == "apple text"));

        let (mut s, announced) = session(None, &hub);
        s.feed(&vec![0.1; 8_000], 16_000);
        let mut f = s.finish();
        hub.fail(announced.lock().unwrap()[0], "no owner");
        assert!(matches!(poll(&mut f), Poll::Ready(Err(_))));
        assert!(hub.is_empty(), "dropping the answer forgets the offer");
    }

    #[test]
    fn a_short_press_or_a_rate_change_never_leaves_the_mac() {
        let hub = CompanionHub::new();
        let (local, _, _) = Local::boxed(Ok("hm"), true);
        let (mut s, announced) = session(Some(local), &hub);
        s.feed(&vec![0.1; 1_600], 16_000);
        let mut f = s.finish();
        assert!(matches!(poll(&mut f), Poll::Ready(Ok(t)) if t == "hm"));
        assert!(announced.lock().unwrap().is_empty());

        let (local, _, _) = Local::boxed(Ok("local"), true);
        let (mut s, announced) = session(Some(local), &hub);
        s.feed(&vec![0.1; 16_000], 16_000);
        s.feed(&vec![0.1; 48_000], 48_000);
        let mut f = s.finish();
        assert!(matches!(poll(&mut f), Poll::Ready(Ok(t)) if t == "local"));
        assert!(announced.lock().unwrap().is_empty());
    }

    #[test]
    fn the_mac_microphone_is_resampled_to_sixteen_kilohertz() {
        let hub = CompanionHub::new();
        let (mut s, announced) = session(None, &hub);
        s.feed(&vec![0.5; 48_000], 48_000);
        let _f = s.finish();
        let info = hub.info(announced.lock().unwrap()[0]).unwrap();
        assert_eq!((info.ms, info.bytes), (1000, 44 + 32_000));
        assert_eq!(resample(&[0.0, 1.0, 0.0, 1.0], 32_000, 16_000), vec![0.0, 0.0]);
    }

    #[test]
    fn a_late_answer_is_refused_and_ids_round_trip() {
        let hub = CompanionHub::new();
        let id = hub.offer(AgentId("a".into()), vec![1, 2, 3], 10);
        assert!(hub.bind(id, SESSION));
        hub.resolve(id, SESSION, Ok("first".into())).unwrap();
        assert_eq!(hub.resolve(id, SESSION, Ok("second".into())).unwrap_err(), HubError::NotFound);
        assert_eq!(parse_offer_id(&offer_id(id)), Some(id));
        assert_eq!(parse_offer_id("ut_1"), None);
    }
}
