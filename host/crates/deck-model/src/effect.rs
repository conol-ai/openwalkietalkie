//! Every observable change (`Event`) and every side effect (`Effect`) the
//! reducer produces, as data (design §3).
use crate::{
    AgentId, Answer, CardId, Deliver, DeviceOp, FaceEntry, Principal, SetKey, Slot, Ts, Usage,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// The timer wheel's key: a later `Command::Tick` re-enters the reducer,
/// which looks at the state, never at the key alone (stale timers are harmless).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerKey {
    DeliveryTimeout(CardId),
    HoldExpiry(CardId),
    Lease(AgentId),
    Grace(AgentId),
    ReadyTtl(CardId),
    /// The 60 s PTT cap.
    PttCap,
    /// §7.4's `Arming` liveness deadline: no non-zero sample by then and
    /// the microphone is reported unavailable (V2).
    PttArming,
    /// §7.4's `Finalizing` budget: the release has happened and the words
    /// are still on their way; past it the last partial is the final.
    PttFinalize,
    /// Re-send the cached `usage` if no feeder answered within 1.5 s.
    UsageRefresh,
}

/// Why a parked hook was resolved without a decision.
///
/// The distinction an adapter needs is not *who* released the hold but
/// **what is left on the deck**: a release that runs `Store::degrade_card`
/// leaves the same card id in place as §5.6's `⌨ Focus terminal`, pointing
/// at the dialog the CLI is still showing, and one that clears or retires
/// the card leaves nothing. Both used to arrive as `Expiry`, so an adapter
/// that remembered "the degraded card of this session" filed a card id the
/// store no longer had and then believed that session was carded for ever.
/// [`Release::degrades_card`] is that distinction, and the variants below
/// are grouped by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Release {
    /// Device CANCEL / `op.snooze`: degraded, still on the deck.
    Snooze,
    /// The hold budget (`[hooks.<cli>] hold_s`) ran out: degraded, still
    /// on the deck.
    Expiry,
    /// The hook client went away (its timeout, Ctrl-C, a reset
    /// connection): degraded at once, still on the deck (§12).
    HoldGone,
    /// The owning session went away and the agent is orphaned: degraded,
    /// still on the deck under a disconnected agent.
    AgentGone,
    /// The card was taken **off** the deck under the parked hook:
    /// `agent.card.clear`, a new card retiring this one, `op.ack`, a Ready
    /// card's TTL, or the agent being removed for good. Nothing is left
    /// behind — unlike every variant above, there is no degraded card.
    Cleared,
    /// The daemon is shutting down: every parked hook is answered `{}` so
    /// the CLIs show their own dialogs. Nothing is degraded, because
    /// nothing is about to be drawn.
    Shutdown,
    /// A group card answered: the accumulator keeps the answer, the card
    /// is cleared and card k+1 follows.
    GroupStep,
}

impl Release {
    /// Whether this release leaves the card on the deck, degraded to
    /// §5.6's `⌨ Focus terminal` under its own id.
    ///
    /// An adapter that tracks "the card of this session the CLI's own turn
    /// may take off the deck" asks this before it files one: a card that
    /// was cleared is not there to be cleared again.
    #[must_use]
    pub const fn degrades_card(self) -> bool {
        match self {
            Release::Snooze | Release::Expiry | Release::HoldGone | Release::AgentGone => true,
            Release::Cleared | Release::Shutdown | Release::GroupStep => false,
        }
    }
}

/// Which microphone a PTT session opens (§4.9 `voice.listen{input}`, §7.1's
/// `fallback_input`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoiceInput {
    /// The deck's own input UID — the only one a deck `ptt` edge ever means.
    #[default]
    Deck,
    /// The Mac's default input device.
    Mac,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "audio", rename_all = "snake_case")]
