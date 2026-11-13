//! The methods the core answers from its own state (contract §3.2), as
//! `CoreMsg::Call`s: the redacted snapshot and agents, the ring
//! (`state.events`), the sessions, the link's metrics and debug controls,
//! `service.info`, `service.restart`/`service.stop`, the admin surface
//! (`config.get`/`config.set`/`config.reload`, `token.*`, `log.tail`).
//! `agent.wait` is intercepted in `Core::on_msg` (it may park and owns its
//! reply channel). The scope check happened in the door (`api::dispatch`);
//! here the caller's scopes only shape the redaction. Of the voice methods
//! only the two read-only ones are here (`voice.engines`, `voice.test`),
//! and both answer from what the audio side reported rather than asking it
//! — a main-thread hop on this thread would park every agent, every hook
//! and every device line behind a framework read; B5 made
//! `voice.say|stop|listen|cancel` reducer methods, which moved their trust
//! guard and rate limit onto `Guard::check` with every other reducer
//! method's. `hooks.*` is **not** here and must never be: it parks for
//! the whole hold budget and this runs on the core thread. The door answers
//! it in `api::hooks` (B5).
use super::handle::{CallCtx, Reply};
use super::{event_agent, wall_now, Core};
use crate::config::{self, Config, HOT_KEYS};
use crate::hid::HidDebug;
use crate::tokens::{self, Tokens};
use deck_model::{
    redact_event, redact_for, AgentId, ApiError, EventKind, Listener, Permission, Scope, Ts,
};
use std::time::Instant;

/// What this build can actually do (`/v1/info.capabilities`,
/// `service.info.capabilities` — one list, both surfaces read it).
///
/// It advertises only what is **built**, never what is designed: §4.1's
/// example list ends at `["device", "voice.stt.apple", "voice.tts.apple",
/// "mcp", "hooks.claude", "hooks.codex", "hooks.gemini", "inject.ghostty",
/// "virtual_agents"]`, and each of those joins this array in the commit
/// that makes it true — `hooks.<cli>` with its adapter in `api::hooks`,
/// `voice.*` with an engine, `inject.ghostty` with the injector.
///
/// This is not decoration (B8): `integrations.status` reads it to tell
/// "the hooks are installed and the daemon answers them" from "the hooks
/// are installed and this daemon has no adapter for them", which are
/// otherwise the same silence at the CLI. Keep it sorted and unique; the
/// test below is what keeps an adapter from landing without its entry.
pub const CAPABILITIES: &[&str] = &[
    "device",
    "hooks.claude",
    "hooks.codex",
    "inject.ghostty",
    // `caps.transcribe: "push"`: `agent.transcribe` offers, the
    // `agent.transcribe.audio` / `agent.transcribe.result` methods.
    "transcribe.companion",
    "virtual_agents",
];
/// The version this daemon reports (`/v1/info.daemon`, `service.info`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `perm.status` cannot tell you (B9), in the shape
/// `integrations.status` already uses for the same job: an answer that
/// names its own blind spots, rather than a confident value that is a
/// guess.
pub const PERM_LIMITS: &[&str] = &[
    "microphone is what the audio side last reported; with no engine in this build it stays \
     `unknown` and that is not the same as `not_determined`",
    "speech is macOS's own answer (SFSpeechRecognizer.authorizationStatus), read when this is \
     asked while the daemon has an audio engine, whose Apple recogniser is what needs it; a \
     read never asks for the grant. With no engine it is what the audio side last reported, \
     `unknown` like the microphone",
    "bluetooth is CoreBluetooth's authorization for this process as the BLE voice link last \
     read it (`[voice] transport = \"ble\"`, where the deck's microphone and speaker need it \
     instead of Microphone); `unknown` on the CoreAudio transport and until the link has \
     started. macOS asks for it by itself the first time the link starts, so there is no \
     `perm.request` for it, and a denial is lifted in System Settings > Privacy & Security > \
     Bluetooth",
    "automation.ghostty is what the injector last learned by trying: `granted` once a script \
     has run, `denied` after a -1743, and `unknown` until it has had occasion to ask. \
     accessibility is what the Macropad keys last learned the same way: `granted` once a \
     keystroke went out, `denied` after macOS refused one (which also puts the system prompt \
     up, once per run), and `unknown` until a key has been pressed or `perm.request` asked",
    "a grant reported here is this process's. A launchd-spawned deckd is its own responsible \
     process with its own TCC row, so a status read from a terminal-spawned daemon says \
     nothing about the shipped one (W4 V9)",
];

/// Why `perm.request` cannot prompt yet. This is the contract's honesty
/// note, on the wire where a caller sees it.
pub const PERM_REQUEST_LIMITS: &[&str] = &[
    "the prompt is not shown. The signed bundle this used to wait for now exists (week 7: both \
     Mach-Os carry Identifier=ai.cohdl.openwalkietalkie and TeamIdentifier=4F93FJA939, so a grant \
     to one is a grant to the other), and what remains is a dependency and a person: \
     AVCaptureDevice/AVAudioApplication is not reachable from this workspace's pinned \
     objc2-avf-audio feature set, and a grant needs a human to click Allow in a system dialog, \
     so the completion-handler path cannot be exercised by any test here. The speech half is \
     reachable today through objc2-speech",
    "`prompt_not_shown` is §7.8's own answer for a prompt that did not surface, so a caller \
     that handles the hedge already handles this",
    "a dev-loop grant proves the terminal's entitlement and not the daemon's: a probe with no \
     Info.plist opens the mic today only because \
     responsibility_get_pid_responsible_for_pid attributes it to the terminal app (W4 V9)",
];

/// `log.tail{lines}` when the caller names none, and its ceiling.
pub const LOG_TAIL_DEFAULT: usize = 200;
pub const LOG_TAIL_MAX: usize = 5_000;

/// **The managed keys** — every key `config.set` will write, by name (§9:
/// "`config.set` edits managed keys only").
///
/// A whitelist, and it has to be one. `config.toml` is not a settings file
/// with a few sharp edges — it is where the **trust boundary** is
/// declared, and an `admin` caller that can edit it arbitrarily has granted
/// itself the deck's own authority:
///
/// * `trust.require_trusted_for_allow` is the whole of §4.2's guarded
///   operations. `false` and a permission allow, a dictated order and an
///   `inject` no longer need a trusted principal — week 5's guard 1, gone,
///   from one write.
/// * `trust.gui_requirement` / `trust.dev_requirement` are the code
///   requirements `MacVerifier` checks a peer against. Writable means
///   "declare anything trusted", and the requirements are hot (§3.7), so
///   it would take effect on the next connection.
/// * `desktop.inject` is which terminals may be typed into.
/// * `api.allowed_origins` is the browser-origin check, and
///   `api.uds_default_scope`/`uds_max_scope` are what a bare Unix peer is
///   granted.
/// * `privacy.*` decides what of the operator's prompts and tool inputs is
///   kept and logged; `privacy.log_content` writes hook payloads to disk.
/// * `voice.stt.allow_server` sends the operator's speech off the machine
///   (§7.6). It is the one `[voice]` key that is not a local preference,
///   the design says the GUI has to *say so* where it is offered, and no
///   week-6 screen offers it — so it is not here either.
///
/// So the rule is the conservative one: a key is managed when it is named
/// here, and a key added to `Config` later is refused until somebody adds
/// it. The alternative — a list of what is forbidden — fails open the next
/// time `[trust]` grows a field, and fails silently.
///
/// What is here is what week 6 has a caller for: the Service screen's port,
/// the tray's "Pause hooks", and the `[voice]` preferences the Voice screen
/// shows. The `[voice]` keys are spelled out rather than taken as
/// `voice.*` for the same fail-closed reason. The companion app's
/// Integrations page added the source switches: `[sources]` and the Codex
/// switch, `codex.daemon` (its `daemon_socket` stays the operator's).
/// `sources.claude_channels` among them is an opt-in rather than a pause:
/// it decides whether the Claude plugin carries the channel helper and the
/// channel door serves it, both local, and Claude still asks each session
/// to opt in to channels itself. Its
/// Service page added the Local API switch, `api.tcp`: whether the TCP door
/// is bound at all, and a door that answers only the bearer tokens
/// `tokens.toml` holds grants nobody anything by being open. Its Usage
/// panel added `usage.providers`, whose Claude switch says where it is
/// offered that it reads the Claude Code login (the rule
/// `voice.tts.api_key` joined under).
pub const MANAGED: &[&str] = &[
    "api.port",
    "api.tcp",
    "codex.daemon",
    "hooks.enabled",
    "sources.apps_paused",
    "sources.claude",
    "sources.claude_channels",
    "sources.paused",
    "usage.providers",
    "voice.announce_questions",
    "voice.commands",
    "voice.device_uid",
    "voice.fallback_input",
    "voice.fallback_output",
    "voice.mic_warm",
    "voice.slow_sco_ms",
    "voice.speak_over_sco",
    // Whether a companion's own recogniser hears the presses addressed to
    // its agents: the switch that sends that audio off the machine.
    "voice.stt.companion",
    "voice.stt.engine",
    "voice.stt.language",
    "voice.stt.locale",
    "voice.stt.model",
    // The app's first-start question about a local model, answered once.
    "voice.stt.prompted",
    "voice.tts.announce_max_chars",
    // The cloud synthesiser's key and choices. The key is what sends the
    // announcements off the machine, so it joined this list in the same
    // change as the Voice screen's control that says so (the rule
    // `voice.stt.allow_server` still waits on). `config.get` publishes the
    // key as a hint, never the value, and `config_set` checks that a key
    // took by its hint.
    "voice.tts.api_key",
    "voice.tts.engine",
    "voice.tts.instruct",
    "voice.tts.lang",
    "voice.tts.model",
    "voice.tts.provider",
    "voice.tts.voices",
    "voice.voices.default_pool",
];

/// Why the two read-only voice methods are `-32013` when they are.
///
/// It names the **state**, not the calendar: they were written and they
/// answer, and what is missing is an engine to answer about. `--no-device`
/// installs none (and must not: the `api` gate step runs with it and a door
/// test may not open a microphone), a non-macOS build has none, and for the
/// first turns of the run loop the engines are up but their one report has
/// not landed yet.
pub const NO_ENGINE: &str =
    "this daemon has no audio engine: `--no-device`, a build without one, or the moment before \
     the engines' first report reaches the core. `voice.engines` answers from that report and \
     never asks the run loop, so there is nothing to wait for here";

/// `perm.status`'s `speech`. While this daemon has an audio engine (its
/// report has landed), whose Apple recogniser is what needs the grant, it is
/// the OS's answer, `read` when asked (`crate::audio::speech_authorization`,
/// which never prompts). Without an engine — `--no-device`, a build with
/// none, the moment before its report — nothing is read and it is what the
/// audio side last `reported`, and so it is on a system with no answer to
/// read. The read changes no state: the snapshot's `voice.permissions` stays
/// the audio side's report.
fn speech_grant(
    reported: Permission,
    engine: bool,
    read: impl FnOnce() -> Option<Permission>,
) -> Permission {
    if !engine {
        return reported;
    }
    read().unwrap_or(reported)
}

/// The one `-32013` both voice reads answer with.
fn no_engine() -> ApiError {
    let mut error = ApiError::degraded("voice_unavailable");
    error.data["why"] = NO_ENGINE.into();
    error
}

/// The platform name of `/v1/info`.
pub const fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

