//! The admin replies the Integrations and Service screens read (§8.2's
//! Integrations and Service rows), as types a `#[test]` can build.
//!
//! None of this is the mirror. `integrations.status`, `token.list`,
//! `log.tail`, `service.info` and `config.get` are **calls**, not events —
//! the daemon emits nothing that folds into a `Snapshot` for any of them —
//! so the two screens ask, keep the answer in their own view state, and ask
//! again. That is why these are here and not in [`crate::host_state`]: the
//! reducer never sees one, and [`HostState`](crate::HostState) never holds
//! one.
//!
//! Every parser below is deliberately hand-written rather than a
//! `#[derive(Deserialize)]`. A daemon newer than this GUI will answer with
//! a status word, a limit or a field this build has never heard of, and a
//! derive would fail the whole row over it; here an unknown word survives as
//! [`Status::Unknown`] and an unknown field is ignored, which is the rule
//! §4.1 already sets for every other client ("unknown fields are ignored").
use deck_client::ClientError;
use deck_model::{ApiError, Listener};
use serde_json::Value;

/// What `integrations.install`/`uninstall`/`repair` is being asked to do.
///
/// Three verbs and not four: §8.2's **Update** is an `install` over an
/// `outdated` block. The installer is idempotent by content (`Planned::
/// is_noop`), so re-installing the current template *is* the update, and a
/// fourth wire verb would only be a second name for one that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Install,
    Uninstall,
    Repair,
}

impl Verb {
    pub const fn method(self) -> &'static str {
        match self {
            Verb::Install => "integrations.install",
            Verb::Uninstall => "integrations.uninstall",
            Verb::Repair => "integrations.repair",
        }
    }

    /// Whether this verb may write `[hooks.state]` for Codex (§5.2's trust
    /// gate). An uninstall never does, so the sheet offers no tick there.
    pub const fn takes_trust(self) -> bool {
        matches!(self, Verb::Install | Verb::Repair)
    }
}

/// §5.7's status vocabulary, and the one word this build may not know yet.
///
/// `hooks_hijack_risk` is **not** in here, deliberately: §5.7 says
/// `integrations.status` cannot see it (it answers on a blocking pool from
/// its own `Paths`/`Config`, with no handle on the daemon's service state),
/// and the daemon's own `Status::ALL` does not carry it. The Integrations
/// screen still shows it — from the mirror's `service.degraded`/`listener`,
/// which is where §4.1 puts it — and [`Hijack`] is that path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Installed,
    Outdated,
    Absent,
    Conflict,
    PathDrift,
    HooksNotFiring,
    Paused,
    Untrusted,
    Modified,
    /// A word this build has never heard of. Kept verbatim and shown as
    /// itself: a GUI that silently rounded a newer daemon's status down to
    /// "absent" would offer the wrong button.
    Unknown(String),
}

impl Status {
    /// The wire words, in the order §5.7 lists them.
    pub const KNOWN: [Status; 9] = [
        Status::Installed,
        Status::Outdated,
        Status::Absent,
        Status::Conflict,
        Status::PathDrift,
        Status::HooksNotFiring,
        Status::Paused,
        Status::Untrusted,
        Status::Modified,
    ];

    pub fn parse(word: &str) -> Status {
        Status::KNOWN
            .iter()
            .find(|s| s.as_str() == word)
            .cloned()
            .unwrap_or_else(|| Status::Unknown(word.to_string()))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Status::Installed => "installed",
            Status::Outdated => "outdated",
            Status::Absent => "absent",
            Status::Conflict => "conflict",
            Status::PathDrift => "path_drift",
            Status::HooksNotFiring => "hooks_not_firing",
            Status::Paused => "paused",
            Status::Untrusted => "untrusted",
            Status::Modified => "modified",
            Status::Unknown(word) => word,
        }
    }

    /// Is this target carrying deckd's files at all? `absent` is the only
    /// status that says no — every other word describes a block that is on
    /// disk and wrong in some particular way, and all of those can be
    /// removed.
    pub fn installed(&self) -> bool {
        !matches!(self, Status::Absent)
    }

    /// Is this a state the operator has to act on? `installed` is the only
    /// word that is not, and an unknown word counts as one: a status this
    /// build cannot read is not a status it may call healthy.
    pub fn wants_attention(&self) -> bool {
        !matches!(self, Status::Installed)
    }
}