pub enum AudioCmd {
    Say {
        utterance: String,
        text: String,
        agent: Option<AgentId>,
        lane: String,
        voice: Option<String>,
        interrupt: bool,
        /// §4.9's `ttl_s?:30`: drop this one if it has not started by then.
        /// Carried rather than dropped at the API (B5) — a caller that
        /// asked for a deadline and silently did not get one is worse off
        /// than one that was refused.
        #[serde(default)]
        ttl_s: Option<u32>,
    },
    Stop {
        utterance: Option<String>,
        lane: Option<String>,
        all: bool,
    },
    Chime {
        which: String,
    },
    /// Open capture for a PTT session.
    ///
    /// B3: `{agent, ctx, card, max_s}` was everything the engine was told,
    /// and none of it says **which microphone** or **what language**. The
    /// four fields below are the rest of §7.1/§7.6's open, and they have to
    /// come from here because the reducer is the only place that holds the
    /// snapshot this open is about.
    Listen {
        agent: Option<AgentId>,
        ctx: crate::PttCtx,
        card: Option<CardId>,
        max_s: u32,
        /// The deck's own PTT press opened this session (`principal:
        /// "device"`, no API session behind it), so the deck is already
        /// streaming its microphone. False for `voice.listen`: over the BLE
        /// voice link the deck streams only when asked (a MIC request).
        #[serde(default)]
        from_deck: bool,
        /// §4.9: the deck's own mic, or the Mac's default input for a
        /// `voice.listen{input:"mac"}` and for the `fallback_input` of §7.1
        /// when the deck is BLE-HID-only.
        input: VoiceInput,
        /// `[voice.stt] locale` — the recogniser is built per locale, so it
        /// cannot be chosen after the first buffer lands.
        locale: String,
        /// §7.6's `contextual_strings`: the agent names and the pending
        /// card's choice labels, so `"cohdl-api"` and `"Allow always"`
        /// survive recognition instead of coming back as English words.
        contextual: Vec<String>,
        /// The agent whose owner transcribes this press (`caps.transcribe:
        /// "push"` and `[voice.stt] companion`): the engine records the
        /// press and offers it to that owner, keeping its own recogniser as
        /// the fallback. `None` is the daemon's recogniser alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        companion: Option<crate::AgentId>,
    },
    /// Finish (or abort) the capture and dispatch the final transcript.
    Finish {
        abort: bool,
    },
    /// `set speaker` / `set mic` (§6.4): the mixer's master gain and the
    /// capture scale, 0..=10 on §7.3's perceptual curve (0 = mute).
    ///
    /// B4: turning the knob persisted the value and emitted `device.set`
    /// and **nothing reached the audio side**, so `set speaker` on the deck
    /// changed a number in `state.json` and the next announcement was
    /// exactly as loud as before. It rides beside the `Persist` so the two
    /// can never disagree, and it carries the key rather than two commands
    /// because the pair is one knob to the mixer.
    Gain {
        key: SetKey,
        value: u16,
    },
    /// `[voice]`'s device selection, re-read on a hot config reload (B14).
    ///
    /// §7 selects every device by **UID** and never touches the system
    /// default, so the audio side cannot re-derive this from anywhere: it
    /// is told. `fallback_output`/`fallback_input` are §7.1/§7.2's
    /// behaviour when the deck's UID is absent (BLE HID only), and they
    /// change the answer to "what happens when the deck is not there",
    /// which is exactly the setting somebody edits and expects to take
    /// effect without a restart.
    Devices {
        device_uid: Option<String>,
        fallback_output: String,
        fallback_input: String,
    },
    /// `[voice]`'s **timing policy**, re-read on a hot config reload — the
    /// three keys V1 and V2 forced (measured 2026-09-21).
    ///
    /// Separate from [`AudioCmd::Devices`] because it answers a different
    /// question: `Devices` says *which* endpoints, this says *when* the
    /// input may be open, *whether* an announcement may play over the SCO
    /// tail, and *when* a bring-up is worth reporting. A reload that
    /// changes only the policy must not re-select a device mid-session.
    ///
    /// None of it can be re-derived on the audio side — they are operator
    /// settings — and a key the daemon reads nowhere is the failure B14
    /// was about: `config.reload` reports it under `applied` and nothing
    /// changes.
    ///
    /// * `mic_warm` — `"never" | "card_pending" | "always"` (§7.1).
    /// * `speak_over_sco` — play at the SCO rate rather than wait out the
    ///   2307 ms A2DP resume. **Never while capture is live** (V5: the
    ///   deck's loopback is ≈ +17 dB, so it would feed our own TTS back
    ///   into the recogniser).
    /// * `slow_sco_ms` — press → first **non-zero sample**, not first
    ///   callback (V2), above which the engine reports
    ///   `voice.route{slow_sco_ms}`.
    Policy {
        mic_warm: String,
        speak_over_sco: bool,
        slow_sco_ms: u32,
    },
    /// Whether a **Question card is pending** — the fact `[voice] mic_warm
    /// = "card_pending"` is a policy about (§7.1).
    ///
    /// The reducer is the only place that holds the card set, and the audio
    /// side is the only place that can act on it, so the fact has to cross
    /// the seam. It is an **edge**, emitted when the answer changes and not
    /// per command, and it says only what is true — `mic_warm` itself lives
    /// on the audio side, so `"never"` ignores this and `"always"` needs no
    /// signal at all.
    ///
    /// Why this matters enough to be a command: V1 measured SCO bring-up
    /// from start-IO to the **first non-zero sample** at 1031 ms median
    /// (815–1246, n=16). That second is the operator's first syllables. A
    /// pending Question is exactly the moment they are about to press, so
    /// it is the one moment worth paying the second in advance.
    Warm {
        pending: bool,
    },
}