impl Core {
    /// One core-answered method.
    pub fn call(
        &mut self,
        mut ctx: CallCtx,
        method: &str,
        params: serde_json::Value,
        now: Instant,
    ) -> Reply {
        ctx.own = self.owned_by(ctx.session);
        self.sessions.touch(ctx.session, now);
        match method {
            "image.get" => {
                use base64::Engine;
                let id = params["image"].as_str().unwrap_or_default();
                let valid = agent_param(&params).and_then(|a| self.store.snapshot().agents.get(&a))
                    .is_some_and(|a| a.messages.iter().any(|m| m.message.images.iter().any(|i| i.id == id)));
                if !valid { return Reply::err(ApiError::not_found("image")); }
                match crate::chat_images::get(id) {
                    Some(p) => {
                        let offset = params.get("offset").and_then(|n| n.as_u64()).unwrap_or(0) as usize;
                        if offset >= p.png.len() || !offset.is_multiple_of(48 * 1024) { return Reply::err(ApiError::invalid_params("invalid image offset")); }
                        let end = (offset + 48*1024).min(p.png.len());
                        Reply::ok(serde_json::json!({"mime":"image/png", "offset":offset, "total":p.png.len(), "data":base64::engine::general_purpose::STANDARD.encode(&p.png[offset..end])}))
                    },
                    None => Reply::err(ApiError::not_found("image preview expired")),
                }
            }
            "agent.transcribe.audio" => {
                use base64::Engine;
                use crate::audio::companion::{parse_offer_id, PAGE};
                let Some(id) = params["transcription"].as_str().and_then(parse_offer_id) else {
                    return Reply::err(ApiError::invalid_params("transcription: a tr_ id"));
                };
                // Another connection's offer is as absent as an expired one.
                let Ok(wav) = self.transcribe.audio(id, ctx.session) else {
                    return Reply::err(ApiError::not_found("transcription"));
                };
                let offset = params.get("offset").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
                if offset >= wav.len() || !offset.is_multiple_of(PAGE) {
                    return Reply::err(ApiError::invalid_params("invalid transcription offset"));
                }
                let end = (offset + PAGE).min(wav.len());
                Reply::ok(serde_json::json!({
                    "mime": "audio/wav", "offset": offset, "total": wav.len(),
                    "data": base64::engine::general_purpose::STANDARD.encode(&wav[offset..end]),
                }))
            }
            "agent.transcribe.result" => {
                use crate::audio::companion::parse_offer_id;
                let Some(id) = params["transcription"].as_str().and_then(parse_offer_id) else {
                    return Reply::err(ApiError::invalid_params("transcription: a tr_ id"));
                };
                let result = match (params["text"].as_str(), params["error"].as_str()) {
                    (Some(text), None) if text.len() <= 100_000 => Ok(text.to_string()),
                    (None, Some(error)) => Err(error.chars().take(200).collect()),
                    _ => return Reply::err(ApiError::invalid_params("exactly one of text (≤100000 bytes) or error")),
                };
                match self.transcribe.resolve(id, ctx.session, result) {
                    Ok(()) => Reply::ok(serde_json::json!({})),
                    Err(_) => Reply::err(ApiError::not_found("transcription")),
                }
            }
            "session.ping" => Reply::ok(serde_json::json!({"ts": wall_now()})),
            "state.get" => {
                let snapshot = redact_for(self.store.snapshot(), &ctx.scopes, &ctx.own);
                Reply::ok(serde_json::json!({"rev": snapshot.rev, "snapshot": snapshot}))
            }
            "state.events" => self.state_events(&ctx, &params),
            "agent.list" => {
                let snapshot = redact_for(self.store.snapshot(), &ctx.scopes, &ctx.own);
                // `agents` holds slotted and unslotted alike (§4.6).
                let agents: Vec<&deck_model::Agent> = snapshot.agents.values().collect();
                Reply::ok(serde_json::json!({"rev": snapshot.rev, "agents": agents}))
            }
            "agent.get" => {
                let Some(id) = agent_param(&params) else {
                    return Reply::err(ApiError::invalid_params("agent is required"));
                };
                if !self.store.snapshot().agents.contains_key(&id) {
                    return Reply::err(ApiError::not_found("agent"));
                }
                let snapshot = redact_for(self.store.snapshot(), &ctx.scopes, &ctx.own);
                match snapshot.agents.get(&id) {
                    Some(agent) => {
                        Reply::ok(serde_json::to_value(agent).unwrap_or(serde_json::Value::Null))
                    }
                    None => Reply::err(ApiError::not_found("agent")),
                }
            }
            "device.get" => Reply::ok(
                serde_json::to_value(&self.store.snapshot().device)
                    .unwrap_or(serde_json::Value::Null),
            ),
            "voice.get" => Reply::ok(
                serde_json::to_value(&self.store.snapshot().voice)
                    .unwrap_or(serde_json::Value::Null),
            ),
            // `service.info` is an `observe` method every client polls, so it
            // never waits on the hid thread (§9, the C row): it answers from
            // the last report and only nudges the hid side for the next one.
            // `metrics.get` (admin) is the method that may wait.
            "service.info" => {
                self.nudge_metric();
                Reply::ok(self.service_info(now))
            }
            // Counts, times and switches per source, never a session
            // (`care::sources`); `{}` is all it takes.
            "sources.list" => Reply::ok(self.sources_list()),
            // The test agent (`care::test`): a card on the deck from the
            // daemon's own care session, a spoken sample, and how the last
            // test of each kind went.
            "care.test.question" | "care.test.voice" | "care.test.say" | "care.test.status" => {
                self.care_test(&ctx, method, &params, now)
            }
            "metrics.get" => {
                self.refresh_metric(now);
                match serde_json::to_value(self.metric) {
                    Ok(mut m) => {
                        m["overflows"] = self.overflows.into();
                        m["ops_dropped"] = self.ops_dropped.into();
                        m["sessions"] = self.sessions.len().into();
                        m["holds"] = self.holds.len().into();
                        m["core_queue"] = self.bus.receivers().into();
                        // B1's back-pressure, visible: what a full audio
                        // queue refused and dropped, what it is holding,
                        // and what is waiting to be retried.
                        m["audio_dropped"] = self.audio_dropped().into();
                        m["audio_queue"] = self.audio.queued().into();
                        m["audio_retry"] = self.audio_retry_len().into();
                        Reply::ok(m)
                    }
                    Err(e) => Reply::err(ApiError::internal(&e.to_string())),
                }
            }
            "device.flood" => {
                let n = params
                    .get("n")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if n == 0 {
                    return Reply::err(ApiError::invalid_params("n must be ≥ 1"));
                }
                if self.hid.debug.send(HidDebug::Flood(n)).is_err() {
                    return Reply::err(ApiError::degraded("hid_thread_gone"));
                }
                Reply::ok(serde_json::json!({"queued": n}))
            }
            "device.sync" => {
                if self.hid.debug.send(HidDebug::Sync).is_err() {
                    return Reply::err(ApiError::degraded("hid_thread_gone"));
                }
                Reply::ok(serde_json::json!({"ok": true}))
            }
            // The TCP door's bind result (internal: the door calls it with
            // `admin`; the API's table never lists it, so no client can).
            "service.bound" => {
                if !ctx.scopes.contains(&Scope::Admin) {
                    return Reply::err(ApiError::forbidden("admin"));
                }
                let port = params
                    .get("port")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|p| u16::try_from(p).ok());
                let degraded = params
                    .get("degraded")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                // §4.1/§4.12: the door's `lsof` lookup rides along and has
                // to reach the event and `service.info`, not just the log.
                let listener = params
                    .get("listener")
                    .filter(|v| !v.is_null())
                    .and_then(|v| match serde_json::from_value::<Listener>(v.clone()) {
                        Ok(listener) => Some(listener),
                        Err(e) => {
                            tracing::debug!(error = %e, "service.bound: unreadable listener");
                            None
                        }
                    });
                self.step(
                    deck_model::Command::ServiceBound {
                        port,
                        degraded,
                        listener,
                    },
                    now,
                );
                // The bound port is a fact without an event: the cell must
                // carry it for `/v1/info` even though `rev` did not move.
                self.bus.set_snapshot(self.store.snapshot().clone());
                self.write_status(now);
                Reply::ok(serde_json::json!({"rev": self.store.snapshot().rev}))
            }
            "config.get" => Reply::ok(self.config_get()),
            "config.set" => self.config_set(&params, now),
            "config.reload" => self.config_reload(&ctx, now),
            "service.restart" | "service.stop" => {
                let voice = &self.store.snapshot().voice;
                let busy_voice = voice.ptt.is_some() || voice.speaking.is_some() || self.store.is_finalizing();
                if self.firmware.status().busy() || (params.get("when_idle").and_then(serde_json::Value::as_bool) == Some(true) && busy_voice) {
                    Reply::err(ApiError::invalid("firmware", "Wait for voice and firmware operations to finish."))
                } else { self.service_exit(method, now) }
            },
            "token.create" | "token.list" | "token.revoke" | "token.rotate" => {
                self.token_call(method, &params, now)
            }
            "log.tail" => self.log_tail(&params),
            // B5: `voice.say|stop|listen|cancel` are reducer methods now
            // and never arrive here. These two only read.
            "voice.engines" => self.voice_engines(),
            "voice.test" => self.voice_test(&params),
            "firmware.status" => Reply::ok(serde_json::to_value(self.firmware.status()).unwrap()),
            "firmware.check" | "firmware.install" => {
                let cache = self.paths.caches.join(crate::firmware::CACHE_DIR);
                let result = if method == "firmware.check" {
                    self.firmware.check(cache, params.get("force").and_then(serde_json::Value::as_bool).unwrap_or(false))
                } else {
                    if !self.quiet() {
                        return Reply::err(ApiError::invalid("firmware", crate::firmware::VOICE_BUSY));
                    }
                    let text = |key| params.get(key).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
                    let install = crate::firmware::Install {
                        board: text("board"),
                        serial: text("serial"),
                        version: text("version"),
                        allow_bootloader_update: params
                            .get("allow_bootloader_update")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    };
                    // A deck that can show a card is asked first, and the
                    // install waits for ENTER on it (`care::test`); a deck
                    // in recovery cannot, and installs as it always has;
                    // any other deck is refused until it links.
                    match self.install_gate() {
                        super::test_agent::InstallGate::AskTheDeck => {
                            return self.install_after_press(install, now);
                        }
                        super::test_agent::InstallGate::NotLinked => {
                            return self.install_refused_until_linked(&install);
                        }
                        super::test_agent::InstallGate::Recovery => {}
                    }
                    self.firmware.install(
                        &install.board,
                        &install.serial,
                        &install.version,
                        install.allow_bootloader_update,
                        cache,
                        self.hid.maintenance.clone(),
                    )
                };
                match result {
                    Ok(status) => Reply::ok(serde_json::to_value(status).unwrap()),
                    Err(why) => Reply::err(ApiError::invalid("firmware", &why)),
                }
            }
            "stt.status" => self.stt_status(),
            "stt.install" => self.stt_install(&params),
            "stt.remove" => self.stt_remove(),
            "tts.status" => self.tts_status(),
            "tts.install" => self.tts_install(&params),
            "tts.remove" => self.tts_remove(),
            "perm.status" => Reply::ok(self.perm_status()),
            "perm.request" => self.perm_request(&params),
            other => Reply::err(ApiError::method_not_found(other)),
        }
    }

    /// `state.events{since, limit?:500, types?}` → `{events, gap}` from the
    /// ring, each event redacted for the caller (`own_agent` keeps the
    /// content of an agent this session owns, §4.12).
    fn state_events(&self, ctx: &CallCtx, params: &serde_json::Value) -> Reply {
        let since = params
            .get("since")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let limit = params
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(crate::bus::REPLAY_LIMIT, |n| {
                (n as usize).clamp(1, crate::bus::REPLAY_LIMIT)
            });
        let types: Option<Vec<String>> = params.get("types").and_then(|v| match v {
            serde_json::Value::Array(list) => Some(
                list.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect(),
            ),
            serde_json::Value::String(one) => Some(vec![one.clone()]),
            _ => None,
        });
        let (events, gap) = self.bus.replay(since, limit, types.as_deref());
        let events: Vec<deck_model::Event> = events
            .iter()
            .filter_map(|e| {
                let own = event_agent(e).is_some_and(|id| ctx.own.contains(&id));
                redact_event(e, &ctx.scopes, own)
            })
            .collect();
        Reply::ok(serde_json::json!({"events": events, "gap": gap}))
    }

    /// `config.get`: [`config_json`] of the config in force, with the two
    /// keys whose value is not the file's alone resolved, and a client is
    /// told what they resolve to, never `null`. `sources.claude_channels`
    /// left out of the file is whatever this daemon's Claude plugin carries
    /// now ([`Core::channels_on`]): it is what the Integrations page's switch
    /// shows. `api.tcp` left out is off, unless the pre-plugin Claude hooks
    /// kept the door open at start ([`Core::tcp_door`]): it is what the
    /// Service page's Local API switch shows, and the app reads a boolean
    /// there as the mark of a service that writes `usage.providers`. Beside
    /// it, and only in this reply, `api.tcp_rule` says which rule decided
    /// it ([`crate::config::TcpRule::word`]: `explicit`, `default` or
    /// `legacy_hooks`), so the app can say why the door is open; it is no
    /// key of the file, and `config.set` refuses it as it refuses any
    /// unmanaged key.
    fn config_get(&self) -> serde_json::Value {
        let mut published = config_json(self.config());
        published["sources"]["claude_channels"] = self.channels_on().into();
        let door = self.tcp_door();
        published["api"]["tcp"] = door.on.into();
        published["api"]["tcp_rule"] = door.rule.word().into();
        published
    }

    /// `config.set{patch}` (§4.11, §9): the **managed** keys of `config.toml`,
    /// written together or not at all.
    ///
    /// `patch` is `{"<dotted key>": <value>}` — the dotted names `config.get`
    /// publishes, so `{"api.port": 47299}` and
    /// `{"voice.stt.locale": "en-GB", "voice.tts.engine": "apple"}` are both
    /// one call. A `null` value **removes** its key, so the documented
    /// default applies again: there is no TOML null to write, and "unset it"
    /// is what a control with a Default position has to be able to say.
    ///
    /// **What the reply calls applied is in force when it is written.** Once
    /// the file is on disk the core takes the config it just validated,
    /// exactly as `config.reload` takes one ([`Core::take_config`]): the
    /// pauses are worked out again and the Usage face is redrawn before the
    /// answer goes. The pages that flip a switch read back at once
    /// (`config.get`, `sources.list`); when the change landed only one
    /// debounce later they read the old value, showed it, and built their
    /// next write from it, undoing the one before. The notify watcher of
    /// §3.7 still reads the file a debounce later: it swaps the `ArcSwap`
    /// the doors read (the hook doors, the Codex client) and raises the
    /// same `config.changed{keys}` an editor's save does, and the
    /// `CoreMsg::Config` it sends is the config the core already holds,
    /// which changes nothing. Both take the file, so there is still one
    /// idea of what is in force. A daemon started without a watcher
    /// (`WatchHandle::watching() == false`) has its doors keep the old
    /// config until a restart, which is the deal an edit gets there.
    ///
    /// **A patch is all-or-nothing.** Every check below runs against the
    /// whole patch applied to one in-memory copy of the file, and the file
    /// is written once at the end — so a patch with one bad key writes
    /// none of its keys, and there is no state in which half of it is in
    /// force. That is not only tidiness: the one caller week 6 has is the
    /// Service screen's "change port and re-install hooks", and a port that
    /// moved while the rest of the patch was refused would leave the hooks
    /// pointing at the old one.
    ///
    /// Four things are checked, and a failure of any of them is `-32602`
    /// with the file untouched: every key is in [`MANAGED`] (a whitelist,
    /// and why it has to be one is written out there); every key is one
    /// `config.get` publishes; the patched file still parses as a `Config`
    /// — **validated whole**, because keys that are each plausible can
    /// still be invalid together; and every edit **took**, with nothing
    /// outside the patch moving. That last one is not paranoia.
    /// `agents.pins` is published as one key but is `#[serde(flatten)]` in
    /// the file, so writing `[agents] pins = {…}` would define a pin
    /// *named* `pins` and leave `agents.pins` looking untouched; the round
    /// trip is what catches that and every other key whose published name
    /// is not its TOML path.
    ///
    /// **Nothing refused ever reaches the disk, even for an instant.** The
    /// patched file is built in memory and validated there, and only then
    /// written — through [`crate::config::write`], which is a temp file
    /// plus a rename, so §3.7's watcher can never read a half-written
    /// config and decide the last good one has to stay. The temp file is
    /// `config.toml.<pid>.tmp`, which is not one of the two names the
    /// reloader acts on, so it does not even provoke a reload attempt.
    ///
    /// Everything is compared **file against file**, never against the
    /// config in force. The two differ whenever a write is still inside the
    /// watcher's debounce or an operator has edited by hand, and a `changed`
    /// computed from the in-force one would then name the other write's
    /// keys and refuse this one for moving them.
    fn config_set(&mut self, params: &serde_json::Value, now: Instant) -> Reply {
        let Some(patch) = params.get("patch").and_then(serde_json::Value::as_object) else {
            return Reply::err(ApiError::invalid_params(
                "patch is required: {\"<dotted key>\": <value>}, the names config.get publishes \
                 (a null value removes its key and restores the documented default)",
            ));
        };
        if patch.is_empty() {
            return Reply::err(ApiError::invalid_params("patch names no keys"));
        }
        // The whitelist first, before the file is so much as read, and for
        // the **whole** patch: one unmanaged key refuses all of it, by name,
        // and the answer says what the list is rather than leaving a caller
        // to guess whether it was a key or a value.
        let unmanaged: Vec<&str> = patch
            .keys()
            .map(String::as_str)
            .filter(|key| !MANAGED.contains(key))
            .collect();
        if !unmanaged.is_empty() {
            return Reply::err(ApiError::invalid_params(&format!(
                "{} is not a managed key. config.set writes only these: {}. Everything else in \
                 config.toml — `[trust]`, `[privacy]`, `[desktop] inject`, `[api] \
                 allowed_origins` and the scope defaults, `[voice.stt] allow_server` — is the \
                 operator's own file to edit, because a caller that could write it would be \
                 granting itself trust rather than changing a setting",
                unmanaged.join(", "),
                MANAGED.join(", ")
            )));
        }
        let path = self.paths.config.clone();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // No file is the documented defaults, which is the same thing
            // `Config::load` says: the first `config.set` creates it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Reply::err(ApiError::internal(&e.to_string())),
        };
        // A file the daemon could not have loaded is the operator's to fix,
        // and is `-32009 invalid{config.toml}` — the same error
        // `config.reload` gives for the same file — never a write over it.
        let current = match Config::parse(&text) {
            Ok(current) => current,
            Err(e) => return Reply::err(ApiError::invalid("config.toml", &e.to_string())),
        };
        let before = config_json(&current);
        // Every edit onto **one** copy, so the validation below sees the
        // whole patch and the write at the end is a single publish.
        // `serde_json`'s map iterates in key order, so the same patch always
        // produces the same file.
        let mut edited = text.clone();
        for (key, value) in patch {
            if key_of(&before, key).is_none() {
                return Reply::err(ApiError::invalid_params(&format!(
                    "{key} is not a key of config.get"
                )));
            }
            edited = match config::edit(&edited, key, value) {
                Ok(edited) => edited,
                Err(e @ config::EditError::Unparsable(_)) => {
                    return Reply::err(ApiError::invalid("config.toml", &e.to_string()))
                }
                Err(e) => return Reply::err(ApiError::invalid_params(&e.to_string())),
            };
        }
        // Validated whole, exactly as the reload path validates it.
        let next = match Config::parse(&edited) {
            Ok(next) => next,
            Err(e) => return Reply::err(ApiError::invalid_params(&e.to_string())),
        };
        let after = config_json(&next);
        let defaults = config_json(&Config::default());
        // Did each edit **take**? Asked for a value, `config.get` has to
        // read that value back; asked for `null`, the key is gone from the
        // file and `config.get` reads back whatever §9 documents as its
        // default — which is not `null` for most of them. There is no
        // "well, nothing changed" escape here on purpose: a write that
        // changed nothing *and* did not take is a silent no-op, and a GUI
        // toggle that reports success and moves nothing is the worst of
        // the three outcomes.
        for (key, value) in patch {
            let took = match value {
                serde_json::Value::Null => key_of(&after, key) == key_of(&defaults, key),
                // The one key `config.get` does not read back as written:
                // the hint of the key is what it publishes.
                serde_json::Value::String(secret) if key == "voice.tts.api_key" => {
                    let hint = published_key_hint(secret);
                    key_of(&after, key) == Some(&serde_json::Value::String(hint))
                }
                value => key_of(&after, key) == Some(value),
            };
            if !took {
                return Reply::err(ApiError::invalid_params(&format!(
                    "{key} does not take that value: config.get would read back {}",
                    key_of(&after, key).unwrap_or(&serde_json::Value::Null)
                )));
            }
        }
        let changed = changed_keys(&before, &after);
        let stray: Vec<&str> = changed
            .iter()
            .map(String::as_str)
            .filter(|moved| !patch.contains_key(*moved))
            .collect();
        if !stray.is_empty() {
            return Reply::err(ApiError::invalid_params(&format!(
                "this patch also moves {}, which it does not name",
                stray.join(", ")
            )));
        }
        if let Err(e) = config::write(&path, &edited) {
            return Reply::err(ApiError::internal(&e.to_string()));
        }
        let (hot, restart): (Vec<String>, Vec<String>) = changed
            .iter()
            .cloned()
            .partition(|k| is_hot(k) && config::is_hot_change(k, &current, &next));
        // In force before the reply says so; the watcher's reload of this
        // same file is then the config the core already holds.
        self.take_config(std::sync::Arc::new(next), now);
        let keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        tracing::info!(
            keys = %keys.join(","),
            hot = hot.len(),
            "config.toml written and applied"
        );
        Reply::ok(serde_json::json!({
            "keys": keys, "changed": changed, "applied": hot, "restart_needed": restart,
        }))
    }

    /// `service.restart` (exit 75) / `service.stop` (exit 0), §4.11.
    ///
    /// The reply comes **first** and the process comes down after it:
    /// [`crate::service::Exit`] is armed with a deadline, the supervisor on
    /// `main` may not look at it until then, and by then the door has
    /// written the line. Without that a client would see its connection
    /// close on an unanswered call and could not tell "restarted" from
    /// "crashed" — which is the difference between a tray that says
    /// "Restarting…" and one that says nothing at all.
    fn service_exit(&mut self, method: &str, now: Instant) -> Reply {
        let (action, want) = if method == "service.restart" {
            ("restart", crate::service::EXIT_RESTART)
        } else {
            ("stop", crate::service::EXIT_STOP)
        };
        // The first request wins, so a `service.stop` behind a
        // `service.restart` is told what will really happen rather than
        // what it asked for.
        let code = self.exit.request(want, now);
        tracing::info!(action, code, "service exit requested through the API");
        Reply::ok(serde_json::json!({
            "action": if code == want { action } else if code == crate::service::EXIT_RESTART {
                "restart"
            } else {
                "stop"
            },
            "exit": code,
            "pid": std::process::id(),
            "in_ms": crate::service::Exit::GRACE.as_millis() as u64,
        }))
    }

    /// `voice.engines` (§4.9), answered from the one report the audio side
    /// sent when its engines came up (M5).
    ///
    /// Reading it live is a main-thread hop — `SFSpeechRecognizer::
    /// supportedLocales` and the installed voice list — and this runs on
    /// the core thread, which may never park on the run loop. So the value
    /// is pushed, cached in `Core::engines`, and served from there;
    /// `-32013` until it lands, which is also the answer for every build
    /// that has no engine at all.
    ///
    /// The design's shape for the TTS half was `[{id, voices}]`. What an
    /// `EngineInfo` carries is the locale set (for `apple`, the distinct
    /// locales of the installed voices), so that is what is reported; the
    /// per-agent voice **pool** is `[voice.voices] default_pool` in
    /// `config.get` and the assignments are `state.json.voices`, which is
    /// where the GUI's picker already reads them.
    fn voice_engines(&self) -> Reply {
        let Some((stt, tts)) = self.engines() else {
            return Reply::err(no_engine());
        };
        let config = self.config();
        // The synthesiser in force is a function of the config — the cloud
        // engine speaks only while there is a key — so it is named from
        // the config and Apple's own report, never from a cached read. The
        // recogniser likewise: the local model answers only once it is
        // selected and on disk.
        let kokoro_present = crate::kokoro::present(&self.paths.kokoro, &config.voice.tts.model);
        let tts = crate::audio::tts_info(&config.voice.tts, kokoro_present, tts);
        let stt = self.stt_info(stt);
        Reply::ok(serde_json::json!({
            "stt": [engine_json(&stt)],
            "tts": [engine_json(&tts)],
            // Which of them `[voice]` selects, so a badge can say "apple"
            // without the GUI re-reading `config.get` to find out.
            "selected": {
                "stt": config.voice.stt.engine,
                "tts": config.voice.tts.engine,
                "locale": config.voice.stt.locale,
            },
        }))
    }

    /// The recogniser `voice.engines` names: the local model when it is
    /// selected and on disk, else Apple's own report.
    fn stt_info(&self, apple: &crate::audio::traits::EngineInfo) -> crate::audio::traits::EngineInfo {
        let settings = crate::stt::Settings::from_config(self.config());
        let present = crate::stt::present(&self.paths.stt, &settings.model);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            crate::audio::whisper::engine_info(&settings, present, apple)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (settings, present);
            apple.clone()
        }
    }

    /// `stt.status`: the local recogniser's model — selected, on disk,
    /// loaded — and the download in flight, if any. Reading it also loads
    /// a model that has just finished downloading, so the app's poll ends
    /// with the engine ready and not merely the file.
    fn stt_status(&self) -> Reply {
        let config = self.config();
        let settings = crate::stt::Settings::from_config(config);
        let path = crate::stt::model_path(&self.paths.stt, &settings.model);
        let present = crate::stt::present(&self.paths.stt, &settings.model);
        let (loaded, loading, load_error) = {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                match &self.whisper {
                    Some(whisper) => {
                        whisper.ensure_loaded();
                        let s = whisper.status();
                        (s.loaded, s.loading, s.error)
                    }
                    None => (false, false, None),
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                (false, false, Some("no local recogniser in this build".to_string()))
            }
        };
        let catalogue: Vec<serde_json::Value> = crate::stt::MODELS
            .iter()
            .map(|m| serde_json::json!({"name": m.name, "bytes": m.bytes, "about": m.about}))
            .collect();
        Reply::ok(serde_json::json!({
            "engine": config.voice.stt.engine,
            "active": settings.enabled && present,
            "model": settings.model,
            "path": path.display().to_string(),
            "total": crate::stt::model(&settings.model).map_or(0, |m| m.bytes),
            "present": present,
            "loaded": loaded,
            "loading": loading,
            "load_error": load_error,
            "download": self.stt_install.status().map(|p| p.json()),
            "models": catalogue,
            "prompted": config.voice.stt.prompted,
        }))
    }

    /// `stt.install {model?}`: fetch the model's weights into the caches
    /// directory, in the background; `stt.status` reports the progress.
    /// This is the opt-in: nothing is downloaded before it is called.
    fn stt_install(&self, params: &serde_json::Value) -> Reply {
        let configured = crate::stt::Settings::from_config(self.config()).model;
        let name = params
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map_or(configured.as_str(), str::trim)
            .to_string();
        let name = crate::stt::model_name(&name).to_string();
        if crate::stt::model(&name).is_none() {
            return Reply::err(ApiError::invalid_params(&format!(
                "{name} is not a model this daemon can fetch: {}",
                crate::stt::MODELS
                    .iter()
                    .map(|m| m.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        match self.stt_install.start(&crate::stt::job(&self.paths.stt, &name)) {
            Ok(progress) => {
                tracing::info!(model = %name, "stt: model download started");
                let _ = progress;
                self.stt_status()
            }
            Err(e) => Reply::err(ApiError::internal(&e)),
        }
    }

    /// `stt.remove`: stop any download and delete the selected model's
    /// file. Apple's recogniser takes the next press.
    fn stt_remove(&self) -> Reply {
        self.stt_install.cancel();
        let settings = crate::stt::Settings::from_config(self.config());
        if let Err(e) = crate::stt::job(&self.paths.stt, &settings.model).remove() {
            return Reply::err(ApiError::internal(&e));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(whisper) = &self.whisper {
            whisper.unload();
        }
        self.stt_status()
    }

    /// `tts.status`: the local voice's package — selected, on disk, loaded
    /// — and the download in flight, if any. Reading it also loads a
    /// package that has just finished downloading.
    fn tts_status(&self) -> Reply {
        let config = self.config();
        let settings = crate::kokoro::Settings::from_config(config);
        let job = crate::kokoro::job(&self.paths.kokoro, &settings.model);
        let present = crate::kokoro::present(&self.paths.kokoro, &settings.model);
        let (loaded, loading, load_error) = {
            #[cfg(target_os = "macos")]
            {
                match &self.kokoro {
                    Some(kokoro) => {
                        kokoro.ensure_loaded();
                        let s = kokoro.status();
                        (s.loaded, s.loading, s.error)
                    }
                    None => (false, false, None),
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                (false, false, Some("no local voice in this build".to_string()))
            }
        };
        let catalogue: Vec<serde_json::Value> = crate::kokoro::PACKAGES
            .iter()
            .map(|p| serde_json::json!({"name": p.name, "bytes": p.bytes, "about": p.about}))
            .collect();
        Reply::ok(serde_json::json!({
            "engine": config.voice.tts.engine,
            "active": settings.enabled && present,
            "model": settings.model,
            "path": job.path().display().to_string(),
            "total": job.total,
            "present": present,
            "loaded": loaded,
            "loading": loading,
            "load_error": load_error,
            "download": self.tts_install.status().map(|p| p.json()),
            "models": catalogue,
            "voices": settings.pool(),
            "speakers": crate::kokoro::SPEAKERS.len(),
        }))
    }

    /// `tts.install {model?}`: fetch and unpack the voice package in the
    /// background; `tts.status` reports the progress. This is the opt-in.
    fn tts_install(&self, params: &serde_json::Value) -> Reply {
        let configured = crate::kokoro::Settings::from_config(self.config()).model;
        let name = params
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map_or(configured.as_str(), str::trim)
            .to_string();
        let name = crate::kokoro::model_name(&name).to_string();
        if crate::kokoro::package(&name).is_none() {
            return Reply::err(ApiError::invalid_params(&format!(
                "{name} is not a voice package this daemon can fetch: {}",
                crate::kokoro::PACKAGES
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        match self.tts_install.start(&crate::kokoro::job(&self.paths.kokoro, &name)) {
            Ok(_) => {
                tracing::info!(model = %name, "tts: voice package download started");
                self.tts_status()
            }
            Err(e) => Reply::err(ApiError::internal(&e)),
        }
    }

    /// `tts.remove`: stop any download and delete the selected package.
    /// Apple's synthesiser speaks from the next utterance.
    fn tts_remove(&self) -> Reply {
        self.tts_install.cancel();
        let settings = crate::kokoro::Settings::from_config(self.config());
        if let Err(e) = crate::kokoro::job(&self.paths.kokoro, &settings.model).remove() {
            return Reply::err(ApiError::internal(&e));
        }
        #[cfg(target_os = "macos")]
        if let Some(kokoro) = &self.kokoro {
            kokoro.unload();
        }
        self.tts_status()
    }

    /// `voice.test` (§4.9, §7): the setup check, and **only what it can
    /// honestly observe**.
    ///
    /// `speaker` (the default) queues §7.3's `ready` chime — 110 ms, peak
    /// 0.2, rendered by `audio::tts::chime_pcm` — through the ordinary
    /// mixer path, and reports the route it went out on. It is the one
    /// `what` that makes a sound, and it changes neither the system default
    /// device nor anybody's volume: §7 selects by UID and the chime plays
    /// at the operator's own `set speaker` gain.
    ///
    /// `mic`, `stt` and `tts` are **reports, not exercises**, and say so in
    /// `limits`. Opening the microphone is a PTT session with an owner, a
    /// 60 s cap and a TCC grant behind it (§7.4, §7.8) — a diagnostic may
    /// not start one behind the operator's back — and driving the engines
    /// is a main-thread hop this thread must not take. What they report is
    /// the route machine's own view: which devices are selected, what the
    /// last capture did, and what TCC last said.
    ///
    /// `transport` is the one the running engine was built around
    /// (`[voice] transport` at start-up; the key needs a restart). On
    /// `"ble"` the chime takes the same lanes and mixer and leaves through
    /// the deck's BLE speaker characteristic as one talk spurt
    /// (`audio::ble`), so this is a check of that path end to end; the
    /// limits say which path it was.
    fn voice_test(&mut self, params: &serde_json::Value) -> Reply {
        let what = params
            .get("what")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("speaker");
        if !matches!(what, "speaker" | "mic" | "stt" | "tts") {
            return Reply::err(ApiError::invalid("what", "speaker, mic, stt or tts"));
        }
        if !self.audio.engine() {
            return Reply::err(no_engine());
        }
        let voice = self.store.snapshot().voice.clone();
        let transport = self
            .audio
            .transport()
            .unwrap_or(self.config().voice.transport);
        let ble = transport == config::VoiceTransport::Ble;
        let mut observed = serde_json::json!({
            "what": what,
            "transport": transport.as_str(),
            "route": voice.route,
            "output_uid": voice.output_uid,
            "input_uid": voice.input_uid,
            "listening": voice.ptt.is_some(),
            "speaking": voice.speaking,
            "permissions": voice.permissions,
        });
        match what {
            "speaker" => {
                // Queued, not played: the engine owns the lane and the
                // chime carries no utterance id, so there is no report to
                // wait for. What follows on the bus is `voice.route` if the
                // output had to reopen — which is the route this test is
                // about. A refused queue is the honest `false`.
                let queued = self
                    .audio
                    .send_or_return(deck_model::AudioCmd::Chime {
                        which: "ready".into(),
                    })
                    .is_none();
                observed["played"] = queued.into();
                observed["chime"] = "ready".into();
                observed["limits"] = serde_json::json!([
                    "`played` means the chime was queued on the audio thread, not that anybody \
                     heard it: a chime carries no utterance id, so there is no `voice.utterance` \
                     report to wait for. The `voice.route` events after this call are the route \
                     it took",
                    if ble {
                        "the system default device and the system volume are never touched \
                         (§7): with `transport = \"ble\"` the chime goes to the deck's speaker \
                         characteristic over Bluetooth LE, as one talk spurt (START, 20 ms \
                         AUDIO frames, END), at the operator's `set speaker` gain"
                    } else {
                        "the system default device and the system volume are never touched \
                         (§7): the chime goes to the deck's own output UID at the operator's \
                         `set speaker` gain"
                    },
                ]);
            }
            "mic" => {
                observed["limits"] = serde_json::json!([
                    "the microphone is not opened: a capture is a PTT session with an owner, a \
                     60 s cap and a TCC grant behind it (§7.4), and a diagnostic may not start \
                     one. Hold PTT, or call `voice.listen`, and watch `voice.level`",
                    if ble {
                        "with `transport = \"ble\"` the deck's microphone arrives as packets \
                         over Bluetooth LE: it needs the Bluetooth permission, not Microphone, \
                         and `mic_warm` has no effect because the deck starts streaming on the \
                         press itself. `permissions.microphone` is about the Mac's own input \
                         (`fallback_input`)"
                    } else {
                        "`permissions.microphone` is what the audio side last reported, and a \
                         grant read from a terminal-spawned daemon says nothing about the \
                         shipped one (§10.4, W4 V9)"
                    },
                ]);
                observed["mic_warm"] = self.config().voice.mic_warm.clone().into();
                observed["mic_warm_effective"] = if ble {
                    "never"
                } else {
                    &self.config().voice.mic_warm
                }
                .into();
            }
            // "stt" | "tts": the engine as it reported itself.
            engine => {
                let Some((stt, tts)) = self.engines() else {
                    return Reply::err(no_engine());
                };
                let info = if engine == "stt" { stt } else { tts };
                observed["engine"] = engine_json(info);
                observed["configured"] = if engine == "stt" {
                    serde_json::json!({
                        "engine": self.config().voice.stt.engine,
                        "locale": self.config().voice.stt.locale,
                        "allow_server": self.config().voice.stt.allow_server,
                    })
                } else {
                    serde_json::json!({"engine": self.config().voice.tts.engine})
                };
                observed["limits"] = serde_json::json!([
                    "this is what the engine reported when it came up, not a run: driving it is \
                     a main-thread hop and this answer comes from the core thread, which may \
                     never park on the run loop",
                    "whether the configured locale is on-device is answered where it matters — \
                     `voice.listen` refuses a locale without on-device support `-32009` before \
                     anything opens (§7.6) — and not here",
                ]);
            }
        }
        Reply::ok(observed)
    }

    /// `config.reload`: re-read `config.toml`, keep the last good one when
    /// it is invalid (§3.7), apply it to the core and announce the changed
    /// `<table>.<key>` names on the bus.
    fn config_reload(&mut self, ctx: &CallCtx, now: Instant) -> Reply {
        let path = self.paths.config.clone();
        let next = match Config::load(&path) {
            Ok(next) => next,
            Err(e) => {
                let message = e.to_string();
                tracing::warn!(path = %path.display(), error = %message, "config.reload refused");
                self.publish_event(
                    EventKind::Error,
                    ctx.principal.clone(),
                    serde_json::json!({
                        "code": "config_invalid",
                        "context": "config.toml",
                        "message": message,
                    }),
                    now,
                );
                return Reply::err(ApiError::invalid("config.toml", &message));
            }
        };
        let before = config_json(self.config());
        let after = config_json(&next);
        let changed = changed_keys(&before, &after);
        let (hot, restart): (Vec<String>, Vec<String>) = changed
            .iter()
            .cloned()
            .partition(|k| is_hot(k) && config::is_hot_change(k, self.config(), &next));
        // Pauses and the Usage face follow, as on the watcher's path.
        self.take_config(std::sync::Arc::new(next), now);
        for key in &restart {
            tracing::info!(key = %key, "config: restart needed");
        }
        if !changed.is_empty() {
            self.publish_event(
                EventKind::ConfigChanged,
                ctx.principal.clone(),
                serde_json::json!({"keys": changed}),
                now,
            );
        }
        Reply::ok(serde_json::json!({
            "changed": changed, "applied": hot, "restart_needed": restart,
        }))
    }

    /// `token.create|list|revoke|rotate` through `deckd::tokens`: the file is
    /// re-read, edited and written atomically, so a `deckd token` run while
    /// the daemon is up never loses an entry. A plaintext exists once, in
    /// the reply.
    fn token_call(&self, method: &str, params: &serde_json::Value, now: Instant) -> Reply {
        let path = &self.paths.tokens;
        let ts = self.store.ts(now);
        let mut tokens = match Tokens::load(path) {
            Ok(tokens) => tokens,
            Err(e) => return Reply::err(ApiError::internal(&e.to_string())),
        };
        let name = || {
            params
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let result = match method {
            "token.list" => {
                // Contract §3.2: the bare list `[{name, scopes, created,
                // expires, embedded_in, client}]`, no wrapper object.
                let rows: Vec<serde_json::Value> = tokens
                    .list()
                    .iter()
                    .map(crate::tokens::Token::public)
                    .collect();
                return Reply::ok(serde_json::Value::Array(rows));
            }
            "token.create" => match token_scopes(params) {
                Err(e) => Err(e),
                Ok(scopes) => match token_expiry(params, ts) {
                    Err(e) => Err(e),
                    Ok(expires) => {
                        let client = params
                            .get("client")
                            .and_then(serde_json::Value::as_str)
                            .map(String::from);
                        tokens
                            .create(&name(), scopes, expires, client, ts)
                            .map(|plaintext| serde_json::json!({"token": plaintext}))
                    }
                },
            },
            "token.revoke" => tokens
                .revoke(&name())
                .map(|t| serde_json::json!({"revoked": t.name})),
            // "token.rotate", the only remaining arm of the caller's match.
            _ => tokens
                .rotate(&name(), ts)
                .map(|plaintext| serde_json::json!({"token": plaintext})),
        };
        let value = match result {
            Ok(value) => value,
            Err(e) => return Reply::err(token_error(&e)),
        };
        if let Err(e) = tokens.save(path) {
            return Reply::err(ApiError::internal(&e.to_string()));
        }
        tracing::info!(method, tokens = tokens.list().len(), "tokens.toml written");
        Reply::ok(value)
    }

    /// `log.tail{lines?:200, level?}`: the newest `deckd.log.*`, last lines
    /// first written. Nothing is logged at `info` or above that carries
    /// content (§3.7), so no redaction happens here.
    fn log_tail(&self, params: &serde_json::Value) -> Reply {
        let lines = params
            .get("lines")
            .and_then(serde_json::Value::as_u64)
            .map_or(LOG_TAIL_DEFAULT, |n| (n as usize).clamp(1, LOG_TAIL_MAX));
        let level = params
            .get("level")
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_uppercase);
        let Some(path) = newest_log(&self.paths.logs) else {
            return Reply::ok(serde_json::json!({"file": null, "lines": []}));
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => return Reply::err(ApiError::internal(&e.to_string())),
        };
        let mut out: Vec<&str> = text
            .lines()
            .filter(|l| level.as_deref().is_none_or(|want| l.contains(want)))
            .collect();
        if out.len() > lines {
            out.drain(..out.len() - lines);
        }
        Reply::ok(serde_json::json!({"file": path, "lines": out}))
    }

    /// `perm.status` (§4.11, §7.8) — B9: neither of the two permission
    /// methods existed at all, so §7.8's whole flow had no wire surface and
    /// `Permissions` was never written.
    ///
    /// It reports what the daemon actually knows and says, in `limits`,
    /// what it does not. `automation.ghostty` is now answered from what the
    /// injector learned by *trying* — the only way this permission can be
    /// read, since TCC offers no query for Apple Events — so it is `unknown`
    /// until a script has run, then `granted`, or `denied` after a -1743.
    /// `accessibility` is the same kind of answer from the Macropad keys:
    /// what the last keystroke (or `perm.request`) learned, `unknown`
    /// until one has been pressed. `speech` is macOS's own answer, read
    /// here and now ([`speech_grant`]) once there is a recogniser to need
    /// it; a read, so asking for the status never asks for the grant.
    fn perm_status(&self) -> serde_json::Value {
        let p = self.store.snapshot().voice.permissions;
        let learned = |granted: Option<bool>| match granted {
            Some(true) => "granted",
            Some(false) => "denied",
            None => "unknown",
        };
        let speech = speech_grant(
            p.speech,
            self.engines().is_some(),
            crate::audio::speech_authorization,
        );
        serde_json::json!({
            "microphone": p.microphone,
            "speech": speech,
            "bluetooth": p.bluetooth,
            "automation": {"ghostty": learned(self.automation_denied.map(|denied| !denied))},
            "accessibility": learned(self.accessibility_granted),
            "limits": PERM_LIMITS,
        })
    }

    /// `perm.request{which}` (§7.8) — **built to its interface and stubbed
    /// at the prompt**, on purpose.
    ///
    /// The contract's honesty note (W4 §1, V9, V15) is the whole of this
    /// function's reason to look like it does. The daemon cannot make a
    /// grant it can stand behind yet: a launchd-spawned `deckd` is its own
    /// responsible process with its own TCC row, and the only reason a
    /// probe with no Info.plist gets a microphone today is that
    /// `responsibility_get_pid_responsible_for_pid` attributes it to the
    /// **terminal** (measured: Zed.app, whose plist carries
    /// `NSMicrophoneUsageDescription`). A dev-loop test that went green
    /// here would be proving the terminal's grant and not ours.
    ///
    /// So it answers `{status:"prompt_not_shown"}` — which is exactly the
    /// answer §7.8 already defines for a prompt that does not surface — and
    /// puts the reason in `limits`. Nothing here may say the daemon's grant
    /// path works.
    ///
    /// **Week 7 removed one of the two reasons and not the other.** The signed
    /// bundle arrived: `tools/package-macos.sh` signs both Mach-Os
    /// `-i ai.cohdl.openwalkietalkie` with the shared entitlements, verified by
    /// `tools/assert-bundle-identity.sh`, so a grant to one really is a grant
    /// to the other and the identifier argument above is settled. What is left
    /// is a dependency and a person. `AVCaptureDevice::requestAccess` (or
    /// `AVAudioApplication`, macOS 14+) is not reachable from this workspace's
    /// pinned `objc2-avf-audio` features, and adding it is a supply-chain
    /// decision for a workspace that pins every version with its reason. And a
    /// grant needs a human to click Allow, so the risky half — the main-thread
    /// hop, the completion handler reaching the core, `perm.changed` firing —
    /// is exactly the half no test on this machine can exercise. The speech
    /// side is already reachable through `objc2-speech` if only that is wanted.
    fn perm_request(&self, params: &serde_json::Value) -> Reply {
        let which = params
            .get("which")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !matches!(which, "microphone" | "speech" | "accessibility") {
            return Reply::err(ApiError::invalid(
                "which",
                "microphone, speech or accessibility",
            ));
        }
        // Accessibility is the one grant the daemon can ask for itself:
        // the desktop side has macOS put its dialog up (once per run) and
        // reports what it learned as `perm.status.accessibility`.
        if which == "accessibility" {
            let status = match self
                .desktop
                .send_or_return(crate::desktop::DesktopJob::Accessibility)
            {
                None => "prompted",
                Some(_) => "prompt_not_shown",
            };
            return Reply::ok(serde_json::json!({
                "which": which,
                "status": status,
                "limits": ["the prompt is macOS's own and names the responsible process: for a daemon \
                            started from a terminal that is the terminal's app, for the installed \
                            service it is deckd; it is shown at most once per run"],
            }));
        }
        Reply::ok(serde_json::json!({
            "which": which,
            "status": "prompt_not_shown",
            "limits": PERM_REQUEST_LIMITS,
        }))
    }

    /// The `/v1/info` object plus the `status.json` fields (contract §3.2).
    pub fn service_info(&self, now: Instant) -> serde_json::Value {
        let snap = self.store.snapshot();
        let degraded = snap.service.degraded.clone();
        // §4.1: while the port is somebody else's — because the bind
        // failed (`port_in_use`) or because a stranger answers on it
        // (`hooks_hijack_risk`) — the daemon does not hold it, and saying
        // it does would send a client (and Claude's hooks) to the squatter.
        // Nor does it hold one with no door bound at all (the door off,
        // `config::Api::tcp_door`).
        let port = crate::api::http::held_port(&snap.service);
        let mut info = serde_json::json!({
            "api": "1",
            "daemon": VERSION,
            "executable": self.executable.as_ref().map(|p| p.to_string_lossy()),
            "platform": platform(),
            "capabilities": CAPABILITIES,
            "uds": self.paths.socket,
            "port": port,
            "mcp": serde_json::Value::Null,
            "degraded": degraded,
            "listener": snap.service.listener,
            "sessions": self.sessions.len(),
        });
        if let Ok(serde_json::Value::Object(status)) = serde_json::to_value(self.status(now)) {
            for (k, v) in status {
                info.as_object_mut()
                    .expect("info is an object")
                    .entry(k)
                    .or_insert(v);
            }
        }
        info
    }
}

/// One `<table>.<leaf>` of a `config_json` object, addressed the way
/// [`changed_keys`] names it: the table is the part before the **first**
/// dot and the leaf is everything after it, because the published shape
/// spells nested tables out flat (`hooks` carries `claude.hold_s`, `voice`
/// carries `stt.locale`). `None` for a key `config.get` does not publish,
/// which is what makes the key list of `config.set` exactly the key list of
/// `config.get` rather than a second one to keep in step.
fn key_of<'a>(config: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let (table, leaf) = key.split_once('.')?;
    config.get(table)?.get(leaf)
}

/// One [`crate::audio::traits::EngineInfo`] in §4.9's shape.
fn engine_json(info: &crate::audio::traits::EngineInfo) -> serde_json::Value {
    serde_json::json!({
        "id": info.name,
        "on_device": info.on_device,
        "locales": info.locales,
    })
}

/// `params.agent` as an id.
fn agent_param(params: &serde_json::Value) -> Option<AgentId> {
    params
        .get("agent")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// `scopes` as a list of names or one comma-separated string.
fn token_scopes(params: &serde_json::Value) -> Result<Vec<Scope>, tokens::TokenError> {
    match params.get("scopes") {
        Some(serde_json::Value::Array(list)) => {
            let spec: Vec<String> = list
                .iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect();
            tokens::parse_scopes(&spec.join(","))
        }
        Some(serde_json::Value::String(spec)) => tokens::parse_scopes(spec),
        _ => Err(tokens::TokenError::NoScopes),
    }
}

/// `expires` as `30d`/`12h`/`YYYY-MM-DD`/RFC 3339, or none.
fn token_expiry(params: &serde_json::Value, now: Ts) -> Result<Option<Ts>, tokens::TokenError> {
    match params.get("expires").and_then(serde_json::Value::as_str) {
        Some(spec) if !spec.trim().is_empty() => tokens::parse_expiry(spec, now).map(Some),
        _ => Ok(None),
    }
}

/// A `TokenError` as the API error it is: a bad name, scope or expiry is
/// `-32009 invalid{field}`, a duplicate a conflict-shaped `invalid`, an
/// unknown name `-32003`, anything else an internal error (I/O).
fn token_error(e: &tokens::TokenError) -> ApiError {
    match e {
        tokens::TokenError::Name(_) => ApiError::invalid("name", &e.to_string()),
        tokens::TokenError::Duplicate(_) => ApiError::invalid("name", &e.to_string()),
        tokens::TokenError::NotFound(_) => ApiError::not_found("token"),
        tokens::TokenError::Expiry(_) => ApiError::invalid("expires", &e.to_string()),
        tokens::TokenError::Scope(_) | tokens::TokenError::NoScopes => {
            ApiError::invalid("scopes", &e.to_string())
        }
        _ => ApiError::internal(&e.to_string()),
    }
}

/// The newest `deckd.log.*` in the log directory (the daily rotation names
/// them `deckd.log.YYYY-MM-DD`, so the last name sorts newest).
fn newest_log(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("deckd.log"))
        })
        .collect();
    names.sort();
    names.pop()
}

/// A `<table>.<key>` name is hot-applicable when `HOT_KEYS` names it or its
/// table with a `*` (§3.7), and it is not one of the keys inside a hot table
/// that are read once at start-up ([`config::RESTART_KEYS`]).
pub fn is_hot(key: &str) -> bool {
    if config::RESTART_KEYS.contains(&key) {
        return false;
    }
    HOT_KEYS.iter().any(|hot| match hot.strip_suffix(".*") {
        Some(table) => key.strip_prefix(table).is_some_and(|r| r.starts_with('.')),
        None => *hot == key,
    })
}

/// The `<table>.<key>` names whose value differs between two `config_json`
/// objects (one level deep, which is how `HOT_KEYS` names them; a nested
/// table like `hooks.claude` is compared per leaf).
pub fn changed_keys(before: &serde_json::Value, after: &serde_json::Value) -> Vec<String> {
    let mut keys = Vec::new();
    let empty = serde_json::Map::new();
    let tables = before.as_object().unwrap_or(&empty);
    let others = after.as_object().unwrap_or(&empty);
    let mut names: Vec<&String> = tables.keys().chain(others.keys()).collect();
    names.sort();
    names.dedup();
    for table in names {
        let a = tables.get(table);
        let b = others.get(table);
        if a == b {
            continue;
        }
        match (a.and_then(|v| v.as_object()), b.and_then(|v| v.as_object())) {
            (Some(a), Some(b)) => {
                let mut leaves: Vec<&String> = a.keys().chain(b.keys()).collect();
                leaves.sort();
                leaves.dedup();
                for leaf in leaves {
                    if a.get(leaf) != b.get(leaf) {
                        keys.push(format!("{table}.{leaf}"));
                    }
                }
            }
            _ => keys.push(table.clone()),
        }
    }
    keys
}

/// What `config.get` publishes for `voice.tts.api_key`: the key's hint
/// (`Tts::api_key_hint`), or `""` for no key — never the key.
fn published_key_hint(key: &str) -> String {
    let tts = crate::config::Tts {
        api_key: key.to_string(),
        ..Default::default()
    };
    if tts.has_key() {
        tts.api_key_hint()
    } else {
        String::new()
    }
}

/// The effective config as JSON (`config.get`). `Config` is
/// `Deserialize`-only (it is `config.rs`'s, owner B3), so the shape is
/// written out here; every §9 key the daemon reads is present.
///
/// It is a function of the file alone, which is what `config.set` and
/// `config.reload` compare: `sources.claude_channels` and `api.tcp` are the
/// file's own values here, `null` when the file leaves them out.
/// `config.get` publishes those keys resolved (`Core::config_get`), because
/// an unset Channels switch follows the plugin on this machine and an unset
/// TCP door follows what `deckd run` found in Claude's settings.
pub fn config_json(c: &Config) -> serde_json::Value {
    let macropad: serde_json::Map<String, serde_json::Value> = c
        .macropad
        .iter()
        .map(|(name, layout)| {
            let keys: Vec<serde_json::Value> = layout
                .keys
                .iter()
                .map(|k| {
                    serde_json::json!({
                        "l": k.l,
                        "b": k.b,
                        "action": serde_json::to_value(k.action.clone())
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            (name.clone(), serde_json::json!({"keys": keys}))
        })
        .collect();
    let pins: serde_json::Map<String, serde_json::Value> = c
        .agents
        .pins
        .iter()
        .map(|(name, pin)| {
            (
                name.clone(),
                serde_json::json!({"slot": pin.slot, "voice": pin.voice, "name": pin.name}),
            )
        })
        .collect();
    serde_json::json!({
        "api": {
            "port": c.api.port,
            "allowed_origins": c.api.allowed_origins,
            "uds_default_scope": c.api.uds_default_scope,
            "uds_max_scope": c.api.uds_max_scope,
            "mailbox_ttl_s": c.api.mailbox_ttl_s,
            "max_held_hooks": c.api.max_held_hooks,
            "tcp": c.api.tcp,
        },
        "trust": {
            "require_trusted_for_allow": c.trust.require_trusted_for_allow,
            "confirm": c.trust.confirm,
            "gui_requirement": c.trust.gui_requirement,
            "dev_requirement": c.trust.dev_requirement,
        },
        "privacy": {
            "keep_tool_input": c.privacy.keep_tool_input,
            "keep_prompts": c.privacy.keep_prompts,
            "keep_replies": c.privacy.keep_replies,
            "transcript_log": c.privacy.transcript_log,
            "log_content": c.privacy.log_content,
            "event_ring": c.privacy.event_ring,
        },
        "agents": {"grace_s": c.agents.grace_s, "evict": c.agents.evict, "pins": pins},
        "codex": {"daemon": c.codex.daemon, "daemon_socket": c.codex.daemon_socket},
        "hooks": {
            "enabled": c.hooks.enabled,
            "claude.hold_s": c.hooks.claude.hold_s,
            "claude.tool_titles": c.hooks.claude.tool_titles,
            "claude.report_via": c.hooks.claude.report_via,
            "claude.desk_presence": c.hooks.claude.desk_presence,
            "codex.hold_s": c.hooks.codex.hold_s,
            "gemini.permission": c.hooks.gemini.permission,
        },
        "sources": {
            "paused": c.sources.paused,
            "claude": c.sources.claude,
            "claude_channels": c.sources.claude_channels,
            "apps_paused": c.sources.apps_paused,
        },
        "cards": {
            "paging": c.cards.paging,
            "paging_back": c.cards.paging_back,
            "ready_ttl_s": c.cards.ready_ttl_s,
        },
        "voice": {
            "transport": c.voice.transport,
            "device_uid": c.voice.device_uid,
            "fallback_output": c.voice.fallback_output,
            "fallback_input": c.voice.fallback_input,
            "mic_warm": c.voice.mic_warm,
            "speak_over_sco": c.voice.speak_over_sco,
            "slow_sco_ms": c.voice.slow_sco_ms,
            "commands": c.voice.commands,
            "announce_questions": c.voice.announce_questions,
            "stt.engine": c.voice.stt.engine,
            "stt.locale": c.voice.stt.locale,
            "stt.allow_server": c.voice.stt.allow_server,
            "stt.model": c.voice.stt.model,
            "stt.language": c.voice.stt.language,
            "stt.threads": c.voice.stt.threads,
            "stt.prompted": c.voice.stt.prompted,
            "stt.companion": c.voice.stt.companion,
            "tts.engine": c.voice.tts.engine,
            "tts.announce_max_chars": c.voice.tts.announce_max_chars,
            "tts.provider": c.voice.tts.provider,
            // Never the key: its hint (length and last four), so a change
            // shows in `config.changed` and a screen can say one is set;
            // `""` when there is none, as in the file.
            "tts.api_key": published_key_hint(&c.voice.tts.api_key),
            "tts.model": c.voice.tts.model,
            "tts.voices": c.voice.tts.voices,
            "tts.lang": c.voice.tts.lang,
            "tts.instruct": c.voice.tts.instruct,
            "tts.python": c.voice.tts.python,
            "tts.interval_s": c.voice.tts.interval_s,
            "tts.threads": c.voice.tts.threads,
            "voices.default_pool": c.voice.voices.default_pool,
        },
        "desktop": {"inject": c.desktop.inject, "focus": c.desktop.focus},
        "device": {
            "pace_ms": c.device.pace_ms,
            "hello_s": c.device.hello_s,
            "clock": c.device.clock,
            "usage_period": c.device.usage_period,
            "status_poll_s": c.device.status_poll_s,
        },
        "macropad": macropad,
        "usage": {"providers": c.usage.providers},
        "log": {"level": c.log.level},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testkit::{detached_core, register};
    use deck_model::{AgentId, Principal, Request, SessionId};
    use std::collections::BTreeSet;

    fn ctx(session: u64, scopes: &[Scope]) -> CallCtx {
        CallCtx {
            principal: Principal::Uid(501),
            session: SessionId(session),
            scopes: scopes.to_vec(),
            trusted: false,
            own: BTreeSet::new(),
        }
    }

    #[test]
    fn image_reads_require_membership_and_device_transfers_survive_history_rollover() {
        use base64::Engine;
        let (mut c, ops, _lines, _debug, dir) = detached_core("image-reads");
        let now = Instant::now();
        let registered = c.api_call(Principal::Uid(501), SessionId(2), false,
            Request::AgentRegister(register("image-test", None)), now).unwrap();
        let agent: AgentId = serde_json::from_value(registered["agent"].clone()).unwrap();
        let images = crate::chat_images::blocks(&serde_json::json!([{"type":"image","data":crate::chat_images::fixture()}]));
        let image = images[0].id.clone();
        let expected = crate::chat_images::get(&image).unwrap().deck.clone();
        let params = serde_json::json!({"agent":agent,"image":image});
        assert!(c.call(ctx(2,&[Scope::ObserveContent]), "image.get", params.clone(), now).result.is_err());
        c.api_call(Principal::Uid(501), SessionId(2), false, Request::Reply(deck_model::ReplySet {
            agent:agent.clone(), text:String::new(), speak:false, announce:None, lane:None,
            message:Some(deck_model::MessageMeta { summary: None, images: images.clone(), id:"image".into(), role:deck_model::ChatRole::User, streaming:false, plan:None, runs:None, activity:None }),
        }), now).unwrap();
        let result = c.call(ctx(2,&[Scope::ObserveContent]), "image.get", params, now).result.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(result["data"].as_str().unwrap()).unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
        assert!(serde_json::to_vec(&result).unwrap().len() < 256*1024);
        let a = &c.store.snapshot().agents[&agent];
        let (slot, rev) = (a.slot.unwrap().0, a.chat_rev);
        while ops.try_recv().is_ok() {}
        let image_bytes = || ops.try_iter().find_map(|op| match op {
            deck_model::DeviceOp::Control(deck_model::Control::Image(deck_proto::HostLine::Image { total, data, .. })) => {
                Some((total, base64::engine::general_purpose::STANDARD.decode(data).unwrap()))
            }
            _ => None,
        }).expect("an image response");
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev:rev+1, index:0, offset:0 }, now);
        assert_eq!(image_bytes(), (0, Vec::new()), "a stale initial selection is refused");
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:0 }, now);
        let (total, mut received) = image_bytes();
        assert_eq!(usize::from(total), expected.len());
        assert_eq!(received, expected[..384]);
        for n in 0..65 {
            c.api_call(Principal::Uid(501), SessionId(2), false, Request::Reply(deck_model::ReplySet {
                agent:agent.clone(), text:format!("New message {n}"), speak:false, announce:None, lane:None,
                message:Some(deck_model::MessageMeta { summary: None, images:Vec::new(), id:format!("message-{n}"), role:deck_model::ChatRole::Assistant, streaming:false, plan:None, runs:None, activity:None }),
            }), now).unwrap();
        }
        let owner = &c.store.snapshot().agents[&agent];
        assert_ne!(owner.chat_rev, rev);
        assert!(owner.messages.iter().all(|m| m.message.images.is_empty()));
        // Both a retried first chunk and subsequent chunks remain the image
        // selected before the 64-message history rolled over.
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:0 }, now);
        assert_eq!(image_bytes().1, received);
        while received.len() < expected.len() {
            c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:received.len() as u16 }, now);
            let (total, chunk) = image_bytes();
            assert_eq!(usize::from(total), expected.len());
            assert!(!chunk.is_empty());
            received.extend(chunk);
        }
        assert!(received == expected, "every chunk belongs to the originally selected image");
        let later = now + std::time::Duration::from_secs(121);
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:384 }, later);
        assert_eq!(image_bytes(), (0, Vec::new()), "an abandoned transfer expires");
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:0 }, later);
        assert_eq!(image_bytes(), (0, Vec::new()), "an expired transfer cannot reopen an old index");

        c.api_call(Principal::Uid(501), SessionId(2), false, Request::Reply(deck_model::ReplySet {
            agent:agent.clone(), text:String::new(), speak:false, announce:None, lane:None,
            message:Some(deck_model::MessageMeta { summary: None, images, id:"another-image".into(), role:deck_model::ChatRole::User, streaming:false, plan:None, runs:None, activity:None }),
        }), later).unwrap();
        let rev = c.store.snapshot().agents[&agent].chat_rev;
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:0 }, later);
        assert_eq!(image_bytes().1, expected[..384]);
        c.api_call(Principal::Uid(501), SessionId(2), false,
            Request::AgentRelease { agent, reason: None }, later).unwrap();
        let replacement = c.api_call(Principal::Uid(501), SessionId(2), false,
            Request::AgentRegister(register("replacement", None)), later).unwrap();
        let replacement: AgentId = serde_json::from_value(replacement["agent"].clone()).unwrap();
        assert_eq!(c.store.snapshot().agents[&replacement].slot.unwrap().0, slot);
        c.on_device_line(deck_proto::DeviceLine::Image { slot, rev, index:0, offset:384 }, later);
        assert_eq!(image_bytes(), (0, Vec::new()), "a reused slot must not retain another agent's image");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn state_get_is_redacted_for_observe_only_and_keeps_own_agents() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-state");
        let t0 = Instant::now();
        let mine = c
            .api_call(
                Principal::Uid(501),
                SessionId(2),
                false,
                Request::AgentRegister(register("mine", Some("k:mine"))),
                t0,
            )
            .unwrap();
        let mine: AgentId = serde_json::from_value(mine["agent"].clone()).unwrap();
        let theirs = c
            .api_call(
                Principal::Uid(501),
                SessionId(3),
                false,
                Request::AgentRegister(register("theirs", Some("k:theirs"))),
                t0,
            )
            .unwrap();
        let theirs: AgentId = serde_json::from_value(theirs["agent"].clone()).unwrap();
        for (id, text) in [(&mine, "mine?"), (&theirs, "theirs?")] {
            c.api_call(
                Principal::Uid(501),
                c.store.snapshot().agents[id].owner.session,
                false,
                Request::CardSet(crate::core::testkit::card(id, text)),
                t0,
            )
            .unwrap();
        }
        // observe only, session 2: the own card keeps its text, the other is redacted.
        let reply = c.call(
            ctx(2, &[Scope::Observe]),
            "state.get",
            serde_json::json!({}),
            t0,
        );
        let v = reply.result.unwrap();
        assert_eq!(v["rev"], c.store.snapshot().rev);
        let agents = &v["snapshot"]["agents"];
        assert_eq!(agents[&mine.0]["card"]["text"], "mine?");
        assert_eq!(agents[&mine.0].get("redacted"), None);
        assert_eq!(agents[&theirs.0]["card"]["text"], serde_json::Value::Null);
        assert_eq!(agents[&theirs.0]["card"]["redacted"], true);
        assert_eq!(agents[&theirs.0]["card"]["choices_n"], 2);
        assert_eq!(agents[&theirs.0]["redacted"], true);
        // observe:content sees everything; a session owning nothing sees nothing.
        let v = c
            .call(
                ctx(9, &[Scope::ObserveContent]),
                "state.get",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(
            v["snapshot"]["agents"][&theirs.0]["card"]["text"],
            "theirs?"
        );
        let v = c
            .call(
                ctx(9, &[Scope::Observe]),
                "state.get",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(
            v["snapshot"]["agents"][&mine.0]["card"]["text"],
            serde_json::Value::Null
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// B8: `capabilities` is what `integrations.status` reads to tell "the
    /// hooks are installed and answered" from "installed, and this daemon
    /// has no adapter for them" — the same silence at the CLI otherwise. So
    /// it has to be one list that every surface serves and that an adapter
    /// can append itself to in the commit that makes it true.
    #[test]
    fn the_capability_list_is_sorted_unique_and_advertises_only_what_is_built() {
        let mut tidy = CAPABILITIES.to_vec();
        tidy.sort_unstable();
        tidy.dedup();
        assert_eq!(tidy, CAPABILITIES.to_vec(), "sorted and unique");
        assert!(CAPABILITIES.iter().all(|c| !c.trim().is_empty()));
        // `/v1/info` (the HTTP door, no core round trip) and `service.info`
        // (the core) serve the same constant, so an adapter's entry reaches
        // both by being added once.
        let api = crate::api::testkit::test_api("caps", crate::trust::FakeVerifier::default());
        let http = crate::api::http::info(&api.api);
        assert_eq!(http["capabilities"], serde_json::json!(CAPABILITIES));
        // Only what exists: the adapters land in week 3, their names with
        // them (the design's §4.1 example is the v1 end state, not this).
        for built in ["hooks.claude", "hooks.codex", "inject.ghostty"] {
            assert!(
                CAPABILITIES.contains(&built),
                "this daemon answers {built}, so it says so"
            );
        }
        for unbuilt in ["hooks.gemini", "mcp"] {
            assert!(
                !CAPABILITIES.contains(&unbuilt),
                "{unbuilt} is advertised but nothing answers it"
            );
        }
        // …and the other way round: every `hooks.*` this build advertises
        // has an adapter behind it in `api::hooks`, or `integrations
        // .status` would report a working chain into a `-32601`.
        for advertised in CAPABILITIES.iter().filter(|c| c.starts_with("hooks.")) {
            assert!(
                crate::api::hooks::answered(advertised),
                "{advertised} is advertised but no adapter answers it"
            );
        }
    }

    #[test]
    fn the_week_one_ctl_methods_answer_as_calls_and_the_rest_are_not_found() {
        let (mut c, _ops, lines, debug, dir) = detached_core("dispatch-ctl");
        let t0 = Instant::now();
        let ping = c
            .call(ctx(2, &[]), "session.ping", serde_json::json!({}), t0)
            .result
            .unwrap();
        assert!(ping["ts"].as_str().unwrap().ends_with('Z'));
        let r = c.call(
            ctx(2, &[Scope::Admin]),
            "device.flood",
            serde_json::json!({"n": 5}),
            t0,
        );
        assert_eq!(r.result.unwrap()["queued"], 5);
        assert_eq!(debug.try_recv(), Ok(HidDebug::Flood(5)));
        let err = c
            .call(
                ctx(2, &[Scope::Admin]),
                "device.flood",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID_PARAMS);
        c.call(
            ctx(2, &[Scope::Admin]),
            "device.sync",
            serde_json::json!({}),
            t0,
        )
        .result
        .unwrap();
        assert_eq!(debug.try_recv(), Ok(HidDebug::Sync));
        // `metrics.get` (admin) waits for a fresh report from the hid side;
        // `service.info` (observe) only nudges and answers from the last
        // one, so it can never park the core thread (§9, the C row).
        let feeder = std::thread::spawn(move || {
            assert_eq!(debug.recv().unwrap(), HidDebug::Metric);
            lines
                .send(crate::hid::FromHid::Metric(crate::metrics::Metric {
                    lines_sent: 9,
                    ..Default::default()
                }))
                .unwrap();
            // The nudge from `service.info` is answered by nobody.
            assert_eq!(debug.recv().unwrap(), HidDebug::Metric);
            (debug, lines)
        });
        let m = c
            .call(
                ctx(2, &[Scope::Admin]),
                "metrics.get",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(m["lines_sent"], 9);
        assert_eq!(m["overflows"], 0);
        assert_eq!(m["sessions"], 0);
        let started = Instant::now();
        let info = c
            .call(
                ctx(2, &[Scope::Observe]),
                "service.info",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert!(
            started.elapsed() < crate::core::METRIC_WAIT,
            "service.info never waits for the hid thread"
        );
        let (debug, lines) = feeder.join().unwrap();
        drop((debug, lines));
        assert_eq!(info["api"], "1");
        assert_eq!(info["daemon"], VERSION);
        assert_eq!(info["platform"], platform());
        assert_eq!(info["capabilities"], serde_json::json!(CAPABILITIES));
        assert_eq!(info["mcp"], serde_json::Value::Null);
        assert_eq!(info["degraded"], serde_json::Value::Null);
        assert_eq!(info["port"], 47201);
        assert!(info["uds"].as_str().unwrap().ends_with("deckd.sock"));
        assert_eq!(info["pid"], std::process::id());
        assert_eq!(info["metric"]["lines_sent"], 9);
        assert_eq!(info["sid"], 7);
        // service.bound: admin only; a failed bind degrades and hides the port.
        let err = c
            .call(
                ctx(2, &[Scope::Observe]),
                "service.bound",
                serde_json::json!({"port": null, "degraded": "port_in_use"}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::FORBIDDEN);
        c.call(
            ctx(2, &[Scope::Admin]),
            "service.bound",
            serde_json::json!({
                "port": null, "degraded": "port_in_use",
                "listener": {"pid": 4242, "command": "python3"},
            }),
            t0,
        )
        .result
        .unwrap();
        let info = c.service_info(t0);
        assert_eq!(info["port"], serde_json::Value::Null);
        assert_eq!(info["degraded"], "port_in_use");
        // §4.1: the door's `lsof` lookup reaches the event and the object,
        // so the banner can name who holds the port.
        assert_eq!(
            info["listener"],
            serde_json::json!({"pid": 4242, "command": "python3"})
        );
        c.publish();
        let degraded = c.bus.replay(0, 10, Some(&["service.*".to_string()])).0;
        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].data["reason"], "port_in_use");
        assert_eq!(degraded[0].data["port"], 47201);
        assert_eq!(
            degraded[0].data["listener"],
            serde_json::json!({"pid": 4242, "command": "python3"})
        );
        // A stranger answering `/v1/info` on our port means the port is not
        // ours either: the object must not name one (§4.1).
        c.call(
            ctx(2, &[Scope::Admin]),
            "service.bound",
            serde_json::json!({
                "port": null, "degraded": "hooks_hijack_risk",
                "listener": {"pid": 4242, "command": "python3"},
            }),
            t0,
        )
        .result
        .unwrap();
        let info = c.service_info(t0);
        assert_eq!(info["degraded"], "hooks_hijack_risk");
        assert_eq!(
            info["port"],
            serde_json::Value::Null,
            "a hijacked port is not the daemon's"
        );
        assert_eq!(info["listener"]["pid"], 4242);
        c.call(
            ctx(2, &[Scope::Admin]),
            "service.bound",
            serde_json::json!({"port": 50123, "degraded": null, "listener": null}),
            t0,
        )
        .result
        .unwrap();
        let info = c.service_info(t0);
        assert_eq!(info["port"], 50123);
        assert_eq!(info["degraded"], serde_json::Value::Null);
        assert_eq!(info["listener"], serde_json::Value::Null);
        assert_eq!(
            c.bus.snapshot().service.port,
            50123,
            "the cell carries the port"
        );
        c.publish();
        assert_eq!(
            c.bus
                .replay(0, 10, Some(&["service.*".to_string()]))
                .0
                .len(),
            3,
            "every degradation change reached the bus"
        );
        // device/voice state, the voice stubs, and the unknown.
        assert!(c
            .call(
                ctx(2, &[Scope::Observe]),
                "device.get",
                serde_json::json!({}),
                t0
            )
            .result
            .unwrap()["link"]
            .is_string());
        assert!(c
            .call(
                ctx(2, &[Scope::Observe]),
                "voice.get",
                serde_json::json!({}),
                t0
            )
            .result
            .is_ok());
        // B5: the two read-only voice methods are core-answered, and with
        // no engine in this rig they are `-32013` — with a `why` that says
        // *that* and not "week 4 has not happened yet".
        for method in ["voice.engines", "voice.test"] {
            let err = c
                .call(ctx(2, &[Scope::Voice]), method, serde_json::json!({}), t0)
                .result
                .unwrap_err();
            assert_eq!(err.code, ApiError::DEGRADED, "{method}");
            assert_eq!(err.data["reason"], "voice_unavailable", "{method}");
            assert_eq!(err.data["why"], NO_ENGINE, "{method}");
        }
        // …and the four that change something are reducer methods, so they
        // do not arrive here at all. A door routes them to `CoreMsg::Api`
        // (`api::dispatch`), which is the whole point of B5.
        for method in ["voice.say", "voice.stop", "voice.listen", "voice.cancel"] {
            assert!(
                crate::api::dispatch::method(method)
                    .expect("in the table")
                    .reducer,
                "{method}"
            );
            let err = c
                .call(
                    ctx(2, &[Scope::Voice]),
                    method,
                    serde_json::json!({"text": "hi"}),
                    t0,
                )
                .result
                .unwrap_err();
            assert_eq!(err.code, ApiError::METHOD_NOT_FOUND, "{method}");
        }
        let err = c
            .call(
                ctx(2, &[Scope::Agent]),
                "agent.wait",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::METHOD_NOT_FOUND);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `config.set{patch}` writes the operator's own file and refuses
    /// everything it cannot write cleanly — with the file untouched, which
    /// is the half a "validated" method usually gets wrong.
    ///
    /// The applying is checked beside it
    /// (`config_set_is_in_force_before_it_answers`), and the watcher's leg,
    /// the doors' cell and `config.changed`, end to end in
    /// `tests/config_set.rs`.
    #[test]
    fn config_set_writes_a_patch_and_refuses_anything_it_cannot_write_cleanly() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-set");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let original = "# mine\n[api]\nport = 47201\n";
        std::fs::write(&path, original).unwrap();
        let set = |c: &mut Core, patch: serde_json::Value| {
            c.call(
                ctx(2, &[Scope::Admin]),
                "config.set",
                serde_json::json!({ "patch": patch }),
                t0,
            )
            .result
        };

        // A hot key: written, reported changed and reported applied. The
        // tray's "Pause hooks" is exactly this call.
        let ok = set(&mut c, serde_json::json!({"hooks.enabled": false})).unwrap();
        assert_eq!(ok["keys"], serde_json::json!(["hooks.enabled"]));
        assert_eq!(ok["changed"], serde_json::json!(["hooks.enabled"]));
        assert_eq!(ok["applied"], serde_json::json!(["hooks.enabled"]));
        assert_eq!(ok["restart_needed"], serde_json::json!([]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n"), "the comment stays: {text:?}");
        assert!(!Config::parse(&text).unwrap().hooks.enabled);
        // Writing it again changes nothing and is still an answer, not an
        // error: a toggle the operator flips back is the ordinary case.
        let ok = set(&mut c, serde_json::json!({"hooks.enabled": false})).unwrap();
        assert_eq!(ok["changed"], serde_json::json!([]));

        // Several keys in one call, which is the shape §4.1's "change port
        // and re-install hooks" uses.
        let ok = set(
            &mut c,
            serde_json::json!({"voice.stt.locale": "en-GB", "voice.commands": false}),
        )
        .unwrap();
        assert_eq!(
            ok["changed"],
            serde_json::json!(["voice.commands", "voice.stt.locale"])
        );
        let both = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(both.voice.stt.locale, "en-GB");
        assert!(!both.voice.commands);

        // `null` unsets, and §9's default applies again — which for most
        // keys is **not** null, so "did the write take?" compares against
        // the default and not against what the caller sent.
        set(&mut c, serde_json::json!({"voice.mic_warm": "never"})).unwrap();
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .voice
                .mic_warm,
            "never"
        );
        let ok = set(&mut c, serde_json::json!({"voice.mic_warm": null})).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["voice.mic_warm"]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("mic_warm"), "the key is gone: {text:?}");
        assert_eq!(
            Config::parse(&text).unwrap().voice.mic_warm,
            "card_pending",
            "and §9's documented default is in force again"
        );
        // …including for a key whose default really is null.
        set(&mut c, serde_json::json!({"voice.device_uid": "C2-2D"})).unwrap();
        set(&mut c, serde_json::json!({"voice.device_uid": null})).unwrap();
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .voice
                .device_uid,
            None
        );

        // …and one that is not hot: named, and named under `restart_needed`
        // rather than reported as applied.
        let ok = set(&mut c, serde_json::json!({"api.port": 47_299})).unwrap();
        assert_eq!(ok["applied"], serde_json::json!([]));
        assert_eq!(ok["restart_needed"], serde_json::json!(["api.port"]));
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .api
                .port,
            47_299
        );

        // The refusals, each with the file left exactly as it was.
        let before = std::fs::read_to_string(&path).unwrap();
        for (patch, why) in [
            (
                serde_json::json!({"api.prot": 1}),
                "a key config.get has not",
            ),
            (serde_json::json!({"port": 1}), "no table"),
            (serde_json::json!({"api.port": "47299"}), "the wrong type"),
            (serde_json::json!({"api.port": 70_000}), "out of range"),
            (serde_json::json!({}), "a patch that names nothing"),
        ] {
            let err = set(&mut c, patch).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{why}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{why}");
        }

        // **A patch is all or nothing.** One bad key and none of the good
        // ones is written — the port must not move while the rest of the
        // patch is refused, or the hooks point at a port nothing is on.
        for patch in [
            serde_json::json!({"api.port": 47_310, "voice.stt.locale": 7}),
            serde_json::json!({"api.port": 47_310, "trust.require_trusted_for_allow": false}),
            serde_json::json!({"api.port": 47_310, "api.prot": 1}),
        ] {
            let err = set(&mut c, patch.clone()).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{patch}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                before,
                "{patch}: a refused patch wrote part of itself"
            );
        }
        assert_eq!(
            Config::parse(&before).unwrap().api.port,
            47_299,
            "the port never moved"
        );
        // `agents.pins` is published as one key and is `serde(flatten)` in
        // the file, so writing it would define a **pin named `pins`** and
        // leave `agents.pins` looking untouched. It is unmanaged, so the
        // whitelist refuses it first; the round trip is the check that
        // would catch it if it were ever managed, and it is asserted in
        // `config_set_writes_only_managed_keys_and_never_the_trust_boundary`.
        // Params of the wrong shape never reach the file either.
        for params in [
            serde_json::json!({}),
            serde_json::json!({"patch": 7}),
            serde_json::json!({"key": "api.port", "value": 1}),
        ] {
            let err = c
                .call(ctx(2, &[Scope::Admin]), "config.set", params.clone(), t0)
                .result
                .unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{params}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{params}");
        }
        // An operator's broken edit is theirs to fix: `-32009
        // invalid{config.toml}`, the same error `config.reload` gives, and
        // nothing written over it.
        std::fs::write(&path, "[api\nport = 1").unwrap();
        let err = set(&mut c, serde_json::json!({"api.port": 1})).unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        assert_eq!(err.data["field"], "config.toml");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[api\nport = 1");

        // No file at all is the documented defaults, so the first write
        // creates one carrying just that key.
        std::fs::remove_file(&path).unwrap();
        set(&mut c, serde_json::json!({"voice.commands": false})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!Config::parse(&text).unwrap().voice.commands);
        assert_eq!(Config::parse(&text).unwrap().api.port, 47_201, "{text:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reloading_a_voice_transport_change_reports_the_required_restart() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-voice-transport");
        let now = Instant::now();
        assert_eq!(config_json(c.config())["voice"]["transport"], "ble");
        std::fs::write(&c.paths.config, "[voice]\ntransport = \"coreaudio\"\n").unwrap();
        let reloaded = c
            .call(
                ctx(2, &[Scope::Admin]),
                "config.reload",
                serde_json::json!({}),
                now,
            )
            .result
            .unwrap();
        assert_eq!(reloaded["changed"], serde_json::json!(["voice.transport"]));
        assert_eq!(reloaded["applied"], serde_json::json!([]));
        assert_eq!(
            reloaded["restart_needed"],
            serde_json::json!(["voice.transport"])
        );
        assert_eq!(config_json(c.config())["voice"]["transport"], "coreaudio");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `config.set` writes the file the **trust boundary** is declared in,
    /// so what it will write is the `MANAGED` whitelist and everything else
    /// is refused — including keys that are perfectly valid, hot, and named
    /// by `config.get`.
    ///
    /// The direction matters more than the contents. A list of forbidden
    /// keys fails **open** the next time `[trust]` grows a field: the new
    /// one is writable, nothing warns, and an `admin` token has quietly
    /// gained whatever it declares. This list fails closed, and this row is
    /// what keeps it that way — it asserts the shape of the rule, not just
    /// today's entries.
    #[test]
    fn config_set_writes_only_managed_keys_and_never_the_trust_boundary() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-whitelist");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let original = "[api]\nport = 47201\n";
        std::fs::write(&path, original).unwrap();

        // Every name on the list is a key `config.get` really publishes, so
        // the whitelist cannot rot into a list of keys that no longer exist
        // and quietly stop covering the ones that do.
        let published = config_json(&Config::default());
        for key in MANAGED {
            assert!(
                key_of(&published, key).is_some(),
                "{key} is managed but config.get does not publish it"
            );
        }
        let mut tidy = MANAGED.to_vec();
        tidy.sort_unstable();
        tidy.dedup();
        assert_eq!(tidy, MANAGED.to_vec(), "sorted and unique");

        // The keys that decide who is trusted, what may be typed, which
        // origins are accepted, what is written to disk and where the
        // operator's speech goes. Each one is published by `config.get`,
        // each one is a legitimate thing for a human to edit, and none of
        // them may be written by a caller.
        for key in [
            "trust.require_trusted_for_allow",
            "trust.gui_requirement",
            "trust.dev_requirement",
            "trust.confirm",
            "desktop.inject",
            "desktop.focus",
            "api.allowed_origins",
            "api.uds_default_scope",
            "api.uds_max_scope",
            "privacy.log_content",
            "privacy.keep_prompts",
            "privacy.transcript_log",
            "voice.stt.allow_server",
            "agents.evict",
            "log.level",
        ] {
            assert!(
                key_of(&published, key).is_some(),
                "{key} is not a config.get key at all: fix the test, not the list"
            );
            assert!(!MANAGED.contains(&key), "{key} must not be managed");
            let err = c
                .call(
                    ctx(2, &[Scope::Admin]),
                    "config.set",
                    // A plausible value for each, so the refusal is the
                    // whitelist's and not the type checker's.
                    serde_json::json!({"patch": {key: false}}),
                    t0,
                )
                .result
                .unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{key}");
            assert!(
                err.data["why"]
                    .as_str()
                    .unwrap()
                    .contains("not a managed key"),
                "{key}: {err:?}"
            );
            // And the bytes are the ones we wrote. A refusal that had got
            // as far as the temp file would be a config the §3.7 watcher
            // could read between the write and the validation.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "{key}");
            assert_eq!(
                std::fs::read_dir(&c.paths.data)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_name().to_string_lossy().starts_with("config.toml."))
                    .count(),
                0,
                "{key}: a temp file was left in the watched directory"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The Integrations page's switches are managed keys: `[sources]` and
    /// the Codex switch go in one patch, every one of them hot, and
    /// `config.get` reads them back; `null` puts one back to its default. A
    /// key beside them that is not on the list — the Codex socket is the
    /// operator's — is still refused, and so is a value the table cannot
    /// hold, with the file untouched.
    #[test]
    fn config_set_writes_the_source_switches_and_still_refuses_an_unmanaged_key() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-sources");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let set = |c: &mut Core, patch: serde_json::Value| {
            c.call(
                ctx(2, &[Scope::Admin]),
                "config.set",
                serde_json::json!({ "patch": patch }),
                t0,
            )
            .result
        };
        let ok = set(
            &mut c,
            serde_json::json!({
                "sources.paused": true,
                "sources.claude": false,
                "sources.apps_paused": ["Conol", "uid:501"],
                "codex.daemon": false,
            }),
        )
        .unwrap();
        let all = serde_json::json!([
            "codex.daemon",
            "sources.apps_paused",
            "sources.claude",
            "sources.paused"
        ]);
        assert_eq!(ok["changed"], all);
        assert_eq!(ok["applied"], all, "a pause that waited for a restart is none");
        assert_eq!(ok["restart_needed"], serde_json::json!([]));
        let written = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.sources.paused && !written.sources.claude && !written.codex.daemon);
        assert_eq!(written.sources.apps_paused, ["Conol", "uid:501"]);
        assert_eq!(
            config_json(&written)["sources"],
            serde_json::json!({
                "paused": true, "claude": false, "claude_channels": null,
                "apps_paused": ["Conol", "uid:501"],
            })
        );
        let ok = set(
            &mut c,
            serde_json::json!({"sources.paused": null, "sources.apps_paused": null}),
        )
        .unwrap();
        assert_eq!(
            ok["changed"],
            serde_json::json!(["sources.apps_paused", "sources.paused"])
        );
        let back = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!back.sources.paused && back.sources.apps_paused.is_empty());
        let before = std::fs::read_to_string(&path).unwrap();
        for patch in [
            serde_json::json!({"sources.claude_channels": "off"}),
            serde_json::json!({"codex.daemon_socket": "/tmp/elsewhere.sock"}),
            serde_json::json!({"sources.paused": false, "hooks.claude.tool_titles": true}),
            serde_json::json!({"sources.apps_paused": "Conol"}),
            serde_json::json!({"sources.paused": "yes"}),
        ] {
            let err = set(&mut c, patch.clone()).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{patch}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "{patch}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D4: the Claude Channels switch through `config.set` and back through
    /// `config.get`, which always publishes a bool: the file's value once it
    /// sets one, and while it leaves the key out, whether this daemon's
    /// plugin carries the helper. `null` takes the key out of the file again
    /// and the plugin decides once more; a value that is not a bool is
    /// refused with the file untouched.
    #[test]
    fn config_set_and_get_round_trip_the_claude_channels_switch() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-channels");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let helper = crate::integrations::claude::mcp_json_in(&c.paths.data);
        let admin = || ctx(2, &[Scope::Admin]);
        let get = |c: &mut Core| {
            let published = c
                .call(admin(), "config.get", serde_json::json!({}), t0)
                .result
                .unwrap();
            published["sources"]["claude_channels"].clone()
        };
        // `config.set` alone: what it writes is in force when it answers,
        // so `get` reads it back with no reload in between.
        let set = |c: &mut Core, value: serde_json::Value| {
            let patch = serde_json::json!({"patch": {"sources.claude_channels": value}});
            c.call(admin(), "config.set", patch, t0).result
        };
        assert_eq!(get(&mut c), false, "a new install: unset, and no helper");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, b"{}\n").unwrap();
        assert_eq!(
            get(&mut c),
            true,
            "an install from before the switch: its plugin has the helper"
        );

        let ok = set(&mut c, serde_json::json!(false)).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["sources.claude_channels"]));
        assert_eq!(
            ok["applied"],
            serde_json::json!(["sources.claude_channels"]),
            "the door and the row follow it without a restart"
        );
        assert_eq!(ok["restart_needed"], serde_json::json!([]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("claude_channels = false"), "{text:?}");
        assert_eq!(get(&mut c), false, "the switch over the plugin");
        set(&mut c, serde_json::json!(true)).unwrap();
        assert_eq!(get(&mut c), true);
        std::fs::remove_file(&helper).unwrap();
        assert_eq!(get(&mut c), true, "and before a repair has written one");

        let ok = set(&mut c, serde_json::Value::Null).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["sources.claude_channels"]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("claude_channels"), "the key is gone: {text:?}");
        assert_eq!(get(&mut c), false, "unset again, and the plugin has none");

        for value in [
            serde_json::json!("on"),
            serde_json::json!(1),
            serde_json::json!([true]),
        ] {
            let err = set(&mut c, value.clone()).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{value}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{value}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D3: the Usage panel's switches are one managed, hot key, written as
    /// the whole list (a `ConfigValue::List` on the app's side). Claude on
    /// is the list with it, off the list without it, `[]` no bars at all and
    /// `null` the default, Codex alone. Anything that is not a list of names
    /// is refused with the file untouched.
    #[test]
    fn config_set_switches_the_usage_providers_on_and_off() {
        use crate::usage::Provider;
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-usage");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let set = |c: &mut Core, providers: serde_json::Value| {
            c.call(
                ctx(2, &[Scope::Admin]),
                "config.set",
                serde_json::json!({"patch": {"usage.providers": providers}}),
                t0,
            )
            .result
        };
        let written = || Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let ok = set(&mut c, serde_json::json!(["codex", "claude"])).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["usage.providers"]));
        assert_eq!(
            ok["applied"],
            serde_json::json!(["usage.providers"]),
            "the face redraws without a restart"
        );
        assert_eq!(ok["restart_needed"], serde_json::json!([]));
        let on = written();
        assert!(Provider::Claude.enabled(&on.usage) && Provider::Codex.enabled(&on.usage));
        assert_eq!(
            config_json(&on)["usage"]["providers"],
            serde_json::json!(["codex", "claude"]),
            "config.get reads the list back as written"
        );
        let ok = set(&mut c, serde_json::json!(["codex"])).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["usage.providers"]));
        assert!(!Provider::Claude.enabled(&written().usage));
        set(&mut c, serde_json::json!([])).unwrap();
        assert!(Provider::ALL.iter().all(|p| !p.enabled(&written().usage)));
        let ok = set(&mut c, serde_json::Value::Null).unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["usage.providers"]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("providers"), "the key is gone: {text:?}");
        assert_eq!(
            written().usage.providers,
            ["codex"],
            "and the default is in force"
        );
        for providers in [
            serde_json::json!("claude"),
            serde_json::json!(["claude", null]),
            serde_json::json!([1]),
            serde_json::json!(true),
        ] {
            let err = set(&mut c, providers.clone()).unwrap_err();
            assert_eq!(err.code, ApiError::INVALID_PARAMS, "{providers}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{providers}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D3: `usage.providers` is hot on both paths a config takes in. The
    /// watcher's `CoreMsg::Config` always redrew the face; `config.reload`
    /// does too now, so the Claude windows the feeder still holds stop being
    /// drawn the moment the list leaves Claude out, and Codex's stay.
    #[test]
    fn a_reload_that_leaves_claude_out_redraws_the_face_without_it() {
        use crate::usage::{Provider, Window};
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-usage-reload");
        let t0 = Instant::now();
        let reload = |c: &mut Core, text: &str| {
            std::fs::write(&c.paths.config, text).unwrap();
            c.call(
                ctx(2, &[Scope::Admin]),
                "config.reload",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap()
        };
        let drawn = |c: &Core| -> Vec<String> {
            let usage = c.store.snapshot().usage.as_ref();
            usage
                .map(|u| u.items.iter().map(|i| i.name.clone()).collect())
                .unwrap_or_default()
        };
        reload(&mut c, "[usage]\nproviders = [\"claude\", \"codex\"]\n");
        let window = |label: &str| Window {
            label: label.into(),
            percent: 10,
            resets_at: None,
        };
        c.usage.set(Provider::Claude, vec![window("session")], None);
        c.usage.set(Provider::Codex, vec![window("week")], None);
        c.push_usage(t0);
        assert_eq!(drawn(&c), ["Claude session", "Codex week"]);
        let reloaded = reload(&mut c, "[usage]\nproviders = [\"codex\"]\n");
        assert_eq!(reloaded["applied"], serde_json::json!(["usage.providers"]));
        assert_eq!(drawn(&c), ["Codex week"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R3: what `config.set` reports applied is in force when it answers,
    /// with no reload and no watcher in between. `config.get` reads the
    /// switches back at once, and a Usage face drawn with Claude's windows
    /// is redrawn without them by the call that leaves Claude out. The
    /// reload a watcher makes of the same file afterwards has nothing left
    /// to change. (The pauses' half is in `care::sources`.)
    #[test]
    fn config_set_is_in_force_before_it_answers() {
        use crate::usage::{Provider, Window};
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-in-force");
        let t0 = Instant::now();
        let admin = || ctx(2, &[Scope::Admin]);
        let set = |c: &mut Core, patch: serde_json::Value| {
            c.call(admin(), "config.set", serde_json::json!({ "patch": patch }), t0)
                .result
                .unwrap()
        };
        let get = |c: &mut Core| {
            c.call(admin(), "config.get", serde_json::json!({}), t0)
                .result
                .unwrap()
        };
        let drawn = |c: &Core| -> Vec<String> {
            let usage = c.store.snapshot().usage.as_ref();
            usage
                .map(|u| u.items.iter().map(|i| i.name.clone()).collect())
                .unwrap_or_default()
        };
        let ok = set(
            &mut c,
            serde_json::json!({"sources.paused": true, "usage.providers": ["claude", "codex"]}),
        );
        assert_eq!(
            ok["applied"],
            serde_json::json!(["sources.paused", "usage.providers"])
        );
        let published = get(&mut c);
        assert_eq!(published["sources"]["paused"], true);
        assert_eq!(
            published["usage"]["providers"],
            serde_json::json!(["claude", "codex"])
        );
        let window = |label: &str| Window {
            label: label.into(),
            percent: 10,
            resets_at: None,
        };
        c.usage.set(Provider::Claude, vec![window("session")], None);
        c.usage.set(Provider::Codex, vec![window("week")], None);
        c.push_usage(t0);
        assert_eq!(drawn(&c), ["Claude session", "Codex week"]);
        set(&mut c, serde_json::json!({"usage.providers": ["codex"]}));
        assert_eq!(drawn(&c), ["Codex week"], "redrawn before the reply");
        assert_eq!(get(&mut c)["usage"]["providers"], serde_json::json!(["codex"]));
        set(&mut c, serde_json::json!({"sources.paused": null}));
        assert_eq!(get(&mut c)["sources"]["paused"], false);
        let reloaded = c
            .call(admin(), "config.reload", serde_json::json!({}), t0)
            .result
            .unwrap();
        assert_eq!(
            reloaded["changed"],
            serde_json::json!([]),
            "the core already holds the file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D3: the Local API switch is managed and, like the port, read at
    /// start: the reply names it under `restart_needed` instead of claiming
    /// it was applied, and so does a reload that finds it moved by hand.
    /// `config.get` always publishes a bool: the file's value once it sets
    /// one, and while it leaves the key out, off — unless `run` kept the
    /// door open for the pre-plugin Claude hooks it found at start. `null`
    /// takes the key out of the file again.
    #[test]
    fn config_set_writes_the_tcp_switch_for_the_next_start() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-tcp");
        let t0 = Instant::now();
        let path = c.paths.config.clone();
        let admin = || ctx(2, &[Scope::Admin]);
        // The switch, and the rule that decided it (`api.tcp_rule`).
        let get = |c: &mut Core| {
            let published = c
                .call(admin(), "config.get", serde_json::json!({}), t0)
                .result
                .unwrap();
            serde_json::json!([published["api"]["tcp"], published["api"]["tcp_rule"]])
        };
        assert_eq!(
            get(&mut c),
            serde_json::json!([false, "default"]),
            "left out: off by default"
        );
        c.legacy_hooks = true;
        assert_eq!(
            get(&mut c),
            serde_json::json!([true, "legacy_hooks"]),
            "left out, and the old hooks were found"
        );
        // The rule is published, never written: no key of the file.
        let refused = c
            .call(
                admin(),
                "config.set",
                serde_json::json!({"patch": {"api.tcp_rule": "explicit"}}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(refused.code, ApiError::INVALID_PARAMS, "{refused:?}");
        assert!(!path.exists() || !std::fs::read_to_string(&path).unwrap().contains("tcp"));
        let ok = c
            .call(
                admin(),
                "config.set",
                serde_json::json!({"patch": {"api.tcp": false}}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["api.tcp"]));
        assert_eq!(ok["applied"], serde_json::json!([]));
        assert_eq!(ok["restart_needed"], serde_json::json!(["api.tcp"]));
        let written = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.api.tcp, Some(false));
        assert_eq!(config_json(&written)["api"]["tcp"], false);
        assert_eq!(
            get(&mut c),
            serde_json::json!([false, "explicit"]),
            "an explicit value is always honoured"
        );
        // The operator switches it back by hand; the reload says the same.
        std::fs::write(&path, "[api]\ntcp = true\n").unwrap();
        let reloaded = c
            .call(admin(), "config.reload", serde_json::json!({}), t0)
            .result
            .unwrap();
        assert_eq!(reloaded["restart_needed"], serde_json::json!(["api.tcp"]));
        assert_eq!(get(&mut c), serde_json::json!([true, "explicit"]));
        // Taken out again: the file says nothing, and nothing kept it open.
        c.legacy_hooks = false;
        let ok = c
            .call(
                admin(),
                "config.set",
                serde_json::json!({"patch": {"api.tcp": null}}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(ok["changed"], serde_json::json!(["api.tcp"]));
        assert_eq!(ok["restart_needed"], serde_json::json!(["api.tcp"]));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("tcp"));
        assert_eq!(
            config_json(c.config())["api"]["tcp"],
            serde_json::Value::Null
        );
        assert_eq!(get(&mut c), serde_json::json!([false, "default"]));
        let before = std::fs::read_to_string(&path).unwrap();
        let err = c
            .call(
                admin(),
                "config.set",
                serde_json::json!({"patch": {"api.tcp": "off"}}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID_PARAMS);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The JSON type of a value, for holding a reply to the app's fixture.
    fn json_type(value: &serde_json::Value) -> &'static str {
        match value {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    /// B3.3 for D3's switches: `config.get`'s `api` table has every key of
    /// the app's fixture (`GC/tests/fixtures/daemon/config_get_sources.json`)
    /// with the same JSON type, `tcp` among them — a bool, which the app also
    /// reads as the mark of a service that writes `usage.providers` — and no
    /// key the fixture lacks; the usage providers and the Mac microphone
    /// fallback have the fixture's types; and all three are keys `config.set`
    /// writes. A default daemon's door is off (the owner's decision 12).
    #[test]
    fn config_get_has_the_keys_of_the_apps_fixture_for_the_d3_switches() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json"
        ))
        .unwrap();
        let (c, _ops, _lines, _debug, dir) = detached_core("dispatch-config-fixture");
        let published = c.config_get();
        let want = fixture["api"].as_object().unwrap();
        let got = published["api"].as_object().unwrap();
        for (key, value) in want {
            let mine = got.get(key).unwrap_or_else(|| panic!("api: no {key:?}"));
            assert_eq!(
                json_type(mine),
                json_type(value),
                "api.{key}: {mine} where {value}"
            );
        }
        for key in got.keys() {
            assert!(want.contains_key(key), "api: {key:?} is not in the fixture");
        }
        for key in ["api.tcp", "usage.providers", "voice.fallback_input"] {
            let (mine, theirs) = (key_of(&published, key), key_of(&fixture, key));
            assert_eq!(mine.map(json_type), theirs.map(json_type), "{key}");
            assert!(
                MANAGED.contains(&key),
                "{key} is a switch config.set writes"
            );
        }
        assert_eq!(published["api"]["tcp"], false);
        assert_eq!(published["api"]["tcp_rule"], "default", "why it is off");
        assert!(
            !MANAGED.contains(&"api.tcp_rule"),
            "the rule is published, never written"
        );
        assert_eq!(
            published["usage"]["providers"],
            serde_json::json!(["codex"])
        );
        assert_eq!(published["voice"]["fallback_input"], "none");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D3: with no TCP door there is no port to name. `api::serve` records
    /// port 0 for a door that is off, and `service.info` then says `null` —
    /// not the configured 47201 and not 0 — while nothing is degraded, so no
    /// `service.degraded` event is raised either.
    #[test]
    fn service_info_names_no_port_while_no_door_is_bound() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-no-door");
        let t0 = Instant::now();
        assert_eq!(
            c.service_info(t0)["port"],
            47201,
            "configured, not yet bound"
        );
        c.call(
            ctx(2, &[Scope::Admin]),
            "service.bound",
            serde_json::json!({"port": 0, "degraded": null, "listener": null}),
            t0,
        )
        .result
        .unwrap();
        let info = c.service_info(t0);
        assert_eq!(info["port"], serde_json::Value::Null);
        assert_eq!(info["degraded"], serde_json::Value::Null);
        assert_eq!(c.bus.snapshot().service.port, 0, "the cell carries it");
        c.publish();
        assert!(
            c.bus
                .replay(0, 10, Some(&["service.*".to_string()]))
                .0
                .is_empty(),
            "nothing degraded"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `service.restart`/`service.stop`: the reply names the code, and the
    /// supervisor cannot see it yet.
    ///
    /// That ordering is the whole requirement — the reply is written by a
    /// door after this returns, and a client that never receives it cannot
    /// tell "restarted" from "crashed". Here it is asserted against the
    /// cell; `tests/service_exit.rs` asserts it against a real process,
    /// which is the only place "and then it exits 75" can be proved.
    #[test]
    fn service_restart_and_stop_answer_before_the_supervisor_may_see_the_code() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-service-exit");
        let t0 = Instant::now();
        let exit = std::sync::Arc::new(crate::service::Exit::default());
        c.set_exit(exit.clone());

        let ok = c
            .call(
                ctx(2, &[Scope::Admin]),
                "service.restart",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(ok["action"], "restart");
        assert_eq!(ok["exit"], crate::service::EXIT_RESTART);
        assert_eq!(ok["pid"], std::process::id());
        assert_eq!(
            exit.requested(t0),
            None,
            "the reply has not been written yet, so nothing may come down"
        );
        assert_eq!(
            exit.requested(t0 + crate::service::Exit::GRACE),
            Some(crate::service::EXIT_RESTART)
        );

        // A `service.stop` behind it is answered truthfully rather than
        // turning a restart into a stop.
        let ok = c
            .call(
                ctx(2, &[Scope::Admin]),
                "service.stop",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(ok["exit"], crate::service::EXIT_RESTART);
        assert_eq!(ok["action"], "restart");

        // …and on a daemon of its own, a stop is a 0.
        let (mut c, _ops, _lines, _debug, dir2) = detached_core("dispatch-service-stop");
        let exit = std::sync::Arc::new(crate::service::Exit::default());
        c.set_exit(exit.clone());
        let ok = c
            .call(
                ctx(2, &[Scope::Admin]),
                "service.stop",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(ok["action"], "stop");
        assert_eq!(ok["exit"], crate::service::EXIT_STOP);
        assert_eq!(
            exit.requested(t0 + crate::service::Exit::GRACE),
            Some(crate::service::EXIT_STOP)
        );
        // Both are `admin` rows of the table, so `/v1/schema` names them —
        // which it may only do now that they answer (§3.3).
        for method in ["service.restart", "service.stop"] {
            let spec = crate::api::dispatch::method(method).expect("in the table");
            assert_eq!(spec.scope, Some(Scope::Admin), "{method}");
            assert!(!spec.reducer, "{method}");
        }
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(dir2).unwrap();
    }

    #[test]
    fn an_update_waits_for_ptt_and_the_final_transcript_before_restarting() {
        use deck_model::{Command, DeviceLine, PttCtx, VoiceIn};
        let (mut c, _ops, _lines, _debug, dir) = detached_core("update-waits-for-transcript");
        let (audio, _commands, _events) = crate::audio::AudioHandle::detached();
        c.set_audio(audio);
        let exit = std::sync::Arc::new(crate::service::Exit::default());
        c.set_exit(exit.clone());
        let now = Instant::now();
        let bundle_path = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";
        c.executable = Some(bundle_path.into());
        assert_eq!(c.service_info(now)["executable"], bundle_path);
        for act in [1, 0] {
            c.step(Command::Device(DeviceLine::Ptt {
                act, slot: 1, ctx: PttCtx::Talk, card: None, abort: false, press: None,
            }), now);
            if act == 1 {
                assert!(c.store.snapshot().voice.ptt.is_some());
            } else {
                assert!(c.store.snapshot().voice.ptt.is_none());
                assert!(c.store.is_finalizing());
            }
            for (method, params) in [
                ("service.restart", serde_json::json!({"when_idle":true})),
                ("firmware.install", serde_json::json!({})),
            ] {
                let error = c.call(ctx(2, &[Scope::Admin]), method, params, now).result.unwrap_err();
                assert!(error.data["why"].as_str().unwrap().contains(if method == "service.restart" { "finish" } else { "transcription" }));
                assert_eq!(exit.requested(now + crate::service::Exit::GRACE), None);
            }
        }
        c.step(Command::Voice(VoiceIn::Final { text: "Complete sentence.".into() }), now);
        assert!(!c.store.is_finalizing());
        c.call(ctx(2, &[Scope::Admin]), "service.restart", serde_json::json!({"when_idle":true}), now).result.unwrap();
        assert_eq!(exit.requested(now + crate::service::Exit::GRACE), Some(crate::service::EXIT_RESTART));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// O16: a unit-test core never reaches out. These three used to open
    /// whatever deck was on USB, fetch the release manifest and start the
    /// default models' downloads whenever the API tests swept the method
    /// table; each now answers from what it already has.
    #[test]
    fn a_test_core_answers_the_firmware_check_and_both_downloads_without_reaching_out() {
        let (mut c, _ops, _lines, _debug, dir) = detached_core("offline-guards");
        let now = Instant::now();
        let admin = ctx(2, &[Scope::Admin]);
        let check = c
            .call(admin.clone(), "firmware.check", serde_json::json!({"force": true}), now)
            .result
            .unwrap();
        assert_eq!(check["phase"], "idle", "{check}");
        assert!(check["device"].is_null() && check["release"].is_null(), "{check}");
        let install = serde_json::json!({
            "board": "hw-b", "serial": "X", "version": "9.9.9", "allow_bootloader_update": true,
        });
        let refused = c.call(admin.clone(), "firmware.install", install, now).result.unwrap_err();
        assert_eq!(refused.data["why"], crate::firmware::NO_DEVICE);
        for method in ["stt.install", "tts.install"] {
            let status = c.call(admin.clone(), method, serde_json::json!({}), now).result.unwrap();
            let download = &status["download"];
            assert_eq!(download["running"], false, "{method}: {status}");
            assert_eq!(download["done"], false, "{method}: {status}");
            assert_eq!(download["error"], crate::download::OFFLINE, "{method}: {status}");
        }
        let status = c.call(admin, "firmware.status", serde_json::json!({}), now).result.unwrap();
        assert_eq!(status["phase"], "idle", "{status}");
        for what in ["firmware", "stt", "tts"] {
            assert!(!dir.join("caches").join(what).exists(), "{what} was written");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `voice.engines`/`voice.test` answer from the report the audio side
    /// pushed when its engines came up, and never from the run loop.
    ///
    /// M5: reading `EngineInfo` live is a main-thread hop, and this is the
    /// core thread — every agent, every hook and every device line queue
    /// behind it. So the value arrives as a `FromAudio` like any other
    /// report, is cached, and is served from the cache; the test feeds it
    /// by hand exactly as the week-4 rig feeds a `VoiceIn`.
    #[test]
    fn voice_engines_answers_from_the_report_the_audio_side_pushed_and_test_makes_one_short_tone() {
        use crate::audio::traits::EngineInfo;
        use crate::audio::{AudioHandle, FromAudio};
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-voice-engines");
        let t0 = Instant::now();
        let voice = ctx(2, &[Scope::Voice]);

        // No engine: `-32013`, and the `why` says which of the three ways
        // that happens it is — never "not built yet".
        let err = c
            .call(voice.clone(), "voice.engines", serde_json::json!({}), t0)
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::DEGRADED);
        assert_eq!(err.data["reason"], "voice_unavailable");
        assert!(err.data["why"].as_str().unwrap().contains("--no-device"));

        // An engine, and its one report.
        let (audio, cmds, _events) = AudioHandle::detached();
        c.set_audio(audio);
        assert_eq!(
            c.call(voice.clone(), "voice.engines", serde_json::json!({}), t0)
                .result
                .unwrap_err()
                .code,
            ApiError::DEGRADED,
            "an engine that has not reported yet is still nothing to read"
        );
        c.on_from_audio(
            FromAudio::Engines {
                stt: EngineInfo {
                    name: "apple".into(),
                    on_device: true,
                    locales: vec!["en-US".into(), "en-GB".into()],
                },
                tts: EngineInfo {
                    name: "apple".into(),
                    on_device: true,
                    locales: vec!["en-US".into()],
                },
            },
            t0,
        );
        let engines = c
            .call(voice.clone(), "voice.engines", serde_json::json!({}), t0)
            .result
            .unwrap();
        assert_eq!(engines["stt"][0]["id"], "apple");
        assert_eq!(engines["stt"][0]["on_device"], true);
        assert_eq!(
            engines["stt"][0]["locales"],
            serde_json::json!(["en-US", "en-GB"])
        );
        assert_eq!(engines["tts"][0]["locales"], serde_json::json!(["en-US"]));
        assert_eq!(engines["selected"]["stt"], "apple");
        assert_eq!(engines["selected"]["locale"], "en-US");

        // `voice.test` defaults to the speaker check: §7.3's `ready` chime
        // — 110 ms, peak 0.2 — on the ordinary lane, and the route it went
        // out on. Nothing here touches the system default device or the
        // system volume.
        let _ = cmds.try_iter().count();
        let test = c
            .call(voice.clone(), "voice.test", serde_json::json!({}), t0)
            .result
            .unwrap();
        assert_eq!(test["what"], "speaker");
        assert_eq!(test["chime"], "ready");
        assert_eq!(test["played"], true);
        assert_eq!(test["route"], "none", "the route it took, as observed");
        assert!(test["limits"].as_array().is_some_and(|l| l.len() == 2));
        // `[voice] transport` defaults to the BLE audio service, and the
        // limits say where the chime went.
        assert_eq!(test["transport"], "ble");
        assert!(
            test["limits"][1]
                .as_str()
                .unwrap()
                .contains("speaker characteristic over Bluetooth LE"),
            "{}",
            test["limits"][1]
        );
        assert_eq!(
            cmds.try_iter().collect::<Vec<_>>(),
            vec![deck_model::AudioCmd::Chime {
                which: "ready".into()
            }],
            "one short quiet tone, and nothing else"
        );

        // `mic` is a **report** and says so: a capture has an owner, a 60 s
        // cap and a TCC grant behind it, and a diagnostic may not open one.
        let test = c
            .call(
                voice.clone(),
                "voice.test",
                serde_json::json!({"what": "mic"}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(test["listening"], false);
        assert_eq!(test["mic_warm"], "card_pending");
        assert_eq!(test["mic_warm_effective"], "never");
        assert!(test["limits"][0].as_str().unwrap().contains("not opened"));
        assert!(
            test["limits"][1]
                .as_str()
                .unwrap()
                .contains("Bluetooth permission, not Microphone"),
            "on BLE the deck's microphone is a Bluetooth grant: {}",
            test["limits"][1]
        );
        assert!(cmds.try_iter().next().is_none(), "nothing was opened");

        // `stt`/`tts` report the engine that came up, beside what `[voice]`
        // selects — the two the Voice screen's badges put side by side.
        let test = c
            .call(
                voice.clone(),
                "voice.test",
                serde_json::json!({"what": "stt"}),
                t0,
            )
            .result
            .unwrap();
        assert_eq!(test["engine"]["id"], "apple");
        assert_eq!(test["configured"]["locale"], "en-US");
        assert_eq!(test["configured"]["allow_server"], false);
        let err = c
            .call(
                voice,
                "voice.test",
                serde_json::json!({"what": "headphones"}),
                t0,
            )
            .result
            .unwrap_err();
        assert_eq!(err.code, ApiError::INVALID);
        assert_eq!(err.data["field"], "what");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// R2: `perm.status.speech` is the OS's answer, read when it is asked,
    /// once there is an engine whose recogniser needs it; with no engine,
    /// or no OS answer to read, it is what the audio side reported, and
    /// with no engine nothing is read at all.
    #[test]
    fn the_speech_grant_is_read_only_when_a_recogniser_needs_it() {
        let never = || -> Option<Permission> { panic!("read with no engine") };
        for reported in [Permission::Unknown, Permission::Denied] {
            assert_eq!(speech_grant(reported, false, never), reported);
        }
        assert_eq!(
            speech_grant(Permission::Unknown, true, || Some(Permission::Granted)),
            Permission::Granted
        );
        assert_eq!(
            speech_grant(Permission::Denied, true, || Some(Permission::NotDetermined)),
            Permission::NotDetermined,
            "the OS's answer over a stale report"
        );
        assert_eq!(
            speech_grant(Permission::Unknown, true, || None),
            Permission::Unknown,
            "no OS answer to read"
        );
    }

    /// The same through the method: `unknown` with no engine (the snapshot's
    /// value), then, with an engine up, macOS's own answer — never the
    /// `unknown` of no answer — and still the report elsewhere. The read
    /// moves nothing in the snapshot.
    #[test]
    fn perm_status_reads_the_speech_grant_once_an_engine_is_up() {
        use crate::audio::traits::EngineInfo;
        use crate::audio::{AudioHandle, FromAudio};
        let (mut c, _ops, _lines, _debug, dir) = detached_core("dispatch-perm-speech");
        let t0 = Instant::now();
        let speech = |c: &mut Core| {
            c.call(ctx(2, &[Scope::Voice]), "perm.status", serde_json::json!({}), t0)
                .result
                .unwrap()["speech"]
                .clone()
        };
        assert_eq!(speech(&mut c), "unknown", "no engine, nothing read");
        let (audio, _cmds, _events) = AudioHandle::detached();
        c.set_audio(audio);
        let apple = || EngineInfo {
            name: "apple".into(),
            on_device: true,
            locales: vec!["en-US".into()],
        };
        c.on_from_audio(
            FromAudio::Engines {
                stt: apple(),
                tts: apple(),
            },
            t0,
        );
        let read = speech(&mut c);
        if cfg!(target_os = "macos") {
            assert!(
                ["granted", "denied", "not_determined"].contains(&read.as_str().unwrap()),
                "{read}"
            );
        } else {
            assert_eq!(read, "unknown");
        }
        assert_eq!(
            c.store.snapshot().voice.permissions.speech,
            Permission::Unknown,
            "a status read changes no state"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