/// The CLI this target integrates with, as the daemon found it on `PATH`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detected {
    pub path: String,
    pub version: String,
}

/// One row of `integrations.status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The wire name: `claude`, `codex`, `codex-desktop`, `gemini`, `claude-desktop`.
    pub target: String,
    pub status: Status,
    pub detected: Option<Detected>,
    pub files: Vec<String>,
    pub backups: Vec<String>,
    /// §8.2's "last event time". The daemon declares the field and, as of
    /// this build, never fills it in — so this is `None` on every row today
    /// and the card says so rather than showing an empty line.
    pub last_event_at: Option<String>,
    pub detail: Option<String>,
    /// What this status cannot tell you (§4.11's honest limits). Shown, not
    /// summarised: they are the daemon's own sentences.
    pub limits: Vec<String>,
}

impl Report {
    /// `{targets:[…], warnings:[…]}` — the rows and the installer's own
    /// warnings about this machine (a translocated bundle, a path outside
    /// `/Applications`).
    pub fn list(result: &Value) -> (Vec<Report>, Vec<String>) {
        let rows = result
            .get("targets")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(Report::parse).collect())
            .unwrap_or_default();
        (rows, strings(result.get("warnings")))
    }

    /// One row. `None` only when it has no `target` at all, which is not a
    /// row about anything.
    pub fn parse(row: &Value) -> Option<Report> {
        let target = row.get("target")?.as_str()?.to_string();
        Some(Report {
            target,
            status: Status::parse(
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            detected: row
                .get("detected")
                .filter(|d| !d.is_null())
                .map(|d| Detected {
                    path: text(d.get("path")).unwrap_or_default(),
                    version: text(d.get("version")).unwrap_or_default(),
                }),
            files: strings(row.get("files")),
            backups: strings(row.get("backups")),
            last_event_at: text(row.get("last_event_at")),
            detail: text(row.get("detail")),
            limits: strings(row.get("limits")),
        })
    }
}

/// One file a [`Plan`] would touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// `create`, `update` or `remove`.
    pub action: String,
}

/// "What this writes" — the `dry_run:true` answer, before anything is
/// written (§5.7).
///
/// `notes` is the load-bearing half and not the decoration: it is where the
/// installer says that it is about to **define `allowedHttpHookUrls`**, and
/// §5.7's own text is that defining that key at any settings level makes
/// every http hook on the machine — the user's, a project's, a plugin's —
/// run only if it matches the merged allowlist. The sheet renders these
/// verbatim and [`Plan::allowlist_notes`] is what finds them again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub target: String,
    pub changes: Vec<Change>,
    pub notes: Vec<String>,
    /// What the installer refused to touch because it is not ours.
    pub conflicts: Vec<String>,
    /// What the operator still has to do (Codex's `/hooks` review).
    pub next_steps: Vec<String>,
}

