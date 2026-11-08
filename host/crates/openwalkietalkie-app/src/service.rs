//! The **Service** screen (§8.2's Service row): whether the daemon is up and
//! the three buttons that change that, the port with its degraded strip and
//! "change port + re-install hooks", the allowed origins, a `curl` example,
//! and the live log tail with a level filter and a content-redaction switch.
//!
//! Three of its buttons call methods **this daemon may not have**.
//! `config.set`, `service.restart` and `service.stop` are being added in
//! parallel (contract §3), so every one of them is written against the name
//! the contract gives it and every one of them checks for `-32601` on the
//! way back: a screen that reported "stopped" on a call the daemon never
//! answered would be claiming something that did not happen. What a missing
//! method gets instead is [`present::missing_method_line`] — which method,
//! and that nothing was done.
//!
//! **Start** is the odd one out and is not a daemon method at all: a service
//! that is not running cannot be asked to start itself, so it is
//! `launchctl kickstart -k`, the same call the strip over every screen
//! makes, run off the UI thread on the bridge.
use crate::app;
use crate::present::{self, Launchd};
use crate::shell::Shell;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Row, Severity, Switch, Type as _};
use deck_gui_core::admin::{method_missing, ApiConfig, Info, Tail};
use deck_gui_core::{ConfigValue, UiAction};
use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, Entity, Hsla, ScrollHandle, SharedString,
    Stateful, Subscription, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

/// How many lines of log the screen asks for. `log.tail` clamps its own
/// maximum; this is what fits in a pane without making the window a log
/// viewer, which is what Reveal in Finder is for.
pub const TAIL_LINES: u32 = 200;

/// The Service screen's own view state.
pub struct ServicePane {
    info: Option<Info>,
    config: ApiConfig,
    tail: Tail,
    /// The level filter, `None` for everything.
    level: Option<String>,
    /// §8.2's content-redaction switch. On by default: a window that is
    /// about to be screen-shared is exactly the one nobody thought to turn
    /// it on in.
    redact: bool,
    launchd: Option<Launchd>,
    /// This window's own login item (§10.2). Read, never written, from
    /// here: changing it is the Welcome screen's explicit click.
    login: Option<crate::present::LoginItem>,
    /// The methods this daemon answered `-32601` for. Kept so the buttons
    /// can say so *before* they are pressed a second time.
    missing: Vec<String>,
    port: Entity<InputState>,
    /// The log viewer's scroll, pinned to the newest line when a tail lands.
    log_scroll: ScrollHandle,
    asked: bool,
    busy: bool,
}

impl ServicePane {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> (ServicePane, Vec<Subscription>) {
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("47201"));
        let enter = cx.subscribe_in(&port, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.change_port(window, cx);
            }
        });
        (
            ServicePane {
                info: None,
                config: ApiConfig::default(),
                tail: Tail::default(),
                level: None,
                redact: true,
                launchd: None,
                login: None,
                missing: Vec::new(),
                port,
                log_scroll: ScrollHandle::new(),
                asked: false,
                busy: false,
            },
            vec![enter],
        )
    }

    /// What `service.info` last answered, for the Welcome screen's step 1
    /// and its doctor summary. One call, read in two places, so the two
    /// cannot disagree about whether the daemon is up.
    pub(crate) fn info(&self) -> Option<&Info> {
        self.info.as_ref()
    }

    /// …and what `launchctl print` last said about the agent.
    pub(crate) fn launchd(&self) -> Option<&Launchd> {
        self.launchd.as_ref()
    }

    /// `[hooks] enabled` from the one `config.get` this screen already
    /// makes, for the tray's **Pause hooks** tick. `None` until it has
    /// answered: a tick that guessed would re-enable hooks the operator had
    /// paused by hand.
    pub(crate) fn hooks_enabled(&self) -> Option<bool> {
        self.config.hooks_enabled
    }

    /// Install a `config.get` answered for somebody else — the tray reads
    /// `[hooks] enabled` on every connection, whether or not this screen has
    /// ever been opened. One answer, one place it lands.
    pub(crate) fn set_config(&mut self, config: ApiConfig) {
        self.config = config;
    }

    fn is_missing(&self, method: &str) -> bool {
        self.missing.iter().any(|m| m == method)
    }

    fn note_missing(&mut self, method: &str) {
        if !self.is_missing(method) {
            self.missing.push(method.to_string());
        }
    }
}

