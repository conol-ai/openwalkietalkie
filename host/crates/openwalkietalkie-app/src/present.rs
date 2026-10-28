//! Every string and tone the window shows, as a pure function of the mirror
//! and of the admin replies (§8.2's Deck, Integrations and Service rows).
//!
//! Nothing in here touches `gpui`, and nothing in here decides anything: the
//! screens above it (`deck.rs`, `integrations.rs`, `service.rs`) only lay
//! these answers out, and every button they draw is a
//! [`UiAction`](deck_gui_core::UiAction). A function that returns a string
//! cannot answer a card, install a hook or stop a service.
//!
//! Week 6 added the second and third screens' words below the Deck's. They
//! are here and not in `deck-gui-core` for the same reason the Deck's are:
//! `deck-gui-core` holds the *shapes* the daemon answers with
//! ([`deck_gui_core::admin`]) and this module holds the sentences the window
//! reads them out in, which is the half a designer edits.
//!
//! Two rules it exists to keep. The first: the deck and the window must agree.
//! An operator looking at a red dot on the device and an amber one in the
//! window has been told two different things about one agent, so
//! [`state_tone`] and [`age`] are ports of the firmware's own
//! `ui::theme::agent_state` and `ui::kit::age_label` (`src/ui/theme.rs:73`,
//! `src/ui/kit.rs:482`) rather than a second opinion. The second: nothing
//! here decides anything. A function that returns a string cannot answer a
//! card; the decisions are [`UiAction`](deck_gui_core::UiAction)s in
//! `deck.rs`, and the two that need judgement ([`allow_choice`],
//! [`deny_choice`]) are answered with `deck_model::guard`'s own vocabulary,
//! so the GUI's idea of "allow" is the daemon's.
use deck_gui_core::admin::{ApiConfig, Change, Detected, Hijack, Info, Status, TokenRow, Verb};
use deck_gui_core::voice::{Engine, Prompt, TestReport};
use deck_gui_core::{Level, OrderEntry, Transcript};
use deck_model::{
    guard, Agent, AgentState, Caps, Card, CardStatus, Deliver, Kind, Link, Origin, OriginKind,
    Permission, Principal, Route, SessionInfo, SessionState, Slot, Snapshot, Source, Tone, Ts,
    VoiceTransport, DISCONNECTED,
};
use std::time::Duration;

/// Custom Channels still require Claude's interactive preview opt-in.
pub const CLAUDE_CHANNEL_COMMAND: &str = "claude --dangerously-load-development-channels plugin:openwalkietalkie@openwalkietalkie";
pub const CLAUDE_CHANNEL_DOCS: &str = "https://code.claude.com/docs/en/channels-reference#test-during-the-research-preview";

/// The slot badge of an agent the daemon has not put on the deck (§8.2).
pub const NOT_ON_DECK: &str = "not on deck";

/// What a content field reads as when the reader's scope nulled it (§4.3).
/// The GUI asks for `observe:content`, so this is only ever seen on an
/// object the daemon redacted for another reason.
pub const REDACTED: &str = "(redacted)";

/// The wall clock, for the ages and countdowns below. The reducer may not
/// read a clock (`deck-model` is pure and `HostState` folds what it is
/// given); a view drawing "2m" has no other way to know what minute it is.
pub fn now() -> Ts {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    Ts::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

/// The roster dot (§8.2), and the firmware's rule for it: a stale agent is
/// gray whatever its state, because a dot that still says "working" for a
/// session that has gone is the one thing worse than no dot.
pub const fn state_tone(state: AgentState, stale: bool) -> Tone {
    if stale {
        return Tone::Gray;
    }
    match state {
        AgentState::Working => Tone::Blue,
        AgentState::Question => Tone::Red,
        AgentState::Ready => Tone::Green,
        AgentState::Error => Tone::Amber,
        AgentState::Idle => Tone::Gray,
    }
}

/// An agent whose session went away. The `Snapshot` carries no `stale` flag
/// (the firmware keeps its own); what the daemon does on the orphan path is
/// take the title away and put [`DISCONNECTED`] there instead
/// (`roster.rs:359`), so that title *is* the flag on this side.
pub fn stale(agent: &Agent) -> bool {
    agent.title.as_deref() == Some(DISCONNECTED)
}

/// The state, spelled the way the deck spells it.
pub const fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "Working",
        AgentState::Question => "Question",
        AgentState::Ready => "Ready",
        AgentState::Error => "Error",
        AgentState::Idle => "Idle",
    }
}

/// The kind icon (§8.2). A glyph and not an SVG: the bundle's `assets/lucide`
/// is the device's alpha-mask set, there is no vector icon for "Claude", and
/// a letter in a chip is legible at 11 px where a traced logo is not.
pub const fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Claude => "\u{2733}",
        Kind::Codex => "\u{25ce}",
        Kind::Gemini => "\u{2726}",
        Kind::Other => "\u{25aa}",
        Kind::Conol => "\u{25a6}",
    }
}

pub const fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Claude => "Claude",
        Kind::Codex => "Codex",
        Kind::Gemini => "Gemini",
        Kind::Other => "Agent",
        Kind::Conol => "Conol",
    }
}

/// `Slot 3`, or "not on deck" for an agent the slot policy left off (§8.2).
pub fn slot_badge(slot: Option<Slot>) -> String {
    match slot {
        Some(slot) => format!("Slot {slot}"),
        None => NOT_ON_DECK.to_string(),
    }
}

/// The live title (§8.2), which is content and may have been nulled.
pub fn live_title(agent: &Agent) -> &str {
    match agent.title.as_deref() {
        Some(title) => title,
        None if agent.redacted => REDACTED,
        None => "",
    }
}

/// `now`, `2m`, `3h`, `12d` — the firmware's granularity, so a row that
/// reads "2m" on the deck reads "2m" here (`src/ui/kit.rs:482`). A stamp
/// from the future (a clock that stepped back, a `since` the daemon rounded
/// up) is "now" and never a negative age.
pub fn age(now: Ts, since: Ts) -> String {
    let minutes = now.millis().saturating_sub(since.millis()) / 60_000;
    match minutes {
        0 => "now".to_string(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// The state age of a row: how long the agent has been in this state. `None`
/// for an idle or stale agent, where the deck shows nothing either — the age
/// of "nothing is happening" is not news.
pub fn state_age(agent: &Agent, now: Ts) -> Option<String> {
    if agent.state == AgentState::Idle || stale(agent) {
        return None;
    }
    Some(age(now, agent.since))
}

/// The owner/source badge and §8.2's four words for it. `source` says which
/// door the agent came in by; for the API door the principal behind it is
/// what distinguishes a third party reporting itself (§4.15) from a token
/// running a script.
pub fn owner_source(agent: &Agent) -> &'static str {
    match (agent.source, &agent.owner.principal) {
        (Source::Hook, _) => "hook",
        (Source::Mcp, _) => "mcp",
        (_, Principal::Adapter(deck_model::AdapterKind::Mcp)) => "mcp",
        (Source::Api, Principal::Token(_)) => "script",
        (Source::Api, _) => "self-reported",
    }
}

/// The card's own name: `#17.1/2` — card 17, page 1 of 2 (§8.2). The deck
/// pages a card the operator's thumb cannot fit on one screen; the window
/// shows every choice at once and still names the page, because that is the
/// `#` an operator reads out over a call.
pub fn card_ref(card: &Card) -> String {
    if card.page.count > 1 {
        format!(
            "#{}.{}/{}",
            card.id,
            card.page.index.saturating_add(1),
            card.page.count
        )
    } else {
        format!("#{}", card.id)
    }
}

/// The card text, which is content and may have been nulled (§4.3).
pub fn card_text(card: &Card) -> &str {
    match card.text.as_deref() {
        Some(text) => text,
        None if card.redacted => REDACTED,
        None => "",
    }
}

/// One choice, with the recommendation marked. `choice_map` is B10's
/// shown → sent mapping: when the owner offered more options than the deck
/// can draw, the index the *agent* knows is not the index of this row, and
/// an operator reading a log next to this window needs both.
pub fn choice_label(card: &Card, shown: u8) -> String {
    let index = usize::from(shown);
    let label = card
        .choices
        .get(index)
        .map(|c| c.label.as_str())
        .unwrap_or(REDACTED);
    let mut out = format!("{}. {label}", shown.saturating_add(1));
    if card.rec == Some(shown) {
        out.push_str(" \u{b7} recommended");
    }
    if let Some(sent) = card.choice_map.get(index) {
        if *sent != shown {
            out.push_str(&format!(" \u{b7} sent as {sent}"));
        }
    }
    out
}

/// How this card's answer gets home (§4.8), for the detail pane's route line.
pub const fn route(deliver: Deliver) -> &'static str {
    match deliver {
        Deliver::Hold => "hold",
        Deliver::Push => "push",
        Deliver::Poll => "poll",
        Deliver::Focus => "focus",
        Deliver::None => "none",
    }
}

/// The hold countdown (§8.2): how long before the parked hook is released
/// with no decision. `None` when the card does not expire.
pub fn hold_countdown(card: &Card, now: Ts) -> Option<String> {
    let expires = card.expires?;
    let left = expires.millis().saturating_sub(now.millis()) / 1000;
    if expires.millis() <= now.millis() {
        return Some("expired".to_string());
    }
    Some(if left >= 60 {
        format!("{}m {:02}s left", left / 60, left % 60)
    } else {
        format!("{left}s left")
    })
}

/// The `failed` reason (§8.2), `None` on every other status.
pub fn failed_reason(card: &Card) -> Option<&str> {
    match &card.status {
        CardStatus::Failed { reason } => Some(reason.as_str()),
        _ => None,
    }
}

/// `degraded` with its "focused at" (§8.2): the moment ENTER took the
/// operator to the terminal, which is the only thing that happened to a hold
/// that was never answered. `Some(None)` is degraded and not yet focused.
pub fn degraded_at(card: &Card) -> Option<Option<String>> {
    match &card.status {
        CardStatus::Degraded { focused_at } => Some(focused_at.map(clock)),
        _ => None,
    }
}

/// The time of day of a stamp, `19:42:03Z`. UTC and labelled as such: `Ts`
/// renders RFC 3339 in UTC and this workspace carries no timezone database,
/// so a local time here would be a guess dressed as a fact.
pub fn clock(ts: Ts) -> String {
    let rendered = ts.to_string();
    match (rendered.find('T'), rendered.find('.')) {
        (Some(t), Some(dot)) if t + 1 < dot => format!("{}Z", &rendered[t + 1..dot]),
        _ => rendered,
    }
}

/// What the answering operator chose, for a card mid-flight.
pub fn answering(card: &Card) -> Option<String> {
    let CardStatus::Answering { choice, text } = &card.status else {
        return None;
    };
    if let Some(text) = text {
        return Some(format!("\u{201c}{text}\u{201d}"));
    }
    let choice = (*choice)?;
    Some(
        card.choices
            .get(usize::from(choice))
            .map(|c| c.label.clone())
            .unwrap_or_else(|| format!("choice {choice}")),
    )
}

/// The `custom`-origin badge (§8.2). Only `custom` gets one: every other
/// origin is a built-in adapter's, and a badge on all of them is noise —
/// `custom` is the one that says "a token or a script wrote this card", and
/// on the deck it is the `\u{bb} ` prefix.
pub const fn custom_badge(origin: &Origin) -> Option<&'static str> {
    match origin.kind {
        OriginKind::Custom => Some("custom"),
        _ => None,
    }
}

/// The origin, for the detail pane's own line (`permission \u{b7} Bash`).
pub fn origin_line(origin: &Origin) -> String {
    let kind = match origin.kind {
        OriginKind::Permission => "permission",
        OriginKind::Question => "question",
        OriginKind::Elicitation => "elicitation",
        OriginKind::Custom => "custom",
        OriginKind::Ready => "ready",
    };
    let mut out = kind.to_string();
    if let Some(tool) = &origin.tool {
        out.push_str(" \u{b7} ");
        out.push_str(tool);
    }
    if let Some(group) = origin.group {
        out.push_str(&format!(
            " \u{b7} {} of {}",
            group.index.saturating_add(1),
            group.count
        ));
    }
    out
}

/// Where the agent runs (§8.2's "source (cwd, pid, terminal)"). Content, so
/// every part of it can be absent.
pub fn source_line(session: &SessionInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(cwd) = &session.cwd {
        parts.push(cwd.clone());
    }
    if let Some(pid) = session.pid {
        parts.push(match session.pid_kind {
            Some(kind) => format!("pid {pid} ({})", pid_kind(kind)),
            None => format!("pid {pid}"),
        });
    }
    if let Some(terminal) = &session.terminal {
        let mut term: Vec<&str> = Vec::new();
        if let Some(program) = &terminal.program {
            term.push(program.as_str());
        }
        if let Some(tty) = &terminal.tty {
            term.push(tty.as_str());
        }
        if let Some(pane) = &terminal.tmux_pane {
            term.push(pane.as_str());
        }
        if !term.is_empty() {
            parts.push(term.join(" "));
        }
    }
    if parts.is_empty() {
        return "no source reported".to_string();
    }
    parts.join(" \u{b7} ")
}