impl Plan {
    pub fn parse(result: &Value) -> Plan {
        Plan {
            target: text(result.get("target")).unwrap_or_default(),
            changes: result
                .get("changes")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|c| {
                            Some(Change {
                                path: text(c.get("path"))?,
                                action: text(c.get("action")).unwrap_or_else(|| "write".into()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            notes: strings(result.get("notes")),
            conflicts: strings(result.get("conflicts")),
            next_steps: strings(result.get("next_steps")),
        }
    }

    /// True when the daemon answered a dry run at all: an `install` that was
    /// actually applied answers a `Receipt` (`changed`/`backups`), which has
    /// no `dry_run` key.
    pub fn is_dry_run(result: &Value) -> bool {
        result.get("dry_run").and_then(Value::as_bool) == Some(true)
    }

    /// The plan's own notes about `allowedHttpHookUrls`, in the installer's
    /// words. Empty when this plan does not touch the key — which is not the
    /// same as "the key is harmless", so the sheet shows its own standing
    /// warning beside these whenever the target writes http hooks.
    pub fn allowlist_notes(&self) -> Vec<&str> {
        self.notes
            .iter()
            .filter(|n| n.contains("allowedHttpHookUrls"))
            .map(String::as_str)
            .collect()
    }

    /// Nothing to write: the file is already exactly what the plan wants.
    pub fn is_noop(&self) -> bool {
        self.changes.is_empty()
    }
}

/// One row of `token.list` (§4.11). The plaintext is **not** here: it exists
/// for one reply and is never stored, listed or re-derivable — see
/// [`Secret`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    pub name: String,
    pub scopes: Vec<String>,
    pub created: Option<String>,
    pub expires: Option<String>,
    /// The managed blocks that embed this token's plaintext (the Claude
    /// hooks embed `hooks`), so Revoke can say what it is about to break.
    pub embedded_in: Vec<String>,
    pub client: Option<String>,
}

impl TokenRow {
    /// `token.list` answers a bare array, not a wrapper object.
    pub fn list(result: &Value) -> Vec<TokenRow> {
        result
            .as_array()
            .map(|rows| rows.iter().filter_map(TokenRow::parse).collect())
            .unwrap_or_default()
    }

    pub fn parse(row: &Value) -> Option<TokenRow> {
        Some(TokenRow {
            name: row.get("name")?.as_str()?.to_string(),
            scopes: strings(row.get("scopes")),
            created: text(row.get("created")),
            expires: text(row.get("expires")),
            embedded_in: strings(row.get("embedded_in")),
            client: text(row.get("client")),
        })
    }
}

/// A token's plaintext, which is shown **once at creation and never again**
/// (§4.11, §8.2) because the daemon keeps only its hash and cannot answer it
/// a second time.
///
/// The type is what keeps that promise rather than the discipline:
///
/// * it does not implement `Clone`, so it cannot be copied into the mirror,
///   into [`HostState`](crate::HostState) or into a second view field;
/// * it does not implement `Display`, `Serialize` or a `Debug` that prints
///   it, so no `format!`, no `to_string()` and no `{:?}` in any log line,
///   toast or panic message can carry it;
/// * [`Secret::reveal`] is the one way to the bytes, and it borrows, so the
///   sheet can draw it and nothing can keep it;
/// * dropping it overwrites the bytes it owns.
///
/// The last one is hygiene and not a guarantee, and saying so is the point:
/// the JSON reply that carried the token was parsed into a buffer this code
/// does not own, and the daemon wrote one too. What the type *does*
/// guarantee is that closing the sheet is the end of this process's copy.
pub struct Secret(String);

impl Secret {
    /// `{token:"…"}` from `token.create` or `token.rotate`. `None` when the
    /// reply carried no token, which is an error the caller shows.
    pub fn from_reply(result: &Value) -> Option<Secret> {
        let token = result.get("token")?.as_str()?;
        (!token.is_empty()).then(|| Secret(token.to_string()))
    }

    /// The plaintext, borrowed for exactly as long as the sheet draws it.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    /// `Secret(32 bytes)` — the length is useful in a test failure and the
    /// value is never printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: the bytes are overwritten with NUL, which is ASCII, so the
        // `String` stays valid UTF-8 for the deallocation that follows; and
        // each write is volatile, so the compiler may not elide a store to
        // memory it can prove is never read again.
        unsafe {
            for byte in self.0.as_mut_vec() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

/// `log.tail{lines, level}` → `{file, lines}` (§4.11).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    /// The file the lines came from; `None` when the daemon has written no
    /// log yet.
    pub file: Option<String>,
    pub lines: Vec<String>,
}

impl Tail {
    pub fn parse(result: &Value) -> Tail {
        Tail {
            file: text(result.get("file")),
            lines: strings(result.get("lines")),
        }
    }
}

/// `service.info` (§4.1's `/v1/info` plus the status fields).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub api: Option<String>,
    pub daemon: Option<String>,
    pub platform: Option<String>,
    pub uds: Option<String>,
    /// `None` while the port is somebody else's: §4.1 makes the daemon drop
    /// it from `info` rather than name a port it does not hold.
    pub port: Option<u16>,
    pub degraded: Option<String>,
    pub listener: Option<Listener>,
    pub sessions: Option<u64>,
}

impl Info {
    pub fn parse(result: &Value) -> Info {
        Info {
            api: text(result.get("api")),
            daemon: text(result.get("daemon")),
            platform: text(result.get("platform")),
            uds: text(result.get("uds")),
            port: result
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok()),
            degraded: text(result.get("degraded")),
            listener: result
                .get("listener")
                .filter(|l| !l.is_null())
                .and_then(|l| serde_json::from_value(l.clone()).ok()),
            sessions: result.get("sessions").and_then(Value::as_u64),
        }
    }
}

/// The `[api]` half of `config.get` — the Service screen's port, its allowed
/// origins and the scope ceiling a UDS peer may ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiConfig {
    /// The port `[api] port` names. `None` when it names none, and when it
    /// names `0`: any free port, which the service picks when it starts and
    /// which only `service.info` (the port it holds) can say. A real service
    /// answers `0` whenever its config asks for any port (the gate's own
    /// does), and a page that printed it would name a port nothing listens
    /// on.
    pub port: Option<u16>,
    pub allowed_origins: Vec<String>,
    pub uds_default_scope: Option<String>,
    pub uds_max_scope: Option<String>,
    pub max_held_hooks: Option<u64>,
    /// `[hooks] enabled`, which is not `[api]` at all but comes out of the
    /// same one call. The Integrations page's pause is what needs it (hooks
    /// turned off are a paused source), and a second `config.get` to read
    /// one boolean would be a second answer that could disagree with this
    /// one.
    pub hooks_enabled: Option<bool>,
}

impl ApiConfig {
    pub fn parse(result: &Value) -> ApiConfig {
        let api = result.get("api").unwrap_or(&Value::Null);
        ApiConfig {
            hooks_enabled: result
                .get("hooks")
                .and_then(|h| h.get("enabled"))
                .and_then(Value::as_bool),
            port: api
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .filter(|port| *port != 0),
            allowed_origins: strings(api.get("allowed_origins")),
            uds_default_scope: text(api.get("uds_default_scope")),
            uds_max_scope: text(api.get("uds_max_scope")),
            max_held_hooks: api.get("max_held_hooks").and_then(Value::as_u64),
        }
    }
}

/// §4.1's port-degrade state, which is the Integrations screen's
/// `hooks_hijack_risk` (contract §4) and the Service screen's banner.
///
/// It comes off the **mirror** — `service.degraded{reason, port, listener}`
/// folds into `Snapshot::service` — and never off `integrations.status`,
/// which §5.7 says cannot see it. Reading it from one place is what stops
/// the two screens disagreeing about whose port it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hijack {
    pub port: u16,
    pub pid: Option<u64>,
    pub command: Option<String>,
}