impl Shell {
    /// Ask for everything this screen draws: the service, the config, the
    /// log and launchd's view of the agent.
    pub(crate) fn refresh_service(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.service.busy || (self.service.asked && !force) {
            return;
        }
        self.service.busy = true;
        self.ask(
            UiAction::ServiceInfo,
            window,
            cx,
            |this, result, window, cx| {
                this.service.busy = false;
                if let Ok(value) = result {
                    let info = Info::parse(&value);
                    // The field's hint is the port the daemon holds now.
                    if let Some(port) = info.port {
                        this.service.port.update(cx, |input, cx| {
                            input.set_placeholder(port.to_string(), window, cx);
                        });
                    }
                    this.service.info = Some(info);
                    // Only an answer counts as having asked, so opening the
                    // screen again after a failed read asks again.
                    this.service.asked = true;
                }
            },
        );
        self.ask(UiAction::ConfigGet, window, cx, |this, result, _w, _cx| {
            if let Ok(value) = result {
                this.service.config = ApiConfig::parse(&value);
            }
        });
        self.tail_log(window, cx);
        self.read_launchd(window, cx);
        // A status query, not a registration: reading `SMAppService` changes
        // nothing (contract §1 M2's own measurement starts from that read).
        self.service.login = Some(crate::login_item::status());
    }

