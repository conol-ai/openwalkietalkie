//! The **Integrations** screen (§8.2's Integrations row): one row per party,
//! the "What this writes" diff before anything is written, and the tokens.
//!
//! Six parties, from two different places, because the design's six are not
//! one thing. Five are `integrations.*` targets — files deckd writes and can
//! un-write — and the sixth, **Ghostty Automation**, is a TCC grant that
//! nothing installs: `perm.status` reports what the injector learned by
//! *trying* (§5.5), so its row offers System Settings and never an Install
//! button it could not honour.
//!
//! Two rules this module exists to keep.
//!
//! **Nothing is written without the diff.** Install, Update, Remove and
//! Repair all run `dry_run:true` first and put the plan in a dialog; the
//! second click is what applies it. For Claude that dialog also carries
//! [`present::ALLOWLIST_NOTE`], on every plan and not only on the one that
//! creates the key: `allowedHttpHookUrls` is a machine-wide switch, and the
//! operator has to read that **before** installing rather than afterwards.
//!
//! **A created token's plaintext lives in exactly one place and dies there.**
//! It is held by [`Shown`] in this pane, never in the mirror, never in
//! `HostState`, and never in a `String` that a toast or a log line could
//! carry — `Secret` has no `Clone`, no `Display` and a `Debug` that prints a
//! length. Its dialog is the window's own (see [`crate::shell`]) and reads
//! the value from here while it is drawn, so there is no second copy; Escape
//! does not close it, because closing it is the end of the value.
use crate::present;
use crate::shell::Shell;
use crate::style::{palette, Palette};
use crate::ui::{self, frag, ic, Btn, Row, Severity, Switch, Tint, Type as _};
use deck_gui_core::admin::{Hijack, Plan, Report, Secret, TokenRow, Verb};
use deck_gui_core::UiAction;
use gpui::{
    div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Div, Entity, SharedString,
    Stateful, Window,
};
use gpui_component::input::InputState;
use gpui_component::{h_flex, v_flex};
use serde_json::Value;

/// The plaintext of a token this window just created, while it is on screen.
///
/// It is a type and not an `Option<String>` so that the one thing that must
/// never happen — the value in a formatted string — cannot be written by
/// accident: `Shown` has no `Display` and its `Debug` prints the token's
/// **name** and its length.
pub struct Shown {
    /// What it is called, which is safe to say anywhere.
    pub name: String,
    /// Which call produced it, for the heading: `Created` or `Rotated`.
    pub verb: &'static str,
    secret: Secret,
}

impl Shown {
    pub fn new(name: impl Into<String>, verb: &'static str, secret: Secret) -> Shown {
        Shown {
            name: name.into(),
            verb,
            secret,
        }
    }

    /// The plaintext, borrowed for exactly as long as it is being drawn or
    /// copied.
    pub fn reveal(&self) -> &str {
        self.secret.reveal()
    }
}

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shown")
            .field("name", &self.name)
            .field("verb", &self.verb)
            .field("secret", &self.secret)
            .finish()
    }
}

/// Dismiss the one-time plaintext. `true` when there was one to dismiss.
///
/// This is the whole of "shown once": the value is dropped here (and
/// overwritten as it goes, [`Secret`]), and the daemon keeps only a hash, so
/// there is no second way to it from anywhere.
pub fn close(shown: &mut Option<Shown>) -> bool {
    shown.take().is_some()
}

/// A plan the operator has been shown and has not yet accepted.
#[derive(Clone, Debug)]
pub struct Pending {
    pub verb: Verb,
    pub target: String,
    pub trust: bool,
    pub plan: Plan,
}

/// The Integrations screen's own view state. None of it is the mirror:
/// `integrations.status`, `perm.status` and `token.list` are calls, and
/// nothing the daemon emits folds any of them into a `Snapshot`.
pub struct IntegrationsPane {
    reports: Vec<Report>,
    claude_channels: Vec<(String, bool)>,
    /// Helpers from a previous install that the daemon keeps refusing.
    claude_channels_stale: usize,
    warnings: Vec<String>,
    /// `perm.status`'s `automation.ghostty`: `granted`, `denied`, `unknown`.
    ghostty: Option<String>,
    tokens: Vec<TokenRow>,
    /// Why the last read failed, if it did. Shown instead of an empty
    /// screen: "no integrations" and "we could not ask" are different facts.
    error: Option<String>,
    asked: bool,
    busy: bool,
    /// The plan in the dialog, kept here so that **Write these changes**
    /// applies the plan the operator is looking at and not one a later
    /// click re-computed.
    pending: Option<Pending>,
    /// Whether the plan dialog is up. A re-plan (the trust switch) keeps it
    /// up while the new plan is computed.
    plan_open: bool,
    /// The one-time plaintext. See the module note.
    shown: Option<Shown>,
    /// The parties whose details are unfolded.
    expanded: Vec<String>,
    name: Entity<InputState>,
    scopes: Vec<String>,
}

impl IntegrationsPane {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> IntegrationsPane {
        let name = cx
            .new(|cx| InputState::new(window, cx).placeholder("A name for the new token\u{2026}"));
        IntegrationsPane {
            reports: Vec::new(),
            claude_channels: Vec::new(),
            claude_channels_stale: 0,
            warnings: Vec::new(),
            ghostty: None,
            tokens: Vec::new(),
            error: None,
            asked: false,
            busy: false,
            pending: None,
            plan_open: false,
            shown: None,
            expanded: Vec::new(),
            name,
            // `observe` is the least a token can usefully have, and the one
            // scope that gives a script nothing it could break.
            scopes: vec!["observe".to_string()],
        }
    }

