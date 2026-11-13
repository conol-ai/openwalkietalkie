//! §15 week 4, exit row 8 (HOST-CONTRACT-W4 §5): the announce policy of
//! §7.7 obeys the settings — `set announce`, `set chime`,
//! `[voice] announce_questions` — and the per-agent voice round-robin.
//!
//! The row that was missing entirely is the Question one. §7.7 asks for
//! `"<name> needs a decision: <card text ≤80 chars>"` on the `Question`
//! lane, gated by whether the operator is already looking at the Agents
//! face; the deck played a chime and said nothing, so a question that
//! arrived while the operator was reading a different face was a two-tone
//! beep with no subject. The beep says *something* happened; the sentence
//! says which agent and about what, which is the whole reason the daemon
//! owns a voice at all.
//!
//! The two gates are deliberately separate knobs and this file pins that:
//! `set chime` gates the `Chime` lane **only** (§7.7), so an operator who
//! turned the beeps off still hears the sentence, and one who turned
//! announcements off still hears the beep.

mod core_support;

use core_support::{agent_of, build, poll_register, uid, Parts};
use deck_model::{
    AgentId, AgentState, AgentUpdate, AudioCmd, CardKind, CardSet, Choice, Deliver, DeviceLine,
    DeviceSet, FaceEntry, Origin, ReplySet, Request, SessionId, SetKey, Template,
};
use deckd::adapters::claude::Claude;
use deckd::adapters::{Adapter, HookCtx, HookPayload, Ingest};
use deckd::audio::{AudioHandle, FromAudio};
use deckd::config::Config;
use deckd::core::Core;
use std::sync::Arc;
use std::time::Instant;

struct Rig {
    core: Core,
    parts: Parts,
    now: Instant,
    cmds: crossbeam_channel::Receiver<AudioCmd>,
    _events: crossbeam_channel::Sender<FromAudio>,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let (mut core, parts) = build(name, 512);
        let (audio, cmds, events) = AudioHandle::detached();
        core.set_audio(audio);
        Rig {
            core,
            parts,
            now: Instant::now(),
            cmds,
            _events: events,
        }
    }

    fn config(&mut self, f: impl FnOnce(&mut Config)) {
        let mut cfg = Config::default();
        f(&mut cfg);
        self.core.set_config(Arc::new(cfg));
        self.drain();
    }

    fn drain(&self) {
        while self.cmds.try_recv().is_ok() {}
    }

    fn call(&mut self, req: Request) -> deck_model::ApiResult {
        self.core.api_call(uid(), SessionId(1), true, req, self.now)
    }

    fn agent(&mut self, name: &str) -> AgentId {
        let r = self.call(Request::AgentRegister(poll_register(name)));
        agent_of(&r)
    }

    /// What was **said** since the last call: `(lane, text, voice)`.
    fn said(&self) -> Vec<(String, String, Option<String>)> {
        self.cmds
            .try_iter()
            .filter_map(|c| match c {
                AudioCmd::Say {
                    lane, text, voice, ..
                } => Some((lane, text, voice)),
                _ => None,
            })
            .collect()
    }

    /// Everything the audio side was asked for since the last call.
    fn cmds(&self) -> Vec<AudioCmd> {
        self.cmds.try_iter().collect()
    }

    fn set(&mut self, key: SetKey, value: u16) {
        self.call(Request::DeviceSet(DeviceSet { key, value }))
            .expect("device.set");
        self.drain();
    }

    /// Put the deck on a face of template `t`, the way the device reports
    /// its strip and then which one it is showing.
    fn on_face(&mut self, t: Template) {
        self.core.on_device_line(
            DeviceLine::Faces {
                list: vec![
                    FaceEntry {
                        f: 1,
                        t: Template::Agents,
                        n: "Agents".into(),
                    },
                    FaceEntry {
                        f: 2,
                        t: Template::Usage,
                        n: "Usage".into(),
                    },
                ],
            },
            self.now,
        );
        let f = if t == Template::Agents { 1 } else { 2 };
        self.core.on_device_line(DeviceLine::Face { f }, self.now);
        self.drain();
    }

    fn question(&mut self, agent: &AgentId, text: &str) {
        self.call(Request::CardSet(CardSet {
            agent: agent.clone(),
            id: None,
            kind: CardKind::Q,
            text: text.into(),
            choices: vec![Choice {
                label: "Yes".into(),
                detail: None,
            }],
            rec: None,
            free_text: true,
            deliver: Deliver::Poll,
            timeout_s: None,
            origin: Origin::custom(),
        }))
        .expect("agent.card.set");
    }

    fn state(&mut self, agent: &AgentId, state: AgentState) {
        self.call(Request::AgentUpdate(AgentUpdate {
            agent: agent.clone(),
            state: Some(state),
            title: None,
            title_source: None,
            since: None,
            name: None,
            meta: None, task: None, subagents: None,
        }))
        .expect("agent.update");
    }

    fn cleanup(self) {
        let dir = self.parts.dir.clone();
        drop(self);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A Claude hook event, run against `agent` the way the hook door runs
    /// the adapter's plan: request by request, in order. The register and
    /// the Ready card are the adapter's own (its session, `Deliver::None`)
    /// and are left out; the agent stands in for the one they resolve to.
    fn claude_hook(&mut self, agent: &AgentId, payload: serde_json::Value) {
        let ctx = HookCtx {
            now: Some(Instant::now()),
            ..HookCtx::default()
        };
        let Ingest::Report(plan) = Claude::default().ingest(&HookPayload(payload), &ctx) else {
            panic!("a report-only event");
        };
        for mut req in plan {
            if matches!(req, Request::Reply(_) | Request::AgentUpdate(_)) {
                deckd::adapters::address(&mut req, agent);
                self.call(req).expect("a request of the plan");
            }
        }
    }
}