    /// `log.tail{lines, level}`. The level goes to the daemon as well as to
    /// the local filter: asking for 200 `ERROR` lines and getting 200 is
    /// worth more than filtering 200 mixed ones down to three.
    fn tail_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let level = self.service.level.clone();
        self.ask(
            UiAction::LogTail {
                lines: TAIL_LINES,
                level,
            },
            window,
            cx,
            |this, result, _window, _cx| {
                if let Ok(value) = result {
                    this.service.tail = Tail::parse(&value);
                    this.service.log_scroll.scroll_to_bottom();
                }
            },
        );
    }

    /// What launchd says about the daemon's agent. It is a subprocess, so it
    /// goes where all I/O goes: the bridge's runtime, never the UI thread.
    fn read_launchd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let done = app::launchd_print(&cx.global::<app::Tokio>().0);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(output) = done.recv().await else {
                return;
            };
            this.update_in(cx, |this, _window, cx| {
                this.service.launchd = Some(present::launchd_state(&output));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- the screen ----------------------------------------------------

    pub(crate) fn render_service(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let connected = self.state.connected();
        let info = self.service.info.clone().unwrap_or_default();
        let mut actions: Vec<AnyElement> = Vec::new();
        if !connected {
            actions.push(
                Btn::new("service-start", self.tr("Start"))
                    .solid()
                    .icon(ic::PLAY)
                    .on_click(cx.listener(|this, _, window, cx| this.start_service(window, cx)))
                    .into_any_element(),
            );
        }
        actions.push(
            Btn::new("service-restart", self.tr("Restart"))
                .icon(ic::REFRESH)
                .disabled(dim)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.lifecycle(UiAction::ServiceRestart, window, cx);
                }))
                .into_any_element(),
        );
        actions.push(
            Btn::new("service-stop", self.tr("Stop"))
                .danger_quiet()
                .icon(ic::STOP)
                .disabled(dim)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.lifecycle(UiAction::ServiceStop, window, cx);
                }))
                .into_any_element(),
        );
        let subtitle = v_flex()
            .gap(px(2.))
            .child(present::service_headline_localized(connected, &info, self.language()))
            .children(info.uds.as_ref().map(|uds| {
                div()
                    .mono()
                    .ts(11.5, 16.)
                    .text_color(p.faint)
                    .child(uds.clone())
            }));
        ui::page(
            "service",
            &self.page_scroll,
            self.content_width,
            ui::PAGE_MAX,
            v_flex()
                .child(ui::page_header(
                    self.tr("Service"),
                    Some(subtitle.into_any_element()),
                    actions,
                    &p,
                ))
                .child(self.render_language(cx))
                .children(
                    present::degraded_banner_localized(&self.state.mirror().service, self.language())
                        .map(|line| self.render_degraded(line, dim, cx)),
                )
                .children(self.service.missing.iter().map(|method| {
                    // The methods behind Restart, Stop and the port may not
                    // be in this daemon's table yet, and a button that has
                    // already been refused once should say so before it is
                    // pressed again.
                    div().mt(px(12.)).child(ui::strip(
                        Severity::Warning,
                        None,
                        Some(
                            div()
                                .child(present::missing_method_line_localized(method, self.language()))
                                .into_any_element(),
                        ),
                        Vec::new(),
                        &p,
                    ))
                }))
                .child(self.render_updates(dim, cx))
                .child(ui::section(self.tr("About"), v_flex().gap(px(12.))
                    .child(ui::brand_lockup_localized(&p, self.language()))
                    .child(div().text_color(p.muted).child(self.fmt("Version {0}", &[env!("CARGO_PKG_VERSION")])))
                    .child(Btn::new("about-website", "openwalkietalkie.org")
                        .icon(ic::EXTERNAL)
                        .on_click(|_, _, cx| cx.open_url("https://openwalkietalkie.org/"))), &p))
                .child(self.render_lifecycle(&p))
                .child(self.render_door(dim, cx))
                .child(self.render_log(dim, cx)),
        )
    }

    fn render_language(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let selected = self.language();
        let choices = deck_gui_core::i18n::Language::ALL.into_iter().map(|language| {
            ui::chip(
                SharedString::from(format!("language-{}", language.code())),
                language.name(),
                language == selected,
                &p,
            )
            .when(!self.state.connected(), |chip| chip.opacity(0.45))
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.state.connected() {
                    this.act(UiAction::DeviceSet {
                        key: deck_model::SetKey::Language,
                        value: language as u16,
                    }, window, cx);
                }
            }))
            .into_any_element()
        }).collect::<Vec<_>>();
        ui::section(self.tr("Language"), v_flex().gap(px(10.))
            .child(h_flex().gap(px(8.)).flex_wrap().children(choices))
            .child(ui::hint(self.tr("The app and your deck use the same language. This setting is remembered when the deck is disconnected."), &p)), &p)
    }

    /// §8.2's degraded strip. Its repair is the port field below, so the
    /// button puts the caret there rather than changing anything itself.
    fn render_degraded(&self, line: String, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        div().mt(px(20.)).child(ui::strip(
            Severity::Warning,
            Some(self.tr("The service is degraded").into()),
            Some(div().child(line).into_any_element()),
            vec![Btn::new("degraded-pick-port", self.tr("Pick a new port"))
                .small()
                .disabled(dim)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.service
                        .port
                        .update(cx, |input, cx| input.focus(window, cx));
                }))
                .into_any_element()],
            &p,
        ))
    }

    /// What launchd thinks of the daemon, and the window's own login item
    /// beside it — two different objects, said as two rows.
    fn render_lifecycle(&self, p: &Palette) -> Div {
        let launchd = self.service.launchd.clone().unwrap_or(Launchd::Unknown);
        let running = matches!(launchd, Launchd::Running(_));
        let login = self
            .service
            .login
            .unwrap_or(present::LoginItem::Unknown(-1));
        ui::section(
            self.tr("Lifecycle"),
            ui::card(
                vec![
                    Row::new()
                        .tile(ui::tile(
                            ic::SERVER,
                            if running { p.success_muted } else { p.muted },
                            p,
                        ))
                        .title(self.tr("launchd agent"))
                        .meta_el(
                            v_flex()
                                .gap(px(2.))
                                .child(present::launchd_line_localized(&launchd, self.language()))
                                .child(div().text_color(p.faint).child(self.tr(present::LOGIN_ITEM_NOTE))),
                        )
                        .into_any_element(),
                    Row::new()
                        .tile(ui::tile(ic::MONITOR, p.muted, p))
                        .title(self.tr("This window at login"))
                        .meta(present::login_item_line_localized(login, self.language()))
                        .into_any_element(),
                ],
                p,
            ),
            p,
        )
    }

    /// `service.stop` / `service.restart`, honestly.
    fn lifecycle(&mut self, action: UiAction, window: &mut Window, cx: &mut Context<Self>) {
        let (method, what) = match action {
            UiAction::ServiceStop => ("service.stop", self.tr("Stop")),
            _ => ("service.restart", self.tr("Restart")),
        };
        self.ask(action, window, cx, move |this, result, window, cx| {
            match result {
                // §3: the daemon answers *first* and exits afterwards, so a
                // reply here means the request was accepted, not that the
                // process has gone. The socket dropping is what says that,
                // and the mirror already reports it.
                Ok(_) => this.toast(this.fmt("{0} accepted.", &[(what)]), window, cx),
                Err(e) if method_missing(&e) => {
                    this.service.note_missing(method);
                    this.toast(present::missing_method_line_localized(method, this.language()), window, cx);
                }
                Err(e) => this.toast(this.fmt("{0} failed: {1}", &[(what), &(e).to_string()]), window, cx),
            }
        });
    }

    /// The port, the origins and the `curl` example (§4.1, §8.2).
    fn render_door(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let info = self.service.info.clone().unwrap_or_default();
        let curl = present::curl_example(info.port.or(self.service.config.port));
        ui::section(
            self.tr("The HTTP door"),
            ui::card(
                vec![
                    Row::new()
                        .tile(ui::tile(ic::GLOBAL, p.muted, &p))
                        .title(self.tr("Port"))
                        .meta(present::port_line_localized(&info, &self.service.config, self.language()))
                        .trailing(
                            div()
                                .w(px(96.))
                                .child(ui::field(&self.service.port, &p).disabled(dim).mono()),
                        )
                        .trailing(
                            Btn::new("port-change", self.tr("Change port"))
                                .outlined()
                                .disabled(dim)
                                .tooltip(self.tr("Sets the port and re-installs Claude's hooks to match"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.change_port(window, cx);
                                })),
                        )
                        .detail(ui::hint(
                            self.tr("Changing the port rewrites the URL Claude's http hooks POST to, so \
                             the hooks are re-installed in the same breath \u{2014} a port change \
                             without it leaves every hook pointed at the old one."),
                            &p,
                        ))
                        .into_any_element(),
                    Row::new()
                        .tile(ui::tile(ic::CHECK_CIRCLE, p.muted, &p))
                        .title(self.tr("Allowed origins"))
                        .meta(present::origins_line_localized(&self.service.config.allowed_origins, self.language()))
                        .into_any_element(),
                    Row::new()
                        .tile(ui::tile(ic::TERMINAL, p.muted, &p))
                        .title(self.tr("Try it"))
                        .meta(
                            "The socket is not HTTP, so curl --unix-socket cannot speak to it \
                             \u{2014} that door is deckd ctl.",
                        )
                        .detail(ui::code_block_localized("curl", curl, true, &p, self.language()))
                        .into_any_element(),
                ],
                &p,
            ),
            &p,
        )
    }

    /// §8.2's "change port + re-install hooks": one without the other leaves
    /// every installed Claude hook POSTing to a port the daemon no longer
    /// holds, so the repair follows the write and is not a second button.
    fn change_port(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.service.port.read(cx).value().trim().to_string();
        if typed.is_empty() {
            self.service
                .port
                .update(cx, |input, cx| input.focus(window, cx));
            self.toast(self.tr("Type the new port first."), window, cx);
            return;
        }
        let Ok(port) = typed.parse::<u16>() else {
            self.toast(self.tr("A port is a number from 1 to 65535."), window, cx);
            return;
        };
        if port < 1024 {
            self.toast(
                self.tr("Ports below 1024 need root, and this daemon is a per-user agent."),
                window,
                cx,
            );
            return;
        }
        self.ask(
            UiAction::ConfigSet {
                key: "api.port".into(),
                value: ConfigValue::Int(i64::from(port)),
            },
            window,
            cx,
            move |this, result, window, cx| match result {
                Ok(_) => {
                    this.toast(
                        this.fmt("Port set to {0}. Re-installing the Claude hooks…", &[&(port).to_string()]),
                        window,
                        cx,
                    );
                    // §4.1: `config.set{api.port}` → `integrations.repair
                    // claude`. The repair is not a dry run: the operator
                    // asked for the port change, and a hook left on the old
                    // port is the failure this pair exists to avoid.
                    this.ask(
                        UiAction::Integrations {
                            verb: deck_gui_core::Verb::Repair,
                            target: "claude".into(),
                            trust: false,
                            dry_run: false,
                        },
                        window,
                        cx,
                        |this, result, window, cx| match result {
                            Ok(_) => {
                                this.toast(this.tr("Claude's hooks now point at the new port."), window, cx);
                                this.refresh_service(true, window, cx);
                            }
                            Err(e) => this.toast(
                                this.fmt("The port changed but the hooks did not: {0}. Repair Claude on the Integrations screen before the next session.", &[&(e).to_string()]),
                                window,
                                cx,
                            ),
                        },
                    );
                }
                Err(e) if method_missing(&e) => {
                    this.service.note_missing("config.set");
                    this.toast(present::missing_method_line_localized("config.set", this.language()), window, cx);
                }
                Err(e) => this.toast(this.fmt("Could not set the port: {0}", &[&(e).to_string()]), window, cx),
            },
        );
    }

    // ---- the log tail --------------------------------------------------

    fn render_log(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let redact = self.service.redact;
        let home = std::env::var("HOME").ok();
        let lines: Vec<Div> = self
            .service
            .tail
            .lines
            .iter()
            .filter(|line| present::log_level_matches(line, self.service.level.as_deref()))
            .map(|line| {
                let text = if redact {
                    present::redact_line(line, home.as_deref())
                } else {
                    line.clone()
                };
                div().text_color(level_color(line, &p)).child(text)
            })
            .collect();
        let empty = lines.is_empty();
        let mut chips: Vec<Stateful<Div>> =
            vec![self.level_chip("log-level-all", self.tr("All"), None, dim, cx)];
        chips.extend(present::LOG_LEVELS.iter().map(|level| {
            self.level_chip(
                SharedString::from(format!("log-level-{level}")),
                level,
                Some((*level).to_string()),
                dim,
                cx,
            )
        }));
        ui::section_with(
            self.tr("Log"),
            vec![
                h_flex()
                    .gap(px(8.))
                    .ts(12., 16.)
                    .text_color(p.muted)
                    .child(self.tr("Hide content"))
                    .child(Switch::new("log-redact", redact).on_toggle(cx.listener(
                        |this, on: &bool, _, cx| {
                            this.service.redact = *on;
                            cx.notify();
                        },
                    )))
                    .into_any_element(),
                Btn::icon_only("log-refresh", ic::REFRESH)
                    .small()
                    .disabled(dim)
                    .tooltip(self.tr("Read the log again"))
                    .on_click(cx.listener(|this, _, window, cx| this.tail_log(window, cx)))
                    .into_any_element(),
            ],
            v_flex()
                .gap(px(8.))
                .child(
                    h_flex()
                        .gap(px(2.))
                        .children(chips)
                        .child(div().flex_1())
                        .children(self.service.tail.file.as_ref().map(|file| {
                            div()
                                .min_w_0()
                                .truncate()
                                .mono()
                                .ts(11., 14.)
                                .text_color(p.faint)
                                .child(file.clone())
                        })),
                )
                .child(
                    div()
                        .id("log")
                        .h(px(340.))
                        .overflow_y_scroll()
                        .track_scroll(&self.service.log_scroll)
                        .rounded(px(12.))
                        .bg(p.terminal)
                        .border_1()
                        .border_color(p.hairline(0.06))
                        .px(px(14.))
                        .py(px(12.))
                        .mono()
                        .ts(11.5, 18.)
                        .children(lines)
                        .when(empty, |this| {
                            this.child(div().text_color(p.faint).child(self.tr("Nothing at this level.")))
                        }),
                ),
            &p,
        )
    }

    fn level_chip(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        level: Option<String>,
        dim: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let p = *palette(cx);
        let here = self.service.level == level;
        ui::chip(id, label, here, &p).when(!dim, |this| {
            this.on_click(cx.listener(move |this, _, window, cx| {
                this.service.level = level.clone();
                this.tail_log(window, cx);
            }))
        })
    }
}