impl AudioCmd {
    /// Whether a refused `AudioCmd` must be retried rather than dropped
    /// (contract §3's back-pressure decision).
    ///
    /// Core → audio is a **bounded** channel with `try_send` only, exactly
    /// like `send_op`: the core must never block behind an SCO bring-up
    /// wedged on a Bluetooth timeout (V1 measured bring-up at ~1 s, and a
    /// wedged one has no upper bound). Unlike a device op there is no
    /// re-projection to cover a drop, so each command says for itself what
    /// a refusal means:
    ///
    /// * `Listen` — the session is aborted at once with
    ///   `voice.state{error:"audio_busy"}`. The operator learns now, rather
    ///   than holding a dead button for a capture that was never opened.
    /// * `Say` / `Chime` / `Gain` / `Devices` / `Policy` — dropped, with a counter in
    ///   `metrics.get`. A lost announcement is a missing sentence; a queue
    ///   of stale ones is worse.
    /// * `Stop` / `Finish` / `Warm` — **retried on the next tick**. Losing
    ///   a `Stop` or a `Finish` strands a live capture: the mic stays open,
    ///   the 60 s cap is the only thing that ever closes it, and every lane
    ///   stays cut. Losing a `Warm` is quieter and lasts longer — it is an
    ///   edge, not a level, so the audio side would hold the wrong belief
    ///   about the card set until the next card appeared or was answered,
    ///   which can be minutes, and would leave the microphone open (or
    ///   shut) for all of it.
    #[must_use]
    pub const fn retry_when_refused(&self) -> bool {
        matches!(
            self,
            AudioCmd::Stop { .. } | AudioCmd::Finish { .. } | AudioCmd::Warm { .. }
        )
    }