/// A turn's end speaks that turn's answer. The ready line is spoken from
/// the reply on file the moment the state turns `ready`; the Claude adapter
/// used to turn the session ready first and store the reply after, so the
/// deck read out the answer before. And whichever of `agent_completed` and
/// `Stop` comes first, the line is the turn's own, said once.
#[test]
fn a_stop_speaks_this_turns_reply_never_the_last_one() {
    let mut rig = Rig::new("voice-announce-stop");
    rig.config(|_| {});
    let bot = rig.agent("cohdl-api");
    let pool = Config::default().voice_pool();
    let said = |text: &str| vec![("ready".to_string(), text.to_string(), Some(pool[0].clone()))];
    let stop = |answer: &str| {
        serde_json::json!({
            "session_id": "s1", "cwd": "/w/cohdl-api", "hook_event_name": "Stop",
            "last_assistant_message": answer,
        })
    };
    rig.call(Request::Reply(ReplySet {
        message: None,
        agent: bot.clone(),
        text: "The old answer.".into(),
        announce: None,
        speak: false,
        lane: None,
    }))
    .expect("agent.reply");
    rig.state(&bot, AgentState::Ready);
    rig.drain();

    rig.state(&bot, AgentState::Working);
    rig.claude_hook(&bot, stop("The new answer. Details follow."));
    assert_eq!(rig.said(), said("cohdl-api: The new answer."));

    // Claude's agent view also says `agent_completed`, and nobody has
    // measured whether it comes before the `Stop`.
    rig.state(&bot, AgentState::Working);
    rig.claude_hook(
        &bot,
        serde_json::json!({
            "session_id": "s1", "cwd": "/w/cohdl-api", "hook_event_name": "Notification",
            "notification_type": "agent_completed",
        }),
    );
    assert_eq!(rig.said(), vec![], "not the last turn's answer");
    rig.claude_hook(&bot, stop("The third answer."));
    assert_eq!(rig.said(), said("cohdl-api: The third answer."));
    rig.cleanup();
}