const fn pid_kind(kind: deck_model::PidKind) -> &'static str {
    match kind {
        deck_model::PidKind::Cli => "cli",
        deck_model::PidKind::Shell => "shell",
        deck_model::PidKind::Unknown => "unknown",
    }
}

/// What this agent can be asked to do (§8.2's caps). `focus: false` is worth
/// a word of its own: §4.15 lets an agent register with `caps:{answer,order}`
/// alone, and an operator whose Focus button does nothing should be able to
/// see why before clicking it.
pub fn caps_line(caps: &Caps) -> String {
    let answer = match caps.answer {
        deck_model::AnswerCap::Hold => "hold",
        deck_model::AnswerCap::Push => "push",
        deck_model::AnswerCap::Poll => "poll",
        deck_model::AnswerCap::None => "none",
    };
    let order = match caps.order {
        deck_model::OrderCap::Inject => "inject",
        deck_model::OrderCap::Push => "push",
        deck_model::OrderCap::Poll => "poll",
        deck_model::OrderCap::None => "none",
    };
    format!(
        "answer {answer} \u{b7} order {order} \u{b7} {}",
        if caps.focus { "focus" } else { "no focus" }
    )
}

/// One line of the order history (§8.2): "the exact string typed or pushed,
/// shown before an `inject` executes". The daemon writes
/// `order.dispatched{text}` *before* it types, so this is the string that is
/// about to be typed and not a report of what was.
pub fn order_line(entry: &OrderEntry) -> String {
    let mut out = match &entry.text {
        Some(text) => format!("\u{201c}{text}\u{201d}"),
        // `order.failed` carries no text (§4.12): an order refused before it
        // was ever dispatched never had one to keep.
        None => "(no text)".to_string(),
    };
    if let Some(via) = &entry.via {
        out.push_str(&format!(" \u{b7} {via}"));
    }
    if let Some(reason) = &entry.reason {
        out.push_str(&format!(" \u{b7} failed: {reason}"));
    }
    out
}

/// Whether Copy has anything to copy (§8.2's "Copy on `order.failed`").
pub fn copyable(entry: &OrderEntry) -> Option<&str> {
    entry.reason.as_ref()?;
    entry.text.as_deref()
}

/// Whether the detail pane offers **Retry** on a `failed` card. §4.8: out of
/// `failed` only a `push`/`poll` card can go back to `answering`; for a
/// `hold` card the parked response is already gone (Claude timed out or
/// closed), so Retry would be answered `-32005 bad_state{reason:"hold_gone"}`
/// and only Release is honest.
pub fn retry_offered(card: &Card) -> bool {
    matches!(card.status, CardStatus::Failed { .. })
        && matches!(card.deliver, Deliver::Push | Deliver::Poll)
}

/// Whether the detail pane offers **Release to terminal** (§8.2). Every
/// `failed` card does: releasing is `failed \u{2192} degraded`, the `\u{2328}`
/// card re-pushed under the same id (§4.8).
pub fn release_offered(card: &Card) -> bool {
    matches!(card.status, CardStatus::Failed { .. })
}

/// The choice the confirmation sheet's **Allow** re-issues (§4.2).
/// `op.confirm_requested` carries `{agent, card?, order?, principal}` and no
/// choice (§4.12), so the dialog has to find the allow itself: the card's own
/// recommendation when that is an allow, else the first allow label, judged
/// by `deck_model::guard`'s vocabulary and not by a second list kept here.
/// `None` when the card offers no allow at all, and then the dialog offers no
/// Allow button either.
pub fn allow_choice(card: &Card) -> Option<u8> {
    let is_allow = |n: u8| {
        card.choices
            .get(usize::from(n))
            .is_some_and(|c| guard::label_is_allow(&c.label))
    };
    if card.rec.is_some_and(is_allow) {
        return card.rec;
    }
    (0..card.choices.len())
        .map(|n| u8::try_from(n).unwrap_or(u8::MAX))
        .find(|n| is_allow(*n))
}

/// The choice a **dismissed** sheet answers with. Dismissing must deny and
/// may never allow (§4.2), and the strongest denial available is the card's
/// own Deny: the first choice whose label reads as a deny word.
///
/// Failing that, a **permission** card falls back to the first choice that is
/// not an allow. That is safe there and only there: `origin.kind:"permission"`
/// is reserved for the built-in adapters (§4.6), whose choices are a CLI's own
/// allow/deny prompt, so "Later" or "Stop" withholds the permission exactly
/// as "Deny" would. An **elicitation**'s choices are the caller's own data
/// (`["production", "staging"]`), where the first non-allow is not a refusal
/// but a decision the operator never took — so there the honest answer is to
/// send nothing, which still refuses: the guarded call was already `-32002`
/// and the dialog is only the offer to re-issue it.
pub fn deny_choice(card: &Card) -> Option<u8> {
    let label = |n: u8| card.choices.get(usize::from(n)).map(|c| c.label.as_str());
    let indices: Vec<u8> = (0..card.choices.len())
        .map(|n| u8::try_from(n).unwrap_or(u8::MAX))
        .collect();
    indices
        .iter()
        .copied()
        .find(|n| label(*n).is_some_and(guard::text_is_deny))
        .or_else(|| {
            if card.origin.kind != OriginKind::Permission {
                return None;
            }
            indices
                .iter()
                .copied()
                .find(|n| label(*n).is_some_and(|l| !guard::label_is_allow(l)))
        })
}

/// The group of the sessions that have no working directory.
pub const NO_PROJECT: &str = "No project";

/// One project of the roster: every agent whose session runs in `cwd`.
pub struct Project<'a> {
    /// What the group header reads: the directory's last component, the
    /// word the deck's page header shows for the same sessions
    /// (`deck_model::project_label`); `NO_PROJECT` for sessions without one.
    pub label: String,
    /// The working directory itself; `None` for the unlabelled group.
    pub cwd: Option<String>,
    /// The deck's slots first, in slot order, then every agent the slot
    /// policy could not fit, newest state first so the one that just asked
    /// something is at the top of its project.
    pub agents: Vec<&'a Agent>,
}

/// The roster grouped by project, the way §8.2 draws it and the deck pages
/// it. Groups are by the working directory, not its last component, so two
/// directories of the same name stay apart (the header's tooltip tells them
/// apart); they come in name order, the unlabelled group last.
pub fn projects(snapshot: &Snapshot) -> Vec<Project<'_>> {
    let mut groups: Vec<Project<'_>> = Vec::new();
    for agent in snapshot.agents.values() {
        let cwd = agent
            .session
            .cwd
            .as_deref()
            .map(|cwd| cwd.trim_end_matches('/'))
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_string);
        let (label, cwd) = if let Some(profile) = &agent.companion {
            (profile.group_label.clone(), None)
        } else if agent.kind == Kind::Conol { ("Conol".to_string(), None) } else { match cwd.as_deref().and_then(deck_model::project_label) {
            Some(label) => (label, cwd),
            None => (NO_PROJECT.to_string(), None),
        }};
        match groups.iter_mut().find(|g| g.cwd == cwd && g.agents.first().is_some_and(|a| {
            match (&a.companion, &agent.companion) {
                (Some(left), Some(right)) => left.app_id == right.app_id && left.group_id == right.group_id,
                (None, None) => (a.kind == Kind::Conol) == (agent.kind == Kind::Conol),
                _ => false,
            }
        })) {
            Some(group) => group.agents.push(agent),
            None => groups.push(Project {
                label,
                cwd,
                agents: vec![agent],
            }),
        }
    }
    for group in &mut groups {
        group.agents.sort_by(|a, b| match (a.slot, b.slot) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.since.cmp(&a.since).then_with(|| a.id.cmp(&b.id)),
        });
    }
    groups.sort_by(|a, b| match (a.cwd.is_none(), b.cwd.is_none()) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => a
            .label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.cwd.cmp(&b.cwd)),
    });
    groups
}

/// A row's name under its project's header: the agent's own, or its kind
/// alone when the name is the project's — a hook-registered session is named
/// after its directory, which the header already says. The deck's roster
/// rows follow the same rule.
pub fn row_name(agent: &Agent, project: &Project) -> String {
    if project.cwd.is_some() && agent.name == project.label {
        kind_label(agent.kind).to_string()
    } else {
        agent.name.clone()
    }
}

/// A row's title line under its project's header: the live title, unless it
/// is the project's own name — a session with no prompt title yet is titled
/// after its directory, which the header already says.
pub fn row_title(agent: &Agent, project: &Project) -> String {
    let title = live_title(agent);
    if project.cwd.is_some() && title == project.label {
        String::new()
    } else {
        title.to_string()
    }
}

/// The directory a project header's tooltip shows: the home directory as
/// `~`, the way the log viewer writes it.
pub fn project_path(cwd: &str, home: Option<&str>) -> String {
    match home.map(|home| home.trim_end_matches('/')) {
        Some(home) if !home.is_empty() && cwd == home => "~".to_string(),
        Some(home) if !home.is_empty() && cwd.starts_with(&format!("{home}/")) => {
            format!("~{}", &cwd[home.len()..])
        }
        _ => cwd.to_string(),
    }
}

// ---------------------------------------------------------------------
// The **Integrations** screen (§8.2's Integrations row)
// ---------------------------------------------------------------------

/// The five `integrations.*` targets, in the order §8.2 lists their cards.
///
/// The list is kept here and not taken from the daemon's answer so that a
/// party the daemon did not report still gets a card saying it did not
/// report — an integration that quietly vanished from the screen is the one
/// failure mode this list exists to stop.
pub const TARGETS: [&str; 5] = [
    "claude",
    "codex",
    "codex-desktop",
    "gemini",
    "claude-desktop",
];

/// §8.2's name for a target. The words are ports of the daemon's own
/// `Target::display_name`, not a second set: the card, the CLI's `--help`
/// and the log all say "Claude Code" or none of them do.
pub fn party_name(target: &str) -> &str {
    match target {
        "claude" => "Claude Code",
        "codex" => "Codex CLI",
        "codex-desktop" => "Codex Desktop",
        "gemini" => "Gemini CLI",
        "claude-desktop" => "Claude Desktop",
        other => other,
    }
}

/// One line saying what this target does with this machine, so a card the
/// operator has never installed says what installing it would mean.
pub fn party_what(target: &str) -> &'static str {
    match target {
        "claude" => "http hooks in ~/.claude/settings.json, plus the chained statusLine",
        "codex" => "shared local app-server connection, with command hooks in ~/.codex/hooks.json as fallback",
        "codex-desktop" => "a launcher in ~/Applications that connects Codex Desktop to the shared local app server",
        "gemini" => "command hooks in ~/.gemini/settings.json",
        "claude-desktop" => "the openwalkietalkie-deck stdio entry in claude_desktop_config.json",
        _ => "",
    }
}

/// Whether installing this target can define Claude's `allowedHttpHookUrls`.
/// Only Claude writes **http** hooks, and the key is Claude's (§5.7) — so
/// this is the one card that carries [`ALLOWLIST_NOTE`].
pub fn allowlist_applies(target: &str) -> bool {
    target == "claude"
}

/// What defining `allowedHttpHookUrls` does, in the window's own words.
///
/// It is shown on every Claude plan, including the ones whose notes do not
/// mention the key — because "the key already exists, so we only append"
/// still leaves the operator inside a machine-wide allowlist, and the
/// contract's requirement is that they see this **before** installing, not
/// only on the run that happens to create it.
pub const ALLOWLIST_NOTE: &str = "\u{26a0} allowedHttpHookUrls is a machine-wide switch. Once it \
     is defined at any settings level, EVERY http hook on this machine \u{2014} yours, a \
     project's, a plugin's \u{2014} runs only if its URL matches the merged allowlist. deckd \
     appends its own pattern and never replaces the list, and removes only the exact string it \
     recorded appending.";

/// The §5.7 status word, exactly as the wire spells it.
pub fn status_word(status: &Status) -> &str {
    status.as_str()
}

/// What the §5.7 status word means, in one sentence. These are the words
/// the design defines; an unknown one is reported as unreadable rather than
/// explained, because this build does not know what it means.
pub fn status_help(status: &Status) -> &'static str {
    match status {
        Status::Installed => "deckd's plugin, hook entries, or desktop launcher are in place and current.",
        Status::Outdated => "what is on disk differs from this build, Claude has not registered the plugin yet, or settings.json still carries the pre-plugin hooks; Update rewrites or migrates it.",
        Status::Absent => "nothing of deckd's is installed for this integration.",
        Status::Conflict => "a file deckd must read cannot be parsed, or holds a shape the CLI would reject. deckd never overwrites it.",
        Status::PathDrift => "the hooks name a binary that is not this app's any more; Repair rewrites them.",
        Status::HooksNotFiring => "Codex's own hooks/list reports none of deckd's hooks: they are on disk and dead.",
        Status::Paused => "Codex reports the hooks trusted but switched off in /hooks.",
        Status::Untrusted => "Codex: written but never reviewed, so it does not run. Review it in /hooks, or install with trust.",
        Status::Modified => "Codex: reviewed once, then the path, timeout or matcher changed, so it stopped running silently.",
        Status::Unknown(_) => "this app does not know this word: it is newer than this build. Nothing here can say whether it is healthy.",
    }
}

