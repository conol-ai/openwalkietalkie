//! The **Welcome** flow (§8.2's Welcome row): three steps from a fresh
//! account to a deck that answers.
//!
//! It is a screen and not a modal, deliberately. §8.2 asks for a Welcome
//! that is **re-openable**, and the sidebar is already the thing this window
//! re-opens screens with; a modal would need a second way in and a second
//! way out, and a first-run wizard that cannot be re-read is the one an
//! operator needs most on the day something breaks.
//!
//! Nothing here is a second implementation of anything. Step 1 shares the
//! Service screen's `service.info` and `launchctl print`, step 3 shares the
//! Integrations screen's `integrations.status` and its "What this writes"
//! dialog, and the permission rows and the chime are the Voice screen's own
//! — so a hook installed from the Welcome screen is installed by exactly the
//! path the Integrations screen uses, with the same diff in front of it, and
//! Grant is offered on exactly the same terms.
//!
//! The one thing that *is* only here is the login item (§10.2), and it
//! carries the measured rule with it (contract §1 M2): `register()`
//! returning `Ok` is not evidence, so every switch re-reads `status()` and
//! the row reports **that**. It is also the only control in this window that
//! changes the operator's Mac beyond this process's lifetime, which is why
//! it is behind an explicit click and never a side effect of the screen
//! being open.
use crate::login_item;
use crate::present::{self, LoginItem, Step};
use crate::shell::Shell;
use crate::style::palette;
use crate::ui::{self, ic, Btn, Row, Switch, Tint, Type as _};
use deck_gui_core::admin::Verb;
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, SharedString, Stateful, Window};
use gpui_component::{h_flex, v_flex};

/// The Welcome screen's own view state. Three fields, because everything
/// else it draws belongs to another pane.
pub struct WelcomePane {
    step: Step,
    /// `SMAppService.mainApp`'s status as it was last **read**. Never what a
    /// click hoped for: see the module note.
    login: Option<LoginItem>,
    /// "Not seeing it?" is open.
    troubleshooting: bool,
}

impl Default for WelcomePane {
    fn default() -> Self {
        WelcomePane {
            step: Step::Service,
            login: None,
            troubleshooting: false,
        }
    }
}

/// The glyph each step is headed with.
const fn step_icon(step: Step) -> &'static str {
    match step {
        Step::Service => ic::SERVER,
        Step::Deck => ic::DIAL,
        Step::Integrations => ic::PLUG,
    }
}

impl Shell {
    /// Open the Welcome screen and ask for what its steps draw.
    ///
    /// The login item is **read** here — a status query registers nothing —
    /// and the other two are the calls the Service and Integrations screens
    /// already make, so this screen adds one syscall and no new method.
    pub(crate) fn refresh_welcome(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.welcome.login = Some(login_item::status());
        match self.welcome.step {
            Step::Service => self.refresh_service(force, window, cx),
            Step::Deck => {}
            Step::Integrations => self.refresh_integrations(force, window, cx),
        }
    }