/// Exit row 8: every branch of §7.7's announce policy, and the two gates
/// that turn the branches off independently.
#[test]
fn the_announce_policy_obeys_the_settings() {
    let mut rig = Rig::new("voice-announce-policy");
    rig.config(|_| {});
    let bot = rig.agent("cohdl-api");
    rig.drain();

    // ── `state:ready` → "<name>: <announce ?? first sentence>" on `ready` ─
    rig.call(Request::Reply(ReplySet {
        message: None,
        agent: bot.clone(),
        text: "The migration finished. Seventeen rows moved.".into(),
        announce: None,
        speak: false,
        lane: None,
    }))
    .expect("agent.reply");
    rig.drain();
    rig.state(&bot, AgentState::Ready);
    let pool = Config::default().voice_pool();
    assert_eq!(
        rig.said(),
        vec![(
            "ready".to_string(),
            "cohdl-api: The migration finished.".to_string(),
            Some(pool[0].clone()),
        )],
        "the first sentence, in the agent's own lane and its own voice"
    );

    // ── `state:error` → "<name> failed" ─────────────────────────────────
    rig.state(&bot, AgentState::Error);
    let said = rig.said();
    assert_eq!(said.len(), 1);
    assert_eq!(said[0].1, "cohdl-api failed");

    // ── a new Question card, operator NOT on the Agents face ─────────────
    // The chime says something happened; the sentence says which agent and
    // about what. Without it the operator has to go and look.
    rig.on_face(Template::Usage);
    rig.question(&bot, "Run the destructive migration against staging?");
    let cmds = rig.cmds();
    assert!(
        cmds.iter()
            .any(|c| matches!(c, AudioCmd::Chime { which } if which == "question")),
        "the question chime still plays: {cmds:?}"
    );
    let spoken = cmds
        .iter()
        .find_map(|c| match c {
            AudioCmd::Say { lane, text, .. } => Some((lane.clone(), text.clone())),
            _ => None,
        })
        .expect("§7.7's missing announcement");
    assert_eq!(
        spoken,
        (
            "question".to_string(),
            "cohdl-api needs a decision: Run the destructive migration against staging?"
                .to_string()
        )
    );

    // ── the same card while the operator IS on the Agents face ───────────
    // They can see it. `[voice] announce_questions` is the override for an
    // operator who wants it read out anyway.
    rig.on_face(Template::Agents);
    rig.question(&bot, "And the production one?");
    let cmds = rig.cmds();
    assert!(
        cmds.iter().any(|c| matches!(c, AudioCmd::Chime { .. })),
        "the chime is not gated on the face"
    );
    assert!(
        !cmds.iter().any(|c| matches!(c, AudioCmd::Say { .. })),
        "the deck is showing the card already: {cmds:?}"
    );

    rig.config(|c| c.voice.announce_questions = true);
    rig.question(&bot, "And the production one?");
    let said = rig.said();
    assert_eq!(said.len(), 1, "announce_questions overrides the face");
    assert_eq!(
        said[0].1,
        "cohdl-api needs a decision: And the production one?"
    );
    rig.config(|_| {});

    // ── the card text is clipped to 80 characters, on a char boundary ────
    rig.on_face(Template::Usage);
    let long = "コーデックス".repeat(40);
    rig.question(&bot, &long);
    let said = rig.said();
    let text = &said[0].1;
    let clipped = text
        .strip_prefix("cohdl-api needs a decision: ")
        .expect("the prefix");
    assert_eq!(clipped.chars().count(), 80, "80 characters, not 80 bytes");
    assert!(long.starts_with(clipped), "a prefix of what was asked");
    assert!(
        !clipped.ends_with('\u{fffd}'),
        "never a half character: {clipped:?}"
    );

    // ── `set announce 0` silences every announcement, chime untouched ────
    rig.set(SetKey::Announce, 0);
    rig.question(&bot, "Anything at all?");
    let cmds = rig.cmds();
    assert!(
        cmds.iter().any(|c| matches!(c, AudioCmd::Chime { .. })),
        "`set chime` is the chime's gate, not `set announce`: {cmds:?}"
    );
    assert!(!cmds.iter().any(|c| matches!(c, AudioCmd::Say { .. })));
    rig.state(&bot, AgentState::Ready);
    assert!(rig.said().is_empty(), "ready is gated too");
    rig.state(&bot, AgentState::Error);
    assert!(rig.said().is_empty(), "and error");

    // ── `set chime 0` silences the chime, announcements untouched ────────
    rig.set(SetKey::Announce, 1);
    rig.set(SetKey::Chime, 0);
    rig.question(&bot, "Still there?");
    let cmds = rig.cmds();
    assert!(
        !cmds.iter().any(|c| matches!(c, AudioCmd::Chime { .. })),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| matches!(c, AudioCmd::Say { .. })),
        "`set chime` gates the Chime lane only (§7.7): {cmds:?}"
    );
    rig.cleanup();
}