/// The card's dot. Green only for `installed`; grey for a party that is
/// simply not set up; amber for everything else, including a word this
/// build cannot read.
pub fn status_tone(status: &Status) -> Tone {
    match status {
        Status::Installed => Tone::Green,
        Status::Absent => Tone::Gray,
        _ => Tone::Amber,
    }
}

/// The detected binary and its version (§8.2), or the fact that there is
/// none — which for `claude-desktop` is normal, since it is not a CLI on
/// `PATH`.
pub fn detected_line(target: &str, detected: Option<&Detected>) -> String {
    match detected {
        Some(d) if !d.version.is_empty() => format!("{} \u{b7} {}", d.version, d.path),
        Some(d) => d.path.clone(),
        None if target == "claude-desktop" => {
            "no CLI to detect: this target is a file, not a binary".to_string()
        }
        None if target == "codex-desktop" => {
            "Codex.app was not detected in Applications".to_string()
        }
        None => "not found on PATH".to_string(),
    }
}

/// §8.2's "last event time".
///
/// The daemon declares `last_event_at` on every row and, in this build,
/// never fills it in — so this says which of the two it is instead of
/// drawing an empty line and letting the operator read "never" into it.
pub fn last_event_line(last_event_at: Option<&str>) -> String {
    match last_event_at {
        Some(at) => format!("last hook event {at}"),
        None => "last hook event: not reported by this daemon".to_string(),
    }
}

/// A backup's own name, for the Restore list. The installer writes
/// `backups/<file>.<ts>.bak`, so the file name is the whole label.
pub fn backup_label(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// §4.1's `hooks_hijack_risk`, named for the Integrations screen (contract
/// §4) — who holds the port, and what that does to the hooks.
///
/// It says "may stall" and not "has stalled": the daemon knows a stranger
/// answers on the port, and nothing tells it whether a hook has been sent
/// there yet.
pub fn hijack_line(hijack: &Hijack) -> String {
    let who = match (&hijack.command, hijack.pid) {
        (Some(command), Some(pid)) => format!("pid {pid} ({command})"),
        (Some(command), None) => command.clone(),
        (None, Some(pid)) => format!("pid {pid}"),
        (None, None) => "another process".to_string(),
    };
    format!(
        "Port {} is held by {who}, and it answers. Claude's installed http hooks POST prompts, \
         tool inputs and the hooks bearer token to whoever holds it, and each held hook blocks \
         Claude for its timeout.",
        hijack.port
    )
}

/// The heading of the "What this writes" sheet.
pub fn plan_headline(verb: Verb, target: &str) -> String {
    let what = match verb {
        Verb::Install => "Install",
        Verb::Uninstall => "Remove",
        Verb::Repair => "Repair",
    };
    format!("{what} \u{2014} {}", party_name(target))
}

/// One line of the diff: `update ~/.claude/settings.json`.
pub fn change_line(change: &Change) -> String {
    format!("{} {}", change.action, change.path)
}

/// What a plan that writes nothing says. `install` is idempotent by content,
/// so this is the honest answer for a target that is already exactly right —
/// and it is not an error.
pub const NOTHING_TO_WRITE: &str = "Nothing to write: these files are already what this run \
     would put in them.";

/// §8.2's **Ghostty Automation** card. It is not an `integrations.*` target:
/// nothing is installed for it, and what there is to report is the
/// Automation TCC grant the injector learned by *trying* (§5.5) — which is
/// why `perm.status` answers `unknown` until something has been injected.
pub fn automation_line(ghostty: Option<&str>) -> (&'static str, Tone, &'static str) {
    match ghostty {
        Some("granted") => (
            "granted",
            Tone::Green,
            "an order with `order inject` types into Ghostty.",
        ),
        Some("denied") => (
            "denied",
            Tone::Red,
            "macOS refused this app control of Ghostty. Automation cannot be re-prompted once \
             denied: it has to be ticked in System Settings \u{203a} Privacy & Security \u{203a} \
             Automation.",
        ),
        _ => (
            "unknown",
            Tone::Gray,
            "nothing has tried to inject yet, so macOS has not been asked. The first `inject` \
             order is what raises the prompt.",
        ),
    }
}

/// The `x-apple.systempreferences:` pane a *denied* Automation grant can
/// only be fixed in — the same recourse §7.8 takes for a denied microphone.
pub const AUTOMATION_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation";

// ---------------------------------------------------------------------
// Tokens (§4.11, §8.2)
// ---------------------------------------------------------------------

/// The scopes a token may be created with, as the wire spells them.
pub fn scope_words() -> Vec<String> {
    deck_model::Scope::ALL
        .iter()
        .filter_map(|s| serde_json::to_value(s).ok())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// A token row's scopes, or the fact that it has none — which is a token
/// that can do nothing and is worth seeing as such.
pub fn token_scopes_line(token: &TokenRow) -> String {
    if token.scopes.is_empty() {
        return "no scopes".to_string();
    }
    token.scopes.join(", ")
}

/// When the token was made and when it stops working.
pub fn token_life_line(token: &TokenRow) -> String {
    let created = token
        .created
        .as_deref()
        .map(|at| format!("created {at}"))
        .unwrap_or_else(|| "created at an unrecorded time".to_string());
    match token.expires.as_deref() {
        Some(at) => format!("{created} \u{b7} expires {at}"),
        None => format!("{created} \u{b7} no expiry"),
    }
}

/// What Revoke on this token would break. The `hooks` token's plaintext is
/// embedded in the managed blocks, so revoking it stops the hooks until a
/// `repair` re-embeds the new one — and the operator should read that on
/// the button and not in the log afterwards.
pub fn token_embedded_line(token: &TokenRow) -> Option<String> {
    if token.embedded_in.is_empty() {
        return None;
    }
    Some(format!(
        "embedded in {} \u{2014} revoking it stops those hooks until Repair re-embeds a new one",
        token.embedded_in.join(", ")
    ))
}

/// The sentence over the one-time plaintext (§4.11).
pub const TOKEN_SHOWN_ONCE: &str = "Copy this now. The daemon keeps only a hash of it, so this \
     window is the only place it will ever exist \u{2014} closing this dialog is the end of it.";

/// What a created or rotated token is called in a toast.
///
/// **It names the token and never carries the plaintext.** Every toast goes
/// through a `String`, and a `String` that has held a secret is one
/// screen-share away from being read out loud; the value only ever leaves
/// [`Secret::reveal`](deck_gui_core::Secret::reveal), inside its dialog.
pub fn token_made_toast(verb: &str, name: &str) -> String {
    format!("{verb} the token \u{201c}{name}\u{201d}. Copy its value now: it is shown once.")
}

// ---------------------------------------------------------------------
// The **Service** screen (§8.2's Service row)
// ---------------------------------------------------------------------

/// The one-line answer to "is the service up, and which one is it".
pub fn service_headline(connected: bool, info: &Info) -> String {
    if !connected {
        return "Not running \u{2014} nothing is answering on the socket".to_string();
    }
    let daemon = info.daemon.as_deref().unwrap_or("an unknown version");
    match info.sessions {
        Some(1) => format!("Running \u{b7} deckd {daemon} \u{b7} 1 session"),
        Some(n) => format!("Running \u{b7} deckd {daemon} \u{b7} {n} sessions"),
        None => format!("Running \u{b7} deckd {daemon}"),
    }
}

/// The port line. §4.1 makes the daemon answer **no port at all** while the
/// port is somebody else's, so an absent port here is a fact about the
/// machine and not a missing field.
pub fn port_line(info: &Info, config: &ApiConfig) -> String {
    match (info.port, config.port) {
        (Some(port), _) => format!("http://127.0.0.1:{port} \u{b7} listening"),
        (None, Some(configured)) => {
            format!("{configured} is configured and not held: the daemon is on the socket only")
        }
        (None, None) => "no TCP port: the daemon is on the socket only".to_string(),
    }
}

/// §8.2's degraded banner, from the mirror's own `service.degraded`.
pub fn degraded_banner(service: &deck_model::ServiceState) -> Option<String> {
    let reason = service.degraded.as_deref()?;
    Some(match reason {
        deck_gui_core::admin::HIJACK_RISK => {
            format!(
                "Port {} is held by a process that answers, and Claude's http hooks are still \
                 pointed at it. Change the port and re-install the hooks.",
                service.port
            )
        }
        deck_gui_core::admin::PORT_IN_USE => format!(
            "Port {} is already bound, so the daemon is on the socket only. http hooks and curl \
             cannot reach it until the port is free or changed.",
            service.port
        ),
        "shutdown" => "The service is shutting down.".to_string(),
        other => format!("The service is degraded: {other}."),
    })
}

/// §4.1's allowed origins. An empty list is the default and means "no
/// browser origin is allowed", which is a policy and not a gap.
pub fn origins_line(origins: &[String]) -> String {
    if origins.is_empty() {
        return "none \u{2014} a browser Origin header is refused 403 (the default)".to_string();
    }
    origins.join(", ")
}

/// §8.2's `curl` example, for the port the daemon actually holds.
///
/// It is a `POST /v1/rpc` and not a `GET /v1/info`: `/v1/info` is the one
/// unauthenticated route (§4.1), so an example built on it would prove
/// nothing about a token. `deckd ctl` is named beside it because the UDS
/// door is **not** HTTP and `curl --unix-socket` cannot speak to it.
pub fn curl_example(port: Option<u16>) -> String {
    let port = port.unwrap_or(47201);
    format!(
        "curl -sS -X POST http://127.0.0.1:{port}/v1/rpc \\\n  \
         -H 'Authorization: Bearer <token>' \\\n  \
         -H 'Content-Type: application/json' \\\n  \
         -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"state.get\",\"params\":{{}}}}'"
    )
}

/// The levels the log-tail filter offers. `None` is "everything".
pub const LOG_LEVELS: [&str; 5] = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"];

/// Whether a line survives the level filter.
///
/// It is a substring test on the upper-cased level word, which is exactly
/// what the daemon's own `log.tail{level}` does (`l.contains(want)`). Being
/// the same rule is the point: the screen filters what it already has while
/// a re-tail is in flight, and a local filter that disagreed with the
/// server's would make the list flicker between two different answers.
pub fn log_level_matches(line: &str, level: Option<&str>) -> bool {
    match level {
        None => true,
        Some(level) => line.contains(&level.to_ascii_uppercase()),
    }
}

/// The fields [`redact_line`] masks the value of.
pub const CONTENT_FIELDS: [&str; 10] = [
    "text",
    "title",
    "prompt",
    "order",
    "utterance",
    "transcript",
    "cwd",
    "command",
    "token",
    "name",
];

/// The Service screen's content-redaction toggle (§8.2).
///
/// §3.7 already promises that nothing logged at `info` or above carries
/// content, and `log.tail` therefore redacts nothing on the daemon's side.
/// This is for the other case: the operator turns the level down to `debug`
/// to chase something, and then shares the window. It masks what is inside
/// double quotes (where `tracing` puts a `String` field), the value of every
/// field in [`CONTENT_FIELDS`], and the home directory, which is a login
/// name.
///
/// It is a mask over a log line and not a guarantee about the file, and the
/// toggle's own label says so.
pub fn redact_line(line: &str, home: Option<&str>) -> String {
    let line = match home {
        Some(home) if !home.is_empty() => line.replace(home, "~"),
        _ => line.to_string(),
    };
    mask_fields(&mask_quoted(&line))
}

/// Everything between a pair of double quotes becomes one ellipsis. An
/// unterminated quote masks to the end of the line, which is the safe way
/// round.
fn mask_quoted(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut inside = false;
    for ch in line.chars() {
        match (ch, inside) {
            ('"', false) => {
                out.push_str("\"\u{2026}");
                inside = true;
            }
            ('"', true) => {
                out.push('"');
                inside = false;
            }
            (_, false) => out.push(ch),
            (_, true) => {}
        }
    }
    out
}

/// `text=Bash: psql` \u{2192} `text=\u{2026}`, for the fields that carry content
/// unquoted. Whitespace is preserved exactly: a log line whose columns moved
/// is harder to read than one whose values are masked.
fn mask_fields(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for piece in line.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        let tail = &piece[word.len()..];
        match word.split_once('=') {
            Some((key, value))
                if CONTENT_FIELDS.contains(&key)
                    && !value.is_empty()
                    && !value.starts_with('"') =>
            {
                out.push_str(key);
                out.push_str("=\u{2026}");
            }
            _ => out.push_str(word),
        }
        out.push_str(tail);
    }
    out
}

/// What a `-32601` means and what it does **not** mean (contract §3).
///
/// Three methods the Service screen calls are being added to the daemon in
/// parallel this week. A screen that reported "stopped" on a call the daemon
/// never answered would be claiming something that did not happen, so this
/// is the one sentence that says which method is missing and that nothing
/// was done.
pub fn missing_method_line(method: &str) -> String {
    format!(
        "This daemon has no `{method}`: it is not in its method table, so nothing happened. The \
         method is week 6's daemon-side work; a daemon that has it will answer this button."
    )
}

/// What `launchctl print` says about the daemon's own agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launchd {
    /// Loaded and running, with the pid launchd reports.
    Running(Option<u32>),
    /// Loaded, not running (a `KeepAlive` agent between runs, or one that
    /// exited).
    Loaded,
    /// launchd has never heard of the label: the agent is not installed,
    /// which is what the Welcome sheet's `SMAppService.register` fixes.
    NotLoaded,
    /// `launchctl` answered something this parser does not recognise.
    Unknown,
}