    /// The row for `target`, or `None` when the daemon did not report it.
    /// `pub(crate)` because the Welcome screen's third step lists the same
    /// rows: one `integrations.status`, two screens drawing it.
    pub(crate) fn report(&self, target: &str) -> Option<&Report> {
        self.reports.iter().find(|r| r.target == target)
    }

    /// Whether a token's plaintext is on screen.
    pub(crate) fn showing_token(&self) -> bool {
        self.shown.is_some()
    }

    /// Whether the plan dialog is up.
    pub(crate) fn plan_open(&self) -> bool {
        self.plan_open && self.pending.is_some()
    }

    /// Put a token on screen, or take it off, for `--shots`.
    pub(crate) fn stage_token(&mut self, shown: Option<Shown>) {
        self.shown = shown;
    }

    /// Unfold exactly `targets`' details, for `--shots`.
    pub(crate) fn stage_expanded(&mut self, targets: &[&str]) {
        self.expanded = targets.iter().map(|t| (*t).to_string()).collect();
    }

    /// Whether the Codex trust switch is on for this target and verb. It is
    /// kept on the pending plan, so it survives a re-plan and starts off.
    fn pending_trust(&self, target: &str, verb: Verb) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.target == target && p.verb == verb && p.trust)
    }
}

/// Fold `target`'s details if they are open, unfold them if not.
fn toggle(expanded: &mut Vec<String>, target: &str) {
    if let Some(i) = expanded.iter().position(|t| t == target) {
        expanded.remove(i);
    } else {
        expanded.push(target.to_string());
    }
}

impl Shell {
    /// Ask the daemon for everything this screen draws.
    ///
    /// Three calls, not one, because they are three methods; and only on
    /// arrival or on **Re-read**, never on the repaint tick: `detect:true`
    /// runs `claude --version` and Codex's `hooks/list`, and a screen that
    /// did that every second would be a load generator.
    pub(crate) fn refresh_integrations(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.integrations.busy || (self.integrations.asked && !force) {
            return;
        }
        self.integrations.busy = true;
        self.integrations.error = None;
        self.ask(
            UiAction::IntegrationsStatus { detect: true },
            window,
            cx,
            |this, result, _window, _cx| {
                this.integrations.busy = false;
                match result {
                    Ok(value) => {
                        let (reports, warnings) = Report::list(&value);
                        this.integrations.claude_channels = value["claude_channels"].as_array()
                            .into_iter().flatten().filter_map(|row| Some((
                                row["session"].as_str()?.to_owned(), row["state"] == "ready"
                            ))).collect();
                        this.integrations.claude_channels_stale =
                            value["claude_channels_stale"].as_u64().unwrap_or(0) as usize;
                        this.integrations.reports = reports;
                        this.integrations.warnings = warnings;
                        // Only an answer counts as having asked, so opening
                        // the screen again after a failure asks again
                        // instead of showing the error for ever.
                        this.integrations.asked = true;
                    }
                    Err(e) => this.integrations.error = Some(format!("{e}")),
                }
            },
        );
        self.ask(
            UiAction::PermStatus,
            window,
            cx,
            |this, result, _window, _cx| {
                this.integrations.ghostty = result.ok().and_then(|v| {
                    v.get("automation")?
                        .get("ghostty")?
                        .as_str()
                        .map(str::to_string)
                });
            },
        );
        self.refresh_tokens(window, cx);
    }