    /// Whether a refusal aborts the PTT session (`Listen`, above).
    #[must_use]
    pub const fn aborts_when_refused(&self) -> bool {
        matches!(self, AudioCmd::Listen { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "desktop", rename_all = "snake_case")]
pub enum DesktopCmd {
    Focus { agent: AgentId },
    Inject { agent: AgentId, text: String },
    /// A Macropad key with a keystroke was pressed (or stepped): post the
    /// combo (`ctrl+opt+shift+cmd+<key>`, the deck's own spelling) to the
    /// desktop. The deck itself is never a keyboard (its USB block has one
    /// IN endpoint), so the daemon types for it, with the Accessibility
    /// grant.
    Keystroke { keys: String },
}

/// A write to `state.json` (debounced by the core; `CardId` synchronous).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "patch", rename_all = "snake_case")]
pub enum StatePatch {
    Language { value: u16, revision: u32 },
    Faces(Vec<FaceEntry>),
    Setting {
        key: SetKey,
        value: u16,
    },
    /// Written before the `card` line is emitted.
    CardId {
        slot: Slot,
        last: CardId,
    },
    Slot {
        key: String,
        slot: Slot,
        kind: crate::Kind,
        cwd: Option<String>,
        last_seen: Ts,
    },
    MacropadBinding {
        f: u8,
        layout: String,
    },
    /// The keys of a Macropad face as last set (by the API or the deck's own
    /// editor), so they come back after a restart.
    Macro {
        f: u8,
        src: Option<String>,
        keys: Vec<crate::MacroKey>,
    },
    Voice {
        key: String,
        voice: String,
    },
    Usage(Usage),
}

/// Every side effect, executed by the core after `apply`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Device(DeviceOp),
    /// hold → `holds[card]`; push → owning session; poll → `waiters[agent]`.
    Deliver {
        agent: AgentId,
        card: CardId,
        answer: Answer,
        deliver: Deliver,
    },
    Timer {
        key: TimerKey,
        at: Instant,
    },
    /// `holds[(agent, card)]` resolves with `{}`.
    ReleaseHold {
        agent: AgentId,
        card: CardId,
        reason: Release,
    },
    Audio(AudioCmd),
    Desktop(DesktopCmd),
    Persist(StatePatch),
}

/// Event types of §4.12, serialized as their dotted names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum EventKind {
    #[serde(rename = "session.settings_requested")]
    SessionSettingsRequested,
    #[serde(rename = "agent.added")]
    AgentAdded,
    #[serde(rename = "agent.changed")]
    AgentChanged,
    #[serde(rename = "agent.removed")]
    AgentRemoved,
    #[serde(rename = "agent.slot")]
    AgentSlot,
    #[serde(rename = "agent.answer")]
    AgentAnswer,
    #[serde(rename = "agent.order")]
    AgentOrder,
    #[serde(rename = "card.set")]
    CardSet,
    #[serde(rename = "card.answered")]
    CardAnswered,
    #[serde(rename = "card.delivered")]
    CardDelivered,
    #[serde(rename = "card.failed")]
    CardFailed,
    #[serde(rename = "card.snoozed")]
    CardSnoozed,
    #[serde(rename = "card.degraded")]
    CardDegraded,
    #[serde(rename = "card.focused")]
    CardFocused,
    #[serde(rename = "card.acked")]
    CardAcked,
    #[serde(rename = "card.expired")]
    CardExpired,
    #[serde(rename = "card.cleared")]
    CardCleared,
    #[serde(rename = "card.truncated")]
    CardTruncated,
    #[serde(rename = "order.dispatched")]
    OrderDispatched,
    #[serde(rename = "order.failed")]
    OrderFailed,
    #[serde(rename = "op.confirm_requested")]
    OpConfirmRequested,
    #[serde(rename = "reply.set")]
    ReplySet,
    #[serde(rename = "transcript.partial")]
    TranscriptPartial,
    #[serde(rename = "transcript.final")]
    TranscriptFinal,
    #[serde(rename = "voice.state")]
    VoiceState,
    #[serde(rename = "voice.route")]
    VoiceRoute,
    #[serde(rename = "voice.utterance")]
    VoiceUtterance,
    #[serde(rename = "voice.level")]
    VoiceLevel,
    #[serde(rename = "device.link")]
    DeviceLink,
    #[serde(rename = "device.session")]
    DeviceSession,
    #[serde(rename = "device.face")]
    DeviceFace,
    #[serde(rename = "device.faces")]
    DeviceFaces,
    #[serde(rename = "device.set")]
    DeviceSet,
    #[serde(rename = "device.key")]
    DeviceKey,
    #[serde(rename = "device.macro")]
    DeviceMacro,
    /// The deck's own key editor changed a slot (`device.macro.edit`).
    #[serde(rename = "device.macro.edit")]
    DeviceMacroEdit,
    #[serde(rename = "device.ws")]
    DeviceWs,
    #[serde(rename = "device.refresh")]
    DeviceRefresh,
    #[serde(rename = "device.raw")]
    DeviceRaw,
    #[serde(rename = "focus.changed")]
    FocusChanged,
    #[serde(rename = "usage.refresh_requested")]
    UsageRefreshRequested,
    #[serde(rename = "usage.updated")]
    UsageUpdated,
    #[serde(rename = "config.changed")]
    ConfigChanged,
    #[serde(rename = "integration.changed")]
    IntegrationChanged,
    #[serde(rename = "perm.changed")]
    PermChanged,
    #[serde(rename = "service.degraded")]
    ServiceDegraded,
    #[serde(rename = "service.shutdown")]
    ServiceShutdown,
    #[serde(rename = "hook.raw")]
    HookRaw,
    #[serde(rename = "error")]
    Error,
}