/// Read `launchctl print gui/<uid>/<label>`.
///
/// Only three things are taken from it and each is a literal launchd
/// prints: `Could not find service`, `state = running` and `pid = N`. The
/// rest of that output is a hundred lines of domain detail that changes
/// between OS releases, and a parser that depended on it would be reporting
/// the OS version as a service state.
pub fn launchd_state(output: &str) -> Launchd {
    if output.contains("Could not find service") || output.contains("No such process") {
        return Launchd::NotLoaded;
    }
    let field = |key: &str| {
        output
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix(key)?.trim().strip_prefix("= "))
            .map(str::trim)
    };
    match field("state") {
        Some("running") => Launchd::Running(field("pid").and_then(|p| p.parse().ok())),
        Some(_) => Launchd::Loaded,
        None => Launchd::Unknown,
    }
}

/// The Service screen's line for [`Launchd`].
pub fn launchd_line(state: &Launchd) -> String {
    match state {
        Launchd::Running(Some(pid)) => format!("launchd agent: running, pid {pid}"),
        Launchd::Running(None) => "launchd agent: running".to_string(),
        Launchd::Loaded => "launchd agent: loaded, not running".to_string(),
        Launchd::NotLoaded => {
            "launchd agent: not registered with launchd on this account".to_string()
        }
        Launchd::Unknown => "launchd agent: launchctl said nothing this app can read".to_string(),
    }
}

/// The two objects the Service screen shows side by side, and why they are
/// two.
///
/// `launchctl print` above is the **daemon's** launchd agent. The login item
/// beside it is `SMAppService.mainApp` — this **window**, opened with the
/// Mac. They are different objects with different lifetimes, and an operator
/// who read one row as the other would turn off the wrong thing.
pub const LOGIN_ITEM_NOTE: &str =
    "That is the daemon's launchd agent. The line below is this window's own login item, which is \
     a different thing: the service runs whether or not this app opens at login. Change it on the \
     Welcome screen.";

// ---------------------------------------------------------------------------
// The Voice screen (§8.2's Voice row)
// ---------------------------------------------------------------------------

/// §7.2's route machine, in the order the diagram draws it: what the deck's
/// audio is doing right now.
///
/// §8.2's Voice row names five states (A2DP, SCO ours, SCO external, stuck,
/// none) and the machine has **seven**. The two it does not name are the two
/// the operator is most likely to be looking at when something is wrong —
/// `Reopening` is the 2.3 s after every press (V1: A2DP resume is 2307 ms
/// median) and `SystemDefault` is `[voice] fallback_output = "system_default"`,
/// where the sound is not going to the deck at all — so the diagram draws all
/// seven. A state the machine can be in and the diagram cannot show is a
/// window that goes blank exactly when it is needed.
pub const ROUTE_STEPS: [Route; 7] = [
    Route::None,
    Route::A2dp,
    Route::ScoOurs,
    Route::Reopening,
    Route::ScoExternal,
    Route::ScoStuck,
    Route::SystemDefault,
];

/// The same machine over the deck's BLE voice link (`[voice] transport =
/// "ble"`): it settles as `Ble`, our own capture is `BleOurs`, and nothing
/// else can flip the rate, so `ScoExternal` is not a state it can be in.
/// The other four still are — `Reopening` only for the moment between the
/// release and the speaker's reopening, `ScoStuck` after two failed opens.
pub const BLE_ROUTE_STEPS: [Route; 6] = [
    Route::None,
    Route::Ble,
    Route::BleOurs,
    Route::Reopening,
    Route::ScoStuck,
    Route::SystemDefault,
];

/// The diagram for the transport the engine reported. Before that report
/// arrives, show the current deck's BLE protocol; the legacy diagram needs
/// an explicit CoreAudio transport report.
#[must_use]
pub fn route_steps(transport: Option<VoiceTransport>) -> &'static [Route] {
    match transport {
        Some(VoiceTransport::Coreaudio) => &ROUTE_STEPS,
        _ => &BLE_ROUTE_STEPS,
    }
}

pub const fn transport_line(transport: Option<VoiceTransport>) -> &'static str {
    match transport {
        Some(VoiceTransport::Ble) => "Bluetooth LE \u{b7} custom audio protocol",
        Some(VoiceTransport::Coreaudio) => "CoreAudio \u{b7} legacy Bluetooth audio",
        None => "Bluetooth LE \u{b7} waiting for the service's transport report",
    }
}

/// The route's short label, for the diagram's chips.
pub const fn route_word(route: Route) -> &'static str {
    match route {
        Route::None => "None",
        Route::A2dp => "A2DP",
        Route::ScoOurs => "SCO (ours)",
        Route::ScoExternal => "SCO (external)",
        Route::Reopening => "Reopening",
        Route::ScoStuck => "Stuck",
        Route::SystemDefault => "System default",
        Route::Ble => "BLE",
        Route::BleOurs => "BLE (ours)",
    }
}

/// The route in the window's tones, so an amber route here and an amber dot
/// on the deck mean the same thing: **something is wrong and nobody is
/// broken**. Only `ScoStuck` is red, because it is the one the machine
/// cannot leave on its own.
pub const fn route_tone(route: Route) -> Tone {
    match route {
        Route::A2dp | Route::Ble => Tone::Green,
        Route::ScoOurs | Route::BleOurs => Tone::Blue,
        Route::Reopening => Tone::Blue,
        Route::ScoExternal => Tone::Amber,
        Route::SystemDefault => Tone::Amber,
        Route::ScoStuck => Tone::Red,
        Route::None => Tone::Gray,
    }
}

/// What the route means, in one sentence, from §7.2's own account of the
/// machine — in the transport's own terms where they differ.
pub const fn route_help(route: Route, transport: Option<VoiceTransport>) -> &'static str {
    let ble = !matches!(transport, Some(VoiceTransport::Coreaudio));
    match route {
        Route::Ble => {
            "The deck's speaker is open over its custom Bluetooth LE audio protocol, at 16 kHz, \
             and staying open: announcements and chimes go to the deck as 20 ms packets."
        }
        Route::BleOurs => {
            "Our own capture is streaming the deck's microphone over Bluetooth LE: the deck's \
             speaker is closed for the press, since nothing cancels its echo, and reopens when it \
             ends."
        }
        Route::None if ble => {
            "Nothing is open: the deck's Bluetooth LE audio link is not ready (or offers no \
             speaker), and no capture is running."
        }
        Route::Reopening if ble => {
            "Between the capture and the deck's speaker again. Over Bluetooth LE there is no rate \
             to wait for, so this passes as soon as the speaker reopens."
        }
        Route::ScoStuck if ble => {
            "Two attempts to open the deck's speaker over Bluetooth LE failed. It is tried again \
             when the link comes back."
        }
        Route::None => {
            "Nothing is open. The deck's output is closed and no capture is running, which is \
             where the machine sits between sessions."
        }
        Route::A2dp => {
            "The deck's output is open at 44.1 kHz and staying open: announcements and chimes go \
             to the deck."
        }
        Route::ScoOurs => {
            "Our own capture holds the link: the output is closed for the duration of the press \
             and comes back when it ends."
        }
        Route::Reopening => {
            "Between the capture and A2DP again. macOS's SCO teardown is a fixed 2.3 s timer that \
             cannot be hurried; the output opens at the device's current rate rather than waiting \
             for 44.1 kHz."
        }
        Route::ScoExternal => {
            "Another app's capture flipped the rate: the deck is the system default input here, \
             so this is routine rather than an edge case. It can only be waited out."
        }
        Route::ScoStuck => {
            "Two reopens failed: the output is open at the wrong rate and is being resampled. It \
             recovers on the next rate change."
        }
        Route::SystemDefault => {
            "`[voice] fallback_output = \"system_default\"`: the sound is going to this Mac's own \
             output and not to the deck."
        }
    }
}

/// One of the two device UIDs, or the one honest sentence for not having it.
pub fn uid_line(what: &str, uid: Option<&str>) -> String {
    match uid {
        Some(uid) => format!("{what}: {uid}"),
        None => format!("{what}: none selected"),
    }
}

/// How long a `voice.level` stays on the meter before the meter goes idle.
///
/// The events are per-buffer, so a meter that never expired would hold the
/// last buffer of the last press at full height for ever. One second is past
/// any plausible gap between buffers and short enough that a release is
/// visible as one.
pub const METER_FRESH: Duration = Duration::from_secs(1);

/// The meter's height, 0.0..=1.0, from a capture RMS.
///
/// The scale is dBFS and not linear: a linear meter spends its whole range
/// on the top 6 dB and shows speech as a twitch near zero. −60 dBFS is the
/// floor (which is also what an exact digital zero reads as) and 0 dBFS is
/// full height.
pub fn meter_level(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The meter's own caption. Kept scrupulously free of any verdict about the
/// microphone: V2 measured that a dead mic and a warming SCO link both report
/// exact digital zero for about a second, so a meter that said "no signal"
/// would be guessing at hardware from a number that cannot tell.
pub fn meter_line(level: Option<Level>, now: Ts) -> String {
    let Some(level) = level.filter(|l| fresh(l.at, now)) else {
        return "No capture running. The meter moves while PTT is held.".to_string();
    };
    if level.rms <= 0.0 {
        return "Capturing \u{b7} silence (a warming link and a dead microphone read the same \
                here)"
            .to_string();
    }
    format!("Capturing \u{b7} {:.0} dBFS", 20.0 * level.rms.log10())
}

/// Is this level still the live one?
pub fn fresh(at: Ts, now: Ts) -> bool {
    now.millis().saturating_sub(at.millis()) <= METER_FRESH.as_millis() as u64
}

/// The STT or TTS badge: which engine, and whether it runs here.
pub fn engine_badge(engine: &Engine) -> String {
    let where_ = if engine.on_device {
        "on-device"
    } else {
        "server"
    };
    format!("{} \u{b7} {where_}", engine.id)
}

/// The locales an engine will accept, shortened. `voice.engines` reports
/// **locales and not voices** (§4.9): the per-agent voice pool is
/// `[voice.voices] default_pool` plus `state.json`, and it is not in this
/// reply at all.
pub fn locales_line(locales: &[String], selected: Option<&str>) -> String {
    if locales.is_empty() {
        return "no locales reported".to_string();
    }
    let head: Vec<&str> = locales.iter().take(4).map(String::as_str).collect();
    let rest = locales.len().saturating_sub(head.len());
    let mut line = head.join(", ");
    if rest > 0 {
        line.push_str(&format!(" and {rest} more"));
    }
    match selected {
        Some(locale) if locales.iter().any(|l| l == locale) => format!("{line} (using {locale})"),
        // A configured locale the engine did not report is worth saying:
        // `voice.listen` refuses one without on-device support before
        // anything opens (§7.6), and this is where that shows up first.
        Some(locale) => format!("{line} \u{b7} configured {locale} is not in this list"),
        None => line,
    }
}

/// Why there are no engine badges, in the daemon's own terms. A `-32013`
/// here is a daemon with no audio engine and not a call that failed.
pub fn engines_missing_line(error: &str) -> String {
    format!(
        "No engine report: {error}. `voice.engines` answers from the one report the audio side \
         sends when its engines come up, so this is a daemon started with --no-device, a build \
         without an engine, or the moment before that report lands."
    )
}

/// §7.8's four permission states, spelled the way the daemon spells them.
pub const fn permission_word(permission: Permission) -> &'static str {
    match permission {
        Permission::Unknown => "unknown",
        Permission::NotDetermined => "not determined",
        Permission::Granted => "granted",
        Permission::Denied => "denied",
    }
}

pub const fn permission_tone(permission: Permission) -> Tone {
    match permission {
        Permission::Granted => Tone::Green,
        Permission::Denied => Tone::Red,
        Permission::NotDetermined => Tone::Amber,
        Permission::Unknown => Tone::Gray,
    }
}

/// What each state means here, and — for the two that cannot be fixed from
/// this window — what can be done instead.
pub const fn permission_help(permission: Permission) -> &'static str {
    match permission {
        Permission::Granted => "macOS has granted this to the daemon's process.",
        Permission::Denied => {
            "macOS will not prompt twice for a denied permission, so System Settings is the only \
             way back."
        }
        Permission::NotDetermined => "macOS has not been asked for this yet.",
        Permission::Unknown => {
            "The audio side has not reported this yet. With no engine in this build it stays \
             unknown, which is not the same as `not determined`."
        }
    }
}

/// The Bluetooth row's own note: the one permission this window cannot
/// ask for, because macOS asks by itself.
pub const BLUETOOTH_NOTE: &str =
    "The deck's microphone and speaker use its custom Bluetooth LE protocol. macOS asks for \
     Bluetooth access when the service first starts the link.";

pub const MAC_MICROPHONE_NOTE: &str =
    "Microphone access is needed only when capture falls back to this Mac's input. The deck's \
     own microphone uses Bluetooth access.";

/// The same permission titles and notes are used by Voice and Welcome.
pub fn permission_title(which: &str, transport: Option<VoiceTransport>) -> &'static str {
    match which {
        "microphone" if !matches!(transport, Some(VoiceTransport::Coreaudio)) => {
            "Mac fallback microphone"
        }
        "microphone" => "Microphone",
        "bluetooth" => "Bluetooth",
        _ => "Speech recognition",
    }
}