    fn refresh_tokens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(
            UiAction::TokenList,
            window,
            cx,
            |this, result, _window, _cx| {
                if let Ok(value) = result {
                    this.integrations.tokens = TokenRow::list(&value);
                }
            },
        );
    }

    // ---- the screen ----------------------------------------------------

    pub(crate) fn render_integrations(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let hijack = Hijack::of(&self.state.mirror().service);
        let parties: Vec<AnyElement> = present::TARGETS
            .iter()
            .map(|target| self.render_party(target, hijack.as_ref(), dim, cx))
            .collect();
        let busy = self.integrations.busy;
        let reread = if busy {
            h_flex()
                .h(px(32.))
                .px(px(10.))
                .gap(px(6.))
                .ts(12.5, 16.)
                .text_color(p.muted)
                .child(ui::spinner(14., p.muted))
                .child("Reading\u{2026}")
                .into_any_element()
        } else {
            Btn::new("integrations-refresh", "Re-read")
                .quiet()
                .icon(ic::REFRESH)
                .disabled(dim)
                .tooltip("Ask the service again; this runs each CLI's version check")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.refresh_integrations(true, window, cx);
                }))
                .into_any_element()
        };
        ui::page(
            "integrations",
            &self.page_scroll,
            self.content_width,
            ui::PAGE_MAX,
            v_flex()
                .when(dim, |this| this.opacity(0.55))
                .child(ui::page_header(
                    "Integrations",
                    Some(
                        div()
                            .child(
                                "The hooks each agent CLI reaches the deck through, and the \
                                 tokens scripts use. Nothing is written without showing you the \
                                 diff first.",
                            )
                            .into_any_element(),
                    ),
                    vec![reread],
                    &p,
                ))
                .children(
                    hijack
                        .as_ref()
                        .map(|hijack| self.render_hijack(hijack, dim, cx)),
                )
                .children(self.integrations.error.as_ref().map(|why| {
                    div().mt(px(20.)).child(ui::strip(
                        Severity::Danger,
                        Some("Could not read the integrations".into()),
                        Some(div().child(why.clone()).into_any_element()),
                        vec![Btn::new("integrations-retry", "Try again")
                            .small()
                            .disabled(dim)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_integrations(true, window, cx);
                            }))
                            .into_any_element()],
                        &p,
                    ))
                }))
                .children(self.integrations.warnings.iter().map(|w| {
                    div().mt(px(12.)).child(ui::strip(
                        Severity::Warning,
                        None,
                        Some(div().child(w.clone()).into_any_element()),
                        Vec::new(),
                        &p,
                    ))
                }))
                .child(ui::section("Agent CLIs", ui::card(parties, &p), &p))
                .child(self.render_claude_channels(dim, cx))
                .child(ui::section(
                    "Grants",
                    ui::card(vec![self.render_automation(dim, cx)], &p),
                    &p,
                ))
                .child(self.render_tokens(dim, cx))
                .child(self.render_token_form(dim, cx)),
        )
    }

    fn render_claude_channels(&self, dim: bool, cx: &Context<Self>) -> impl IntoElement {
        let p = *palette(cx);
        let ready = self
            .integrations
            .claude_channels
            .iter()
            .filter(|(_, ready)| *ready)
            .count();
        let status = if !self.integrations.asked {
            "Connection status has not been read yet.".to_string()
        } else if ready > 0 {
            format!("Channels ready in {ready} session(s). Other sessions use the cross-session inbox.")
        } else {
            "Cross-session inbox active. No session has confirmed a Channels connection yet."
                .to_string()
        };
        let steps = [
            "1. Use an up-to-date Claude Code, then Install or Repair its integration above.",
            "2. Run the command below in your project's terminal. To keep an existing conversation, exit it first and add --resume followed by its session ID.",
            "3. Accept Claude's local development confirmation. Allow the OpenWalkieTalkie MCP server and its acknowledge connection check if prompted.",
            "4. Press Re-read. Once your session shows Channels ready, select it on the deck and talk.",
        ];
        let mut body = v_flex().p(px(16.)).gap(px(10.)).ts(12.5, 19.)
            .child(div().text_color(p.muted).child(status))
            .children(steps.into_iter().map(|step| div().child(step)))
            .child(div().mono().ts(11., 17.).child(present::CLAUDE_CHANNEL_COMMAND))
            .child(h_flex().flex_wrap().gap(px(8.))
                .child(Btn::new("claude-channel-copy", "Copy launch command").small().disabled(dim)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(present::CLAUDE_CHANNEL_COMMAND.to_string()));
                    })))
                .child(Btn::new("claude-channel-docs", "Channels documentation").small().quiet()
                    .on_click(cx.listener(|_, _, _, cx| { cx.open_url(present::CLAUDE_CHANNEL_DOCS); })))
                .child(Btn::new("claude-channel-refresh", "Re-read").small().quiet()
                    .disabled(dim || self.integrations.busy)
                    .on_click(cx.listener(|this, _, window, cx| { this.refresh_integrations(true, window, cx); }))))
            .child(div().text_color(p.muted).child(
                "Channels are a Claude research preview. The development flag enables this custom plugin for one interactive session; organization policy still applies. If the check stalls, use /mcp to verify the server, then ask Claude to run OpenWalkieTalkie's check_connection tool."
            ))
            .child(div().text_color(p.muted).child(
                "Until connected, messages use the existing inbox and Claude may label them as coming from another session. To turn Channels off, restart Claude without the flag."
            ));
        if self.integrations.claude_channels_stale > 0 {
            body = body.child(div().text_color(p.muted).child(format!(
                "{} Claude session(s) are running a channel helper from an older OpenWalkieTalkie install and cannot connect. Restart those sessions to reconnect.",
                self.integrations.claude_channels_stale
            )));
        }
        for (session, ready) in &self.integrations.claude_channels {
            body = body.child(
                Row::new()
                    .title(self.state.mirror().agents.values()
                        .find(|agent| agent.session.key.as_deref() == Some(format!("claude:{session}").as_str()))
                        .map(|agent| agent.name.clone())
                        .unwrap_or_else(|| format!("Claude · {}", session.chars().take(12).collect::<String>())))
                    .meta(if *ready {
                        "Channels ready"
                    } else {
                        "Waiting for connection check · inbox fallback"
                    }),
            );
        }
        ui::section(
            "Claude voice messages",
            ui::card(vec![body.into_any_element()], &p),
            &p,
        )
    }

    /// §4.1's `hooks_hijack_risk`, on the screen the contract puts it on.
    /// It is read off the mirror and not off `integrations.status`, which
    /// §5.7 says cannot see it. The repair lives on the Service screen, so
    /// the button goes there.
    fn render_hijack(&self, hijack: &Hijack, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        div().mt(px(20.)).child(ui::strip(
            Severity::Danger,
            Some(format!("Port {} is not this service's", hijack.port).into()),
            Some(div().child(present::hijack_line(hijack)).into_any_element()),
            vec![Btn::new("hijack-change-port", "Change the port\u{2026}")
                .small()
                .disabled(dim)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.go(crate::shell::Screen::Service, window, cx);
                }))
                .into_any_element()],
            &p,
        ))
    }

    /// One `integrations.*` party.
    fn render_party(
        &self,
        target: &'static str,
        hijack: Option<&Hijack>,
        dim: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let p = *palette(cx);
        let (mark, color) = ui::target_mark(target, &p);
        let Some(report) = self.integrations.report(target) else {
            // A party the daemon did not report at all. It still gets a
            // row: an integration that quietly vanished from the screen is
            // worse than one that says it was not answered for.
            return Row::new()
                .tile(ui::tile(mark, p.faint, &p))
                .title(present::party_name(target).to_string())
                .meta(if self.integrations.asked {
                    "This daemon did not report this target."
                } else {
                    "Not read yet."
                })
                .into_any_element();
        };
        let status = report.status.clone();
        let installed = status.installed();
        let tone = present::status_tone(&status);
        // §4.1: the hijack is about Claude's **http** hooks, so it is said on
        // Claude's row and not on every row.
        let hijacked = hijack.is_some() && present::allowlist_applies(target);
        let expanded = self.integrations.expanded.iter().any(|t| t == target);
        let busy = dim || self.integrations.busy;
        let id = |stem: &str| -> SharedString { SharedString::from(format!("{stem}-{target}")) };
        // Every verb goes through the plan first: §5.7's "What this writes
        // shows the diff before install", and the contract's requirement
        // that the `allowedHttpHookUrls` note is read before installing.
        // Nothing on this row applies anything.
        let plan = move |verb: Verb| {
            cx.listener(
                move |this: &mut Shell,
                      _: &gpui::ClickEvent,
                      window: &mut Window,
                      cx: &mut Context<Shell>| {
                    this.plan_integration(verb, target, window, cx);
                },
            )
        };
        let mut row = Row::new()
            .tile(ui::tile(mark, color, &p))
            .title_el(
                h_flex()
                    .gap(px(8.))
                    .child(present::party_name(target).to_string())
                    .child(ui::badge(
                        present::status_word(&status).to_string(),
                        Tint::of(tone),
                        &p,
                    ))
                    .when(hijacked, |this| {
                        this.child(ui::badge("hooks_hijack_risk", Tint::Danger, &p))
                    }),
            )
            .meta(present::detected_line(target, report.detected.as_ref()));
        row = if installed {
            row.trailing(
                Btn::new(id("repair"), "Repair")
                    .small()
                    .quiet()
                    .disabled(busy)
                    .on_click(plan(Verb::Repair)),
            )
            .trailing(
                Btn::new(id("update"), "Update")
                    .small()
                    .quiet()
                    .disabled(busy)
                    .on_click(plan(Verb::Install)),
            )
            .trailing(
                Btn::new(id("remove"), "Remove")
                    .small()
                    .danger_quiet()
                    .disabled(busy)
                    .on_click(plan(Verb::Uninstall)),
            )
        } else {
            row.trailing(
                Btn::new(id("install"), "Install")
                    .small()
                    .disabled(busy)
                    .on_click(plan(Verb::Install)),
            )
        };
        row = row.trailing(
            Btn::icon_only(
                id("more"),
                if expanded {
                    ic::CHEVRON_UP
                } else {
                    ic::CHEVRON_DOWN
                },
            )
            .small()
            .tooltip(if expanded {
                "Hide the details"
            } else {
                "Show what it writes and keeps"
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(&mut this.integrations.expanded, target);
                cx.notify();
            })),
        );
        if expanded {
            row = row
                .detail(
                    div()
                        .ts(12.5, 18.)
                        .text_color(p.text)
                        .child(present::status_help(&status)),
                )
                .detail(ui::kv("Writes", present::party_what(target), &p))
                .detail(ui::kv(
                    "Last event",
                    present::last_event_line(report.last_event_at.as_deref()),
                    &p,
                ));
            if let Some(detail) = &report.detail {
                row = row.detail(ui::hint(detail.clone(), &p));
            }
            if !report.files.is_empty() {
                row = row.detail(ui::kv(
                    "Files",
                    v_flex().gap(px(2.)).children(
                        report
                            .files
                            .iter()
                            .map(|f| div().mono().ts(12., 18.).child(f.clone())),
                    ),
                    &p,
                ));
            }
            if !report.backups.is_empty() {
                row = row.detail(ui::kv(
                    "Backups",
                    self.render_backups(target, &report.backups, &p),
                    &p,
                ));
            }
            if !report.limits.is_empty() {
                row = row.detail(ui::kv(
                    "Caveats",
                    v_flex().gap(px(4.)).children(
                        report
                            .limits
                            .iter()
                            .map(|l| div().text_color(p.muted).child(l.clone())),
                    ),
                    &p,
                ));
            }
        }
        row.into_any_element()
    }

    /// §8.2's "backups with Restore".
    ///
    /// **There is no `integrations.restore` method**: the daemon's table has
    /// `status`, `install`, `uninstall` and `repair` and nothing else, and
    /// `restore_all` is internal to a failed `apply`. So this lists what is
    /// there, shows it in Finder, and says what putting one back takes —
    /// rather than drawing a Restore button that would answer `-32601`.
    fn render_backups(&self, target: &str, backups: &[String], p: &Palette) -> Div {
        let rows: Vec<Div> = backups
            .iter()
            .enumerate()
            .rev()
            .map(|(n, path)| {
                let reveal = path.clone();
                let copy = path.clone();
                h_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .mono()
                            .ts(12., 18.)
                            .child(present::backup_label(path).to_string()),
                    )
                    .child(
                        Btn::icon_only(
                            SharedString::from(format!("backup-reveal-{target}-{n}")),
                            ic::FOLDER,
                        )
                        .small()
                        .tooltip("Show in Finder")
                        .on_click(move |_, _, cx: &mut App| {
                            cx.reveal_path(std::path::Path::new(&reveal));
                        }),
                    )
                    .child(
                        Btn::icon_only(
                            SharedString::from(format!("backup-copy-{target}-{n}")),
                            ic::COPY,
                        )
                        .small()
                        .tooltip("Copy the path")
                        .on_click(move |_, _, cx: &mut App| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        }),
                    )
            })
            .collect();
        v_flex()
            .gap(px(2.))
            .children(rows)
            .child(div().mt(px(4.)).child(ui::hint(
                "Backups are renames, so each one is inode- and mode-exact. This daemon has no \
             restore method: putting one back is a move on disk, from the folder Finder shows.",
                p,
            )))
    }

    // ---- "What this writes" (§5.7) -------------------------------------

    /// Run the verb as a **dry run** and put the answer in the dialog.
    /// Nothing is written by this.
    ///
    /// `pub(crate)` so the Welcome screen's Install goes through exactly
    /// this path: the diff comes first there too, and a hook installed from
    /// the first-run flow is installed by the same code as one installed
    /// from the Integrations screen.
    pub(crate) fn plan_integration(
        &mut self,
        verb: Verb,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = target.to_string();
        let trust = verb.takes_trust() && self.integrations.pending_trust(&target, verb);
        self.integrations.busy = true;
        self.ask(
            UiAction::Integrations {
                verb,
                target: target.clone(),
                trust,
                dry_run: true,
            },
            window,
            cx,
            move |this, result, window, cx| {
                this.integrations.busy = false;
                match result {
                    Ok(value) => {
                        // A daemon that ignored `dry_run` would have written
                        // the files already; saying so is the only honest
                        // answer, and there is nothing to preview.
                        if !Plan::is_dry_run(&value) {
                            this.integrations.pending = None;
                            this.integrations.plan_open = false;
                            this.toast(
                                "This daemon answered that call without honouring dry_run; \
                                 nothing is being previewed.",
                                window,
                                cx,
                            );
                            this.refresh_integrations(true, window, cx);
                            return;
                        }
                        let plan = Plan::parse(&value);
                        let reopened = this.integrations.plan_open();
                        this.integrations.pending = Some(Pending {
                            verb,
                            target,
                            trust,
                            plan,
                        });
                        this.integrations.plan_open = true;
                        if !reopened {
                            this.dialog_opened(window);
                        }
                    }
                    Err(e) => this.toast(format!("Could not plan that: {e}"), window, cx),
                }
            },
        );
    }

    /// Close the plan without writing anything.
    pub(crate) fn cancel_plan(&mut self, cx: &mut Context<Self>) {
        self.integrations.plan_open = false;
        self.integrations.pending = None;
        cx.notify();
    }

    /// The second click: apply the plan the operator is looking at.
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.integrations.plan_open = false;
        let Some(pending) = self.integrations.pending.take() else {
            return;
        };
        self.integrations.busy = true;
        let what = present::plan_headline(pending.verb, &pending.target);
        self.ask(
            UiAction::Integrations {
                verb: pending.verb,
                target: pending.target,
                trust: pending.trust,
                dry_run: false,
            },
            window,
            cx,
            move |this, result, window, cx| {
                this.integrations.busy = false;
                match result {
                    Ok(value) => {
                        this.toast(receipt_line(&what, &value), window, cx);
                        // The status the row shows is now stale by
                        // definition: the files it describes were just
                        // rewritten.
                        this.refresh_integrations(true, window, cx);
                    }
                    Err(e) => this.toast(format!("{what} failed: {e}"), window, cx),
                }
            },
        );
    }

    /// Codex's `/hooks` trust opt-in (§5.2), switched in the dialog.
    /// Changing it changes what would be written, so the plan is computed
    /// again and the dialog shows the new one: a diff that did not match the
    /// switch above it would be a diff of something else.
    fn set_trust(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.integrations.pending.as_mut() else {
            return;
        };
        pending.trust = trust;
        let (verb, target) = (pending.verb, pending.target.clone());
        self.plan_integration(verb, &target, window, cx);
    }

    /// The "What this writes" dialog (§5.7).
    pub(crate) fn render_plan_dialog(&self, cx: &Context<Self>) -> Option<Div> {
        let p = *palette(cx);
        let Pending {
            verb,
            target,
            trust,
            plan,
        } = self.integrations.pending.as_ref()?;
        let allowlist = present::allowlist_applies(target);
        // Codex only, and never on an uninstall: §5.2's trust gate records
        // hook hashes, and removing a hook records nothing.
        let trustable = verb.takes_trust() && target == "codex";
        let noop = plan.is_noop();
        let busy = self.integrations.busy;
        let heading = |text: &'static str| {
            div()
                .mt(px(18.))
                .mb(px(6.))
                .ts(12., 16.)
                .medium()
                .text_color(p.muted)
                .child(text)
        };
        let changes: Vec<Div> = plan
            .changes
            .iter()
            .map(|change| {
                let tint = match change.action.as_str() {
                    "create" => Tint::Success,
                    "remove" => Tint::Danger,
                    _ => Tint::Blue,
                };
                h_flex()
                    .gap(px(10.))
                    .items_start()
                    .child(div().w(px(56.)).flex_none().child(ui::badge(
                        change.action.clone(),
                        tint,
                        &p,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mono()
                            .ts(12., 18.)
                            .child(change.path.clone()),
                    )
            })
            .collect();
        let (on_cancel, on_write) = (
            cx.listener(|this, _, _, cx| this.cancel_plan(cx)),
            cx.listener(|this, _, window, cx| this.apply_pending(window, cx)),
        );
        Some(
            v_flex()
                .min_h_0()
                .child(
                    v_flex()
                        .flex_none()
                        .px(px(22.))
                        .pt(px(20.))
                        .pb(px(14.))
                        .gap(px(6.))
                        .child(
                            div()
                                .ts(15., 20.)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(present::plan_headline(*verb, target)),
                        )
                        .child(
                            div()
                                .ts(13., 19.)
                                .text_color(p.muted)
                                .child("Nothing is written until you press Write these changes."),
                        ),
                )
                .child(
                    v_flex()
                        .id("plan-body")
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(22.))
                        .pb(px(18.))
                        .child(
                            v_flex()
                                .gap(px(8.))
                                .rounded(px(10.))
                                .bg(p.wash(0.04))
                                .p(px(12.))
                                .children(changes)
                                .when(noop, |this| {
                                    this.child(ui::hint(present::NOTHING_TO_WRITE, &p))
                                }),
                        )
                        // The installer's own sentences, verbatim. One of them
                        // is the `allowedHttpHookUrls` note, and paraphrasing
                        // the daemon's account of what it is about to do would
                        // be writing a second account of it.
                        .when(!plan.notes.is_empty(), |this| {
                            this.child(heading("What that means")).child(
                                v_flex().gap(px(6.)).children(
                                    plan.notes
                                        .iter()
                                        .map(|n| div().ts(12.5, 18.).child(n.clone())),
                                ),
                            )
                        })
                        // And the standing warning, on every Claude plan —
                        // including the ones whose notes do not mention the
                        // key, because "the list already exists" still leaves
                        // this machine inside a merged allowlist.
                        .when(allowlist, |this| {
                            this.child(div().mt(px(16.)).child(ui::strip(
                                Severity::Warning,
                                None,
                                // The strip draws its own warning glyph.
                                Some(
                                    div()
                                        .child(present::ALLOWLIST_NOTE.trim_start_matches('\u{26a0}').trim_start())
                                        .into_any_element(),
                                ),
                                Vec::new(),
                                &p,
                            )))
                        })
                        .when(!plan.conflicts.is_empty(), |this| {
                            this.child(heading("Left alone, because it is not ours")).child(
                                v_flex().gap(px(4.)).children(
                                    plan.conflicts
                                        .iter()
                                        .map(|c| div().mono().ts(12., 18.).child(c.clone())),
                                ),
                            )
                        })
                        .when(!plan.next_steps.is_empty(), |this| {
                            this.child(heading("Then, on your side")).child(
                                v_flex().gap(px(6.)).children(
                                    plan.next_steps
                                        .iter()
                                        .map(|s| div().ts(12.5, 18.).child(s.clone())),
                                ),
                            )
                        })
                        .when(trustable, |this| {
                            let trust = *trust;
                            this.child(
                                h_flex()
                                    .mt(px(16.))
                                    .gap(px(12.))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().ts(13., 17.).medium().child("Record the hook hashes now"))
                                            .child(ui::hint(
                                                "Codex's /hooks trust: the hooks run without Codex asking first.",
                                                &p,
                                            )),
                                    )
                                    .child(Switch::new("plan-trust", trust).disabled(busy).on_toggle(
                                        cx.listener(|this, on: &bool, window, cx| {
                                            this.set_trust(*on, window, cx);
                                        }),
                                    )),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .px(px(22.))
                        .py(px(14.))
                        .gap(px(8.))
                        .border_t_1()
                        .border_color(p.hairline(0.08))
                        .when(busy, |this| {
                            this.child(
                                h_flex()
                                    .gap(px(6.))
                                    .ts(12., 16.)
                                    .text_color(p.muted)
                                    .child(ui::spinner(13., p.muted))
                                    .child("Planning\u{2026}"),
                            )
                        })
                        .child(div().flex_1())
                        .child(Btn::new("plan-cancel", "Cancel").quiet().on_click(on_cancel))
                        .child(
                            Btn::new("plan-write", "Write these changes")
                                .solid()
                                .disabled(noop || busy)
                                .on_click(on_write),
                        ),
                ),
        )
    }

    // ---- Ghostty Automation (§5.5, §10.4) ------------------------------

    fn render_automation(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let (status, tone, help) = present::automation_line(self.integrations.ghostty.as_deref());
        let denied = status == "denied";
        Row::new()
            .tile(ui::tile(ic::TERMINAL, p.muted, &p))
            .title_el(
                h_flex()
                    .gap(px(8.))
                    .child("Ghostty Automation")
                    .child(ui::badge(status, Tint::of(tone), &p)),
            )
            .meta_el(
                v_flex()
                    .gap(px(2.))
                    .child(help)
                    .child(div().text_color(p.faint).child(
                        "Nothing is installed for this one: it is a macOS grant, so there is no \
                         file to write and no diff to show.",
                    )),
            )
            .when(denied, |row| {
                row.trailing(
                    Btn::new("automation-settings", "Open Settings")
                        .small()
                        .outlined()
                        .icon(ic::EXTERNAL)
                        .disabled(dim)
                        .on_click(|_, _, cx: &mut App| {
                            cx.open_url(present::AUTOMATION_SETTINGS);
                        }),
                )
            })
            .into_any_element()
    }

    // ---- tokens (§4.11, §8.2) ------------------------------------------

    fn render_tokens(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let mut rows: Vec<AnyElement> = self
            .integrations
            .tokens
            .iter()
            .enumerate()
            .map(|(n, token)| self.render_token(n, token, dim, cx))
            .collect();
        if rows.is_empty() {
            rows.push(
                Row::new()
                    .tile(ui::tile(ic::KEY, p.faint, &p))
                    .title("No tokens yet")
                    .meta(
                        "The Claude hooks embed one called \u{201c}hooks\u{201d}; installing \
                         them creates it.",
                    )
                    .into_any_element(),
            );
        }
        ui::section("Tokens", ui::card(rows, &p), &p)
    }

    fn render_token(
        &self,
        n: usize,
        token: &TokenRow,
        dim: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let p = *palette(cx);
        let (rotate, revoke) = (token.name.clone(), token.name.clone());
        let mut meta = vec![
            frag(present::token_scopes_line(token)),
            frag(present::token_life_line(token)),
        ];
        if let Some(client) = &token.client {
            meta.push(frag(client.clone()));
        }
        Row::new()
            .tile(ui::tile(ic::KEY, p.muted, &p))
            .title_el(div().mono().child(token.name.clone()))
            .meta_el(
                v_flex()
                    .gap(px(2.))
                    .child(ui::meta_line(meta, &p))
                    .children(
                        present::token_embedded_line(token)
                            .map(|line| div().text_color(p.warning_muted).child(line)),
                    ),
            )
            .trailing(
                Btn::new(SharedString::from(format!("token-rotate-{n}")), "Rotate")
                    .small()
                    .quiet()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.token_call(
                            UiAction::TokenRotate {
                                name: rotate.clone(),
                            },
                            &rotate,
                            "Rotated",
                            window,
                            cx,
                        );
                    })),
            )
            .trailing(
                Btn::new(SharedString::from(format!("token-revoke-{n}")), "Revoke")
                    .small()
                    .danger_quiet()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.token_call(
                            UiAction::TokenRevoke {
                                name: revoke.clone(),
                            },
                            &revoke,
                            "Revoked",
                            window,
                            cx,
                        );
                    })),
            )
            .into_any_element()
    }

    fn render_token_form(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let chips: Vec<Stateful<Div>> =
            present::scope_words()
                .into_iter()
                .map(|scope| {
                    let on = self.integrations.scopes.contains(&scope);
                    let word = scope.clone();
                    ui::toggle_chip(SharedString::from(format!("scope-{scope}")), scope, on, &p)
                        .when(!dim, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                let had = this.integrations.scopes.contains(&word);
                                this.integrations.scopes.retain(|s| s != &word);
                                if !had {
                                    this.integrations.scopes.push(word.clone());
                                }
                                cx.notify();
                            }))
                        })
                })
                .collect();
        let ready = !dim && !self.integrations.scopes.is_empty();
        ui::section(
            "New token",
            ui::panel(&p)
                .gap(px(12.))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(ui::field(&self.integrations.name, &p).disabled(dim)),
                        )
                        .child(
                            Btn::new("token-create", "Create")
                                .solid()
                                .icon(ic::KEY)
                                .disabled(!ready)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_token(window, cx);
                                })),
                        ),
                )
                .child(h_flex().flex_wrap().gap(px(6.)).children(chips))
                .child(ui::hint(
                    "A token is the whole of what a script can do (\u{a7}4.2). Give it the least \
                     that works \u{2014} observe:raw is raw hook payloads and is almost never it.",
                    &p,
                )),
            &p,
        )
    }

    /// The one-time plaintext (§4.11), in the window's own dialog. It reads
    /// the value from [`Shown`] while it is drawn; nothing else holds it.
    pub(crate) fn render_token_dialog(&self, cx: &Context<Self>) -> Option<Div> {
        let p = *palette(cx);
        let shown = self.integrations.shown.as_ref()?;
        let plaintext = shown.reveal().to_string();
        Some(
            v_flex()
                .p(px(22.))
                .gap(px(6.))
                .child(
                    h_flex()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_none()
                                .size(px(36.))
                                .rounded(px(10.))
                                .bg(p.accent_wash())
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(ui::icon(ic::KEY, 18., p.accent)),
                        )
                        .child(
                            div()
                                .ts(15., 20.)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("{} \u{201c}{}\u{201d}", shown.verb, shown.name)),
                        ),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .ts(13., 19.)
                        .text_color(p.muted)
                        .child(present::TOKEN_SHOWN_ONCE),
                )
                .child(div().mt(px(10.)).child(ui::code_block(
                    "token-value",
                    plaintext.clone(),
                    false,
                    &p,
                )))
                .child(
                    h_flex()
                        .mt(px(16.))
                        .gap(px(8.))
                        .justify_end()
                        .child(
                            Btn::new("token-done", "Done \u{2014} I have copied it")
                                .quiet()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    close(&mut this.integrations.shown);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Btn::new("token-copy", "Copy token")
                                .solid()
                                .icon(ic::COPY)
                                .on_click(move |_, _, cx: &mut App| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        plaintext.clone(),
                                    ));
                                }),
                        ),
                ),
        )
    }

    fn create_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.integrations.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.toast("A token needs a name.", window, cx);
            return;
        }
        let scopes = self.integrations.scopes.clone();
        self.integrations.name.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.token_call(
            UiAction::TokenCreate {
                name: name.clone(),
                scopes,
                expires: None,
            },
            &name,
            "Created",
            window,
            cx,
        );
    }

    /// Every `token.*` call, in one place.
    ///
    /// The reply is read **once**, here, into a [`Secret`]; the toast is
    /// built from the token's *name* and never from the reply
    /// ([`present::token_made_toast`]), so nothing that carries the value
    /// ever becomes an ordinary `String`.
    fn token_call(
        &mut self,
        action: UiAction,
        name: &str,
        verb: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.to_string();
        self.ask(action, window, cx, move |this, result, window, cx| {
            match result {
                Ok(value) => {
                    match Secret::from_reply(&value) {
                        Some(secret) => {
                            // A new plaintext replaces an undismissed one,
                            // which drops (and overwrites) the old one —
                            // there is never more than one in this process.
                            this.integrations.shown = Some(Shown::new(&name, verb, secret));
                            this.dialog_opened(window);
                            this.toast(present::token_made_toast(verb, &name), window, cx);
                        }
                        // A revoke answers no value, and its toast must not
                        // promise one.
                        None => this.toast(
                            format!("{verb} the token \u{201c}{name}\u{201d}."),
                            window,
                            cx,
                        ),
                    }
                    this.refresh_tokens(window, cx);
                }
                Err(e) => this.toast(
                    format!("{verb} \u{201c}{name}\u{201d} failed: {e}"),
                    window,
                    cx,
                ),
            }
        });
    }
}