    pub(crate) fn go_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.welcome.step = step;
        self.welcome.troubleshooting = false;
        self.refresh_welcome(false, window, cx);
        cx.notify();
    }

    /// Register or unregister the login item, then **re-read the status and
    /// report that** (contract §1 M2).
    ///
    /// The measurement this obeys: on a fresh state the first `register()`
    /// returned `Ok` while `status()` was still 3 (`notFound`). A switch
    /// that believed the return value would have said "on" having
    /// registered nothing.
    fn set_login_item(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = if on {
            login_item::register()
        } else {
            login_item::unregister()
        };
        match answer {
            Ok(status) => {
                self.welcome.login = Some(status);
                // Even the happy path reports the status and not the call:
                // `Ok` with a `notFound` status is the M2 case, and it reads
                // as the failure it is rather than as a success.
                self.toast(present::login_item_line_localized(status, self.language()), window, cx);
            }
            Err(why) => {
                self.welcome.login = Some(login_item::status());
                self.toast(self.fmt("macOS refused that: {0}", &[&(why).to_string()]), window, cx);
            }
        }
        cx.notify();
    }

    // ---- the screen ----------------------------------------------------

    pub(crate) fn render_welcome(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let step = self.welcome.step;
        let index = Step::ALL.iter().position(|s| *s == step).unwrap_or(0);
        #[allow(clippy::cast_precision_loss)]
        let fraction = (index + 1) as f32 / Step::ALL.len() as f32;
        ui::page(
            "welcome",
            &self.page_scroll,
            self.content_width,
            640.,
            v_flex()
                .pt(px(8.))
                .child(div().mb(px(22.)).child(ui::brand_lockup_localized(&p, self.language())))
                .child(
                    h_flex()
                        .gap(px(12.))
                        .child(
                            div()
                                .ts(12., 16.)
                                .text_color(p.muted)
                                .child(self.fmt("Step {0}", &[&(step.counter()).to_string()])),
                        )
                        .child(div().w(px(120.)).child(ui::progress(fraction, &p))),
                )
                .child(
                    h_flex()
                        .mt(px(22.))
                        .gap(px(14.))
                        .items_start()
                        .child(
                            div()
                                .flex_none()
                                .size(px(44.))
                                .rounded(px(12.))
                                .bg(p.wash(0.06))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(ui::icon(step_icon(step), 22., p.text)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(6.))
                                .child(div().ts(20., 26.).medium().child(self.tr(step.title())))
                                .child(
                                    div()
                                        .ts(13., 19.)
                                        .text_color(p.muted)
                                        .child(ui::para(self.tr(step.blurb()))),
                                ),
                        ),
                )
                .child(match step {
                    Step::Service => self.render_step_service(cx),
                    Step::Deck => self.render_step_deck(dim, cx),
                    Step::Integrations => self.render_step_integrations(dim, cx),
                })
                .child(self.render_step_nav(cx)),
        )
    }

    fn render_step_nav(&self, cx: &Context<Self>) -> Div {
        let step = self.welcome.step;
        h_flex()
            .mt(px(28.))
            .gap(px(8.))
            .children(step.back().map(|back| {
                Btn::new("welcome-back", self.tr("Back"))
                    .quiet()
                    .icon(ic::CHEVRON_LEFT)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.go_step(back, window, cx);
                    }))
            }))
            .child(div().flex_1())
            .children(step.next().map(|next| {
                Btn::new("welcome-next", self.tr("Next"))
                    .solid()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.go_step(next, window, cx);
                    }))
            }))
            .when(step.next().is_none(), |this| {
                this.child(Btn::new("welcome-done", self.tr("Go to the Deck")).solid().on_click(
                    cx.listener(|this, _, window, cx| {
                        this.go(crate::shell::Screen::Deck, window, cx);
                    }),
                ))
            })
    }

    /// Step 1: the service, and the login item that starts this window with
    /// the Mac.
    ///
    /// Nothing on this step is dimmed by a dropped socket, and that is the
    /// point of it: Start is what one presses *because* the socket is down,
    /// and the login item is this app's own and has no daemon behind it.
    fn render_step_service(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let connected = self.state.connected();
        let login = self.welcome.login.unwrap_or(LoginItem::Unknown(-1));
        let launchd = present::launchd_line_localized(
            self.service
                .launchd()
                .unwrap_or(&crate::present::Launchd::Unknown), self.language());
        let service = Row::new()
            .tile(ui::tile(
                ic::SERVER,
                if connected { p.success_muted } else { p.muted },
                &p,
            ))
            .title(self.tr("The service"))
            .meta_el(
                v_flex()
                    .gap(px(2.))
                    .child(if connected {
                        self.tr("Running. The deck, the audio and every agent belong to it.")
                    } else {
                        self.tr("Not running. Start it, and this window connects on its own.")
                    })
                    .child(div().text_color(p.faint).child(launchd)),
            )
            .trailing(if connected {
                ui::badge(self.tr("running"), Tint::Success, &p).into_any_element()
            } else {
                Btn::new("welcome-start", self.tr("Start"))
                    .solid()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.start_service(window, cx)))
                    .into_any_element()
            });
        let registered = login.registered();
        let login_row = Row::new()
            .tile(ui::tile(ic::MONITOR, p.muted, &p))
            .title(self.tr("Open this window at login"))
            .meta(present::login_item_line_localized(login, self.language()))
            // Never disabled by a dropped socket: the login item is this
            // app's own and has nothing to do with the daemon.
            .trailing(Switch::new("welcome-login", registered).on_toggle(
                cx.listener(|this, on: &bool, window, cx| this.set_login_item(*on, window, cx)),
            ))
            .detail(
                h_flex()
                    .gap(px(10.))
                    .child(div().flex_1().min_w_0().child(ui::hint(
                        self.tr("This is the window, not the service: the service is a launchd agent \
                         and runs whether this app is open or not."),
                        &p,
                    )))
                    .child(
                        Btn::new("welcome-login-items", self.tr("Login Items"))
                            .small()
                            .quiet()
                            .icon(ic::EXTERNAL)
                            .on_click(|_, _, cx: &mut App| {
                                cx.open_url(present::LOGIN_ITEMS_SETTINGS);
                            }),
                    ),
            );
        div().mt(px(24.)).child(ui::card(
            vec![service.into_any_element(), login_row.into_any_element()],
            &p,
        ))
    }

    /// Step 2: the deck, the permissions the voice half needs, and the chime.
    fn render_step_deck(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let device = &self.state.mirror().device;
        let ready = present::deck_ready(device);
        let voice = &self.state.mirror().voice;
        let troubleshooting = self.welcome.troubleshooting;
        let mut deck = Row::new()
            .tile(ui::tile(
                ic::DIAL,
                if ready { p.success_muted } else { p.muted },
                &p,
            ))
            .title(self.tr("The deck"))
            .meta(present::deck_line_localized(device, self.language()))
            .trailing(ui::badge(
                if ready { self.tr("connected") } else { self.tr("waiting") },
                if ready { Tint::Success } else { Tint::Neutral },
                &p,
            ))
            .trailing(
                Btn::new("welcome-troubleshoot", self.tr("Not seeing it?"))
                    .small()
                    .quiet()
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.welcome.troubleshooting = !this.welcome.troubleshooting;
                        cx.notify();
                    })),
            );
        if troubleshooting {
            deck = deck.detail(self.render_troubleshooting(cx));
        }
        let mut rows: Vec<AnyElement> = vec![deck.into_any_element()];
        rows.extend(self.voice_permission_rows(dim, cx));
        let mut chime = Row::new()
            .tile(ui::tile(ic::VOLUME, p.muted, &p))
            .title(self.tr("Play the ready chime"))
            .meta(self.fmt("Route now: {0}", &[(self.tr(present::route_word(voice.route)))]))
            .trailing(
                Btn::new("welcome-test", self.tr("Play"))
                    .solid()
                    .small()
                    .icon(ic::PLAY)
                    .disabled(dim)
                    .on_click(cx.listener(|this, _, window, cx| this.speaker_test(window, cx))),
            );
        if let Some(report) = self.voice_report() {
            chime = chime.detail(crate::voice::test_result_localized(report, false, &p, self.language()));
        }
        rows.push(chime.into_any_element());
        v_flex()
            .mt(px(24.))
            .gap(px(10.))
            .child(ui::card(rows, &p))
            .child(
                div()
                    .px(px(4.))
                    .child(ui::hint(self.tr(present::PERMISSION_CAVEAT), &p)),
            )
    }

    /// "Not seeing it?": the Bluetooth pane, and what this window can see.
    fn render_troubleshooting(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let lines = present::doctor_lines_localized(
            self.state.connected(),
            &self.service.info().cloned().unwrap_or_default(),
            self.service.launchd(),
            &self.state.mirror().device,
            &self.state.mirror().service, self.language());
        v_flex()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(10.))
            .bg(p.wash(0.04))
            .children(
                lines
                    .into_iter()
                    .map(|line| div().ts(12., 18.).text_color(p.muted).child(line)),
            )
            .child(
                h_flex().child(
                    Btn::new("welcome-bluetooth", self.tr("Bluetooth settings"))
                        .small()
                        .outlined()
                        .icon(ic::EXTERNAL)
                        .on_click(|_, _, cx: &mut App| cx.open_url(present::BLUETOOTH_SETTINGS)),
                ),
            )
    }

    /// Step 3: the hooks, and the exit test.
    fn render_step_integrations(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let mut rows: Vec<AnyElement> = present::TARGETS
            .into_iter()
            .filter_map(|target| {
                let report = self.integrations.report(target)?;
                // Only the CLIs this machine actually has. A Welcome that
                // offered to install Gemini's hooks on a Mac without Gemini
                // would be asking the operator to fix something that is not
                // wrong.
                let detected = report.detected.as_ref()?;
                let installed = report.status.installed();
                let (mark, color) = ui::target_mark(target, &p);
                let install = Btn::new(
                    SharedString::from(format!("welcome-install-{target}")),
                    if installed { self.tr("Re-install") } else { self.tr("Install") },
                )
                .small()
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.plan_integration(Verb::Install, target, window, cx);
                }));
                Some(
                    Row::new()
                        .tile(ui::tile(mark, color, &p))
                        .title_el(
                            h_flex()
                                .gap(px(8.))
                                .child(present::party_name(target).to_string())
                                .when(installed, |this| {
                                    this.child(ui::badge(self.tr("installed"), Tint::Success, &p))
                                }),
                        )
                        .meta(present::detected_line_localized(target, Some(detected), self.language()))
                        .trailing(if installed { install } else { install.solid() })
                        .into_any_element(),
                )
            })
            .collect();
        if rows.is_empty() {
            rows.push(
                Row::new()
                    .tile(ui::tile(ic::PLUG, p.faint, &p))
                    .title(self.tr("No supported CLI found"))
                    .meta(
                        self.tr("None of the supported CLIs was found on PATH. The Integrations screen \
                         shows every party and what it would write."),
                    )
                    .into_any_element(),
            );
        }
        let transcript = present::transcript_line_localized(self.state.transcript(), self.language());
        let heard = self.state.transcript().is_some();
        v_flex()
            .mt(px(24.))
            .gap(px(10.))
            .child(ui::card(rows, &p))
            .child(div().px(px(4.)).child(ui::hint(
                self.tr("Install shows what it would write before it writes anything \u{2014} the same \
                 diff the Integrations screen shows."),
                &p,
            )))
            .child(ui::section(
                self.tr("Say something"),
                ui::panel(&p)
                    .gap(px(12.))
                    .child(div().ts(13., 19.).child(ui::para(self.tr(present::SAY_HELLO))))
                    .child(
                        h_flex()
                            .gap(px(10.))
                            .px(px(14.))
                            .py(px(12.))
                            .rounded(px(10.))
                            .bg(p.wash(0.05))
                            .child(ui::icon(
                                ic::MICROPHONE,
                                16.,
                                if heard { p.accent } else { p.faint },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .ts(14., 20.)
                                    .text_color(if heard { p.text } else { p.faint })
                                    .child(transcript),
                            ),
                    ),
                &p,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_welcome_opens_on_the_first_step_and_walks_all_three_in_order() {
        let pane = WelcomePane::default();
        assert_eq!(pane.step, Step::Service);
        assert!(pane.login.is_none(), "nothing is read before the screen is");
        assert!(!pane.troubleshooting);
        let mut walked = vec![Step::Service];
        let mut step = Step::Service;
        while let Some(next) = step.next() {
            walked.push(next);
            step = next;
        }
        assert_eq!(walked, Step::ALL.to_vec());
        assert_eq!(Step::Integrations.next(), None, "the last step is the last");
        assert_eq!(Step::Service.back(), None);
        assert_eq!(Step::ALL.map(Step::counter), ["1 of 3", "2 of 3", "3 of 3"]);
    }

    #[test]
    fn every_step_has_a_glyph_of_its_own() {
        let glyphs = Step::ALL.map(step_icon);
        assert_ne!(glyphs[0], glyphs[1]);
        assert_ne!(glyphs[1], glyphs[2]);
        assert_ne!(glyphs[0], glyphs[2]);
    }
}
