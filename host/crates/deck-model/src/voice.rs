//! The PTT session at the model level (§7.4): start on a deck press, finish
//! or abort on the release, `up`, a link drop, the 60 s cap. Capture, STT
//! and the transcript dispatch are the core's (`Effect::Audio`).
//!
//! …and **the §5.6 spoken grammar**, which lives here because §5.6 says it is
//! shared and it was not: `deckd::adapters::claude` had its own list with
//! `yep|yeah|nope` in it and `deckd::adapters::codex` had its own with `sure`,
//! so the same sentence meant different things to two CLIs on the same deck
//! (measured, HOST-CONTRACT-W4 §3 B13: "sure" allowed a Codex permission card
//! and was free text to Claude; "yeah" was the reverse). [`intent`] and
//! [`talk_intent`] are that one grammar; both adapters call them.
//!
//! [`crate::guard`] keeps its own, deliberately different pair and is **not**
//! folded in here. It answers a different question: not "what did the operator
//! mean?" but "could this possibly grant power, so must I demand trust?" — and
//! its answer for an unrecognised word has to be *yes* while this module's is
//! "an instruction, i.e. a deny". Merging them would make every word the
//! grammar does not know skip the trust gate.
use crate::store::PTT_CAP;
use crate::{
    AudioCmd, CardId, Effect, EventKind, Principal, PttCtx, PttInfo, Slot, Store, TimerKey,
    UtterancePhase, VoiceIn, VoiceInput,
};

/// What a spoken or typed answer on a permission/elicitation card means
/// (§5.6, `ctx:"answer"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Allow,
    Deny,
    /// Neither: the operator said something else, which is a deny that
    /// carries the words so the agent can re-plan.
    Say,
}

/// What a spoken `ctx:"talk"` utterance means (§5.6's `talk` verbs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkIntent {
    /// `report|status`: speak the agent's state and the head of its reply.
    Report,
    /// `stop|quiet`: `voice.stop{all:true}`.
    Stop,
    /// Everything else: `op.order{source:"ptt"}`.
    Order,
}

/// §5.6's allow words. The union of what the two adapters recognised, which
/// is the only merge that is not a regression for one of them: dropping
/// `yeah` would turn a Claude operator's yes into an instruction, and
/// dropping `sure` would do the same to a Codex operator's.
const ALLOW: [&str; 10] = [
    "yes", "allow", "approve", "go ahead", "do it", "ok", "okay", "yep", "yeah", "sure",
];
/// §5.6's deny words, likewise the union.
const DENY: [&str; 7] = ["no", "deny", "reject", "stop", "dont", "don't", "nope"];
/// `talk`: ask the deck to speak the agent's state.
const REPORT: [&str; 2] = ["report", "status"];
/// `talk`: cut everything that is speaking.
const QUIET: [&str; 2] = ["stop", "quiet"];
/// The mixer's lanes (§4.9, §7.3), highest priority last-but-one; `ptt` is
/// virtual and is never named by a caller.
const LANES: [&str; 4] = ["question", "ready", "chatter", "chime"];

/// The transcript as the grammar compares it: lowercased, stripped of
/// everything but letters, digits, spaces and the apostrophe of `don't`, with
/// runs of whitespace collapsed.
///
/// A dictated word arrives punctuated (`"Sure!"`, `"go ahead."`) and a typed
/// one does not; without this the same word matches from the deck and misses
/// from the GUI.
fn folded(text: &str) -> String {
    let kept: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '\'')
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The en-US intent grammar of §5.6 on a permission/elicitation card.
///
/// It only ever runs for a principal that already passed §4.2's trust gate:
/// a real deck `ptt` edge (`principal:"device"`) or a trusted caller of
/// `voice.listen`. An untrusted caller was refused `-32002` before capture
/// started (§4.9), so widening the allow list cannot widen who may allow.
#[must_use]
pub fn intent(text: &str) -> Intent {
    let words = folded(text);
    if ALLOW.contains(&words.as_str()) {
        Intent::Allow
    } else if DENY.contains(&words.as_str()) {
        Intent::Deny
    } else {
        Intent::Say
    }
}

/// The `ctx:"talk"` verbs of §5.6: **exactly** `report|status` or
/// `stop|quiet`; everything else is an order, words and all.
#[must_use]
pub fn talk_intent(text: &str) -> TalkIntent {
    let words = folded(text);
    if REPORT.contains(&words.as_str()) {
        TalkIntent::Report
    } else if QUIET.contains(&words.as_str()) {
        TalkIntent::Stop
    } else {
        TalkIntent::Order
    }
}

impl Store {
    /// §7.6's `contextual_strings` for one capture (B3): the agent names,
    /// then the choice labels of the card this session is about.
    ///
    /// These are exactly the words an en-US recogniser has no reason to
    /// know and every reason to mangle — `cohdl-api` comes back as "code
    /// all API", `Allow always` as "a lot always" — and they are also the
    /// only words the §5.6 grammar and the answer mapping look for. The
    /// reducer builds the list because it is the only place holding the
    /// snapshot the capture is about; the engine cannot ask for it later,
    /// because the recogniser takes its hints at `start`.
    pub(crate) fn contextual_strings(
        &self,
        agent: Option<&crate::AgentId>,
        card: Option<CardId>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in self.snapshot.agents.values() {
            let name = a.name.trim();
            if !name.is_empty() && !out.iter().any(|s| s == name) {
                out.push(name.to_string());
            }
        }
        // The card this press is about: the one named, else the agent's
        // current card (a `ptt ctx:answer` from the deck latches an id, a
        // `voice.listen` may name none).
        let labels = agent
            .and_then(|id| self.snapshot.agents.get(id))
            .and_then(|a| a.card.as_ref())
            .filter(|c| card.is_none_or(|id| id == c.id))
            .map(|c| c.choices.iter().map(|ch| ch.label.clone()));
        for label in labels.into_iter().flatten() {
            let label = label.trim().to_string();
            if !label.is_empty() && !out.contains(&label) {
                out.push(label);
            }
        }
        out
    }

    pub(crate) fn on_ptt(
        &mut self,
        act: u8,
        slot: Slot,
        ctx: PttCtx,
        card: Option<CardId>,
        abort: bool,
        press: Option<u16>,
    ) {
        let agent = self.snapshot.agent_at(slot).map(|a| a.id.clone());
        if act == 1 {
            // A deck edge is `principal:"device"` with no session at all —
            // there is no connection behind a button (B2).
            self.start_listen(
                Principal::Device,
                None,
                agent,
                ctx,
                card,
                VoiceInput::Deck,
                PTT_CAP.as_secs() as u32,
            );
            if let Some(ptt) = &mut self.snapshot.voice.ptt { ptt.device_press = press; }
            // A companion's press shows a recording waveform from the start:
            // the local recogniser's live words are not what will be sent.
            if let Some(p) = self.snapshot.voice.ptt.clone().filter(|p| p.companion) {
                self.recording_line(&p);
            }
        } else {
            if press.is_some() && self.snapshot.voice.ptt.as_ref().is_none_or(|p| p.device_press != press) { return; }
            self.finish_listen(abort);
        }
    }