/// What an applied run says, from the receipt the daemon answered.
fn receipt_line(what: &str, receipt: &Value) -> String {
    let changed = receipt
        .get("changed")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let backups = receipt
        .get("backups")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    format!("{what}: {changed} file(s) written, {backups} backed up.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLAINTEXT: &str = "owt_live_9f3c1b7a55e04d2c";

    fn shown() -> Shown {
        let secret = Secret::from_reply(&json!({"token": PLAINTEXT})).expect("a created token");
        Shown::new("script", "Created", secret)
    }

    /// The exit test (contract §6): the plaintext is shown once and is gone
    /// afterwards. This is the half that does not need a window; the other
    /// half — that it is in neither the mirror nor `HostState` nor a log
    /// line — is `tests/integrations_screen.rs`.
    #[test]
    fn a_created_tokens_plaintext_is_readable_once_and_unreachable_after_it_is_dismissed() {
        let mut slot = Some(shown());
        assert_eq!(slot.as_ref().map(Shown::reveal), Some(PLAINTEXT));
        // The only formatted rendering of it there is.
        let debug = format!("{slot:?}");
        assert!(!debug.contains(PLAINTEXT), "{debug}");
        assert!(debug.contains("script"), "the name is safe to print");
        assert!(close(&mut slot), "there was one to dismiss");
        assert!(slot.is_none());
        assert!(!close(&mut slot), "and only one");
        assert!(!format!("{slot:?}").contains(PLAINTEXT));
    }

    #[test]
    fn the_toast_for_a_new_token_names_it_and_never_carries_it() {
        let toast = present::token_made_toast("Created", "script");
        assert!(toast.contains("script"));
        assert!(!toast.contains(PLAINTEXT));
        // The reply itself is the only thing that ever holds the value, and
        // it is read into a `Secret` and dropped.
        let reply = json!({"token": PLAINTEXT});
        let secret = Secret::from_reply(&reply).unwrap();
        assert_eq!(secret.reveal(), PLAINTEXT);
        assert!(!format!("{secret:?}").contains(PLAINTEXT));
    }

    #[test]
    fn an_applied_run_is_reported_from_its_receipt_and_never_guessed() {
        assert_eq!(
            receipt_line(
                "Install \u{2014} Claude Code",
                &json!({"changed": ["a", "b"], "backups": ["a.bak"]})
            ),
            "Install \u{2014} Claude Code: 2 file(s) written, 1 backed up."
        );
        // A receipt with nothing in it is still a receipt: `install` is
        // idempotent, so "0 written" is the right answer and not a failure.
        assert_eq!(
            receipt_line(
                "Repair \u{2014} Codex CLI",
                &json!({"changed": [], "backups": []})
            ),
            "Repair \u{2014} Codex CLI: 0 file(s) written, 0 backed up."
        );
        assert!(receipt_line("x", &Value::Null).contains("0 file(s)"));
    }

    #[test]
    fn a_partys_details_fold_and_unfold_one_party_at_a_time() {
        let mut expanded: Vec<String> = Vec::new();
        toggle(&mut expanded, "claude");
        toggle(&mut expanded, "codex");
        assert_eq!(expanded, vec!["claude", "codex"]);
        toggle(&mut expanded, "claude");
        assert_eq!(expanded, vec!["codex"], "folding one leaves the other open");
    }
}