/// Exit row 8, second half: **per-agent voices** (§7.7).
///
/// Nothing emitted `StatePatch::Voice`, so the whole auto-assignment was
/// unbuilt: every agent spoke in the same default voice and the
/// `state.json.voices` map §9 describes was written by nobody. Four agents
/// announcing in one voice is four announcements the operator has to parse
/// by content; four voices is four announcements they can tell apart before
/// the second word.
///
/// The precedence §7.7 names, strongest first: the `agent.register{voice}`
/// the caller asked for, the `[agents."<key>"].voice` pin, the voice this
/// key was given last time, and only then the round-robin.
#[test]
fn per_agent_voices_are_assigned_round_robin_and_reach_the_announcement() {
    let mut rig = Rig::new("voice-announce-voices");
    rig.config(|_| {});
    // The model's own speakers while the local synthesiser is the engine
    // (the default), Apple's `default_pool` otherwise.
    let pool = Config::default().voice_pool();
    assert_eq!(pool.len(), 4, "§9's default pool");

    // Four agents, four distinct voices, in pool order.
    let agents: Vec<AgentId> = ["one", "two", "three", "four"]
        .iter()
        .map(|n| rig.agent(n))
        .collect();
    let voices: Vec<String> = agents
        .iter()
        .map(|a| {
            rig.core.store.snapshot().agents[a]
                .voice
                .clone()
                .unwrap_or_else(|| panic!("{a:?} was given no voice"))
        })
        .collect();
    assert_eq!(voices, pool, "round-robin, in the order the pool lists");

    // The fifth wraps rather than running out.
    let five = rig.agent("five");
    assert_eq!(
        rig.core.store.snapshot().agents[&five].voice.as_ref(),
        Some(&pool[0])
    );

    // The assignment is persisted under the agent's key, so it survives a
    // restart — an agent whose voice changed every morning would be worse
    // than one voice for everybody.
    rig.core.flush_state();
    let state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(rig.parts.dir.join("state.json")).expect("state.json"),
    )
    .expect("state.json parses");
    assert_eq!(state["voices"]["b1:one"], pool[0]);
    assert_eq!(state["voices"]["b1:four"], pool[3]);

    // …and it is the voice the announcement carries.
    rig.on_face(Template::Usage);
    rig.drain();
    rig.question(&agents[1], "Ready?");
    let said = rig.said();
    assert_eq!(said.len(), 1);
    assert_eq!(
        said[0].2.as_ref(),
        Some(&pool[1]),
        "the agent's own voice, not the default"
    );

    // A caller that names a voice keeps it, and does not consume a slot in
    // the rotation: the pool is for agents that did not choose.
    let mut r = poll_register("picky");
    r.voice = Some("com.apple.voice.enhanced.en-US.Ava".into());
    let picky = agent_of(&rig.call(Request::AgentRegister(r)));
    assert_eq!(
        rig.core.store.snapshot().agents[&picky].voice.as_deref(),
        Some("com.apple.voice.enhanced.en-US.Ava")
    );
    let next = rig.agent("six");
    assert_eq!(
        rig.core.store.snapshot().agents[&next].voice.as_ref(),
        Some(&pool[1]),
        "the rotation carries on from where it was"
    );
    rig.cleanup();
}

/// §7.6's sanitiser is what the announcements are actually built with, and
/// `[voice.tts] announce_max_chars` is the budget they cannot compose their
/// way past.
///
/// The unit row (`voice_tts`) proves the function; this proves it is
/// **wired**. A `speech()` nobody called would leave the deck reading
/// "star star all tests pass star star" out loud with a perfectly green
/// test suite.
#[test]
fn an_announcement_is_sanitised_and_kept_inside_announce_max_chars() {
    let mut rig = Rig::new("voice-announce-sanitiser");
    rig.config(|_| {});
    let bot = rig.agent("bot");
    rig.drain();

    // A model's reply, as a model writes one.
    rig.call(Request::Reply(ReplySet {
        message: None,
        agent: bot.clone(),
        text: "\u{1b}[32m## Result\u{1b}[0m\n\n**All tests pass.** \
               The `deck-model` crate is unchanged.\n```rust\nfn main() {}\n```"
            .into(),
        announce: None,
        speak: false,
        lane: None,
    }))
    .expect("agent.reply");
    rig.drain();
    rig.state(&bot, AgentState::Ready);
    let said = rig.said();
    assert_eq!(
        said[0].1, "bot: Result All tests pass.",
        "no markup, no fence, one sentence"
    );

    // The budget is hot and it is applied to the **whole** composed line,
    // name and all — a long name plus a long question is still one
    // announcement the operator has to sit through.
    rig.config(|c| c.voice.tts.announce_max_chars = 20);
    rig.on_face(Template::Usage);
    rig.question(&bot, "Would you like me to run the whole migration now?");
    let said = rig.said();
    assert_eq!(said[0].1.chars().count(), 20, "{:?}", said[0].1);
    assert!(
        said[0].1.starts_with("bot needs a decision"),
        "{:?}",
        said[0].1
    );
    rig.cleanup();
}