impl EventKind {
    pub const ALL: [EventKind; 49] = [
        EventKind::SessionSettingsRequested,
        EventKind::AgentAdded,
        EventKind::AgentChanged,
        EventKind::AgentRemoved,
        EventKind::AgentSlot,
        EventKind::AgentAnswer,
        EventKind::AgentOrder,
        EventKind::CardSet,
        EventKind::CardAnswered,
        EventKind::CardDelivered,
        EventKind::CardFailed,
        EventKind::CardSnoozed,
        EventKind::CardDegraded,
        EventKind::CardFocused,
        EventKind::CardAcked,
        EventKind::CardExpired,
        EventKind::CardCleared,
        EventKind::CardTruncated,
        EventKind::OrderDispatched,
        EventKind::OrderFailed,
        EventKind::OpConfirmRequested,
        EventKind::ReplySet,
        EventKind::TranscriptPartial,
        EventKind::TranscriptFinal,
        EventKind::VoiceState,
        EventKind::VoiceRoute,
        EventKind::VoiceUtterance,
        EventKind::VoiceLevel,
        EventKind::DeviceLink,
        EventKind::DeviceSession,
        EventKind::DeviceFace,
        EventKind::DeviceFaces,
        EventKind::DeviceSet,
        EventKind::DeviceKey,
        EventKind::DeviceMacro,
        EventKind::DeviceMacroEdit,
        EventKind::DeviceWs,
        EventKind::DeviceRefresh,
        EventKind::DeviceRaw,
        EventKind::FocusChanged,
        EventKind::UsageRefreshRequested,
        EventKind::UsageUpdated,
        EventKind::ConfigChanged,
        EventKind::IntegrationChanged,
        EventKind::PermChanged,
        EventKind::ServiceDegraded,
        EventKind::ServiceShutdown,
        EventKind::HookRaw,
        EventKind::Error,
    ];