pub fn permission_note(which: &str, transport: Option<VoiceTransport>) -> Option<&'static str> {
    match which {
        "bluetooth" => Some(BLUETOOTH_NOTE),
        "microphone" if !matches!(transport, Some(VoiceTransport::Coreaudio)) => {
            Some(MAC_MICROPHONE_NOTE)
        }
        _ => None,
    }
}

/// §10.4's standing caveat about **every** permission this screen shows: a
/// grant belongs to the process that asked, and a daemon spawned from a
/// terminal is not the daemon that ships.
pub const PERMISSION_CAVEAT: &str =
    "A grant reported here is the daemon process's own. A launchd-spawned deckd is its own \
     responsible process with its own TCC row, so a status read from a terminal-spawned daemon \
     says nothing about the shipped one.";

/// The `perm.request` reply, said out loud. A `prompt_not_shown` is not a
/// failure and must not read as one: it is §7.8's own word for a prompt that
/// did not surface, and the sentences under it are the daemon's.
pub fn prompt_line(prompt: &Prompt) -> String {
    if prompt.not_shown() {
        return format!(
            "macOS was not asked for the {}: this build cannot show that prompt yet, so nothing \
             changed.",
            prompt.which
        );
    }
    format!("{}: {}", prompt.which, prompt.status)
}

/// The headline of a `voice.test` result — what was checked and what came of
/// it, with no claim the reply does not make.
pub fn test_headline(report: &TestReport) -> String {
    match report.what.as_str() {
        "speaker" => match report.played {
            Some(true) => "Chime queued on the audio thread".to_string(),
            Some(false) => "The audio thread refused the chime".to_string(),
            None => "The daemon answered without saying whether it queued anything".to_string(),
        },
        "mic" => "Input as the route machine sees it".to_string(),
        "stt" => "The recogniser, as it reported itself at start-up".to_string(),
        "tts" => "The synthesiser, as it reported itself at start-up".to_string(),
        // A `what` from a newer daemon: named, and nothing claimed for it.
        other => format!("{other}: the daemon answered"),
    }
}

/// The body of a `voice.test` result: only fields the reply carried.
pub fn test_lines(report: &TestReport) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(transport) = report.transport.as_deref() {
        lines.push(format!("transport: {transport}"));
    }
    if let Some(route) = report.route.as_deref() {
        lines.push(format!("route: {route}"));
    }
    if let Some(uid) = report.output_uid.as_deref() {
        lines.push(format!("output: {uid}"));
    }
    if let Some(uid) = report.input_uid.as_deref() {
        lines.push(format!("input: {uid}"));
    }
    if let Some(chime) = report.chime.as_deref() {
        lines.push(format!("chime: {chime}"));
    }
    if let Some(warm) = report.mic_warm.as_deref() {
        lines.push(format!("mic_warm: {warm}"));
    }
    if let Some(warm) = report.mic_warm_effective.as_deref() {
        lines.push(format!("mic_warm_effective: {warm}"));
    }
    if let Some(engine) = report.engine.as_ref() {
        lines.push(format!("engine: {}", engine_badge(engine)));
        lines.push(format!("locales: {}", locales_line(&engine.locales, None)));
    }
    if report.listening {
        lines.push("a capture is running right now".to_string());
    }
    lines
}

/// The **Test** row's own standing note. The daemon puts its caveats in each
/// reply's `limits` and the screen shows those verbatim; this is the one
/// sentence that has to be true before a button is pressed, so it sits over
/// the buttons rather than under a result.
pub const TEST_NOTE: &str =
    "Only Speaker makes a sound \u{2014} a 110 ms chime to the deck's own output, at your own \
     `set speaker` gain. Nothing here changes the system default device or the system volume, \
     and Mic does not open the microphone: a capture is a PTT session with an owner and a TCC \
     grant, and a diagnostic may not start one.";

// ---------------------------------------------------------------------------
// The login item (§10.2, contract §1 M2)
// ---------------------------------------------------------------------------

/// `SMAppService.status` (contract §1 M2). The numbers are Apple's:
/// 0 `notRegistered`, 1 `enabled`, 2 `requiresApproval`, 3 `notFound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginItem {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    /// A status this build has no name for, kept as itself.
    Unknown(isize),
}

impl LoginItem {
    pub const fn from_code(code: isize) -> LoginItem {
        match code {
            0 => LoginItem::NotRegistered,
            1 => LoginItem::Enabled,
            2 => LoginItem::RequiresApproval,
            3 => LoginItem::NotFound,
            other => LoginItem::Unknown(other),
        }
    }

    pub const fn code(self) -> isize {
        match self {
            LoginItem::NotRegistered => 0,
            LoginItem::Enabled => 1,
            LoginItem::RequiresApproval => 2,
            LoginItem::NotFound => 3,
            LoginItem::Unknown(code) => code,
        }
    }

    /// **The measured rule (M2).** `register()` returned `Ok` on a fresh
    /// state while the status was still 3 (`notFound`), so the return value
    /// is not evidence that anything was registered. Only these two statuses
    /// are: `enabled` is on, and `requiresApproval` is on and waiting for the
    /// operator in Login Items.
    pub const fn registered(self) -> bool {
        matches!(self, LoginItem::Enabled | LoginItem::RequiresApproval)
    }

    pub const fn tone(self) -> Tone {
        match self {
            LoginItem::Enabled => Tone::Green,
            LoginItem::RequiresApproval => Tone::Amber,
            LoginItem::NotRegistered => Tone::Gray,
            LoginItem::NotFound | LoginItem::Unknown(_) => Tone::Red,
        }
    }
}

/// What the login item's status says, in the status's own terms and never in
/// the return value's.
pub fn login_item_line(status: LoginItem) -> String {
    match status {
        LoginItem::Enabled => {
            "Open at login: on. macOS reports this app as an enabled login item.".to_string()
        }
        LoginItem::RequiresApproval => {
            "Open at login: waiting for your approval in System Settings \u{203a} General \
             \u{203a} Login Items."
                .to_string()
        }
        LoginItem::NotRegistered => "Open at login: off.".to_string(),
        LoginItem::NotFound => {
            "macOS answered `notFound`: it has no record of this login item, so nothing is \
             registered whatever the last attempt returned."
                .to_string()
        }
        LoginItem::Unknown(code) => {
            format!("macOS answered status {code}, which this build has no name for.")
        }
    }
}

/// System Settings › General › Login Items, verified against the extension
/// bundle id on this machine.
pub const LOGIN_ITEMS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

/// System Settings › Bluetooth, likewise — the Welcome step's "Not seeing
/// it?".
pub const BLUETOOTH_SETTINGS: &str = "x-apple.systempreferences:com.apple.BluetoothSettings";

// ---------------------------------------------------------------------------
// The Welcome sheet (§8.2's Welcome row)
// ---------------------------------------------------------------------------

/// The three steps, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Service,
    Deck,
    Integrations,
}

impl Step {
    pub const ALL: [Step; 3] = [Step::Service, Step::Deck, Step::Integrations];

    pub const fn title(self) -> &'static str {
        match self {
            Step::Service => "Start the service",
            Step::Deck => "Connect the deck and voice",
            Step::Integrations => "Install the hooks",
        }
    }

    pub const fn blurb(self) -> &'static str {
        match self {
            Step::Service => {
                "Everything else talks to the service: it owns the deck's link, the audio and \
                 every agent on the roster. This window is only a client of it."
            }
            Step::Deck => {
                "Pair OpenWalkieTalkie in Bluetooth settings. The service adopts it and its face \
                 strip, then opens the deck's custom Bluetooth LE audio protocol. Allow \
                 Bluetooth access for deck audio; speech permission depends on the recognizer."
            }
            Step::Integrations => {
                "Each CLI reports itself through hooks this app writes for you. Install the ones \
                 you use, then hold PTT and say hello."
            }
        }
    }

    pub fn next(self) -> Option<Step> {
        match self {
            Step::Service => Some(Step::Deck),
            Step::Deck => Some(Step::Integrations),
            Step::Integrations => None,
        }
    }

    pub fn back(self) -> Option<Step> {
        match self {
            Step::Service => None,
            Step::Deck => Some(Step::Service),
            Step::Integrations => Some(Step::Deck),
        }
    }

    /// `1 of 3`.
    pub fn counter(self) -> String {
        let n = Step::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1;
        format!("{n} of {}", Step::ALL.len())
    }
}

/// The deck, as step 2 reports it: the link, the firmware and the strip.
///
/// `device.link` carries two things and they are not the same: `link` is the
/// BLE link and `session` is §6.3's session on top of it. A deck that is
/// linked but whose session is not `Live` yet is connected and not ready, and
/// a step that called that "connected" would send the operator on to hold
/// PTT at a deck that cannot answer.
pub fn deck_line(device: &deck_model::DeviceState) -> String {
    match device.link {
        Link::Offline => "No deck yet. The service reports one the moment it links.".to_string(),
        link => {
            let fw = device.fw.as_deref().unwrap_or("firmware not reported");
            let faces = device.faces.len();
            let what = if link == Link::Degraded {
                "Deck linked, degraded"
            } else {
                "Deck connected"
            };
            let session = match device.session {
                SessionState::Live => String::new(),
                other => format!(" \u{b7} session {other:?}"),
            };
            format!("{what} \u{b7} {fw} \u{b7} {faces} faces adopted{session}")
        }
    }
}

/// Whether step 2's "waiting for `device.link`" is over: a deck that is
/// linked **and** whose session is live.
pub fn deck_ready(device: &deck_model::DeviceState) -> bool {
    device.link != Link::Offline && device.session == SessionState::Live
}

/// The Welcome step's own doctor summary: **what this window can see**, which
/// is not the same thing as what `deckd doctor` runs.
///
/// Saying which it is matters. `deckd doctor` is a CLI command with checks
/// this app has no method for (it reads the data dir and asks launchd about
/// both labels); what is here is the service, the launchd agent, the port and
/// the link as the GUI's own mirror and calls have them. Each line is
/// something the operator can act on, and the last one names the command that
/// goes further.
pub fn doctor_lines(
    connected: bool,
    info: &Info,
    launchd: Option<&Launchd>,
    device: &deck_model::DeviceState,
    service: &deck_model::ServiceState,
) -> Vec<String> {
    let mut lines = vec![
        if connected {
            format!(
                "service: answering on the socket{}",
                info.daemon
                    .as_deref()
                    .map(|v| format!(" (deckd {v})"))
                    .unwrap_or_default()
            )
        } else {
            "service: not answering on the socket".to_string()
        },
        launchd_line(launchd.unwrap_or(&Launchd::Unknown)),
        match info.port {
            Some(port) => format!("http door: listening on {port}"),
            None => "http door: not held \u{2014} the hooks have nowhere to POST".to_string(),
        },
        deck_line(device),
    ];
    if let Some(reason) = service.degraded.as_deref() {
        lines.push(format!("degraded: {reason}"));
    }
    lines.push(
        "`deckd doctor` checks more than this window can see \u{2014} the data dir, the tokens \
         file and both launchd labels."
            .to_string(),
    );
    lines
}

/// Step 3's closing line: the exit test, in the words the operator has to
/// act on.
pub const SAY_HELLO: &str =
    "Hold PTT on the deck and say hello. The words appear below as the recogniser sends them.";

/// What the live transcript reads while nothing has been said.
pub const NO_TRANSCRIPT: &str = "Nothing heard yet.";

/// The live transcript line: partial and final are drawn the same and told
/// apart by their label, because a partial that looked final would have the
/// operator believe the recogniser had settled when it had not.
pub fn transcript_line(transcript: Option<&Transcript>) -> String {
    let Some(transcript) = transcript else {
        return NO_TRANSCRIPT.to_string();
    };
    let text = if transcript.text.is_empty() {
        "\u{2026}"
    } else {
        &transcript.text
    };
    let label = if transcript.final_ {
        "final"
    } else {
        "hearing"
    };
    match transcript.intent.as_deref() {
        Some(intent) => format!("{label}: {text} \u{b7} matched {intent}"),
        None => format!("{label}: {text}"),
    }
}

// ---------------------------------------------------------------------------
// The tray icon (§8.2's tray row)
// ---------------------------------------------------------------------------

/// The status item's bitmap, in pixels. macOS scales whatever it is given to
/// 18 pt high, so 36 is exactly 2x on a Retina menu bar and needs no
/// resampling.
pub const TRAY_ICON_PX: u32 = 36;