impl Hijack {
    /// `Some` only while the daemon says a **stranger answers on the port**.
    /// `port_in_use` alone is not this: a bind that lost to a process which
    /// answers nothing still fails the hooks closed, and §4.1's fail-open
    /// hold is what makes the hijack case the dangerous one.
    pub fn of(service: &deck_model::ServiceState) -> Option<Hijack> {
        (service.degraded.as_deref() == Some(HIJACK_RISK)).then(|| Hijack {
            port: service.port,
            pid: service.listener.as_ref().map(|l| l.pid),
            command: service.listener.as_ref().and_then(|l| l.command.clone()),
        })
    }
}

/// §4.1's reason word for "a stranger answers on our port".
pub const HIJACK_RISK: &str = "hooks_hijack_risk";

/// §4.1's reason word for "the bind failed".
pub const PORT_IN_USE: &str = "port_in_use";

/// Whether `reason` is one of §4.1's two port-degrade words: another program
/// holds the TCP door's port ([`PORT_IN_USE`]), or holds it and answers on it
/// ([`HIJACK_RISK`]).
///
/// Both are about the local API alone. The deck, the Claude Code plugin's
/// hooks and every connected app reach the service over its socket, so a
/// lost port takes nothing of theirs away: the window says it on the Service
/// page's Local API row, and the attention list leaves it out
/// ([`crate::attention::attention`]).
pub fn port_clash(reason: &str) -> bool {
    reason == PORT_IN_USE || reason == HIJACK_RISK
}

/// Did the daemon answer `-32601`? Week 6 ships the GUI against three
/// methods that are being added in parallel (contract §3), and a screen
/// that cannot tell "this daemon is older than this app" from "the call
/// failed" has to guess — so it is asked here once and named.
pub fn method_missing(error: &ClientError) -> bool {
    error
        .api_error()
        .is_some_and(|e| e.code == ApiError::METHOD_NOT_FOUND)
}