    /// The dotted wire name (the serde rename, checked by a test).
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::SessionSettingsRequested => "session.settings_requested",
            EventKind::AgentAdded => "agent.added",
            EventKind::AgentChanged => "agent.changed",
            EventKind::AgentRemoved => "agent.removed",
            EventKind::AgentSlot => "agent.slot",
            EventKind::AgentAnswer => "agent.answer",
            EventKind::AgentOrder => "agent.order",
            EventKind::CardSet => "card.set",
            EventKind::CardAnswered => "card.answered",
            EventKind::CardDelivered => "card.delivered",
            EventKind::CardFailed => "card.failed",
            EventKind::CardSnoozed => "card.snoozed",
            EventKind::CardDegraded => "card.degraded",
            EventKind::CardFocused => "card.focused",
            EventKind::CardAcked => "card.acked",
            EventKind::CardExpired => "card.expired",
            EventKind::CardCleared => "card.cleared",
            EventKind::CardTruncated => "card.truncated",
            EventKind::OrderDispatched => "order.dispatched",
            EventKind::OrderFailed => "order.failed",
            EventKind::OpConfirmRequested => "op.confirm_requested",
            EventKind::ReplySet => "reply.set",
            EventKind::TranscriptPartial => "transcript.partial",
            EventKind::TranscriptFinal => "transcript.final",
            EventKind::VoiceState => "voice.state",
            EventKind::VoiceRoute => "voice.route",
            EventKind::VoiceUtterance => "voice.utterance",
            EventKind::VoiceLevel => "voice.level",
            EventKind::DeviceLink => "device.link",
            EventKind::DeviceSession => "device.session",
            EventKind::DeviceFace => "device.face",
            EventKind::DeviceFaces => "device.faces",
            EventKind::DeviceSet => "device.set",
            EventKind::DeviceKey => "device.key",
            EventKind::DeviceMacro => "device.macro",
            EventKind::DeviceMacroEdit => "device.macro.edit",
            EventKind::DeviceWs => "device.ws",
            EventKind::DeviceRefresh => "device.refresh",
            EventKind::DeviceRaw => "device.raw",
            EventKind::FocusChanged => "focus.changed",
            EventKind::UsageRefreshRequested => "usage.refresh_requested",
            EventKind::UsageUpdated => "usage.updated",
            EventKind::ConfigChanged => "config.changed",
            EventKind::IntegrationChanged => "integration.changed",
            EventKind::PermChanged => "perm.changed",
            EventKind::ServiceDegraded => "service.degraded",
            EventKind::ServiceShutdown => "service.shutdown",
            EventKind::HookRaw => "hook.raw",
            EventKind::Error => "error",
        }
    }

    /// Never matched by a `*` subscription glob; must be named (§4.5).
    pub const fn is_high_volume(self) -> bool {
        matches!(
            self,
            EventKind::TranscriptPartial
                | EventKind::VoiceLevel
                | EventKind::DeviceKey
                | EventKind::DeviceMacro
                | EventKind::DeviceRaw
                | EventKind::HookRaw
        )
    }

    /// Carries content fields that fan-out redacts for `observe`-only readers
    /// (`crate::redact_event`).
    pub const fn has_content(self) -> bool {
        matches!(
            self,
            EventKind::AgentAdded
                | EventKind::AgentChanged
                | EventKind::AgentAnswer
                | EventKind::AgentOrder
                | EventKind::CardSet
                | EventKind::CardAnswered
                | EventKind::OrderDispatched
                | EventKind::OrderFailed
                | EventKind::OpConfirmRequested
                | EventKind::ReplySet
                | EventKind::TranscriptPartial
                | EventKind::TranscriptFinal
                | EventKind::HookRaw
        )
    }
}

/// One observable change. `rev` is the snapshot revision after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub rev: u64,
    pub ts: Ts,
    #[serde(rename = "type")]
    pub kind: EventKind,
    pub principal: Principal,
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_like_the_envelope() {
        let e = Event {
            rev: 1202,
            ts: Ts(1_789_760_523_120),
            kind: EventKind::CardSet,
            principal: Principal::Token("hooks".into()),
            data: serde_json::json!({"agent": "ag_1"}),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"rev":1202,"ts":"2026-09-18T19:42:03.120Z","type":"card.set","principal":"token:hooks","data":{"agent":"ag_1"}}"#
        );
        assert_eq!(
            EventKind::OpConfirmRequested.as_str(),
            "op.confirm_requested"
        );
        for kind in EventKind::ALL {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                kind.as_str(),
                "{kind:?}"
            );
        }
        assert!(EventKind::TranscriptPartial.is_high_volume());
        assert!(!EventKind::CardSet.is_high_volume());
        assert!(EventKind::ReplySet.has_content());
        assert!(!EventKind::DeviceLink.has_content());
    }

    /// The one thing an adapter asks a `Release`: is there still a card on
    /// the deck? Every variant answers it, and the answer is the same one
    /// the store's own code path takes — the four that run `degrade_card`
    /// against the three that clear, retire or abandon the card.
    #[test]
    fn a_release_says_whether_a_degraded_card_is_left_behind() {
        for left in [
            Release::Snooze,
            Release::Expiry,
            Release::HoldGone,
            Release::AgentGone,
        ] {
            assert!(left.degrades_card(), "{left:?}");
        }
        for gone in [Release::Cleared, Release::Shutdown, Release::GroupStep] {
            assert!(!gone.degrades_card(), "{gone:?}");
        }
        // The wire name is the variant's own, so a `Cleared` never reads
        // back as the `Expiry` it used to be spelled as.
        assert_eq!(
            serde_json::to_value(Release::Cleared).unwrap(),
            serde_json::json!("cleared")
        );
        assert_eq!(
            serde_json::to_value(Release::Expiry).unwrap(),
            serde_json::json!("expiry")
        );
    }
}