    /// Arm a PTT session for `principal`/`session` (§7.4's `Idle → Arming`).
    ///
    /// B2: this used to be the body of [`Store::on_ptt`], which hard-coded
    /// `Principal::Device` and took a `Slot` — so `voice.listen` had no way
    /// in, and §4.9's promise that a dictated answer carries **the caller's**
    /// principal and session (never `device`) could not be kept. The deck's
    /// press is now one caller of this among others, and it is the one that
    /// passes `(Device, None)`.
    ///
    /// A press while a session is live restarts it, which is what the
    /// operator means by pressing again.
    ///
    /// Returns the `{rev}` a reducer method answers with; every refusal
    /// this session could earn (an unknown agent, the §4.9 trust guard)
    /// belongs to the caller and has already happened by here.
    // Seven arguments is the shape the contract names, and the same
    // `#[allow]` `Store::answer`'s own entry point carries: they are seven
    // unrelated facts about one capture, and bundling them into a struct
    // would only move the list.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_listen(
        &mut self,
        principal: Principal,
        session: Option<crate::SessionId>,
        agent: Option<crate::AgentId>,
        ctx: PttCtx,
        card: Option<CardId>,
        input: VoiceInput,
        max_s: u32,
    ) -> serde_json::Value {
        // Barge-in first (§7.7): everything speaking is cut before the mic
        // opens, or the capture hears our own TTS (V5 measured the deck's
        // loopback at +17 dB, so this is load-bearing, not tidiness).
        //
        // **Cut, not `all`.** The cut is `AudioCmd::Listen`'s own job —
        // §7.3's virtual `Ptt` lane silences every lane in 10 ms and the
        // route SM closes the output entirely — so V5 is satisfied without
        // discarding anything. What this command adds is the *throwing
        // away*, and §7.2/§7.3 are explicit about who deserves it: stale
        // `Chatter` is a reply to something the operator has since talked
        // over, while `Question` and `Ready` are **held, not flushed**,
        // because nobody has heard them and something is still waiting on
        // an answer. `Stop{all}` here contradicted both, and threw away the
        // very Question the operator had pressed the button to answer.
        // §4.9's `voice.stop{all}` is unchanged: that one is an operator
        // saying "be quiet", and it means all.
        self.effects.push(Effect::Audio(AudioCmd::Stop {
            utterance: None,
            lane: Some("chatter".into()),
            all: false,
        }));
        // The restart replaces the capture that is open and nothing else:
        // a session already released keeps its own budget and its own
        // words (M1/D6).
        self.abort_live("restart");
        // Apple's recogniser stops at one minute, so the cap is a ceiling
        // and not a suggestion (§7.4).
        let max_s = max_s.clamp(1, PTT_CAP.as_secs() as u32);
        let ts = self.ts_now();
        // The id every transcript of this session will carry: allocated
        // here because the partials start arriving before anything else
        // could name them (B1).
        self.utterance_seq += 1;
        let utterance = format!("ut_{}", self.utterance_seq);
        self.snapshot.voice.ptt = Some(PttInfo {
            agent: agent.clone(),
            ctx,
            card,
            principal: principal.clone(),
            session,
            utterance: utterance.clone(),
            // §7.4's `Arming`: the capture is opening and has produced
            // nothing yet. It becomes `Listening` on the first non-zero
            // sample (V2), ~1 s later on this hardware.
            live: false,
            since: ts, device_press: None,
            companion: false,
        });
        let at = self.now + std::time::Duration::from_secs(u64::from(max_s));
        self.ptt_cap_at = Some(at);
        self.timer(TimerKey::PttCap, at);
        let arming = self.now + crate::store::ARMING_LIVENESS;
        self.arming_at = Some(arming);
        self.timer(TimerKey::PttArming, arming);
        let contextual = self.contextual_strings(agent.as_ref(), card);
        // A deck edge is the one caller with `(Device, None)` (B2).
        let from_deck = matches!(principal, Principal::Device) && session.is_none();
        let companion = self.companion_transcriber(agent.as_ref(), ctx);
        if let Some(ptt) = &mut self.snapshot.voice.ptt { ptt.companion = companion.is_some(); }
        self.effects.push(Effect::Audio(AudioCmd::Listen {
            agent: agent.clone(),
            ctx,
            card,
            max_s,
            from_deck,
            input,
            locale: self.cfg.locale.clone(),
            contextual,
            companion,
        }));
        let by_companion = self.snapshot.voice.ptt.as_ref().is_some_and(|p| p.companion);
        self.emit(
            EventKind::VoiceState,
            principal.clone(),
            serde_json::json!({
                "state": "arming", "agent": agent, "ctx": ctx, "card": card,
                "principal": principal, "session": session, "utterance": utterance,
                "companion": by_companion,
            }),
        );
        self.rev()
    }

    /// The agent whose owner transcribes this press, if any: a talk or an
    /// answer addressed to an agent registered with `caps.transcribe:
    /// "push"`, while `[voice.stt] companion` is on. Naming the deck stays
    /// local — its words are for the daemon, not for any agent.
    pub(crate) fn companion_transcriber(
        &self,
        agent: Option<&crate::AgentId>,
        ctx: PttCtx,
    ) -> Option<crate::AgentId> {
        if !self.cfg.companion_stt || matches!(ctx, PttCtx::Name) {
            return None;
        }
        let agent = agent?;
        let caps = self.caps_of(agent);
        (caps.transcribe == crate::TranscribeCap::Push).then(|| agent.clone())
    }

    /// The release (§7.4's `Finalizing`/`Aborting`).
    ///
    /// A clean release keeps the session in `finalizing` until
    /// [`VoiceIn::Final`] arrives: the operator has let go, and the words
    /// are still on their way.
    pub(crate) fn finish_listen(&mut self, abort: bool) {
        let Some(p) = self.snapshot.voice.ptt.take() else {
            return;
        };
        self.ptt_cap_at = None;
        // No capture is open any more, so the dead-mic deadline is
        // meaningless: the operator let go, which is not a broken
        // microphone.
        self.arming_at = None;
        self.effects.push(Effect::Audio(AudioCmd::Finish { abort }));
        // The capture is closed, so §7.4's held `channel` applies (M6).
        self.apply_pending_channel();
        self.emit(
            EventKind::VoiceState,
            p.principal.clone(),
            serde_json::json!({
                "state": if abort { "aborted" } else { "finalizing" },
                "agent": p.agent, "ctx": p.ctx, "card": p.card,
                "reason": abort.then_some("abort"),
                "principal": p.principal, "session": p.session,
                "utterance": p.utterance,
            }),
        );
        // An abort is about **this** session and no other: a session
        // released a moment ago is a finished sentence of the operator's,
        // already on its way (M1).
        if !abort {
            self.queue_finalizing(p);
        }
    }

    /// Keep the session until the recogniser completes. The watchdog fails a
    /// stuck decode without sending the preview shown while PTT was held.
    fn queue_finalizing(&mut self, ptt: PttInfo) {
        let deadline = self.now + crate::store::FINALIZE_BUDGET;
        self.finalizing.push_back(crate::store::Finalizing {
            ptt: Some(ptt),
            deadline,
        });
        self.rearm_finalize();
    }

    /// Point `PttFinalize` at the earliest deadline still owed to a session
    /// that has not been settled. A tombstone has no deadline of its own:
    /// nothing waits on it but the engine, and the engine has no budget we
    /// can enforce from here.
    ///
    /// A `Tick` reads `finalizing_at` and never the timer key, so a timer
    /// left armed for a deadline that has moved is harmless.
    fn rearm_finalize(&mut self) {
        let at = self
            .finalizing
            .iter()
            .filter(|f| f.ptt.is_some())
            .map(|f| f.deadline)
            .min();
        if at != self.finalizing_at {
            self.finalizing_at = at;
            if let Some(at) = at {
                self.timer(TimerKey::PttFinalize, at);
            }
        }
    }

    /// CANCEL, `up`, a link drop, the cap: the utterance is discarded.
    ///
    /// **One** session, and the live one first (M1). An abort means "I take
    /// back what I am saying", and a session released a moment ago is not
    /// what is being said: it is a finished sentence the operator watched
    /// arrive, already on its way to an agent. Discarding it used to happen
    /// in total silence — no dispatch, no `transcript.final`, no
    /// `voice.state`, nothing at all on the bus for a caller to wait on.
    pub(crate) fn abort_ptt(&mut self, reason: &str) {
        if self.snapshot.voice.ptt.is_some() {
            self.abort_live(reason);
        } else {
            // With nothing open, the cancel is about the session that is
            // still waiting for its words (M2).
            self.abort_finalizing(reason);
        }
    }

    /// Shutdown and the link drop: nothing survives, live or released.
    /// §6.3 — with the deck gone an utterance in flight cannot be confirmed.
    pub(crate) fn abort_all_ptt(&mut self, reason: &str) {
        self.abort_live(reason);
        while self.abort_finalizing(reason) {}
    }

    /// Abort the **newest** session still waiting for its words, and say so.
    /// Returns whether there was one.
    ///
    /// The newest, because that is what "take it back" means when more than
    /// one is outstanding: the last thing the operator said. The entry stays
    /// behind as a tombstone — the engine still owes it one answer, which
    /// must not be allowed to land on anybody else (D6).
    ///
    /// No `AudioCmd::Finish` goes with it. The capture was closed by the
    /// release; what is outstanding is the engine's `stt.finish()`, and
    /// §7.6's traits have nothing that interrupts one.
    fn abort_finalizing(&mut self, reason: &str) -> bool {
        let Some(i) = self.finalizing.iter().rposition(|f| f.ptt.is_some()) else {
            return false;
        };
        let p = {
            let f = &mut self.finalizing[i];
            f.ptt.take().expect("rposition found an unsettled session")
        };
        if p.device_press.is_some() { self.transcript_failure(&p); }
        self.rearm_finalize();
        self.emit(
            EventKind::VoiceState,
            p.principal.clone(),
            serde_json::json!({
                "state": "aborted", "agent": p.agent, "ctx": p.ctx, "card": p.card,
                "reason": reason, "principal": p.principal, "session": p.session,
                "utterance": p.utterance,
            }),
        );
        true
    }

    /// Abort the capture that is open, if one is, and tell the engine and
    /// the bus. A no-op when nothing is live.
    pub(crate) fn abort_live(&mut self, reason: &str) {
        let Some(p) = self.snapshot.voice.ptt.take() else {
            return;
        };
        self.ptt_cap_at = None;
        self.arming_at = None;
        self.effects
            .push(Effect::Audio(AudioCmd::Finish { abort: true }));
        // The capture is closed, so §7.4's held `channel` applies (M6).
        self.apply_pending_channel();
        self.emit(
            EventKind::VoiceState,
            p.principal.clone(),
            serde_json::json!({
                "state": "aborted", "agent": p.agent, "ctx": p.ctx, "card": p.card,
                "reason": reason, "principal": p.principal, "session": p.session,
                "utterance": p.utterance,
            }),
        );
    }

    /// `voice.say` (§4.9): allocate an utterance, queue it, and say so.
    ///
    /// B5: this used to be a dead end outside the reducer, so there was
    /// nowhere to allocate the id from and nothing to set `Speaking` on —
    /// `{utterance}` could not be returned and §4.12's `voice.utterance`
    /// had no producer.
    pub(crate) fn voice_say(&mut self, r: crate::VoiceSay) -> crate::ApiResult {
        let text = crate::sanitize::clean(&r.text);
        if text.is_empty() {
            return Err(crate::ApiError::invalid("text", "empty"));
        }
        if !LANES.contains(&r.lane.as_str()) {
            return Err(crate::ApiError::invalid("lane", "unknown lane"));
        }
        if let Some(agent) = &r.agent {
            self.exists(agent)?;
        }
        self.utterance_seq += 1;
        let utterance = format!("ut_{}", self.utterance_seq);
        let voice = r.voice.clone().or_else(|| {
            r.agent
                .as_ref()
                .and_then(|id| self.snapshot.agents.get(id))
                .and_then(|a| a.voice.clone())
        });
        self.queue_say(AudioCmd::Say {
            utterance: utterance.clone(),
            text,
            agent: r.agent,
            lane: r.lane,
            voice,
            interrupt: r.interrupt,
            ttl_s: r.ttl_s,
        });
        Ok(serde_json::json!({"rev": self.snapshot.rev, "utterance": utterance}))
    }

    /// Queue one `Say` and record it: the snapshot's `speaking` and
    /// §4.12's `voice.utterance{phase:"queued"}`.
    ///
    /// `speaking` is the daemon's **intent** — what it has asked for and
    /// not yet been told about. The later phases (`started`, `ended`) are
    /// the engine's to report, and until an engine does, a reader should
    /// take this as "queued or playing" and nothing finer.
    pub(crate) fn queue_say(&mut self, cmd: AudioCmd) {
        let AudioCmd::Say {
            utterance,
            lane,
            agent,
            ..
        } = &cmd
        else {
            debug_assert!(false, "queue_say takes a Say");
            return;
        };
        let (utterance, lane, agent) = (utterance.clone(), lane.clone(), agent.clone());
        self.snapshot.voice.speaking = Some(crate::Speaking {
            utterance: utterance.clone(),
            lane: lane.clone(),
            agent: agent.clone(),
        });
        self.effects.push(Effect::Audio(cmd));
        self.emit(
            EventKind::VoiceUtterance,
            Principal::Uid(0),
            serde_json::json!({
                "utterance": utterance, "lane": lane, "agent": agent, "phase": "queued",
            }),
        );
    }

    /// `voice.stop{utterance?|lane?|all}` (§4.9): the barge-in path.
    pub(crate) fn voice_stop(
        &mut self,
        utterance: Option<String>,
        lane: Option<String>,
        all: bool,
    ) -> crate::ApiResult {
        if let Some(lane) = &lane {
            if !LANES.contains(&lane.as_str()) {
                return Err(crate::ApiError::invalid("lane", "unknown lane"));
            }
        }
        if !all && utterance.is_none() && lane.is_none() {
            return Err(crate::ApiError::invalid(
                "all",
                "name an utterance, a lane, or all",
            ));
        }
        // Whatever we believed was speaking is cut if this names it.
        let cut = self.snapshot.voice.speaking.clone().filter(|s| {
            all || utterance.as_ref().is_some_and(|u| *u == s.utterance)
                || lane.as_ref().is_some_and(|l| *l == s.lane)
        });
        self.effects.push(Effect::Audio(AudioCmd::Stop {
            utterance,
            lane,
            all,
        }));
        if let Some(s) = cut {
            self.snapshot.voice.speaking = None;
            self.emit(
                EventKind::VoiceUtterance,
                Principal::Uid(0),
                serde_json::json!({
                    "utterance": s.utterance, "lane": s.lane, "agent": s.agent, "phase": "cut",
                }),
            );
        }
        Ok(self.rev())
    }

    /// `voice.listen` (§4.9): the API entry to §7.4's machine.
    ///
    /// The trust guard for `ctx:"answer"` on a permission card runs in the
    /// **core**, before this (§4.2: no TOCTOU), and is what stops a caller
    /// listening to a card it is not allowed to answer.
    pub(crate) fn voice_listen(
        &mut self,
        principal: Principal,
        session: crate::SessionId,
        r: crate::VoiceListen,
    ) -> crate::ApiResult {
        self.exists(&r.agent)?;
        // §4.9 admits exactly two: `name` is a device edge and an API
        // caller may not synthesise it.
        let ctx = match r.ctx.as_str() {
            "talk" => PttCtx::Talk,
            "answer" => PttCtx::Answer,
            _ => return Err(crate::ApiError::invalid("ctx", "talk or answer")),
        };
        if r.max_s == 0 {
            return Err(crate::ApiError::invalid("max_s", "at least 1"));
        }
        // M5: a capture that is open belongs to whoever started it, and
        // `start_listen`'s restart would take it. The deck's own press does
        // not come through here — the operator holding the button always
        // wins, and pressing again is what restarting means.
        if let Some(owner) = &self.snapshot.voice.ptt {
            if owner.session != Some(session) {
                return Err(crate::ApiError::held_by(&owner.principal, owner.session));
            }
        }
        Ok(self.start_listen(
            principal,
            Some(session),
            Some(r.agent),
            ctx,
            r.card,
            r.input,
            r.max_s,
        ))
    }

    /// `voice.cancel` (§4.9): the equivalent of `ptt abort:1`. B5: this
    /// could not reach `abort_ptt` at all, so a client that started a
    /// capture had no way to let go of the microphone.
    ///
    /// M5: and then it could let go of **anybody's**. A caller may only
    /// cancel the session it started (§4.2: ownership is per session and
    /// never per principal — two windows of the same GUI are the same
    /// `uid:501` and are still two sessions). A deck press has no session
    /// at all, so no API caller can ever cancel one; the operator's own
    /// thumb is the cancel for that, and it never asks.
    ///
    /// With nothing listening it stays idempotent: a client that lost the
    /// race to the release should not have to tell that apart from a
    /// refusal.
    pub(crate) fn voice_cancel(&mut self, session: crate::SessionId) -> crate::ApiResult {
        if let Some(err) = self.not_this_sessions_capture(session) {
            return Err(err);
        }
        self.abort_ptt("cancel");
        Ok(self.rev())
    }

    /// The refusal for a caller that does not own the session
    /// [`Store::abort_ptt`] would take — the live capture, else the newest
    /// one still waiting for its words, which a cancel discards just as
    /// completely.
    fn not_this_sessions_capture(&self, session: crate::SessionId) -> Option<crate::ApiError> {
        let owner = self
            .snapshot
            .voice
            .ptt
            .as_ref()
            .or_else(|| self.finalizing.iter().rev().find_map(|f| f.ptt.as_ref()))?;
        if owner.session == Some(session) {
            return None;
        }
        Some(crate::ApiError::held_by(&owner.principal, owner.session))
    }

    /// One report from the audio side (B1).
    pub(crate) fn on_voice(&mut self, v: VoiceIn) {
        match v {
            VoiceIn::Partial { text } => self.on_partial(&text),
            VoiceIn::Final { text } => self.on_final(&text),
            VoiceIn::FinalFailed { code, message } => {
                if let Some(p) = self.claim_final() { self.fail_final(&p, &code, &message); }
            }
            VoiceIn::Route {
                route,
                output_uid,
                input_uid,
                stuck,
                slow_sco_ms,
            } => self.on_route(route, output_uid, input_uid, stuck, slow_sco_ms),
            VoiceIn::Live => self.on_live(),
            VoiceIn::Level { rms } => self.emit(
                EventKind::VoiceLevel,
                Principal::Device,
                serde_json::json!({"rms": rms}),
            ),
            VoiceIn::Permissions(p) => self.on_permissions(p),
            VoiceIn::Bluetooth { permission } => self.on_bluetooth(permission),
            VoiceIn::Transport { transport } => self.on_transport(transport),
            VoiceIn::Utterance { utterance, phase } => self.on_utterance(&utterance, phase),
            VoiceIn::Error { code, message } => self.on_voice_error(&code, message.as_deref()),
        }
    }

    /// An interim transcript: the deck's transcript line and
    /// `transcript.partial` (§7.4). Pacing is the outbox's (B16).
    fn on_partial(&mut self, text: &str) {
        // Words are proof of samples: an engine that got as far as a
        // partial has had non-zero audio, whether or not it also sent
        // `VoiceIn::Live`. Cheap insurance against an engine that reports
        // one and not the other.
        if !text.trim().is_empty() {
            self.on_live();
        }
        let Some(p) = self.snapshot.voice.ptt.clone() else {
            // The session ended between the engine's send and here.
            return;
        };
        let text = crate::sanitize::collapse(text);
        // A companion transcribes this press; the local partial is only the
        // fallback's progress, so the deck keeps its recording waveform.
        if !p.companion {
            self.transcript_line(&p, &text, false);
        }
        self.emit(
            EventKind::TranscriptPartial,
            p.principal.clone(),
            serde_json::json!({
                "agent": p.agent, "ctx": p.ctx, "text": text, "utterance": p.utterance,
            }),
        );
    }

    /// Put the transcript on the deck for the session's slot. The **cut**
    /// is the wire encoder's; the **pacing** is the outbox's token bucket
    /// (B16), because a bucket in the reducer would make the reducer
    /// depend on real time.
    /// The deck's `rec` marker for a companion's press: no words, a waveform.
    fn recording_line(&mut self, p: &PttInfo) {
        let slot = p.agent.as_ref().and_then(|id| self.snapshot.agents.get(id)).and_then(|a| a.slot);
        self.device_op(crate::DeviceOp::Control(crate::Control::Transcript {
            slot: slot.unwrap_or(crate::Slot(0)),
            text: String::new(),
            last: false, press: p.device_press, failed: false, channel_tip: false, rec: true,
        }));
    }

    fn transcript_line(&mut self, p: &PttInfo, text: &str, last: bool) {
        let slot = p
            .agent
            .as_ref()
            .and_then(|id| self.snapshot.agents.get(id))
            .and_then(|a| a.slot);
        // A session with nobody on the strip (a face name, a key label, an
        // agent that has no slot) still gets its words shown while the
        // button is held: slot 0 is "whoever is holding PTT" on the wire.
        self.device_op(crate::DeviceOp::Control(crate::Control::Transcript {
            slot: slot.unwrap_or(crate::Slot(0)),
            text: text.to_string(),
            last, press: p.device_press, failed: false, channel_tip: false, rec: false,
        }));
        if let Some(slot) = slot {
            // Only the **final** transcript gets a window. A partial is
            // replaced every few hundred milliseconds and the line is already
            // rate-limited; a window per partial would be ~600 B against a
            // link that measured 0.26 s per report, so every one of them would
            // land stale. The final text is stable, so it is the one worth
            // laying out.
            if last {
                self.transcript_doc = Some((slot, text.to_string()));
                self.transcript_rev = self.transcript_rev.wrapping_add(1).max(1);
                let rev = self.transcript_rev;
                if let Some(window) = crate::store::doc_window(
                    slot,
                    crate::DocKind::Tr,
                    rev,
                    text,
                    u16::MAX,
                    self.text_size(),
                ) {
                    self.device_op(crate::DeviceOp::Put(
                        crate::Key::DocText(slot, crate::DocKind::Tr, rev),
                        window,
                    ));
                }
            }
        }
    }

    /// The final transcript: `transcript.final`, then §7.4's dispatch by
    /// `ctx`, then `voice.state{state:"ended", empty}` (B8).
    fn on_final(&mut self, text: &str) {
        let Some(p) = self.claim_final() else {
            return;
        };
        self.dispatch_session(&p, text);
    }

    /// **Whose** final this is (D6).
    ///
    /// The engine answers one `AudioCmd::Finish{abort:false}` with exactly
    /// one `VoiceIn::Final` or `VoiceIn::FinalFailed`, in release order, so
    /// the oldest released session is always the claimant — including when
    /// it has already been settled, in which case its tombstone eats the
    /// answer instead of letting it be dispatched under whatever session
    /// happens to be live by now.
    ///
    /// `None` means the final belongs to nobody: a session that was
    /// aborted, or an engine that answered twice.
    fn claim_final(&mut self) -> Option<PttInfo> {
        if let Some(f) = self.finalizing.pop_front() {
            self.rearm_finalize();
            return f.ptt;
        }
        // Nothing has been released, so this belongs to the capture that is
        // still open: the engine reached its own end of the session while
        // the operator was still holding the button.
        let p = self.snapshot.voice.ptt.take()?;
        self.ptt_cap_at = None;
        self.arming_at = None;
        // D5: after this nothing else can close the capture —
        // `finish_listen` needs a `ptt` to take, so the release is a no-op,
        // and the 60 s cap went with `ptt_cap_at`. Without it the
        // microphone stays open for ever, `lanes.ptt` stays true (which
        // silences every lane) and the route sticks in `ScoOurs` until the
        // next press.
        //
        // `abort: true` is the accurate word for "let go, I already have
        // the words": `Finish{abort:false}` would make the engine run
        // `stt.finish()` and report a **second** `Final` for a session that
        // is already dispatched.
        self.effects
            .push(Effect::Audio(AudioCmd::Finish { abort: true }));
        self.apply_pending_channel();
        Some(p)
    }

    /// §7.4's tail for **one** session: `transcript.final`, then
    /// `Dispatch(ctx)`, then `voice.state{state:"ended", empty}` (B8).
    ///
    /// Everything here reads `p` and never the live session, so the words
    /// cannot be attributed to anyone but the operator who said them.
    fn dispatch_session(&mut self, p: &PttInfo, text: &str) {
        let text = crate::sanitize::clean(text);
        let intent = match p.ctx {
            PttCtx::Answer => match intent(&text) {
                Intent::Allow => Some("allow"),
                Intent::Deny => Some("deny"),
                Intent::Say => None,
            },
            PttCtx::Talk => match talk_intent(&text) {
                TalkIntent::Report => Some("report"),
                TalkIntent::Stop => Some("stop"),
                TalkIntent::Order => None,
            },
            _ => None,
        };
        self.transcript_line(p, &text, true);
        self.emit(
            EventKind::TranscriptFinal,
            p.principal.clone(),
            serde_json::json!({
                "agent": p.agent, "ctx": p.ctx, "text": text,
                "utterance": p.utterance, "intent": intent,
            }),
        );
        let empty = text.is_empty();
        if !empty {
            self.dispatch_final(p, &text);
        }
        self.end_session(p, empty);
    }

    /// §7.4's `Dispatch(ctx)`.
    fn dispatch_final(&mut self, p: &PttInfo, text: &str) {
        match p.ctx {
            // §5.6: exactly `report|status` and `stop|quiet` are verbs;
            // everything else is an order, words and all.
            PttCtx::Talk => match talk_intent(text) {
                TalkIntent::Report => self.speak_report(p.agent.as_ref()),
                TalkIntent::Stop => self.effects.push(Effect::Audio(AudioCmd::Stop {
                    utterance: None,
                    lane: None,
                    all: true,
                })),
                TalkIntent::Order => self.dispatch_order(p, text, "talk"),
            },
            PttCtx::Answer => self.dispatch_answer(p, text),
            // §7.4: the answer to `ptt ctx:name`, cut and title-cased by
            // `device_name` itself (B15).
            PttCtx::Name => {
                let _ = self.device_name(text.to_string());
            }
        }
    }

    /// `op.order{source:"ptt"}` as the session's own principal (§4.9).
    fn dispatch_order(&mut self, p: &PttInfo, text: &str, ctx: &str) {
        let Some(agent) = p.agent.clone() else {
            // No agent on the slot: there is nobody to order.
            self.emit(
                EventKind::OrderFailed,
                p.principal.clone(),
                serde_json::json!({"agent": null, "reason": "no_agent", "text_kept": true}),
            );
            return;
        };
        let session = p.session.unwrap_or(crate::SessionId(0));
        let _ = self.op_order(
            p.principal.clone(),
            session,
            crate::OpOrder {
                agent,
                text: text.to_string(),
                ctx: ctx.to_string(),
                source: crate::OrderSource::Ptt,
                // B12: the order names the utterance it was dictated from.
                utterance: Some(p.utterance.clone()),
            },
        );
    }

    /// `ctx:"answer"` (§7.4): free text on a `q` card is the answer
    /// verbatim — the adapter runs §5.6's grammar on it, because only the
    /// adapter knows what "allow" means to its CLI — and a release on an
    /// `r` card is a step, i.e. an order.
    fn dispatch_answer(&mut self, p: &PttInfo, text: &str) {
        let Some(agent) = p.agent.clone() else {
            return;
        };
        let card = p.card.or_else(|| {
            self.snapshot
                .agents
                .get(&agent)
                .and_then(|a| a.card.as_ref())
                .map(|c| c.id)
        });
        let Some(card_id) = card else {
            // Nothing to answer: treat it as what was said, an order.
            self.dispatch_order(p, text, "talk");
            return;
        };
        let kind = self
            .snapshot
            .agents
            .get(&agent)
            .and_then(|a| a.card.as_ref())
            .filter(|c| c.id == card_id)
            .map(|c| c.kind);
        if kind == Some(crate::CardKind::R) {
            // The device latches the Ready id; a spoken release on it is a
            // step (§5.6), dispatched as an ordinary `op.order` — so on an
            // `inject` agent it **is** injected, and the words that go to
            // the terminal are the operator's own transcript.
            //
            // §5.6's "never `inject`, whatever `caps.order` says" is about
            // the *other* door: `Store::answer` refuses a chosen step on an
            // `inject` agent with `-32006 no_route`, because a chosen step's
            // text is the **agent's own label** and guard 1 never types text
            // an `agent.*` call wrote. Both doors are right; they are not
            // the same rule, and reading one comment as the other is how a
            // future edit makes them agree by breaking the safe one.
            self.dispatch_order(p, text, "step");
            return;
        }
        let _ = self.answer(
            &agent,
            card_id,
            crate::AnswerInput {
                choice: None,
                text: Some(text.to_string()),
                source: crate::AnswerSource::Ptt,
                principal: p.principal.clone(),
                session: p.session,
                // The seam of contract §8: this is the path a spoken
                // answer to a question really takes, and it could not name
                // what the operator said it with.
                utterance: Some(p.utterance.clone()),
            },
        );
    }

    /// §5.6's `report|status`: "`name` is `state` since `age`." plus the
    /// first sentence of the reply, on `chatter`.
    fn speak_report(&mut self, agent: Option<&crate::AgentId>) {
        let now = self.ts_now().millis();
        let max_chars = self.cfg.announce_max_chars;
        let line = agent
            .and_then(|id| self.snapshot.agents.get(id))
            .map(|a| {
                let age = now.saturating_sub(a.since.millis()) / 1000;
                let state = match a.state {
                    crate::AgentState::Working => "working",
                    crate::AgentState::Question => "waiting for you",
                    crate::AgentState::Ready => "ready",
                    crate::AgentState::Error => "in error",
                    crate::AgentState::Idle => "idle",
                };
                let head = a
                    .reply
                    .as_ref()
                    .and_then(|r| r.text.as_deref())
                    .map(|t| format!(" {}", crate::sanitize::speech(t, max_chars)))
                    .unwrap_or_default();
                format!("{} is {state} since {age} seconds.{head}", a.name)
            })
            .unwrap_or_else(|| "nothing on this channel".to_string());
        self.say(line, agent.cloned(), "chatter");
    }

    /// §7.4's end of a session (B8): `voice.state{state:"ended", empty}`.
    ///
    /// §7.4 names this edge `ptt.ended`, §4.12's table does not list it and
    /// no `EventKind` existed. **This contract decides it is
    /// `voice.state`**, not a new kind: §4.12 already carries `voice.state`,
    /// and a second event for the same edge would have to be subscribed to
    /// separately — every client already watching `voice.*` would silently
    /// miss the one edge that says a session is over.
    ///
    /// `empty` is the fact the caller cannot otherwise get. "Nothing was
    /// dispatched" and "the transcript was empty" look identical from the
    /// outside — no `op.order`, no `op.answer`, no `card.answered` — and
    /// they are completely different things: the first is a failure worth
    /// showing, the second is the operator pressing the button and saying
    /// nothing, which is normal. Without the flag a GUI has to guess from
    /// the absence of events, and absence has no timeout.
    fn end_session(&mut self, p: &PttInfo, empty: bool) {
        self.emit(
            EventKind::VoiceState,
            p.principal.clone(),
            serde_json::json!({
                "state": "ended", "empty": empty,
                "agent": p.agent, "ctx": p.ctx, "card": p.card,
                "principal": p.principal, "session": p.session, "utterance": p.utterance,
            }),
        );
    }

    /// §7.4's `Arming → Listening`: the capture produced a sample that was
    /// not an exact digital zero (V2).
    ///
    /// This is the whole correction. The design's liveness check was "1.5 s
    /// without samples", and samples — buffers — start at ~28 ms on a mic
    /// that is not there at all: CoreAudio hands the callback 512 frames of
    /// zeros and `AudioUnitRender` says `noErr`. A check that counted those
    /// would have passed on every dead microphone ever plugged in. A check
    /// keyed here fires on exactly one thing.
    fn on_live(&mut self) {
        let Some(p) = self.snapshot.voice.ptt.as_mut() else {
            return;
        };
        if p.live {
            // The engine may report it per buffer; the edge happens once.
            return;
        }
        p.live = true;
        let p = p.clone();
        self.arming_at = None;
        self.emit(
            EventKind::VoiceState,
            p.principal.clone(),
            serde_json::json!({
                "state": "listening", "agent": p.agent, "ctx": p.ctx, "card": p.card,
                "principal": p.principal, "session": p.session, "utterance": p.utterance,
            }),
        );
    }

    /// The `Arming` deadline ran out with no non-zero sample (§7.4): the
    /// microphone is not delivering audio.
    ///
    /// The operator gets told on the deck as well as on the bus, because
    /// the deck is where they are looking — they are holding its button.
    /// `(no mic)` on the transcript line is §7.4's own wording, and it is
    /// the only thing that distinguishes "nothing you said was heard" from
    /// "you were heard and said nothing".
    ///
    /// `perm.changed` is **not** raised from here: a denied TCC grant is
    /// the engine's news and arrives as [`VoiceIn::Permissions`] (§7.8), and
    /// guessing at a denial from silence would report one on every unplugged
    /// headset.
    pub(crate) fn arming_timed_out(&mut self) {
        let Some(p) = self.snapshot.voice.ptt.clone() else {
            self.arming_at = None;
            return;
        };
        self.transcript_line(&p, "(no mic)", true);
        self.on_voice_error(
            "mic unavailable",
            Some("no non-zero sample before the arming deadline"),
        );
    }

    /// A failed final belongs to its released session, never a later press.
    fn fail_final(&mut self, p: &PttInfo, code: &str, message: &str) {
        self.transcript_failure(p);
        self.emit(EventKind::VoiceState, p.principal.clone(), serde_json::json!({
            "state": "aborted", "error": code, "message": message,
            "agent": p.agent, "ctx": p.ctx, "card": p.card,
            "principal": p.principal, "session": p.session, "utterance": p.utterance,
        }));
    }

    fn transcript_failure(&mut self, p: &PttInfo) {
        let slot = p.agent.as_ref().and_then(|id| self.snapshot.agents.get(id)).and_then(|a| a.slot);
        self.device_op(crate::DeviceOp::Control(crate::Control::Transcript {
            slot: slot.unwrap_or(crate::Slot(0)),
            text: "Transcription failed. Nothing was sent.".into(),
            last: true,
            press: p.device_press,
            failed: true, channel_tip: false, rec: false,
        }));
    }

    /// A hung engine must fail visibly, never turn its preview into an order.
    pub(crate) fn finalize_timed_out(&mut self) {
        let now = self.now;
        // Oldest first, each with its own identity (D6). The entry
        // stays behind as a tombstone: the engine still owes one answer for
        // it, and that answer must not land on anybody else.
        while let Some(i) = self
            .finalizing
            .iter()
            .position(|f| f.ptt.is_some() && f.deadline <= now)
        {
            let p = {
                let f = &mut self.finalizing[i];
                f.ptt.take().expect("the position found a live entry")
            };
            self.fail_final(&p, "engine", "Speech recognition timed out. Nothing was sent; please try again.");
        }
        self.rearm_finalize();
    }

    /// §7.2's route machine reported a move.
    fn on_route(
        &mut self,
        route: crate::Route,
        output_uid: Option<String>,
        input_uid: Option<String>,
        stuck: bool,
        slow_sco_ms: Option<u32>,
    ) {
        let voice = &mut self.snapshot.voice;
        let changed = voice.route != route
            || (output_uid.is_some() && voice.output_uid != output_uid)
            || (input_uid.is_some() && voice.input_uid != input_uid);
        voice.route = route;
        if output_uid.is_some() {
            voice.output_uid = output_uid.clone();
        }
        if input_uid.is_some() {
            voice.input_uid = input_uid.clone();
        }
        // `stuck` and `slow_sco_ms` are news even when the route did not
        // move: the first is the second failed reopen (§7.2) and the second
        // is §7.5.4's report that this press waited longer for its first
        // real sample than the operator's threshold allows.
        if changed || stuck || slow_sco_ms.is_some() {
            let mut data = serde_json::json!({
                "route": route,
                "output_uid": self.snapshot.voice.output_uid,
                "input_uid": self.snapshot.voice.input_uid,
                "stuck": stuck,
                "transport": self.snapshot.voice.transport,
            });
            if let Some(ms) = slow_sco_ms {
                data["slow_sco_ms"] = serde_json::json!(ms);
            }
            self.emit(EventKind::VoiceRoute, Principal::Device, data);
        }
    }

    /// B6's remainder: `voice.utterance{phase:"started"|"ended"}` from the
    /// engine.
    ///
    /// The lane and the agent come from `speaking` rather than from the
    /// engine, because they are the store's own record of what it asked
    /// for: an engine that reported a different lane from the one the
    /// `Say` named would be reporting a bug, and the event a client reads
    /// should be about the utterance the client was told about.
    ///
    /// An `ended` that names the utterance we believe is speaking clears
    /// `speaking`. One that names an older utterance is **not** an error
    /// and must not clear it: a cut utterance's `ended` routinely arrives
    /// after the next `Say` was queued, because the cut and the queue are
    /// two threads.
    fn on_utterance(&mut self, utterance: &str, phase: UtterancePhase) {
        let speaking = self
            .snapshot
            .voice
            .speaking
            .clone()
            .filter(|s| s.utterance == utterance);
        let (lane, agent) = match &speaking {
            Some(s) => (Some(s.lane.clone()), s.agent.clone()),
            None => (None, None),
        };
        if phase == UtterancePhase::Ended && speaking.is_some() {
            self.snapshot.voice.speaking = None;
        }
        self.emit(
            EventKind::VoiceUtterance,
            Principal::Uid(0),
            serde_json::json!({
                "utterance": utterance, "lane": lane, "agent": agent, "phase": phase,
            }),
        );
    }

    /// §7.8: the TCC state, and `perm.changed` when it moved.
    fn on_permissions(&mut self, p: crate::Permissions) {
        // Microphone and speech are the audio side's; Bluetooth is the BLE
        // link's own report and is not this one's to reset.
        let p = crate::Permissions {
            bluetooth: self.snapshot.voice.permissions.bluetooth,
            ..p
        };
        self.set_permissions(p);
    }

    /// The BLE voice link read CoreBluetooth's authorization.
    fn on_bluetooth(&mut self, permission: crate::Permission) {
        let p = crate::Permissions {
            bluetooth: permission,
            ..self.snapshot.voice.permissions
        };
        self.set_permissions(p);
    }

    fn set_permissions(&mut self, p: crate::Permissions) {
        if self.snapshot.voice.permissions == p {
            return;
        }
        self.snapshot.voice.permissions = p;
        self.emit(
            EventKind::PermChanged,
            Principal::Uid(0),
            serde_json::json!({
                "microphone": p.microphone,
                "speech": p.speech,
                "bluetooth": p.bluetooth,
            }),
        );
    }

    /// The engine said which transport it was built for: part of the
    /// route picture, so it goes out as a `voice.route`.
    fn on_transport(&mut self, transport: crate::VoiceTransport) {
        if self.snapshot.voice.transport == Some(transport) {
            return;
        }
        self.snapshot.voice.transport = Some(transport);
        let voice = &self.snapshot.voice;
        let data = serde_json::json!({
            "route": voice.route,
            "output_uid": voice.output_uid,
            "input_uid": voice.input_uid,
            "stuck": false,
            "transport": transport,
        });
        self.emit(EventKind::VoiceRoute, Principal::Device, data);
    }

    /// An engine or device error. A live session ends with it; the code is
    /// what the GUI shows (§7.4's `mic unavailable`, the contract's
    /// `audio_busy`).
    fn on_voice_error(&mut self, code: &str, message: Option<&str>) {
        let p = self.snapshot.voice.ptt.take();
        if let Some(p) = &p {
            if p.device_press.is_some() { self.transcript_failure(p); }
        }
        if p.is_some() {
            self.ptt_cap_at = None;
            self.arming_at = None;
        } else {
            // Nothing of ours is open, so what failed is not this session's
            // microphone — it is the engine that owes the released sessions
            // their words, and they go with it (M1: with a capture live,
            // they do not).
            // Keep tombstones: background final decodes still owe their
            // ordered answers, which must not land on a later press.
            while self.abort_finalizing("engine error") {}
        }
        let (agent, ctx, card, principal, session) = match &p {
            Some(p) => (
                p.agent.clone(),
                Some(p.ctx),
                p.card,
                p.principal.clone(),
                p.session,
            ),
            None => (None, None, None, Principal::Device, None),
        };
        if p.is_some() {
            // The engine is told to let go of whatever it holds; a refused
            // `Listen` never opened anything, and `Finish{abort}` on a
            // capture that does not exist is a no-op there.
            self.effects
                .push(Effect::Audio(AudioCmd::Finish { abort: true }));
            // The capture is closed, so §7.4's held `channel` applies (M6).
            self.apply_pending_channel();
        }
        self.emit(
            EventKind::VoiceState,
            principal.clone(),
            serde_json::json!({
                "state": "aborted", "error": code, "message": message,
                "agent": agent, "ctx": ctx, "card": card,
                "principal": principal, "session": session,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Sim, UID};
    use crate::{Command, DeviceLine, LinkEvent, SessionId};
    use std::time::Duration;

    fn voice(v: VoiceIn) -> Command {
        Command::Voice(v)
    }

    fn partial(text: &str) -> Command {
        voice(VoiceIn::Partial { text: text.into() })
    }

    fn final_(text: &str) -> Command {
        voice(VoiceIn::Final { text: text.into() })
    }

    /// B1: the inbound path exists at all.
    ///
    /// `Command` had no voice variant and `AudioCmd::Finish` returned
    /// nothing, so every partial, every final transcript, every route
    /// change, every level and every permission grant had nowhere to land —
    /// the whole of §7.4 downstream of "the mic is open" was unreachable.
    /// These rows drive the reducer with the events a **fake** engine would
    /// send, which is what makes the §15 week-4 exit tests possible without
    /// a microphone.
    #[test]
    fn the_audio_side_can_report_a_transcript_a_route_a_level_and_a_grant() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        // Nothing is listening: a stray report changes nothing and does
        // not panic.
        sim.apply(partial("hello"));
        sim.apply(final_("hello"));
        assert!(sim.events.is_empty() && sim.effects.is_empty());

        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let utterance = sim
            .store
            .snapshot()
            .voice
            .ptt
            .as_ref()
            .expect("listening")
            .utterance
            .clone();
        assert!(utterance.starts_with("ut_"), "{utterance}");
        sim.drain();
        // The press only **arms** the capture (§7.4): the first non-zero
        // sample is what makes it `Listening`, about a second later on
        // this hardware (V1/V2).
        sim.apply(voice(VoiceIn::Live));
        assert_eq!(sim.events.len(), 1);
        assert_eq!(sim.events[0].data["state"], "listening");
        assert!(sim.store.snapshot().voice.ptt.as_ref().unwrap().live);
        sim.apply(voice(VoiceIn::Live));
        assert!(sim.events.is_empty(), "the edge happens once");

        // A partial reaches the deck and the bus, keyed by the utterance.
        sim.apply(partial("  run   the tests  "));
        assert_eq!(sim.events.len(), 1);
        assert_eq!(sim.events[0].kind, EventKind::TranscriptPartial);
        assert_eq!(sim.events[0].data["text"], "run the tests");
        assert_eq!(sim.events[0].data["utterance"], utterance);
        assert_eq!(sim.events[0].data["ctx"], "talk");
        assert_eq!(
            sim.effects,
            vec![Effect::Device(crate::DeviceOp::Control(
                crate::Control::Transcript {
                    slot: Slot(1),
                    text: "run the tests".into(),
                    last: false, press: None, failed: false, channel_tip: false, rec: false,
                }
            ))]
        );

        // The release does not end the session: the words are still on
        // their way (§7.4's `Finalizing`).
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert!(sim.store.snapshot().voice.ptt.is_none());
        sim.drain();
        sim.apply(final_("run the tests"));
        let kinds = sim.kinds();
        assert_eq!(
            kinds,
            vec![
                EventKind::TranscriptFinal,
                EventKind::OrderDispatched,
                EventKind::AgentOrder,
                EventKind::VoiceState,
            ],
            "the final, the dispatch, then the end of the session"
        );
        assert_eq!(sim.events[0].data["utterance"], utterance);
        assert_eq!(sim.events[0].data["intent"], serde_json::Value::Null);
        assert_eq!(sim.events[1].data["text"], "run the tests");
        assert_eq!(
            sim.events[1].data["utterance"], utterance,
            "B12: the order names the utterance it came from"
        );
        assert_eq!(sim.events[2].data["source"], "ptt");
        assert_eq!(sim.events[3].data["state"], "ended");
        assert!(
            sim.effects.iter().any(|e| matches!(
                e,
                Effect::Device(crate::DeviceOp::Control(crate::Control::Transcript {
                    last: true,
                    ..
                }))
            )),
            "the last transcript line is marked"
        );
        // A second final for a session that is over is nothing.
        sim.apply(final_("again"));
        assert!(sim.events.is_empty());

        // §7.2's route, with the uids and the stuck flag.
        sim.apply(voice(VoiceIn::Route {
            route: crate::Route::A2dp,
            output_uid: Some("C2-2D:output".into()),
            input_uid: None,
            stuck: false,
            slow_sco_ms: None,
        }));
        assert_eq!(sim.events[0].kind, EventKind::VoiceRoute);
        assert_eq!(sim.store.snapshot().voice.route, crate::Route::A2dp);
        assert_eq!(
            sim.store.snapshot().voice.output_uid.as_deref(),
            Some("C2-2D:output")
        );
        // The same route again is not news…
        sim.apply(voice(VoiceIn::Route {
            route: crate::Route::A2dp,
            output_uid: None,
            input_uid: None,
            stuck: false,
            slow_sco_ms: None,
        }));
        assert!(sim.events.is_empty());
        // …but `stuck` is, even when the route did not move (§7.2: the
        // second failed reopen, which the GUI has to show).
        sim.apply(voice(VoiceIn::Route {
            route: crate::Route::A2dp,
            output_uid: None,
            input_uid: None,
            stuck: true,
            slow_sco_ms: None,
        }));
        assert_eq!(sim.events[0].data["stuck"], true);

        // The meter and the grant.
        sim.apply(voice(VoiceIn::Level { rms: 0.25 }));
        assert_eq!(sim.events[0].kind, EventKind::VoiceLevel);
        assert!((sim.events[0].data["rms"].as_f64().unwrap() - 0.25).abs() < 1e-6);
        let granted = crate::Permissions {
            microphone: crate::Permission::Granted,
            speech: crate::Permission::NotDetermined,
            bluetooth: crate::Permission::Unknown,
        };
        sim.apply(voice(VoiceIn::Permissions(granted)));
        assert_eq!(sim.events[0].kind, EventKind::PermChanged);
        assert_eq!(sim.events[0].data["microphone"], "granted");
        assert_eq!(sim.store.snapshot().voice.permissions, granted);
        sim.apply(voice(VoiceIn::Permissions(granted)));
        assert!(sim.events.is_empty(), "unchanged is not an event");
        // The BLE link's own report: only Bluetooth moves, and the event
        // carries all three.
        sim.apply(voice(VoiceIn::Bluetooth {
            permission: crate::Permission::Granted,
        }));
        assert_eq!(sim.events[0].kind, EventKind::PermChanged);
        assert_eq!(sim.events[0].data["bluetooth"], "granted");
        assert_eq!(sim.events[0].data["microphone"], "granted");
        // ...and the audio side's next report (which knows nothing of
        // Bluetooth) does not reset it.
        sim.apply(voice(VoiceIn::Permissions(granted)));
        assert!(sim.events.is_empty(), "Bluetooth is kept, nothing moved");
        assert_eq!(
            sim.store.snapshot().voice.permissions.bluetooth,
            crate::Permission::Granted
        );
        assert!(id.0.starts_with("ag_"));
    }

    /// The engine says which transport it was built for, once: the
    /// snapshot keeps it, and it rides on every `voice.route` after.
    #[test]
    fn the_transport_is_part_of_the_route_picture() {
        let mut sim = Sim::new();
        sim.drain();
        assert_eq!(sim.store.snapshot().voice.transport, None);
        sim.apply(voice(VoiceIn::Transport {
            transport: crate::VoiceTransport::Ble,
        }));
        assert_eq!(
            sim.store.snapshot().voice.transport,
            Some(crate::VoiceTransport::Ble)
        );
        assert_eq!(sim.events[0].kind, EventKind::VoiceRoute);
        assert_eq!(sim.events[0].data["transport"], "ble");
        assert_eq!(sim.events[0].data["route"], "none");
        sim.apply(voice(VoiceIn::Transport {
            transport: crate::VoiceTransport::Ble,
        }));
        assert!(sim.events.is_empty(), "the same transport is not news");
        sim.apply(voice(VoiceIn::Route {
            route: crate::Route::Ble,
            output_uid: Some("ble:FAKE".into()),
            input_uid: Some("ble:FAKE".into()),
            stuck: false,
            slow_sco_ms: None,
        }));
        assert_eq!(sim.events[0].data["route"], "ble");
        assert_eq!(sim.events[0].data["transport"], "ble");
        sim.apply(voice(VoiceIn::Route {
            route: crate::Route::BleOurs,
            output_uid: None,
            input_uid: None,
            stuck: false,
            slow_sco_ms: None,
        }));
        assert_eq!(sim.events[0].data["route"], "ble_ours");
    }

    /// B6's remainder: the two phases only the engine can know.
    ///
    /// The store can say `queued` and `cut` on its own. It cannot say
    /// `started` or `ended`, and on a route that spends 2307 ms reopening
    /// after every press (V1) the gap between "queued" and "the operator
    /// heard it" is most of a conversation. Before this, `speaking` was
    /// cleared only by the next `Say` or by a `Stop`.
    #[test]
    fn the_engine_reports_the_two_utterance_phases_the_store_cannot_know() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        let answer = sim
            .api(
                UID,
                SessionId(1),
                crate::Request::VoiceSay(crate::VoiceSay {
                    text: "bot is ready".into(),
                    agent: Some(id.clone()),
                    lane: "ready".into(),
                    voice: None,
                    interrupt: false,
                    ttl_s: None,
                }),
            )
            .expect("voice.say is answered for real (B5)");
        let utterance = answer["utterance"].as_str().unwrap().to_string();
        assert_eq!(sim.kinds(), vec![EventKind::VoiceUtterance]);
        assert_eq!(sim.events[0].data["phase"], "queued");
        assert_eq!(
            sim.store
                .snapshot()
                .voice
                .speaking
                .as_ref()
                .unwrap()
                .utterance,
            utterance
        );

        // The engine got as far as the lane.
        sim.apply(voice(VoiceIn::Utterance {
            utterance: utterance.clone(),
            phase: UtterancePhase::Started,
        }));
        assert_eq!(sim.kinds(), vec![EventKind::VoiceUtterance]);
        assert_eq!(sim.events[0].data["phase"], "started");
        assert_eq!(
            sim.events[0].data["lane"], "ready",
            "the lane comes from what the store asked for, not from the engine"
        );
        assert_eq!(sim.events[0].data["agent"], id.0);
        assert!(
            sim.store.snapshot().voice.speaking.is_some(),
            "started does not end it"
        );

        // …and to the end of it.
        sim.apply(voice(VoiceIn::Utterance {
            utterance: utterance.clone(),
            phase: UtterancePhase::Ended,
        }));
        assert_eq!(sim.events[0].data["phase"], "ended");
        assert!(
            sim.store.snapshot().voice.speaking.is_none(),
            "ended is the only thing that clears `speaking` on its own"
        );

        // A late `ended` for an utterance that was cut arrives *after* the
        // next one was queued — the cut and the queue are two threads — and
        // it must not clear the new one.
        let second = sim
            .api(
                UID,
                SessionId(1),
                crate::Request::VoiceSay(crate::VoiceSay {
                    text: "bot failed".into(),
                    agent: Some(id.clone()),
                    lane: "ready".into(),
                    voice: None,
                    interrupt: false,
                    ttl_s: None,
                }),
            )
            .unwrap()["utterance"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(second, utterance);
        sim.apply(voice(VoiceIn::Utterance {
            utterance,
            phase: UtterancePhase::Ended,
        }));
        assert_eq!(
            sim.store
                .snapshot()
                .voice
                .speaking
                .as_ref()
                .map(|s| s.utterance.clone()),
            Some(second),
            "a stale `ended` is not an error and must not silence the live one"
        );
        assert_eq!(sim.events[0].data["lane"], serde_json::Value::Null);
    }

    /// B8: the end of a PTT session is `voice.state{state:"ended", empty}`.
    ///
    /// §7.4 names the edge `ptt.ended`, §4.12's table does not list it and
    /// no `EventKind` existed, so it had no home. This contract puts it on
    /// `voice.state` rather than inventing a kind, because a second kind
    /// for the same edge has to be subscribed to separately and every
    /// client already watching `voice.*` would miss exactly the event that
    /// says a session is over.
    ///
    /// `empty` is the fact nothing else gives: "nothing was dispatched" and
    /// "the transcript was empty" look identical from outside — no
    /// `op.order`, no `card.answered` — and they are different things. One
    /// is worth showing the operator; the other is them pressing the button
    /// and saying nothing. Absence of events has no timeout to wait for.
    #[test]
    fn a_session_ends_on_voice_state_and_says_whether_anything_was_said() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        let ended = |sim: &Sim| -> crate::Event {
            sim.events
                .iter()
                .find(|e| e.kind == EventKind::VoiceState && e.data["state"] == "ended")
                .cloned()
                .expect("voice.state{state:\"ended\"}")
        };

        // Words: `empty:false`, and the order really went out.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(final_("run the tests"));
        let e = ended(&sim);
        assert_eq!(e.data["empty"], false);
        assert!(sim.kinds().contains(&EventKind::OrderDispatched));

        // Silence: `empty:true`, and **nothing** was dispatched (§7.4).
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(final_(""));
        let e = ended(&sim);
        assert_eq!(e.data["empty"], true);
        assert!(!sim.kinds().contains(&EventKind::OrderDispatched));
        // Whitespace, and text that is nothing but escapes, are silence too
        // — the sanitiser runs before the emptiness test, so a transcript
        // of `\x1b[2J` is not "something was said".
        for quiet in ["   ", "\u{1b}[2J", "\n\t"] {
            sim.apply(ptt(1, PttCtx::Talk, None, false));
            sim.apply(ptt(0, PttCtx::Talk, None, false));
            sim.drain();
            sim.apply(final_(quiet));
            assert_eq!(ended(&sim).data["empty"], true, "{quiet:?}");
        }

        // It is `voice.state`, not a kind of its own: a client subscribed
        // to `voice.*` sees the whole session, start to end.
        sim.apply(ptt(1, PttCtx::Answer, None, false));
        sim.apply(ptt(0, PttCtx::Answer, None, false));
        sim.drain();
        sim.apply(final_("hello"));
        let states: Vec<&str> = sim
            .events
            .iter()
            .filter(|e| e.kind == EventKind::VoiceState)
            .filter_map(|e| e.data["state"].as_str())
            .collect();
        assert_eq!(states, vec!["ended"]);
        assert!(
            !crate::EventKind::ALL
                .iter()
                .any(|k| k.as_str() == "ptt.ended"),
            "no new event kind was invented for this edge"
        );
        // An abort ends the session without ever reaching here: the
        // operator took the words back, and `aborted` already said so.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, true));
        sim.drain();
        sim.apply(final_("ignored"));
        assert!(sim.events.is_empty());
    }

    /// B7: §7.2's route machine is reachable at all, and it can name the
    /// two states it could not before.
    ///
    /// `Route` had zero uses, the variant was `Sco` where §7.2 says
    /// `ScoOurs`, and `Reopening` did not exist — so the state the deck
    /// spends the 2307 ms after **every** press in (V1's measured A2DP
    /// resume) had no name, and `voice.route` could not report it. The
    /// distinction `ScoOurs` draws is the machine's whole point: SCO
    /// because we opened the mic is a state we leave on the release, SCO
    /// because another app did (V7: the deck is the system default input
    /// here, so that is the routine path) is one we can only wait out.
    #[test]
    fn the_route_machine_can_name_our_sco_and_the_reopen_after_it() {
        let mut sim = Sim::new();
        let route = |route: crate::Route, stuck: bool| {
            voice(VoiceIn::Route {
                route,
                output_uid: None,
                input_uid: None,
                stuck,
                slow_sco_ms: None,
            })
        };
        let wire = |r: crate::Route| serde_json::to_value(r).unwrap();
        // The names §7.2 draws the machine with.
        assert_eq!(wire(crate::Route::ScoOurs), "sco_ours");
        assert_eq!(wire(crate::Route::Reopening), "reopening");
        assert_eq!(wire(crate::Route::ScoExternal), "sco_external");
        assert_eq!(wire(crate::Route::ScoStuck), "sco_stuck");
        assert_eq!(crate::Route::default(), crate::Route::None);

        // The happy path of §7.2, as the engine reports it: our press, our
        // release, the reopen, A2DP again.
        for (state, name) in [
            (crate::Route::A2dp, "a2dp"),
            (crate::Route::ScoOurs, "sco_ours"),
            (crate::Route::Reopening, "reopening"),
            (crate::Route::A2dp, "a2dp"),
        ] {
            sim.apply(route(state, false));
            assert_eq!(sim.events[0].kind, EventKind::VoiceRoute, "{name}");
            assert_eq!(sim.events[0].data["route"], name);
            assert_eq!(sim.store.snapshot().voice.route, state);
        }
        // V7's routine path: someone else's capture, and no press of ours.
        sim.apply(route(crate::Route::ScoExternal, false));
        assert_eq!(sim.events[0].data["route"], "sco_external");
        assert!(
            sim.store.snapshot().voice.ptt.is_none(),
            "an external flip is not a session of ours"
        );
        // Two failed reopens, then recovery on the next rate change.
        sim.apply(route(crate::Route::ScoStuck, true));
        assert_eq!(sim.events[0].data["stuck"], true);
        sim.apply(route(crate::Route::A2dp, false));
        assert_eq!(sim.events[0].data["stuck"], false);
        assert_eq!(sim.store.snapshot().voice.route, crate::Route::A2dp);
    }

    /// B1 + §7.4's `Dispatch(ctx)`: the same final transcript means four
    /// different things depending on the ctx the press carried.
    #[test]
    fn the_final_transcript_is_dispatched_by_its_ctx() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let card = sim.set_card(
            &id,
            crate::CardKind::Q,
            crate::Deliver::Poll,
            vec!["Allow", "Deny"],
        );
        sim.drain();

        // `answer` on a `q` card: `op.answer{text}` verbatim. The §5.6
        // grammar runs in the **adapter**, which is the only side that
        // knows what "allow" means to its CLI; the reducer's job is to say
        // what was heard and to whom.
        sim.apply(ptt(1, PttCtx::Answer, Some(card.0), false));
        sim.apply(ptt(0, PttCtx::Answer, Some(card.0), false));
        sim.drain();
        sim.apply(final_("yes"));
        assert!(sim.kinds().contains(&EventKind::CardAnswered));
        assert_eq!(
            sim.events[0].data["intent"], "allow",
            "§4.12: the event names what the grammar made of it"
        );
        let answered = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::CardAnswered)
            .expect("answered");
        assert_eq!(answered.data["text"], "yes");
        assert_eq!(answered.data["source"], "ptt");
        assert_eq!(answered.data["principal"], "device");

        // `name`: `device.name`, cut and title-cased by B15 on the way; the
        // words themselves go to the deck as well, on the press's slot.
        sim.apply(ptt(1, PttCtx::Name, None, false));
        sim.apply(ptt(0, PttCtx::Name, None, false));
        sim.drain();
        sim.apply(final_("deploy staging now"));
        assert!(sim
            .effects
            .contains(&Effect::Device(crate::DeviceOp::Control(
                crate::Control::Name("Deploy Staging N".into())
            ))));
        assert!(sim
            .effects
            .contains(&Effect::Device(crate::DeviceOp::Control(
                crate::Control::Transcript {
                    slot: crate::Slot(1),
                    text: "deploy staging now".into(),
                    last: true, press: None, failed: false, channel_tip: false, rec: false,
                }
            ))));

        // `talk` + `stop|quiet`: a barge-in, not an order.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(final_("quiet"));
        assert_eq!(sim.events[0].data["intent"], "stop");
        assert!(sim.effects.contains(&Effect::Audio(AudioCmd::Stop {
            utterance: None,
            lane: None,
            all: true,
        })));
        assert!(!sim.kinds().contains(&EventKind::OrderDispatched));

        // `talk` + `report|status`: the deck speaks the agent's state.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(final_("status"));
        assert_eq!(sim.events[0].data["intent"], "report");
        let said = sim
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Audio(AudioCmd::Say { text, lane, .. }) => {
                    Some((text.clone(), lane.clone()))
                }
                _ => None,
            })
            .expect("a report is spoken");
        assert!(said.0.starts_with("bot is "), "{said:?}");
        assert_eq!(said.1, "chatter");

        // An empty final dispatches nothing at all (§7.4).
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(final_("   "));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::TranscriptFinal, EventKind::VoiceState]
        );
        assert_eq!(sim.events[1].data["state"], "ended");
    }

    /// B1: an engine error ends the session and says why, and an abort
    /// throws away a transcript that was still in flight.
    #[test]
    fn an_engine_error_ends_the_session_and_an_abort_discards_the_words() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(voice(VoiceIn::Error {
            code: "mic unavailable".into(),
            message: Some("no non-zero samples".into()),
        }));
        assert_eq!(sim.events[0].kind, EventKind::VoiceState);
        assert_eq!(sim.events[0].data["state"], "aborted");
        assert_eq!(sim.events[0].data["error"], "mic unavailable");
        assert_eq!(sim.events[0].data["message"], "no non-zero samples");
        assert!(sim.store.snapshot().voice.ptt.is_none());
        assert!(sim
            .effects
            .contains(&Effect::Audio(AudioCmd::Finish { abort: true })));
        // …and the engine's final for that session is discarded.
        sim.apply(final_("this never happened"));
        assert!(sim.events.is_empty(), "{:?}", sim.events);

        // An abort on the release is the same: the operator took the words
        // back, so a final that arrives after it dispatches nothing.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, true));
        sim.drain();
        sim.apply(final_("rm -rf /"));
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        // An error with no session at all is still reportable (the route
        // SM and the permissions live outside a session).
        sim.apply(voice(VoiceIn::Error {
            code: "device_gone".into(),
            message: None,
        }));
        assert_eq!(sim.events[0].data["error"], "device_gone");
        assert!(sim.effects.is_empty(), "nothing to tell the engine to stop");
    }

    /// The seam the foundation left open (HOST-CONTRACT-W4 §8): a
    /// **dictated answer** could not name the utterance it came from.
    ///
    /// B12 threaded `utterance` through `OpOrder`, which covers everything
    /// that goes out as an order — including, it turns out, the Ready-card
    /// release the contract worried about, because §7.4 routes that to
    /// `dispatch_order` rather than to `Store::answer`. What it does *not*
    /// cover is the ordinary case: a spoken answer to a **question**,
    /// which goes through `Store::answer`/`AnswerInput`, where there was
    /// no field to carry it.
    ///
    /// That is the one that matters. A permission card answered by voice
    /// is the highest-consequence thing this daemon does, and the owner
    /// was handed `{text: "allow"}` with no way back to the recording it
    /// came from: not the `transcript.final`, not the partials the
    /// operator watched, not the `voice.state` that says which session it
    /// was. Matching on the text does not work, because the grammar
    /// rewrites it and two presses a minute apart produce the same string.
    #[test]
    fn a_dictated_answer_names_the_utterance_it_came_from() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        let card = sim.set_card(
            &id,
            crate::CardKind::Q,
            crate::Deliver::Poll,
            vec!["Allow", "Deny"],
        );
        sim.drain();
        sim.apply(ptt(1, PttCtx::Answer, Some(card.0), false));
        let utterance = sim
            .store
            .snapshot()
            .voice
            .ptt
            .as_ref()
            .expect("listening")
            .utterance
            .clone();
        sim.apply(ptt(0, PttCtx::Answer, Some(card.0), false));
        sim.drain();
        sim.apply(final_("use the staging database"));

        let answered = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::CardAnswered)
            .expect("card.answered");
        assert_eq!(
            answered.data["utterance"], utterance,
            "the decision names the recording it came from"
        );
        // …and so does the answer the **owner** is handed, which is the
        // one that can act on it.
        let delivered = sim
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Deliver { answer, .. } => Some(answer.clone()),
                _ => None,
            })
            .expect("the answer went to the owner");
        assert_eq!(delivered.utterance.as_deref(), Some(utterance.as_str()));
        assert_eq!(delivered.text.as_deref(), Some("use the staging database"));

        // A typed answer has no utterance, and says so rather than
        // inventing one.
        let card = sim.set_card(
            &id,
            crate::CardKind::Q,
            crate::Deliver::Poll,
            vec!["Allow", "Deny"],
        );
        sim.api(
            UID,
            SessionId(1),
            crate::Request::OpAnswer(crate::OpAnswer {
                agent: id.clone(),
                card,
                choice: Some(0),
                text: None,
            }),
        )
        .expect("op.answer");
        let answered = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::CardAnswered)
            .expect("card.answered");
        assert_eq!(answered.data["utterance"], serde_json::Value::Null);

        // The Ready-card release the contract named reaches
        // `order.dispatched` through `op_order`, not through `answer` —
        // and it was already carrying its utterance (B12). Pinned so the
        // two paths cannot drift apart again.
        sim.apply(Command::Api {
            principal: UID,
            session: SessionId(1),
            req: Box::new(crate::Request::CardClear(crate::CardClear {
                agent: id.clone(),
                card,
                delivered: true,
                reason: None,
            })),
        });
        let ready = sim.set_card(&id, crate::CardKind::R, crate::Deliver::Poll, vec!["Go"]);
        sim.drain();
        sim.apply(ptt(1, PttCtx::Answer, Some(ready.0), false));
        let utterance = sim
            .store
            .snapshot()
            .voice
            .ptt
            .as_ref()
            .expect("listening")
            .utterance
            .clone();
        sim.apply(ptt(0, PttCtx::Answer, Some(ready.0), false));
        sim.drain();
        sim.apply(final_("ship it"));
        let dispatched = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::OrderDispatched)
            .expect("a Ready release is an order");
        assert_eq!(dispatched.data["utterance"], utterance);
    }

    /// A press with nobody on the strip (`ptt ctx:name` carries agent 0: a
    /// face name, a key label) still has its words shown while the button
    /// is held: the transcript goes to the deck on slot 0, partials and the
    /// final alike, and the name goes as its own line after it.
    #[test]
    fn a_press_with_nobody_on_the_strip_still_shows_its_words() {
        let mut sim = Sim::new();
        let press = |act: u8| {
            Command::Device(DeviceLine::Ptt {
                act,
                slot: 0,
                ctx: PttCtx::Name,
                card: None,
                abort: false, press: None,
            })
        };
        sim.apply(press(1));
        sim.drain();
        sim.apply(partial("copy"));
        let transcript = |text: &str, last: bool| {
            Effect::Device(crate::DeviceOp::Control(crate::Control::Transcript {
                slot: crate::Slot(0),
                text: text.into(),
                last, press: None, failed: false, channel_tip: false, rec: false,
            }))
        };
        assert!(sim.effects.contains(&transcript("copy", false)), "{:?}", sim.effects);
        sim.apply(press(0));
        sim.drain();
        sim.apply(final_("copy that"));
        assert!(sim.effects.contains(&transcript("copy that", true)), "{:?}", sim.effects);
        assert!(sim
            .effects
            .contains(&Effect::Device(crate::DeviceOp::Control(
                crate::Control::Name("Copy That".into())
            ))));
        // No slot, no transcript window: that is the agents face's.
        assert!(!sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Device(crate::DeviceOp::Put(crate::Key::DocText(..), _)))));
    }

    fn ptt(act: u8, ctx: PttCtx, card: Option<u16>, abort: bool) -> Command {
        Command::Device(DeviceLine::Ptt {
            act,
            slot: 1,
            ctx,
            card,
            abort, press: None,
        })
    }

    #[test]
    fn the_device_press_identity_survives_finalization_and_failure() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        let edge = |act, press| Command::Device(DeviceLine::Ptt {
            act, slot: 1, ctx: PttCtx::Talk, card: None, abort: false, press: Some(press),
        });
        sim.apply(edge(1, 41));
        sim.apply(edge(0, 41));
        sim.apply(edge(1, 42));
        sim.apply(edge(0, 41));
        assert!(sim.store.snapshot().voice.ptt.is_some(), "a stale release cannot end the new press");
        sim.drain();
        sim.apply(final_("first complete message"));
        assert!(sim.effects.iter().any(|e| matches!(e, Effect::Device(crate::DeviceOp::Control(crate::Control::Transcript { press: Some(41), last: true, failed: false, channel_tip: false, rec: false, .. })))));
        sim.apply(edge(0, 42));
        sim.drain();
        sim.apply(voice(VoiceIn::FinalFailed { code: "engine".into(), message: "failed".into() }));
        assert!(sim.effects.iter().any(|e| matches!(e, Effect::Device(crate::DeviceOp::Control(crate::Control::Transcript { press: Some(42), last: true, failed: true, channel_tip: false, rec: false, .. })))));
    }

    /// B2: a PTT session records **who** started it, including the session
    /// id, and the deck's press is one caller of `start_listen` among
    /// others rather than the only way in.
    ///
    /// §4.9 promises that a dictated answer carries the caller's principal
    /// **and** session, never `device`. Ownership in this daemon is per
    /// session and never per principal (§4.2), so `principal` alone cannot
    /// say which connection asked — two windows of the same GUI are the
    /// same `uid:501`.
    #[test]
    fn a_session_records_who_started_it_and_a_deck_press_is_one_caller_of_that() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        // A deck edge: `device`, and no session, because there is no
        // connection behind a button.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let p = sim.store.snapshot().voice.ptt.clone().expect("listening");
        assert_eq!(p.principal, Principal::Device);
        assert_eq!(p.session, None);
        assert_eq!(sim.events[0].principal, Principal::Device);
        assert_eq!(sim.events[0].data["session"], serde_json::Value::Null);
        sim.apply(ptt(0, PttCtx::Talk, None, false));

        // The same machine entered by an API caller, which is what
        // `voice.listen` becomes: its principal and its session are kept,
        // and the events are attributed to it.
        sim.store.start_listen(
            UID,
            Some(SessionId(9)),
            Some(id.clone()),
            PttCtx::Answer,
            Some(CardId(3)),
            VoiceInput::Mac,
            60,
        );
        sim.drain();
        let p = sim.store.snapshot().voice.ptt.clone().expect("listening");
        assert_eq!(p.principal, UID);
        assert_eq!(p.session, Some(SessionId(9)));
        assert_eq!(p.agent, Some(id));
        assert_eq!(p.ctx, PttCtx::Answer);
        assert_eq!(sim.events[0].principal, UID);
        assert_eq!(sim.events[0].data["session"], "sess_9");
        assert!(sim.effects.iter().any(|e| matches!(
            e,
            Effect::Audio(AudioCmd::Listen {
                input: VoiceInput::Mac,
                ..
            })
        )));
        // The release and the abort are attributed to the starter too, not
        // to the device that was not involved.
        sim.store.finish_listen(false);
        sim.drain();
        assert_eq!(sim.events[0].principal, UID);
        assert_eq!(sim.events[0].data["state"], "finalizing");
        assert_eq!(sim.events[0].data["session"], "sess_9");

        // `max_s` is a ceiling, not a suggestion: Apple's recogniser stops
        // at a minute whatever the caller asks for.
        sim.store.start_listen(
            UID,
            Some(SessionId(9)),
            None,
            PttCtx::Talk,
            None,
            VoiceInput::Deck,
            9_999,
        );
        sim.drain();
        assert!(sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Audio(AudioCmd::Listen { max_s: 60, .. }))));
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::PttCap,
            at: sim.t + Duration::from_secs(60)
        }));
        // A shorter one is honoured, and it is the timer that moves.
        sim.store
            .start_listen(UID, None, None, PttCtx::Talk, None, VoiceInput::Deck, 5);
        sim.drain();
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::PttCap,
            at: sim.t + Duration::from_secs(5)
        }));
        assert_eq!(
            sim.store.snapshot().voice.ptt.as_ref().unwrap().session,
            None,
            "a caller with no session is not the same as `device`"
        );
        assert_eq!(
            sim.store.snapshot().voice.ptt.as_ref().unwrap().principal,
            UID
        );
    }

    /// B3: the open tells the engine **which microphone**, **what
    /// language** and **which words to expect**.
    ///
    /// `{agent, ctx, card, max_s}` was all it carried, and none of that
    /// chooses a device or builds a recogniser. The hints matter most: an
    /// en-US recogniser has no reason to know `cohdl-api` or `Allow always`
    /// and every reason to mangle them, and those are exactly the strings
    /// §5.6's grammar and the answer mapping then look for.
    #[test]
    fn the_open_carries_the_input_the_locale_and_the_words_to_expect() {
        let mut sim = Sim::with(|cfg| cfg.locale = "de-DE".into());
        assert_eq!(
            sim.store.snapshot().voice.stt.locale,
            "de-DE",
            "the snapshot says what the next capture will use"
        );
        let id = sim.register(UID, SessionId(1), "cohdl-api", None);
        let other = sim.register(UID, SessionId(1), "web-ui", None);
        let card = sim.set_card(
            &id,
            crate::CardKind::Q,
            crate::Deliver::Poll,
            vec!["Allow", "Allow always", "Deny"],
        );
        sim.drain();
        sim.apply(ptt(1, PttCtx::Answer, Some(card.0), false));
        let listen = sim
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Audio(cmd @ AudioCmd::Listen { .. }) => Some(cmd.clone()),
                _ => None,
            })
            .expect("a press opens capture");
        let AudioCmd::Listen {
            input,
            locale,
            contextual,
            ..
        } = listen
        else {
            unreachable!()
        };
        assert_eq!(input, VoiceInput::Deck, "a deck edge is the deck's mic");
        assert_eq!(locale, "de-DE", "the configured locale, not a default");
        // Every agent name, then this card's labels, in that order and
        // without duplicates.
        assert_eq!(
            contextual,
            vec!["cohdl-api", "web-ui", "Allow", "Allow always", "Deny"]
        );
        assert!(other.0.starts_with("ag_"));
        // A press that names no card still carries the names, and the
        // labels of whatever card that agent is showing.
        sim.apply(ptt(0, PttCtx::Answer, Some(card.0), false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let Some(Effect::Audio(AudioCmd::Listen { contextual, .. })) = sim
            .effects
            .iter()
            .find(|e| matches!(e, Effect::Audio(AudioCmd::Listen { .. })))
        else {
            panic!("{:?}", sim.effects)
        };
        assert_eq!(
            contextual,
            &vec![
                "cohdl-api".to_string(),
                "web-ui".into(),
                "Allow".into(),
                "Allow always".into(),
                "Deny".into()
            ]
        );
    }

    #[test]
    fn a_press_to_a_transcribing_companion_names_it_and_nothing_else_does() {
        let listen_companion = |sim: &Sim| {
            sim.effects.iter().find_map(|e| match e {
                Effect::Audio(AudioCmd::Listen { companion, .. }) => Some(companion.clone()),
                _ => None,
            })
        };
        let register = |sim: &mut Sim, transcribe: crate::TranscribeCap| {
            let mut req = crate::testkit::register("conol chat", None);
            req.caps.transcribe = transcribe;
            let res = sim
                .api(UID, SessionId(1), crate::Request::AgentRegister(req))
                .expect("register");
            crate::AgentId(res["agent"].as_str().unwrap().to_string())
        };
        let mut sim = Sim::new();
        let id = register(&mut sim, crate::TranscribeCap::Push);
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        assert_eq!(listen_companion(&sim), Some(Some(id.clone())), "a talk press goes to the owner");
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Name, None, false));
        assert_eq!(listen_companion(&sim), Some(None), "naming the deck is the daemon's own");
        sim.apply(ptt(0, PttCtx::Name, None, false));

        let mut off = Sim::with(|cfg| cfg.companion_stt = false);
        register(&mut off, crate::TranscribeCap::Push);
        off.apply(ptt(1, PttCtx::Talk, None, false));
        assert_eq!(listen_companion(&off), Some(None), "[voice.stt] companion = false keeps it local");

        let mut plain = Sim::new();
        register(&mut plain, crate::TranscribeCap::None);
        plain.apply(ptt(1, PttCtx::Talk, None, false));
        assert_eq!(listen_companion(&plain), Some(None), "no cap, no offer");
    }

    #[test]
    fn a_companion_press_shows_a_recording_and_never_the_local_partials() {
        let transcripts = |sim: &Sim| -> Vec<crate::Control> {
            sim.effects.iter().filter_map(|e| match e {
                Effect::Device(crate::DeviceOp::Control(c @ crate::Control::Transcript { .. })) => Some(c.clone()),
                _ => None,
            }).collect()
        };
        let press = |act| Command::Device(DeviceLine::Ptt {
            act, slot: 1, ctx: PttCtx::Talk, card: None, abort: false, press: Some(9),
        });
        let mut sim = Sim::new();
        let mut req = crate::testkit::register("conol chat", None);
        req.caps.transcribe = crate::TranscribeCap::Push;
        sim.api(UID, SessionId(1), crate::Request::AgentRegister(req)).unwrap();
        sim.apply(press(1));
        assert_eq!(sim.store.snapshot().voice.ptt.as_ref().map(|p| p.companion), Some(true));
        let arming = sim.events.iter().find(|e| e.kind == EventKind::VoiceState).unwrap();
        assert_eq!(arming.data["companion"], true);
        assert!(matches!(
            transcripts(&sim).as_slice(),
            [crate::Control::Transcript { rec: true, last: false, press: Some(9), text, .. }] if text.is_empty()
        ), "the press opens with the recording marker: {:?}", transcripts(&sim));
        sim.apply(partial("the local guess"));
        assert!(transcripts(&sim).is_empty(), "the local recogniser's words never reach the deck");
        assert!(sim.kinds().contains(&EventKind::TranscriptPartial), "API observers still see the partial");
        sim.apply(press(0));
        sim.apply(final_("Conol's transcript"));
        assert!(matches!(
            transcripts(&sim).as_slice(),
            [crate::Control::Transcript { rec: false, last: true, text, .. }] if text == "Conol's transcript"
        ));

        // An ordinary press is unchanged: no marker, live words.
        let mut plain = Sim::new();
        plain.register(UID, SessionId(1), "bot", None);
        plain.apply(press(1));
        assert!(transcripts(&plain).is_empty());
        plain.apply(partial("run the"));
        assert!(matches!(transcripts(&plain).as_slice(), [crate::Control::Transcript { rec: false, last: false, .. }]));
    }

    #[test]
    fn a_press_cuts_audio_opens_capture_and_the_release_finishes() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.apply(ptt(1, PttCtx::Answer, Some(3), false));
        // The barge-in cuts what is stale and **holds** what is not. The
        // silencing is `AudioCmd::Listen`'s own (§7.3's virtual `Ptt` lane
        // plus the route SM closing the output, which is what V5's +17 dB
        // of loopback actually needs); this command only says what may be
        // thrown away, and §7.2/§7.3 say that is `Chatter`. It used to be
        // `all: true`, which discarded the very Question the operator had
        // pressed the button to answer.
        assert_eq!(
            sim.effects[0],
            Effect::Audio(AudioCmd::Stop {
                utterance: None,
                lane: Some("chatter".into()),
                all: false
            }),
            "§7.3: Question and Ready are held across a press, not flushed"
        );
        assert!(sim.effects.contains(&Effect::Audio(AudioCmd::Listen {
            agent: Some(id.clone()),
            ctx: PttCtx::Answer,
            card: Some(CardId(3)),
            max_s: 60,
            from_deck: true,
            input: VoiceInput::Deck,
            locale: "en-US".into(),
            contextual: vec!["bot".into()],
            companion: None,
        })));
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::PttCap,
            at: sim.t + Duration::from_secs(60)
        }));
        let p = sim.store.snapshot().voice.ptt.clone().unwrap();
        assert_eq!(p.principal, Principal::Device);
        assert_eq!(p.agent, Some(id));
        assert_eq!(p.card, Some(CardId(3)));
        assert_eq!(sim.events[0].kind, EventKind::VoiceState);
        assert_eq!(
            sim.events[0].data["state"], "arming",
            "§7.4: a press arms; the first non-zero sample is what listens"
        );
        assert!(!p.live);
        assert!(sim.effects.contains(&Effect::Timer {
            key: TimerKey::PttArming,
            at: sim.t + crate::store::ARMING_LIVENESS
        }));
        sim.apply(ptt(0, PttCtx::Answer, Some(3), false));
        assert_eq!(
            sim.effects,
            vec![
                Effect::Audio(AudioCmd::Finish { abort: false }),
                // A watchdog fails a stuck final without sending partial text.
                Effect::Timer {
                    key: TimerKey::PttFinalize,
                    at: sim.t + crate::store::FINALIZE_BUDGET,
                },
            ]
        );
        assert_eq!(sim.events[0].data["state"], "finalizing");
        assert!(sim.store.snapshot().voice.ptt.is_none());
        sim.apply(ptt(0, PttCtx::Answer, Some(3), false));
        assert!(sim.events.is_empty(), "a stray release is nothing");
    }

    /// The live session's utterance id.
    fn utterance(sim: &Sim) -> String {
        sim.store
            .snapshot()
            .voice
            .ptt
            .as_ref()
            .expect("a live session")
            .utterance
            .clone()
    }

    /// A timed-out session retains its identity until the engine answers.
    /// Its late final must never be sent as the next held session's words.
    #[test]
    fn a_stale_finalizing_session_cannot_dispatch_the_next_sessions_words() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();

        // Session A: pressed, dictated, released.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut_a = utterance(&sim);
        sim.apply(partial("delete everything"));
        sim.advance(Duration::from_millis(300));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert!(sim.store.snapshot().voice.ptt.is_none(), "A is finalizing");

        // Start B shortly before A's watchdog, while A is still pending.
        sim.advance(crate::store::FINALIZE_BUDGET - Duration::from_millis(300));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut_b = utterance(&sim);
        assert_ne!(ut_a, ut_b);
        sim.apply(partial("never mind"));

        // A fails without dispatching its partial; B is untouched.
        sim.advance(Duration::from_millis(500));
        assert!(!sim.events.iter().any(|e| matches!(e.kind, EventKind::AgentOrder | EventKind::TranscriptFinal)));
        let failed = sim.events.iter().find(|e| e.kind == EventKind::VoiceState && e.data["state"] == "aborted").unwrap();
        assert_eq!(failed.data["utterance"], ut_a);
        assert_eq!(failed.data["error"], "engine");
        assert!(sim.store.snapshot().voice.ptt.is_some(), "B is still held");

        // The engine's own answer for A arrives a moment later. It belongs
        // to a session that is over and must not be dispatched under B.
        sim.apply(final_("delete everything"));
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        assert_eq!(
            sim.store
                .snapshot()
                .voice
                .ptt
                .as_ref()
                .map(|p| p.utterance.clone()),
            Some(ut_b.clone()),
            "B survived the echo"
        );

        // …and B's own release and final are dispatched as B, words and all.
        sim.advance(Duration::from_millis(100));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(final_("never mind"));
        let order = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::AgentOrder)
            .expect("B dispatched its own words");
        assert_eq!(order.data["text"], "never mind");
        assert_eq!(order.data["utterance"], ut_b);
    }

    #[test]
    fn a_failed_final_does_not_abort_or_dispatch_the_next_press() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let first = utterance(&sim);
        sim.apply(partial("incomplete first message"));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let second = utterance(&sim);
        sim.apply(partial("second message"));
        sim.apply(voice(VoiceIn::FinalFailed { code: "engine".into(), message: "decode failed".into() }));
        assert!(!sim.events.iter().any(|e| matches!(e.kind, EventKind::AgentOrder | EventKind::TranscriptFinal)));
        let failure = sim.events.iter().find(|e| e.kind == EventKind::VoiceState).unwrap();
        assert_eq!(failure.data["utterance"], first);
        assert_eq!(utterance(&sim), second);
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(final_("second message complete"));
        let order = sim.events.iter().find(|e| e.kind == EventKind::AgentOrder).unwrap();
        assert_eq!(order.data["utterance"], second);
        assert_eq!(order.data["text"], "second message complete");
    }

    /// M1: aborting one session must not throw away **another** session's
    /// finished work.
    ///
    /// `abort_ptt` cleared `finalizing` unconditionally, so `up` on a
    /// session that had only just started silently discarded the sentence
    /// the operator had finished a moment earlier — no dispatch, no
    /// `transcript.final`, no `voice.state`, nothing at all on the bus. An
    /// abort means "I take back what I am saying", and the session that
    /// was already released is not what is being said.
    #[test]
    fn aborting_a_session_leaves_the_one_before_it_alone() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        let dispatched = |sim: &Sim, what: &str| {
            let order = sim
                .events
                .iter()
                .find(|e| e.kind == EventKind::AgentOrder)
                .unwrap_or_else(|| panic!("{what}: nothing was dispatched"));
            (
                order.data["text"].as_str().unwrap_or_default().to_string(),
                order.data["utterance"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        };

        // `up` on the new session. A is pressed, dictated and released:
        // complete, and on its way.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut_a = utterance(&sim);
        sim.apply(partial("ship it"));
        sim.advance(Duration::from_millis(200));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.advance(Duration::from_millis(100));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(Command::Device(DeviceLine::Up));
        assert_eq!(sim.events[0].data["reason"], "up");
        assert_eq!(sim.events.len(), 1, "one session was aborted, not two");
        sim.apply(final_("ship it"));
        assert_eq!(dispatched(&sim, "up"), ("ship it".into(), ut_a));

        // `ptt act:0 abort:1` on the new session.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut_c = utterance(&sim);
        sim.apply(partial("deploy it"));
        sim.advance(Duration::from_millis(100));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, true));
        sim.apply(final_("deploy it"));
        assert_eq!(dispatched(&sim, "abort"), ("deploy it".into(), ut_c));

        // An engine error that kills the live capture.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut_e = utterance(&sim);
        sim.apply(partial("roll it back"));
        sim.advance(Duration::from_millis(100));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(voice(VoiceIn::Error {
            code: "mic unavailable".into(),
            message: None,
        }));
        assert_eq!(sim.events[0].data["error"], "mic unavailable");
        sim.apply(final_("roll it back"));
        assert_eq!(dispatched(&sim, "error"), ("roll it back".into(), ut_e));
    }

    /// M2: cancelling a session that is still `Finalizing` has to say so.
    ///
    /// `voice.cancel`, `up` and a link drop all found `snapshot.voice.ptt`
    /// already `None` — the release cleared it — so `abort_ptt` published
    /// nothing at all. The session was dropped in silence, and a
    /// `voice.listen` caller waiting on the bus for the end of its own
    /// capture waited for ever: `ended` never came, `aborted` never came,
    /// and absence of events has no timeout.
    #[test]
    fn cancelling_a_session_that_is_still_finalizing_says_so_on_the_bus() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut = utterance(&sim);
        sim.apply(partial("ship it"));
        sim.advance(Duration::from_millis(200));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert_eq!(sim.events[0].data["state"], "finalizing");

        sim.apply(Command::Device(DeviceLine::Up));
        assert_eq!(sim.kinds(), vec![EventKind::VoiceState]);
        assert_eq!(sim.events[0].data["state"], "aborted");
        assert_eq!(sim.events[0].data["reason"], "up");
        assert_eq!(
            sim.events[0].data["utterance"], ut,
            "the event names the session that was taken back"
        );

        // …and the words really are gone: the engine's answer lands on a
        // session that is over, and the deadline has nothing to dispatch.
        sim.apply(final_("ship it"));
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        sim.advance(Duration::from_secs(2));
        assert!(!sim.kinds().contains(&EventKind::OrderDispatched));

        // A link drop takes everything there is, and says so for each: §6.3
        // — with the deck gone an utterance in flight cannot be confirmed.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(partial("again"));
        sim.advance(Duration::from_millis(100));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(Command::Link(LinkEvent::Down));
        let reasons: Vec<&str> = sim
            .events
            .iter()
            .filter(|e| e.kind == EventKind::VoiceState)
            .filter_map(|e| e.data["reason"].as_str())
            .collect();
        assert_eq!(
            reasons,
            vec!["link", "link"],
            "the capture that was open and the one still waiting"
        );
    }

    /// M5: the microphone belongs to whoever started the capture, and
    /// `voice` scope alone does not take it off them.
    ///
    /// `voice.cancel` had no ownership check of any kind and `voice.listen`
    /// had none outside §4.2's permission-card guard, so any token with
    /// `voice` could abort the deck operator's sentence mid-word, or open
    /// its own capture over the top of it (`start_listen`'s "restart"). The
    /// *trust* question — may this principal produce an allow? — is the
    /// core guard's. This one is ownership, which §4.2 makes per **session**
    /// and never per principal, and the session that started the capture is
    /// the one fact only the store holds.
    /// `voice.cancel` is owner-only (M5), so the owner going away has to
    /// be what releases the microphone — otherwise a `voice.listen` caller
    /// that disconnects mid-sentence leaves the mic held, the deck mute
    /// (a live capture holds the link in SCO), and nobody entitled to stop
    /// it until the 60 s cap.
    #[test]
    fn a_capture_dies_with_the_session_that_started_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(9), "bot", None);
        sim.store.start_listen(
            UID,
            Some(SessionId(9)),
            Some(id.clone()),
            PttCtx::Talk,
            None,
            VoiceInput::Deck,
            60,
        );
        sim.drain();
        assert!(
            sim.store.snapshot().voice.ptt.is_some(),
            "a capture is open"
        );

        // An unrelated session closing leaves it alone.
        sim.apply(Command::SessionClosed(SessionId(8)));
        assert!(
            sim.store.snapshot().voice.ptt.is_some(),
            "somebody else's disconnect is not this operator's problem"
        );

        // Its own closing releases it, and says so.
        sim.apply(Command::SessionClosed(SessionId(9)));
        assert!(
            sim.store.snapshot().voice.ptt.is_none(),
            "the microphone is let go when its owner disconnects"
        );
        assert!(
            sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Audio(AudioCmd::Finish { abort: true }))),
            "and the engine is told to stop capturing"
        );
        assert!(
            sim.events.iter().any(|e| e.kind == EventKind::VoiceState),
            "a client watching the bus is told the session ended"
        );
    }

    #[test]
    fn a_voice_capture_belongs_to_the_session_that_started_it() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        let listen = |id: &crate::AgentId| {
            crate::Request::VoiceListen(crate::VoiceListen {
                agent: id.clone(),
                ctx: "talk".into(),
                card: None,
                max_s: 60,
                input: VoiceInput::Mac,
            })
        };

        // The deck operator is holding the button.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        let ut = utterance(&sim);
        let err = sim
            .api(UID, SessionId(2), crate::Request::VoiceCancel {})
            .expect_err("a deck press is not an API caller's to cancel");
        assert_eq!(err.code, crate::ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["principal"], "device");
        assert_eq!(err.data["owner"]["session"], serde_json::Value::Null);
        let err = sim
            .api(UID, SessionId(2), listen(&id))
            .expect_err("nor to listen over");
        assert_eq!(err.code, crate::ApiError::CONFLICT);
        assert_eq!(
            sim.store
                .snapshot()
                .voice
                .ptt
                .as_ref()
                .map(|p| &p.utterance),
            Some(&ut),
            "the operator still has the microphone, mid-sentence"
        );
        // The operator's own thumb still works, and the deck never asks.
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(final_("ship it"));

        // A caller owns what it started: its own listen, its own cancel.
        sim.api(UID, SessionId(2), listen(&id))
            .expect("the microphone is free");
        assert_eq!(
            sim.store.snapshot().voice.ptt.as_ref().unwrap().session,
            Some(SessionId(2))
        );
        let err = sim
            .api(UID, SessionId(3), crate::Request::VoiceCancel {})
            .expect_err("another connection of the same uid is another session");
        assert_eq!(err.code, crate::ApiError::CONFLICT);
        assert_eq!(err.data["owner"]["session"], "sess_2");
        sim.api(UID, SessionId(2), crate::Request::VoiceCancel {})
            .expect("its own capture");
        assert!(sim.store.snapshot().voice.ptt.is_none());

        // A session still waiting for its words is owned just the same: a
        // cancel discards it, so it is not a stranger's to discard.
        sim.api(UID, SessionId(2), listen(&id))
            .expect("the microphone is free again");
        sim.store.finish_listen(false);
        sim.drain();
        let err = sim
            .api(UID, SessionId(3), crate::Request::VoiceCancel {})
            .expect_err("still session 2's");
        assert_eq!(err.code, crate::ApiError::CONFLICT);
        sim.api(UID, SessionId(2), crate::Request::VoiceCancel {})
            .expect("its own");
        assert_eq!(sim.events[0].data["state"], "aborted");

        // With nothing to cancel at all, a cancel is idempotent, not a
        // refusal: a client that lost the race should not have to care.
        sim.api(UID, SessionId(3), crate::Request::VoiceCancel {})
            .expect("nothing is listening");
    }

    /// M6: §7.4's "`channel` arriving mid-session is applied after
    /// release" (G:222-223).
    ///
    /// It was applied on the spot, `Effect::Desktop(Focus)` and all — so
    /// turning the knob while dictating raised another terminal under the
    /// operator's hands mid-sentence. Attribution was never at risk (the
    /// agent is latched in `PttInfo` at the press), which is what makes
    /// this a focus-timing divergence and not a mis-dispatch, but the
    /// timing is the whole of what §7.4 legislates here.
    ///
    /// The **ack** still goes back at once: it is about ordering, not
    /// about the PC having done anything (P:118).
    #[test]
    fn a_channel_turned_mid_session_is_applied_after_the_release() {
        let mut sim = Sim::new();
        let a = sim.register(UID, SessionId(1), "bot", None);
        let b = sim.register(UID, SessionId(1), "web-ui", None);
        assert_eq!(sim.slot_of(&a), Some(Slot(1)));
        assert_eq!(sim.slot_of(&b), Some(Slot(2)));
        sim.drain();

        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.drain();
        sim.apply(Command::Device(crate::DeviceLine::Channel {
            slot: 2,
            seq: 7,
        }));
        assert!(
            sim.effects
                .contains(&Effect::Device(crate::DeviceOp::Control(
                    crate::Control::Focus {
                        slot: Slot(2),
                        ack: Some(7),
                    }
                ))),
            "the deck is acked at once: {:?}",
            sim.effects
        );
        assert!(
            !sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))),
            "no terminal is raised under a dictating operator: {:?}",
            sim.effects
        );
        assert!(sim.events.is_empty(), "{:?}", sim.kinds());
        assert_eq!(sim.store.snapshot().focus.agent, None);

        // The release applies it, once, with the seq the deck sent.
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert_eq!(sim.store.snapshot().focus.agent.as_ref(), Some(&b));
        assert_eq!(sim.store.snapshot().focus.seq, Some(7));
        assert!(sim
            .effects
            .contains(&Effect::Desktop(crate::DesktopCmd::Focus {
                agent: b.clone()
            })));
        let moved = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::FocusChanged)
            .expect("focus.changed on the release");
        assert_eq!(moved.data["slot"], 2);
        assert_eq!(moved.data["seq"], 7);
        assert_eq!(moved.data["source"], "device");
        // …and only once: the latch is taken, not copied.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert!(!sim.kinds().contains(&EventKind::FocusChanged));

        // With nothing listening it is immediate, as it always was.
        sim.apply(final_(""));
        sim.apply(Command::Device(crate::DeviceLine::Channel {
            slot: 1,
            seq: 8,
        }));
        assert_eq!(sim.store.snapshot().focus.agent.as_ref(), Some(&a));
        assert!(sim.kinds().contains(&EventKind::FocusChanged));
    }

    /// D5: a final that arrives while the button is **still held** has to
    /// close the capture itself, because nothing else can any more.
    ///
    /// The engine can reach its own end of a session without being asked —
    /// Apple's recogniser stops at a minute whatever the operator's thumb
    /// is doing — and the store used to end the session on that final
    /// without telling the engine. After it, `finish_listen` has no `ptt`
    /// to take, so the release is a no-op; `ptt_cap_at` was cleared, so
    /// the 60 s cap is gone too. The microphone stays open for ever,
    /// `lanes.ptt` stays true (which silences every lane) and the route
    /// sticks in `ScoOurs` until the next press.
    #[test]
    fn a_final_that_arrives_mid_hold_closes_the_capture() {
        let mut sim = Sim::new();
        sim.register(UID, SessionId(1), "bot", None);
        sim.drain();
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        assert!(sim
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Audio(AudioCmd::Listen { .. }))));
        sim.drain();

        // The engine hit its own limit; the operator is still holding.
        sim.apply(final_("run the tests"));
        assert!(
            sim.store.snapshot().voice.ptt.is_none(),
            "the store ended it"
        );
        assert!(
            sim.effects
                .contains(&Effect::Audio(AudioCmd::Finish { abort: true })),
            "the mic is closed on the spot, by the only edge that still can: {:?}",
            sim.effects
        );
        assert!(sim.kinds().contains(&EventKind::OrderDispatched));

        // The release that follows is a stray, not the thing that had to
        // close the capture, and the 60 s cap is not it either.
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        assert!(sim.events.is_empty() && sim.effects.is_empty());
        sim.advance(Duration::from_secs(70));
        assert!(
            !sim.effects
                .iter()
                .any(|e| matches!(e, Effect::Audio(AudioCmd::Finish { .. }))),
            "nothing is left to close"
        );
    }

    #[test]
    fn abort_up_link_drop_and_cap_are_handled() {
        let mut sim = Sim::new();
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(0, PttCtx::Talk, None, true));
        assert_eq!(
            sim.effects,
            vec![Effect::Audio(AudioCmd::Finish { abort: true })]
        );
        assert_eq!(sim.events[0].data["state"], "aborted");
        assert_eq!(sim.events[0].data["reason"], "abort");
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(Command::Device(DeviceLine::Up));
        assert!(sim.store.snapshot().voice.ptt.is_none());
        assert_eq!(sim.events[0].data["reason"], "up");
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(Command::Link(LinkEvent::Down));
        assert_eq!(sim.events.last().unwrap().data["reason"], "link");
        sim.apply(ptt(1, PttCtx::Name, None, false));
        // A capture that is producing audio is not a dead mic, so the 60 s
        // cap is what ends it — without this the `Arming` deadline would
        // (correctly) fire first, which is the point of the row below.
        sim.apply(voice(VoiceIn::Live));
        sim.advance(Duration::from_secs(59));
        assert!(sim.store.snapshot().voice.ptt.is_some());
        sim.advance(Duration::from_secs(2));
        assert!(sim.store.snapshot().voice.ptt.is_none());
        assert_eq!(sim.events[0].data["reason"], "cap");
        // A second press restarts the session.
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        sim.apply(ptt(1, PttCtx::Talk, None, false));
        assert_eq!(sim.events[0].data["reason"], "restart");
        assert_eq!(sim.events[1].data["state"], "arming");
        sim.apply(ptt(0, PttCtx::Talk, None, false));
        sim.apply(Command::Shutdown);
    }
}