/// A `Vec<String>` from a JSON array, dropping anything that is not a
/// string. An absent or null field is an empty list and never an error.
fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A non-empty string field. `null`, absent and `""` are all `None`: a card
/// showing an empty line has said something it does not know.
fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_status_vocabulary_is_the_wires_and_a_word_this_build_does_not_know_survives_as_itself() {
        for status in Status::KNOWN {
            let word = status.as_str().to_string();
            assert_eq!(Status::parse(&word), status, "{word} did not round-trip");
            assert!(word.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        assert_eq!(Status::KNOWN.len(), 9, "§5.7 lists nine words");
        // §5.7: `hooks_hijack_risk` is a port-degrade state and cannot come
        // out of `integrations.status` at all. It must not be mistaken for
        // one of its words.
        assert_eq!(
            Status::parse(HIJACK_RISK),
            Status::Unknown(HIJACK_RISK.into())
        );
        let newer = Status::parse("some_future_word");
        assert_eq!(newer.as_str(), "some_future_word");
        assert!(
            newer.wants_attention() && newer.installed(),
            "a word this build cannot read is never quietly healthy"
        );
        assert!(!Status::Installed.wants_attention());
        assert!(!Status::Absent.installed());
    }

    #[test]
    fn only_the_two_port_words_are_a_clash_on_the_local_apis_port() {
        assert!(port_clash(PORT_IN_USE));
        assert!(port_clash(HIJACK_RISK));
        for other in ["shutdown", "disk_full", "", "PORT_IN_USE", "port_in_use "] {
            assert!(!port_clash(other), "{other:?}");
        }
    }

    #[test]
    fn a_status_row_keeps_every_field_the_card_shows_and_tolerates_the_ones_it_lacks() {
        let reply = json!({
            "targets": [
                {
                    "target": "claude",
                    "status": "outdated",
                    "detected": {"path": "/opt/homebrew/bin/claude", "version": "2.1.0"},
                    "files": ["/Users/t/.claude/settings.json"],
                    "backups": ["/Users/t/Library/.../backups/settings.json.1764.bak"],
                    "detail": "the managed block is from 0.0.9",
                    "limits": ["settings.json is the lowest-precedence level"],
                    "a_field_from_a_newer_daemon": 7
                },
                {"target": "gemini", "status": "absent"},
                {"status": "installed"}
            ],
            "warnings": ["this bundle is outside /Applications"]
        });
        let (rows, warnings) = Report::list(&reply);
        assert_eq!(rows.len(), 2, "a row with no target is not a row");
        assert_eq!(warnings, vec!["this bundle is outside /Applications"]);
        let claude = &rows[0];
        assert_eq!(claude.status, Status::Outdated);
        assert_eq!(
            claude.detected.as_ref().map(|d| d.version.as_str()),
            Some("2.1.0")
        );
        assert_eq!(claude.files.len(), 1);
        assert_eq!(claude.backups.len(), 1);
        assert_eq!(claude.limits.len(), 1);
        // Never filled in by this daemon: the card has to say so rather than
        // draw an empty line.
        assert_eq!(claude.last_event_at, None);
        let icon = &rows[1];
        assert_eq!(icon.status, Status::Absent);
        assert!(icon.detected.is_none() && icon.files.is_empty());
        // Nothing at all is still a list and never an error.
        assert_eq!(Report::list(&Value::Null), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_dry_run_is_recognisable_and_keeps_the_allowed_http_hook_urls_note_verbatim() {
        let note = "ADDS allowedHttpHookUrls, which did not exist: once this key is defined at \
                    any settings level, EVERY http hook on this machine — yours, a project's, a \
                    plugin's — runs only if its URL matches the merged allowlist";
        let reply = json!({
            "target": "claude",
            "dry_run": true,
            "changes": [
                {"path": "/Users/t/.claude/settings.json", "action": "update"},
                {"path": "/Users/t/.claude/x.json"}
            ],
            "notes": [note, "writes the SessionStart hook in exec form"],
            "conflicts": ["a foreign PreToolUse hook is left alone"],
            "next_steps": []
        });
        assert!(Plan::is_dry_run(&reply));
        let plan = Plan::parse(&reply);
        assert_eq!(plan.target, "claude");
        assert_eq!(plan.changes.len(), 2);
        assert_eq!(plan.changes[0].action, "update");
        assert_eq!(
            plan.changes[1].action, "write",
            "a change with no action is still a change"
        );
        assert_eq!(plan.allowlist_notes(), vec![note]);
        assert!(!plan.is_noop());
        // An applied run is a receipt, not a plan.
        let receipt = json!({"target": "claude", "changed": [], "backups": []});
        assert!(!Plan::is_dry_run(&receipt));
        // A plan that writes nothing says so.
        assert!(Plan::parse(&json!({"target": "codex", "changes": []})).is_noop());
        assert!(Plan::parse(&json!({"notes": ["nothing to see"]}))
            .allowlist_notes()
            .is_empty());
    }

    #[test]
    fn the_token_list_is_the_bare_array_and_carries_no_plaintext_anywhere_in_it() {
        let reply = json!([
            {
                "name": "hooks",
                "scopes": ["hooks:ingest"],
                "created": "2026-09-20T10:00:00.000Z",
                "expires": null,
                "embedded_in": ["~/.claude/settings.json"],
                "client": null
            },
            {"name": "script", "scopes": ["observe", "operate"]},
            {"scopes": ["observe"]}
        ]);
        let rows = TokenRow::list(&reply);
        assert_eq!(rows.len(), 2, "a row with no name is not a token");
        assert_eq!(rows[0].name, "hooks");
        assert_eq!(rows[0].scopes, vec!["hooks:ingest"]);
        assert_eq!(rows[0].embedded_in, vec!["~/.claude/settings.json"]);
        assert_eq!(rows[0].expires, None, "null is not an expiry");
        assert_eq!(rows[1].created, None);
        // §4.11: the daemon keeps a hash. Nothing a list can answer is a
        // plaintext, and the type has no field that could hold one.
        let rendered = format!("{rows:?}");
        assert!(!rendered.contains("token\":"), "{rendered}");
        assert!(TokenRow::list(&json!({"tokens": []})).is_empty());
    }

    #[test]
    fn a_created_tokens_plaintext_can_be_shown_once_and_never_printed_or_copied() {
        const PLAINTEXT: &str = "owt_live_0123456789abcdef";
        let secret = Secret::from_reply(&json!({"token": PLAINTEXT})).expect("a created token");
        assert_eq!(secret.reveal(), PLAINTEXT);
        assert_eq!(secret.len(), PLAINTEXT.len());
        assert!(!secret.is_empty());
        // The one thing that must never happen: the value in a formatted
        // string. Every log line, toast and panic message goes through one.
        let debug = format!("{secret:?}");
        assert!(!debug.contains(PLAINTEXT), "{debug}");
        assert_eq!(debug, format!("Secret({} bytes)", PLAINTEXT.len()));
        // A reply with no token is not a secret, and an empty one is not
        // either: showing "" once is not showing a token.
        assert!(Secret::from_reply(&json!({"revoked": "hooks"})).is_none());
        assert!(Secret::from_reply(&json!({"token": ""})).is_none());
        assert!(Secret::from_reply(&Value::Null).is_none());
    }

    #[test]
    fn the_log_tail_the_service_info_and_the_api_config_read_what_the_daemon_answers() {
        let tail = Tail::parse(&json!({
            "file": "/tmp/d/logs/deckd.log.2026-09-22",
            "lines": ["INFO one", "WARN two", 7]
        }));
        assert_eq!(tail.lines, vec!["INFO one", "WARN two"]);
        assert!(tail.file.is_some());
        // No log written yet: `{file:null, lines:[]}` is not an error.
        let empty = Tail::parse(&json!({"file": null, "lines": []}));
        assert_eq!(empty, Tail::default());

        let info = Info::parse(&json!({
            "api": "1", "daemon": "0.1.0", "platform": "macos",
            "uds": "/tmp/d/deckd.sock", "port": 47201,
            "degraded": null, "listener": null, "sessions": 3
        }));
        assert_eq!(info.port, Some(47201));
        assert_eq!(info.sessions, Some(3));
        assert_eq!(info.degraded, None);
        // §4.1: while the port is somebody else's the daemon answers no port
        // at all rather than one it does not hold.
        let held = Info::parse(&json!({
            "port": null,
            "degraded": HIJACK_RISK,
            "listener": {"pid": 812, "command": "node /usr/local/bin/other"}
        }));
        assert_eq!(held.port, None);
        assert_eq!(held.degraded.as_deref(), Some(HIJACK_RISK));
        assert_eq!(held.listener.map(|l| l.pid), Some(812));

        let config = ApiConfig::parse(&json!({
            "api": {
                "port": 47201,
                "allowed_origins": ["http://localhost:5173"],
                "uds_default_scope": "observe",
                "uds_max_scope": "admin",
                "max_held_hooks": 64
            },
            "hooks": {"enabled": true, "claude.hold_s": 130},
            "trust": {}
        }));
        assert_eq!(config.port, Some(47201));
        assert_eq!(config.allowed_origins, vec!["http://localhost:5173"]);
        assert_eq!(config.max_held_hooks, Some(64));
        // The pause's `hooks.enabled`, off the same one call.
        assert_eq!(config.hooks_enabled, Some(true));
        assert_eq!(ApiConfig::parse(&Value::Null), ApiConfig::default());
        // `port = 0` asks for any free port: not a port anybody can call, so
        // none is read (the one the service holds is `service.info`'s).
        let any = ApiConfig::parse(&json!({"api": {"port": 0, "tcp": true}}));
        assert_eq!(any.port, None);
        // A daemon that answered no `[hooks]` table leaves it unread rather
        // than claiming hooks are on.
        assert_eq!(
            ApiConfig::parse(&json!({"api": {"port": 47201}})).hooks_enabled,
            None
        );
    }

    #[test]
    fn the_hijack_comes_off_the_mirror_and_only_when_a_stranger_answers() {
        let mut service = deck_model::ServiceState {
            port: 47201,
            ..deck_model::ServiceState::default()
        };
        assert_eq!(Hijack::of(&service), None);
        // A bind that simply failed is not a hijack: nothing answers, so
        // §4.1's fail-open hold is not sending anything to a stranger.
        service.degraded = Some(PORT_IN_USE.into());
        service.listener = Some(Listener {
            pid: 812,
            command: Some("node /usr/local/bin/other".into()),
        });
        assert_eq!(Hijack::of(&service), None);
        service.degraded = Some(HIJACK_RISK.into());
        assert_eq!(
            Hijack::of(&service),
            Some(Hijack {
                port: 47201,
                pid: Some(812),
                command: Some("node /usr/local/bin/other".into()),
            })
        );
        // `lsof` told us nothing: the port is still somebody's and the card
        // still has to say so.
        service.listener = None;
        assert_eq!(
            Hijack::of(&service),
            Some(Hijack {
                port: 47201,
                pid: None,
                command: None
            })
        );
    }

    #[test]
    fn a_method_this_daemon_does_not_have_is_told_apart_from_a_call_that_failed() {
        let missing = ClientError::Rpc {
            code: ApiError::METHOD_NOT_FOUND,
            message: "method not found".into(),
            data: Some(json!({"method": "config.set"})),
        };
        assert!(method_missing(&missing));
        for other in [
            ClientError::Rpc {
                code: ApiError::FORBIDDEN,
                message: "forbidden".into(),
                data: None,
            },
            ClientError::Rpc {
                code: ApiError::INVALID_PARAMS,
                message: "invalid params".into(),
                data: None,
            },
            ClientError::Disconnected,
        ] {
            assert!(!method_missing(&other), "{other:?}");
        }
    }

    #[test]
    fn the_three_integration_verbs_are_the_daemons_and_only_two_carry_the_codex_trust_opt_in() {
        assert_eq!(Verb::Install.method(), "integrations.install");
        assert_eq!(Verb::Uninstall.method(), "integrations.uninstall");
        assert_eq!(Verb::Repair.method(), "integrations.repair");
        assert!(Verb::Install.takes_trust() && Verb::Repair.takes_trust());
        assert!(
            !Verb::Uninstall.takes_trust(),
            "removing a hook never records a hash for it"
        );
    }
}