/// A log line's colour, from the level word in it: errors and warnings in
/// their status shade, the chatty levels faint, the rest plain.
fn level_color(line: &str, p: &Palette) -> Hsla {
    if line.contains("ERROR") {
        p.danger_muted
    } else if line.contains("WARN") {
        p.warning_muted
    } else if line.contains("DEBUG") || line.contains("TRACE") {
        p.faint
    } else {
        p.text.opacity(0.86)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_tail_asks_for_a_pane_full_and_not_for_the_whole_file() {
        // `log.tail` clamps its own ceiling; this is what a pane can show
        // without the window becoming a log viewer.
        const { assert!(TAIL_LINES >= 50 && TAIL_LINES <= 1000) };
    }

    #[test]
    fn a_method_this_daemon_lacks_is_remembered_so_the_button_says_so_next_time() {
        // The pane's own bookkeeping, which is what puts the sentence under
        // the buttons rather than only in a toast that has scrolled away.
        struct Missing(Vec<String>);
        impl Missing {
            fn note(&mut self, m: &str) {
                if !self.0.iter().any(|x| x == m) {
                    self.0.push(m.to_string());
                }
            }
        }
        let mut missing = Missing(Vec::new());
        missing.note("service.stop");
        missing.note("service.stop");
        missing.note("config.set");
        assert_eq!(missing.0, vec!["service.stop", "config.set"]);
        for method in &missing.0 {
            let line = present::missing_method_line(method);
            assert!(line.contains(method));
            assert!(
                line.contains("nothing happened"),
                "a missing method must never read as a success"
            );
        }
    }

    #[test]
    fn a_log_line_is_coloured_by_the_level_word_in_it() {
        let p = crate::style::Palette::dark(true);
        assert_eq!(
            level_color("2026-10-04 ERROR deckd: boom", &p),
            p.danger_muted
        );
        assert_eq!(
            level_color("2026-10-04 WARN deckd: hmm", &p),
            p.warning_muted
        );
        assert_eq!(level_color("2026-10-04 DEBUG deckd: x", &p), p.faint);
        assert_eq!(
            level_color("2026-10-04 INFO deckd: ok", &p),
            p.text.opacity(0.86)
        );
    }
}