/// The deck glyph and a monochrome status badge, as a black alpha mask.
/// AppKit tints this template image to match the other menu bar icons.
/// Status uses shape: question mark, exclamation mark, check, solid dot,
/// or hollow dot, while the tooltip supplies the complete status text.
pub fn tray_icon_rgba(dot: Tone) -> Vec<u8> {
    let size = TRAY_ICON_PX as f32;
    let mut pixels = vec![0u8; (TRAY_ICON_PX * TRAY_ICON_PX * 4) as usize];
    // The website radio's silhouette, with its screen and keys cut out.
    // The existing badge keeps all five aggregate states distinguishable.
    let radio = include_bytes!("../../../../assets/branding/radio-template.alpha");
    let dot_c = (size - size * 0.24, size - size * 0.24);
    let dot_r = size * 0.22;
    for y in 0..TRAY_ICON_PX {
        for x in 0..TRAY_ICON_PX {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut alpha = f32::from(radio[(y * TRAY_ICON_PX + x) as usize]) / 255.0;
            // The aggregate dot sits over the corner, with a hole punched in
            // the glyph behind it so it reads as a badge and not as a blob.
            let from_dot = ((px - dot_c.0).powi(2) + (py - dot_c.1).powi(2)).sqrt();
            alpha *= 1.0 - coverage(dot_r + 2.0 - from_dot);
            let badge = coverage(dot_r - from_dot)
                * (1.0 - tray_badge_cutout(dot, px - dot_c.0, py - dot_c.1, dot_r));
            alpha = alpha.max(badge);
            let i = ((y * TRAY_ICON_PX + x) * 4) as usize;
            // Straight (un-premultiplied) RGBA, which is what
            // `Icon::from_rgba` takes and what the PNG it builds expects.
            pixels[i + 3] = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

/// Transparent marks inside the filled badge, in device pixels at 2x.
fn tray_badge_cutout(tone: Tone, x: f32, y: f32, radius: f32) -> f32 {
    let stroke = |a: (f32, f32), b: (f32, f32)| {
        let (ax, ay) = (a.0 * radius, a.1 * radius);
        let (dx, dy) = ((b.0 - a.0) * radius, (b.1 - a.1) * radius);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        coverage(1.0 - (x - ax - t * dx).hypot(y - ay - t * dy))
    };
    let point = |cy: f32| coverage(1.1 - x.hypot(y - cy * radius));
    match tone {
        Tone::Red => {
            let hook = [(-0.30, -0.26), (-0.24, -0.42), (0.16, -0.42),
                (0.32, -0.24), (0.24, -0.04), (0.0, 0.10), (0.0, 0.18)];
            hook.windows(2).fold(point(0.48), |alpha, pair| alpha.max(stroke(pair[0], pair[1])))
        }
        Tone::Amber => stroke((0.0, -0.44), (0.0, 0.12)).max(point(0.48)),
        Tone::Green => stroke((-0.45, 0.0), (-0.12, 0.33))
            .max(stroke((-0.12, 0.33), (0.48, -0.34))),
        Tone::Blue => 0.0,
        Tone::Gray => coverage(radius - 2.0 - x.hypot(y)),
    }
}

/// Anti-aliasing: a signed distance in pixels to a coverage in 0.0..=1.0.
fn coverage(distance: f32) -> f32 {
    (distance + 0.5).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{Choice, Page, Terminal};

    fn agent() -> Agent {
        Agent {
            id: deck_model::AgentId("ag_1".into()),
            slot: Some(Slot(1)),
            kind: Kind::Claude,
            name: "cohdl-api".into(),
            state: AgentState::Question,
            title: Some("waiting on psql".into()),
            title_source: deck_model::TitleSource::Prompt,
            since: Ts::from_millis(1_000_000),
            source: Source::Hook,
            owner: deck_model::Owner {
                principal: Principal::Adapter(deck_model::AdapterKind::Claude),
                session: deck_model::SessionId(1),
                lease_expires: None,
            },
            session: SessionInfo::default(),
            caps: Caps::default(),
            companion: None,
            card: None,
            reply: None,
            messages: Vec::new(),
            workflow_runs: Vec::new(), workflow: None,
            chat_rev: 0,
            reply_rev: 0,
            voice: None,
            pinned: false,
            meta: serde_json::Value::Null,
            redacted: false,
        }
    }

    fn card() -> Card {
        Card {
            id: deck_model::CardId(17),
            kind: deck_model::CardKind::Q,
            status: CardStatus::Pending,
            deliver: Deliver::Hold,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            },
            text: Some("Bash: psql -c 'drop table'".into()),
            choices: vec![
                Choice {
                    label: "Allow".into(),
                    detail: None,
                },
                Choice {
                    label: "Allow always".into(),
                    detail: None,
                },
                Choice {
                    label: "Deny".into(),
                    detail: None,
                },
            ],
            choices_n: 3,
            choice_map: Vec::new(),
            rec: Some(0),
            free_text: true,
            page: Page { index: 0, count: 1 },
            created: Ts::from_millis(1_000_000),
            expires: None,
            redacted: false,
        }
    }

    #[test]
    fn the_roster_dot_is_the_tone_the_deck_draws_and_gray_when_the_session_is_gone() {
        assert_eq!(state_tone(AgentState::Working, false), Tone::Blue);
        assert_eq!(state_tone(AgentState::Question, false), Tone::Red);
        assert_eq!(state_tone(AgentState::Ready, false), Tone::Green);
        assert_eq!(state_tone(AgentState::Error, false), Tone::Amber);
        assert_eq!(state_tone(AgentState::Idle, false), Tone::Gray);
        for state in [
            AgentState::Working,
            AgentState::Question,
            AgentState::Ready,
            AgentState::Error,
        ] {
            assert_eq!(state_tone(state, true), Tone::Gray, "{state:?} stale");
        }
    }

    #[test]
    fn ages_read_the_way_the_deck_reads_them_and_never_go_backwards() {
        let ts = |ms| Ts::from_millis(ms);
        assert_eq!(age(ts(59_999), ts(0)), "now");
        assert_eq!(age(ts(60_000), ts(0)), "1m");
        assert_eq!(age(ts(59 * 60_000 + 59_999), ts(0)), "59m");
        assert_eq!(age(ts(60 * 60_000), ts(0)), "1h");
        assert_eq!(age(ts(1440 * 60_000), ts(0)), "1d");
        assert_eq!(age(ts(12 * 1440 * 60_000), ts(0)), "12d");
        // A stamp from the future is "now", not a negative age.
        assert_eq!(age(ts(0), ts(9_000_000)), "now");
    }

    #[test]
    fn a_slot_badge_says_which_slot_or_that_there_is_none() {
        assert_eq!(slot_badge(Some(Slot(3))), "Slot 3");
        assert_eq!(slot_badge(None), NOT_ON_DECK);
    }

    #[test]
    fn a_card_names_its_page_only_when_there_is_more_than_one() {
        let mut c = card();
        assert_eq!(card_ref(&c), "#17");
        c.page = Page { index: 0, count: 2 };
        assert_eq!(card_ref(&c), "#17.1/2");
        c.page = Page { index: 1, count: 2 };
        assert_eq!(card_ref(&c), "#17.2/2");
    }

    #[test]
    fn every_choice_is_listed_with_its_recommendation_and_its_sent_index() {
        let mut c = card();
        assert_eq!(choice_label(&c, 0), "1. Allow \u{b7} recommended");
        assert_eq!(choice_label(&c, 2), "3. Deny");
        // B10: the deck moved the recommendation to slot 0, so the index the
        // owner knows is not this row's.
        c.choice_map = vec![2, 0, 1];
        assert_eq!(
            choice_label(&c, 0),
            "1. Allow \u{b7} recommended \u{b7} sent as 2"
        );
        assert_eq!(choice_label(&c, 1), "2. Allow always \u{b7} sent as 0");
        assert_eq!(choice_label(&c, 9), "10. (redacted)");
    }

    #[test]
    fn a_hold_counts_down_and_then_says_it_expired() {
        let mut c = card();
        assert_eq!(hold_countdown(&c, Ts::from_millis(0)), None);
        c.expires = Some(Ts::from_millis(90_000));
        assert_eq!(
            hold_countdown(&c, Ts::from_millis(60_000)).as_deref(),
            Some("30s left")
        );
        assert_eq!(
            hold_countdown(&c, Ts::from_millis(0)).as_deref(),
            Some("1m 30s left")
        );
        assert_eq!(
            hold_countdown(&c, Ts::from_millis(90_000)).as_deref(),
            Some("expired")
        );
        assert_eq!(
            hold_countdown(&c, Ts::from_millis(900_000)).as_deref(),
            Some("expired")
        );
    }

    #[test]
    fn a_failed_card_names_its_reason_and_a_degraded_one_says_when_it_was_focused() {
        let mut c = card();
        assert_eq!(failed_reason(&c), None);
        assert_eq!(degraded_at(&c), None);
        c.status = CardStatus::Failed {
            reason: "timeout".into(),
        };
        assert_eq!(failed_reason(&c), Some("timeout"));
        assert!(release_offered(&c));
        // A hold card's parked response is gone after a failure (§4.8).
        assert!(!retry_offered(&c));
        c.deliver = Deliver::Poll;
        assert!(retry_offered(&c));
        c.status = CardStatus::Degraded { focused_at: None };
        assert_eq!(degraded_at(&c), Some(None));
        assert!(!retry_offered(&c) && !release_offered(&c));
        c.status = CardStatus::Degraded {
            focused_at: Some(Ts::from_millis(1_789_760_523_120)),
        };
        assert_eq!(degraded_at(&c), Some(Some("19:42:03Z".into())));
    }

    #[test]
    fn an_answering_card_shows_the_label_or_the_text_the_operator_gave() {
        let mut c = card();
        assert_eq!(answering(&c), None);
        c.status = CardStatus::Answering {
            choice: Some(1),
            text: None,
        };
        assert_eq!(answering(&c).as_deref(), Some("Allow always"));
        c.status = CardStatus::Answering {
            choice: Some(9),
            text: None,
        };
        assert_eq!(answering(&c).as_deref(), Some("choice 9"));
        c.status = CardStatus::Answering {
            choice: None,
            text: Some("only the staging table".into()),
        };
        assert_eq!(
            answering(&c).as_deref(),
            Some("\u{201c}only the staging table\u{201d}")
        );
    }

    #[test]
    fn only_a_custom_card_gets_a_badge_and_every_origin_gets_a_line() {
        let mut c = card();
        assert_eq!(custom_badge(&c.origin), None);
        assert_eq!(origin_line(&c.origin), "permission \u{b7} Bash");
        c.origin.group = Some(deck_model::Group { index: 0, count: 3 });
        assert_eq!(
            origin_line(&c.origin),
            "permission \u{b7} Bash \u{b7} 1 of 3"
        );
        c.origin = Origin::custom();
        assert_eq!(custom_badge(&c.origin), Some("custom"));
        assert_eq!(origin_line(&c.origin), "custom");
    }

    #[test]
    fn redacted_content_says_so_instead_of_showing_an_empty_row() {
        let mut c = card();
        c.text = None;
        assert_eq!(card_text(&c), "");
        c.redacted = true;
        assert_eq!(card_text(&c), REDACTED);
    }

    #[test]
    fn the_source_line_names_the_cwd_the_pid_and_the_terminal() {
        let mut session = SessionInfo {
            key: Some("claude:abc".into()),
            cwd: Some("/Users/t/code/api".into()),
            pid: Some(4242),
            pid_kind: Some(deck_model::PidKind::Cli),
            terminal: Some(Terminal {
                program: Some("Ghostty".into()),
                tty: Some("/dev/ttys004".into()),
                ..Terminal::default()
            }),
        };
        assert_eq!(
            source_line(&session),
            "/Users/t/code/api \u{b7} pid 4242 (cli) \u{b7} Ghostty /dev/ttys004"
        );
        session.pid_kind = None;
        assert!(source_line(&session).contains("pid 4242 \u{b7}"));
        assert_eq!(
            source_line(&SessionInfo::default()),
            "no source reported",
            "content the reader cannot see is not an empty line"
        );
    }

    #[test]
    fn the_caps_line_says_what_the_buttons_can_do() {
        assert_eq!(
            caps_line(&Caps::default()),
            "answer none \u{b7} order none \u{b7} no focus"
        );
        assert_eq!(
            caps_line(&Caps {
                answer: deck_model::AnswerCap::Hold,
                order: deck_model::OrderCap::Inject,
                focus: true,
            }),
            "answer hold \u{b7} order inject \u{b7} focus"
        );
    }

    #[test]
    fn an_order_line_is_the_exact_string_and_copy_only_fires_on_a_failure() {
        let mut entry = OrderEntry {
            text: Some("run the smoke tests".into()),
            via: Some("inject".into()),
            reason: None,
            at: Ts::from_millis(0),
        };
        assert_eq!(
            order_line(&entry),
            "\u{201c}run the smoke tests\u{201d} \u{b7} inject"
        );
        assert_eq!(
            copyable(&entry),
            None,
            "a dispatched order is not a failure"
        );
        entry.reason = Some("not_foreground".into());
        assert_eq!(
            order_line(&entry),
            "\u{201c}run the smoke tests\u{201d} \u{b7} inject \u{b7} failed: not_foreground"
        );
        assert_eq!(copyable(&entry), Some("run the smoke tests"));
        // Refused before it was dispatched: nothing to copy, and it says so.
        let refused = OrderEntry {
            text: None,
            via: None,
            reason: Some("dialog_open".into()),
            at: Ts::from_millis(0),
        };
        assert_eq!(order_line(&refused), "(no text) \u{b7} failed: dialog_open");
        assert_eq!(copyable(&refused), None);
    }

    #[test]
    fn the_sheet_finds_the_allow_and_the_deny_in_the_daemons_own_vocabulary() {
        let mut c = card();
        assert_eq!(allow_choice(&c), Some(0), "the recommendation is an allow");
        assert_eq!(deny_choice(&c), Some(2));
        // A recommendation that is not an allow does not become one.
        c.rec = Some(2);
        assert_eq!(allow_choice(&c), Some(0));
        // No allow at all: the dialog offers no Allow button.
        c.choices = vec![
            Choice {
                label: "Later".into(),
                detail: None,
            },
            Choice {
                label: "Deny".into(),
                detail: None,
            },
        ];
        c.rec = None;
        assert_eq!(allow_choice(&c), None);
        assert_eq!(deny_choice(&c), Some(1));
        // No deny word either: the first choice that is not an allow.
        c.choices = vec![
            Choice {
                label: "Allow".into(),
                detail: None,
            },
            Choice {
                label: "Later".into(),
                detail: None,
            },
        ];
        assert_eq!(allow_choice(&c), Some(0));
        assert_eq!(deny_choice(&c), Some(1));
        // A card with nothing but allows can only be dismissed, never denied.
        c.choices = vec![Choice {
            label: "Allow always".into(),
            detail: None,
        }];
        assert_eq!(deny_choice(&c), None);
        // An **elicitation**'s choices are the caller's own data, so there is
        // no "first non-allow" to fall back on: `production` is a decision,
        // not a refusal, and a dismissal must send nothing instead.
        c.origin.kind = OriginKind::Elicitation;
        c.choices = ["Yes, deploy", "production", "staging"]
            .iter()
            .map(|l| Choice {
                label: (*l).into(),
                detail: None,
            })
            .collect();
        assert_eq!(allow_choice(&c), Some(0));
        assert_eq!(deny_choice(&c), None, "Escape must not answer `production`");
        // The same labels under a built-in `permission` card still deny with
        // the first non-allow: those choices are a CLI's own prompt.
        c.origin.kind = OriginKind::Permission;
        assert_eq!(deny_choice(&c), Some(1));
    }

    #[test]
    fn the_owner_source_badge_tells_a_hook_from_a_script_from_a_third_party() {
        let mut agent = agent();
        assert_eq!(owner_source(&agent), "hook");
        agent.source = Source::Mcp;
        assert_eq!(owner_source(&agent), "mcp");
        agent.source = Source::Api;
        assert_eq!(owner_source(&agent), "self-reported");
        agent.owner.principal = Principal::Adapter(deck_model::AdapterKind::Virtual);
        assert_eq!(owner_source(&agent), "self-reported");
        agent.owner.principal = Principal::Token("mcp".into());
        assert_eq!(owner_source(&agent), "script");
        // The MCP adapter is `mcp` whichever door it came in by.
        agent.owner.principal = Principal::Adapter(deck_model::AdapterKind::Mcp);
        assert_eq!(owner_source(&agent), "mcp");
    }

    #[test]
    fn a_disconnected_title_is_what_makes_a_row_stale_and_hides_its_age() {
        let mut agent = agent();
        assert!(!stale(&agent));
        assert_eq!(live_title(&agent), "waiting on psql");
        assert_eq!(
            state_age(&agent, Ts::from_millis(1_120_000)).as_deref(),
            Some("2m")
        );
        // Idle is not news, so the deck shows no age and neither does this.
        agent.state = AgentState::Idle;
        assert_eq!(state_age(&agent, Ts::from_millis(1_120_000)), None);
        agent.state = AgentState::Working;
        agent.title = Some(DISCONNECTED.into());
        assert!(stale(&agent));
        assert_eq!(state_age(&agent, Ts::from_millis(1_120_000)), None);
        // A title the reader's scope nulled says so rather than reading empty.
        agent.title = None;
        agent.redacted = true;
        assert_eq!(live_title(&agent), REDACTED);
    }

    #[test]
    fn conol_is_its_own_group_even_beside_a_directory_named_conol() {
        let mut snapshot = Snapshot::default();
        for (id, kind, cwd) in [("ag_native", Kind::Conol, None), ("ag_native2", Kind::Conol, Some("/ignored")),
            ("ag_terminal", Kind::Claude, Some("/projects/Conol")), ("ag_other", Kind::Other, None)] {
            let mut a = agent(); a.id = deck_model::AgentId(id.into()); a.kind = kind;
            a.session.cwd = cwd.map(String::from); snapshot.agents.insert(a.id.clone(), a);
        }
        let groups = projects(&snapshot);
        assert_eq!(groups.len(), 3);
        let native = groups.iter().find(|g| g.agents[0].kind == Kind::Conol).unwrap();
        assert_eq!(native.label, "Conol"); assert_eq!(native.agents.len(), 2);
        assert!(native.cwd.is_none());
    }

    #[test]
    fn companion_groups_use_app_and_account_identity_independently_of_provider() {
        use deck_model::companion::{CompanionProfile, CompanionRole};
        let mut snapshot = Snapshot::default();
        for (id, app, account, kind) in [
            ("ag_1", "example", "personal", Kind::Other),
            ("ag_2", "example", "personal", Kind::Codex),
            ("ag_3", "example", "work", Kind::Other),
            ("ag_4", "different", "personal", Kind::Other),
        ] {
            let mut a = agent();
            a.id = deck_model::AgentId(id.into()); a.kind = kind;
            a.companion = Some(CompanionProfile { app_id: app.into(), group_id: account.into(),
                group_label: "Example".into(), role: CompanionRole::Session });
            snapshot.agents.insert(a.id.clone(), a);
        }
        let groups = projects(&snapshot);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().any(|g| g.agents.len() == 2));
        assert!(groups.iter().all(|g| g.label == "Example" && g.cwd.is_none()));
    }

    #[test]
    fn the_roster_is_grouped_by_project_each_in_slot_order_then_its_overflow() {
        let mut snapshot = Snapshot::default();
        for (id, slot, since, cwd) in [
            ("ag_deck2", Some(Slot(2)), 10, Some("/Users/t/projects/cohdl/")),
            ("ag_deck1", Some(Slot(1)), 20, Some("/Users/t/projects/cohdl")),
            ("ag_over_old", None, 30, Some("/Users/t/projects/cohdl")),
            ("ag_over_new", None, 40, Some("/Users/t/projects/cohdl")),
            ("ag_alpha", Some(Slot(3)), 50, Some("/srv/Alpha")),
            ("ag_twin", None, 60, Some("/tmp/cohdl")),
            ("ag_nowhere", None, 70, None),
            ("ag_root", None, 80, Some("/")),
        ] {
            let mut a = agent();
            a.id = deck_model::AgentId(id.into());
            a.slot = slot;
            a.since = Ts::from_millis(since);
            a.session.cwd = cwd.map(String::from);
            snapshot.agents.insert(a.id.clone(), a);
        }
        let groups = projects(&snapshot);
        let ids = |agents: &[&Agent]| -> Vec<String> {
            agents.iter().map(|a| a.id.0.clone()).collect()
        };
        let labels: Vec<(&str, Option<&str>)> = groups
            .iter()
            .map(|g| (g.label.as_str(), g.cwd.as_deref()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Alpha", Some("/srv/Alpha")),
                ("cohdl", Some("/Users/t/projects/cohdl")),
                ("cohdl", Some("/tmp/cohdl")),
                (NO_PROJECT, None),
            ],
            "by name with the case folded, twins apart by directory, the unlabelled last"
        );
        assert_eq!(
            ids(&groups[1].agents),
            vec!["ag_deck1", "ag_deck2", "ag_over_new", "ag_over_old"],
            "slots first, then the one that just changed state"
        );
        assert_eq!(ids(&groups[2].agents), vec!["ag_twin"]);
        assert_eq!(
            ids(&groups[3].agents),
            vec!["ag_root", "ag_nowhere"],
            "a directory with no last component is no project either"
        );
        assert!(projects(&Snapshot::default()).is_empty());
    }

    #[test]
    fn a_row_named_after_its_project_shows_its_kind_instead() {
        let group = Project {
            label: "cohdl".into(),
            cwd: Some("/x/cohdl".into()),
            agents: Vec::new(),
        };
        let mut a = agent();
        assert_eq!(row_name(&a, &group), "cohdl-api");
        a.name = "cohdl".into();
        assert_eq!(row_name(&a, &group), "Claude");
        let unlabelled = Project {
            label: NO_PROJECT.into(),
            cwd: None,
            agents: Vec::new(),
        };
        a.name = NO_PROJECT.into();
        assert_eq!(row_name(&a, &unlabelled), NO_PROJECT, "only a real project hides a name");
        // The same for a title that is just the directory's name.
        assert_eq!(row_title(&a, &group), "waiting on psql");
        a.title = Some("cohdl".into());
        a.title_source = deck_model::TitleSource::Cwd;
        assert_eq!(row_title(&a, &group), "");
        a.title = Some(NO_PROJECT.into());
        assert_eq!(row_title(&a, &unlabelled), NO_PROJECT);
        a.title = None;
        a.redacted = true;
        assert_eq!(row_title(&a, &group), REDACTED);
        assert_eq!(project_path("/Users/t/projects/cohdl", Some("/Users/t")), "~/projects/cohdl");
        assert_eq!(project_path("/Users/t", Some("/Users/t/")), "~");
        assert_eq!(project_path("/Users/tony/x", Some("/Users/t")), "/Users/tony/x");
        assert_eq!(project_path("/srv/x", None), "/srv/x");
    }

    #[test]
    fn kinds_have_a_glyph_and_a_word_and_no_two_kinds_share_either() {
        let kinds = [Kind::Claude, Kind::Codex, Kind::Gemini, Kind::Other];
        let icons: Vec<&str> = kinds.iter().copied().map(kind_icon).collect();
        let labels: Vec<&str> = kinds.iter().copied().map(kind_label).collect();
        for set in [&icons, &labels] {
            for (i, a) in set.iter().enumerate() {
                assert!(!a.is_empty());
                assert!(!set[i + 1..].contains(a), "{a} is used twice");
            }
        }
        assert_eq!(kind_label(Kind::Other), "Agent");
        assert_eq!(state_word(AgentState::Question), "Question");
        assert_eq!(route(Deliver::Hold), "hold");
        assert_eq!(route(Deliver::None), "none");
    }
    // ---- the Integrations screen -------------------------------------

    #[test]
    fn every_party_has_a_name_a_sentence_and_only_claude_carries_the_allowlist_warning() {
        for target in TARGETS {
            let name = party_name(target);
            assert!(!name.is_empty() && name != target || target == name);
            assert!(!party_what(target).is_empty(), "{target} says nothing");
        }
        assert_eq!(party_name("claude"), "Claude Code");
        assert_eq!(party_name("claude-desktop"), "Claude Desktop");
        // A target this build has never heard of is still named, as itself.
        assert_eq!(party_name("future-cli"), "future-cli");
        assert_eq!(party_what("future-cli"), "");
        // Only Claude writes **http** hooks, and the key is Claude's (§5.7).
        assert!(allowlist_applies("claude"));
        for other in ["codex", "gemini", "claude-desktop"] {
            assert!(!allowlist_applies(other), "{other}");
        }
        // The one thing the operator has to read before installing.
        assert!(ALLOWLIST_NOTE.contains("allowedHttpHookUrls"));
        assert!(ALLOWLIST_NOTE.contains("EVERY http hook"));
        assert!(ALLOWLIST_NOTE.contains("merged allowlist"));
    }

    #[test]
    fn every_status_word_gets_its_own_sentence_and_only_installed_reads_as_healthy() {
        let mut sentences: Vec<&str> = Vec::new();
        for status in Status::KNOWN {
            let word = status_word(&status).to_string();
            let help = status_help(&status);
            assert!(!help.is_empty(), "{word} has no sentence");
            assert!(
                !sentences.contains(&help),
                "{word} shares its sentence with another word"
            );
            sentences.push(help);
            assert_eq!(
                status_tone(&status),
                match status {
                    Status::Installed => Tone::Green,
                    Status::Absent => Tone::Gray,
                    _ => Tone::Amber,
                }
            );
        }
        // A word from a newer daemon is amber and says it cannot be read —
        // never green, and never silently "absent".
        let newer = Status::parse("some_future_word");
        assert_eq!(status_word(&newer), "some_future_word");
        assert_eq!(status_tone(&newer), Tone::Amber);
        assert!(status_help(&newer).contains("does not know this word"));
    }

    #[test]
    fn the_detected_line_tells_a_missing_cli_from_a_target_that_never_had_one() {
        let found = Detected {
            path: "/opt/homebrew/bin/claude".into(),
            version: "2.1.0".into(),
        };
        assert_eq!(
            detected_line("claude", Some(&found)),
            "2.1.0 \u{b7} /opt/homebrew/bin/claude"
        );
        // A binary whose `--version` said nothing is still a binary.
        let quiet = Detected {
            path: "/usr/local/bin/codex".into(),
            version: String::new(),
        };
        assert_eq!(detected_line("codex", Some(&quiet)), "/usr/local/bin/codex");
        assert_eq!(detected_line("gemini", None), "not found on PATH");
        // A file, not a CLI: "not found on PATH" would read as a problem
        // where there is none.
        assert!(detected_line("claude-desktop", None).contains("not a binary"));
    }

    #[test]
    fn last_event_time_says_whether_it_was_reported_rather_than_showing_nothing() {
        assert_eq!(
            last_event_line(Some("2026-09-22T10:14:02Z")),
            "last hook event 2026-09-22T10:14:02Z"
        );
        // This daemon never fills the field in, so the honest line says it
        // was not reported — and not "never", which would be a claim about
        // the hooks rather than about the answer.
        let none = last_event_line(None);
        assert!(none.contains("not reported"), "{none}");
        assert!(!none.contains("never"), "{none}");
    }

    #[test]
    fn a_backup_is_labelled_by_its_own_name_and_not_by_the_path_to_it() {
        assert_eq!(
            backup_label("/Users/t/Library/.../backups/settings.json.1764.bak"),
            "settings.json.1764.bak"
        );
        assert_eq!(backup_label("settings.json.bak"), "settings.json.bak");
        assert_eq!(backup_label(""), "");
    }

    #[test]
    fn the_hijack_line_names_whoever_holds_the_port_and_what_it_does_to_the_hooks() {
        let full = Hijack {
            port: 47_201,
            pid: Some(812),
            command: Some("node /usr/local/bin/other".into()),
        };
        let line = hijack_line(&full);
        assert!(line.contains("47201"));
        assert!(line.contains("pid 812"));
        assert!(line.contains("node /usr/local/bin/other"));
        assert!(line.contains("bearer token"), "§4.1's actual risk");
        // `may stall`, not `has stalled`: nothing tells the daemon whether a
        // hook has been sent there yet.
        assert!(!line.contains("has stalled"));
        // `lsof` told us nothing: the port is still somebody's.
        let blind = Hijack {
            port: 47_201,
            pid: None,
            command: None,
        };
        assert!(hijack_line(&blind).contains("another process"));
        let pid_only = Hijack {
            port: 1,
            pid: Some(9),
            command: None,
        };
        assert!(hijack_line(&pid_only).contains("pid 9"));
    }

    #[test]
    fn a_plan_headline_and_its_change_lines_read_as_the_verb_that_made_them() {
        assert_eq!(
            plan_headline(Verb::Install, "claude"),
            "Install \u{2014} Claude Code"
        );
        assert_eq!(
            plan_headline(Verb::Uninstall, "claude-desktop"),
            "Remove \u{2014} Claude Desktop"
        );
        assert_eq!(
            plan_headline(Verb::Repair, "codex"),
            "Repair \u{2014} Codex CLI"
        );
        assert_eq!(
            change_line(&Change {
                path: "/Users/t/.claude/settings.json".into(),
                action: "update".into(),
            }),
            "update /Users/t/.claude/settings.json"
        );
        assert!(NOTHING_TO_WRITE.contains("already"));
    }

    #[test]
    fn the_automation_card_offers_settings_only_where_macos_will_not_prompt_again() {
        let (word, tone, help) = automation_line(Some("granted"));
        assert_eq!((word, tone), ("granted", Tone::Green));
        assert!(help.contains("inject"));
        let (word, tone, help) = automation_line(Some("denied"));
        assert_eq!((word, tone), ("denied", Tone::Red));
        assert!(help.contains("System Settings"), "the only recourse (§7.8)");
        // Not asked yet, and a word this build does not know, are the same
        // thing to a card that cannot offer anything: unknown.
        for unknown in [Some("unknown"), None, Some("something_new")] {
            let (word, tone, _) = automation_line(unknown);
            assert_eq!((word, tone), ("unknown", Tone::Gray), "{unknown:?}");
        }
        assert!(AUTOMATION_SETTINGS.starts_with("x-apple.systempreferences:"));
        assert!(AUTOMATION_SETTINGS.contains("Privacy_Automation"));
    }

    #[test]
    fn a_token_row_reads_its_scopes_its_life_and_what_revoking_it_would_break() {
        let mut token = TokenRow {
            name: "hooks".into(),
            scopes: vec!["hooks:ingest".into()],
            created: Some("2026-09-20T10:00:00Z".into()),
            expires: None,
            embedded_in: vec!["~/.claude/settings.json".into()],
            client: None,
        };
        assert_eq!(token_scopes_line(&token), "hooks:ingest");
        assert_eq!(
            token_life_line(&token),
            "created 2026-09-20T10:00:00Z \u{b7} no expiry"
        );
        let embedded = token_embedded_line(&token).expect("the hooks token is embedded");
        assert!(embedded.contains("~/.claude/settings.json"));
        assert!(embedded.contains("Repair"), "and how to get the hooks back");
        token.expires = Some("2026-12-01T00:00:00Z".into());
        assert!(token_life_line(&token).contains("expires 2026-12-01T00:00:00Z"));
        // A token with nothing granted is worth seeing as such.
        token.scopes.clear();
        token.embedded_in.clear();
        token.created = None;
        assert_eq!(token_scopes_line(&token), "no scopes");
        assert_eq!(token_embedded_line(&token), None);
        assert!(token_life_line(&token).contains("unrecorded"));
        // The scopes a new token may be given are the daemon's own words.
        let words = scope_words();
        assert!(words.contains(&"observe".to_string()));
        assert!(words.contains(&"observe:content".to_string()));
        assert!(words.contains(&"hooks:ingest".to_string()));
        assert_eq!(words.len(), deck_model::Scope::ALL.len());
        // And the toast about one names it and never carries it.
        assert!(TOKEN_SHOWN_ONCE.contains("only place"));
        let toast = token_made_toast("Created", "script");
        assert!(toast.contains("script") && toast.contains("shown once"));
    }

    // ---- the Service screen ------------------------------------------

    #[test]
    fn the_service_headline_and_the_port_line_never_claim_a_port_the_daemon_lost() {
        let info = Info {
            daemon: Some("0.1.0".into()),
            port: Some(47_201),
            sessions: Some(3),
            ..Info::default()
        };
        assert_eq!(
            service_headline(true, &info),
            "Running \u{b7} deckd 0.1.0 \u{b7} 3 sessions"
        );
        assert!(service_headline(true, &Info::default()).contains("an unknown version"));
        assert!(service_headline(false, &info).starts_with("Not running"));
        let one = Info {
            sessions: Some(1),
            ..info.clone()
        };
        assert!(service_headline(true, &one).ends_with("1 session"));

        let config = ApiConfig {
            port: Some(47_201),
            ..ApiConfig::default()
        };
        assert_eq!(
            port_line(&info, &config),
            "http://127.0.0.1:47201 \u{b7} listening"
        );
        // §4.1: while the port is somebody else's, `service.info` answers no
        // port. The screen must not put the configured one there as if it
        // were held.
        let lost = Info {
            port: None,
            ..info.clone()
        };
        let line = port_line(&lost, &config);
        assert!(
            line.contains("47201") && line.contains("not held"),
            "{line}"
        );
        assert!(!line.contains("listening"));
        assert!(port_line(&lost, &ApiConfig::default()).contains("socket only"));
    }

    #[test]
    fn the_degraded_banner_tells_a_lost_bind_from_a_stranger_that_answers() {
        let mut service = deck_model::ServiceState {
            port: 47_201,
            ..deck_model::ServiceState::default()
        };
        assert_eq!(degraded_banner(&service), None);
        service.degraded = Some("port_in_use".into());
        let bound = degraded_banner(&service).unwrap();
        assert!(bound.contains("already bound") && bound.contains("47201"));
        service.degraded = Some("hooks_hijack_risk".into());
        let held = degraded_banner(&service).unwrap();
        assert!(held.contains("a process that answers"));
        assert!(held.contains("Change the port"));
        service.degraded = Some("shutdown".into());
        assert!(degraded_banner(&service).unwrap().contains("shutting down"));
        // A reason this build has never seen is still said out loud.
        service.degraded = Some("something_new".into());
        assert!(degraded_banner(&service).unwrap().contains("something_new"));
    }

    #[test]
    fn the_curl_example_is_the_authenticated_door_and_names_the_live_port() {
        let curl = curl_example(Some(47_202));
        assert!(curl.contains("http://127.0.0.1:47202/v1/rpc"));
        assert!(curl.contains("Authorization: Bearer"));
        assert!(curl.contains("\"method\":\"state.get\""));
        // `/v1/info` is the one unauthenticated route (§4.1), so an example
        // built on it would prove nothing about a token.
        assert!(!curl.contains("/v1/info"));
        assert!(!curl.contains("--unix-socket"), "the socket is not HTTP");
        // No port held: the default is still a usable example.
        assert!(curl_example(None).contains("47201"));
        assert_eq!(
            origins_line(&[]),
            "none \u{2014} a browser Origin header is refused 403 (the default)"
        );
        assert_eq!(
            origins_line(&["http://localhost:5173".to_string()]),
            "http://localhost:5173"
        );
    }

    #[test]
    fn the_level_filter_is_the_same_substring_test_the_daemon_runs() {
        let line = "2026-09-22T10:00:00Z  WARN deckd::api: port_in_use";
        assert!(log_level_matches(line, None), "no filter keeps everything");
        assert!(log_level_matches(line, Some("WARN")));
        assert!(
            log_level_matches(line, Some("warn")),
            "case does not matter"
        );
        assert!(!log_level_matches(line, Some("ERROR")));
        assert_eq!(LOG_LEVELS.len(), 5);
        assert_eq!(LOG_LEVELS[0], "TRACE");
        assert_eq!(LOG_LEVELS[4], "ERROR");
    }

    #[test]
    fn redaction_masks_quoted_content_named_fields_and_the_home_directory() {
        let line = "2026-09-22T10:00:00Z DEBUG deckd::core: card.set agent=ag_1 \
                    text=\"Bash: psql -f migrate.sql\" cwd=/Users/tony/code/api rev=1202";
        let masked = redact_line(line, Some("/Users/tony"));
        assert!(!masked.contains("psql"), "{masked}");
        assert!(!masked.contains("migrate.sql"), "{masked}");
        assert!(!masked.contains("/Users/tony"), "{masked}");
        assert!(!masked.contains("code/api"), "{masked}");
        // What is left is the structure: the level, the target, the event
        // and the ids, which is the whole point of reading a log.
        assert!(masked.contains("DEBUG"));
        assert!(masked.contains("deckd::core"));
        assert!(masked.contains("card.set"));
        assert!(masked.contains("agent=ag_1"));
        assert!(masked.contains("rev=1202"));
        assert!(masked.contains("text=\"\u{2026}\""));
        assert!(masked.contains("cwd=\u{2026}"));
        // With no home to mask, the path stays and the content still goes:
        // the two halves are independent, and the toggle is what turns the
        // whole thing off (the view simply does not call this).
        let no_home = redact_line(line, None);
        assert!(no_home.contains("/Users/tony/code/api") || no_home.contains("cwd=\u{2026}"));
        assert!(!no_home.contains("psql"));
        // An unterminated quote masks to the end of the line, which is the
        // safe way round.
        let torn = redact_line("INFO something text=\"half a prom", None);
        assert!(!torn.contains("half a prom"), "{torn}");
        // Nothing to mask is the line itself, spacing and all.
        let plain = "2026-09-22T10:00:00Z  INFO deckd: ready rev=7";
        assert_eq!(redact_line(plain, None), plain);
        for field in CONTENT_FIELDS {
            let masked = redact_line(&format!("INFO {field}=secretish"), None);
            assert_eq!(masked, format!("INFO {field}=\u{2026}"));
        }
    }

    #[test]
    fn a_missing_method_says_which_one_and_that_nothing_happened() {
        for method in ["config.set", "service.stop", "service.restart"] {
            let line = missing_method_line(method);
            assert!(line.contains(method));
            assert!(line.contains("nothing happened"));
            // It must never read as a success or as the operator's fault.
            assert!(!line.contains("stopped"));
            assert!(!line.contains("restarted"));
        }
    }

    #[test]
    fn launchctl_print_is_read_for_the_three_literals_launchd_promises() {
        let running = "ai.cohdl.openwalkietalkie.deckd = {\n\tactive count = 1\n\t\
                       state = running\n\tpid = 1234\n\tlast exit code = 0\n}";
        assert_eq!(launchd_state(running), Launchd::Running(Some(1234)));
        assert!(launchd_line(&Launchd::Running(Some(1234))).contains("pid 1234"));
        let no_pid = "state = running\n";
        assert_eq!(launchd_state(no_pid), Launchd::Running(None));
        let waiting = "state = waiting\npid = 0\n";
        assert_eq!(launchd_state(waiting), Launchd::Loaded);
        assert!(launchd_line(&Launchd::Loaded).contains("not running"));
        let gone = "Could not find service \"ai.cohdl.openwalkietalkie.deckd\" in domain for login";
        assert_eq!(launchd_state(gone), Launchd::NotLoaded);
        assert!(launchd_line(&Launchd::NotLoaded).contains("not registered"));
        // `launchctl` could not even be spawned: that is this app's failure
        // and must not be reported as "not registered".
        assert_eq!(launchd_state("No such file or directory"), Launchd::Unknown);
        assert!(launchd_line(&Launchd::Unknown).contains("said nothing this app can read"));
        // And the line that keeps the daemon's agent and this window's own
        // login item apart, which are two objects and not one.
        assert!(LOGIN_ITEM_NOTE.contains("different thing"));
        assert!(LOGIN_ITEM_NOTE.contains("Welcome"));
    }
}
