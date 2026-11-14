//! Footprint, Service's sub-page (board Footprint): everything this app has
//! put on this Mac, a row each — where it is, what it weighs and what
//! Uninstall does with it — and **Uninstall…**, which takes it all back out.
//!
//! The rows are the service's `care.footprint`, which it reads off the disk,
//! in the board's order ([`items`]). A row is drawn for what is there and
//! nothing else: an item the service found absent is not on this Mac, so it
//! is not in the list and not in the count the Service page gives. The
//! Keychain item is the one row that is not a file: the cloud voice's key,
//! which the service keeps in the Keychain on a Mac and names there only
//! while one is saved.
//!
//! **Each row's button goes through the part of the app that owns the
//! thing.** The Claude Code plugin, a Codex setup left from before the
//! plugin and the Codex Desktop launcher (`~/Applications/Codex with
//! OpenWalkieTalkie.app`, which the Integrations page's **Set Up** writes)
//! are the installer's: **Remove…** is its uninstall, shown first in the
//! Integrations page's plan dialog, where nothing is written until it is
//! confirmed. The two voice models and the firmware downloads are the
//! service's caches: **Delete…** asks first, then has the service delete
//! them. The rest goes only with Uninstall, or stays (the logs).
//!
//! **Uninstall** is the bundled `deckd uninstall --purge --yes`
//! ([`uninstall_argv`]), run off the UI thread, then the two registrations
//! that are the app's own to take back (the service's launch agent and this
//! window's login item, which `deckd uninstall` says it leaves to the app).
//! Only the installed app runs it: the command is built by a pure function,
//! and every other run — a build, a test, a harness walk — refuses before
//! anything is started.
use crate::app;
use crate::login_item;
use crate::present;
use crate::shell::{Screen, Sheet, Shell};
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Row, Type as _};
use deck_gui_core::admin::{method_missing, Verb};
use deck_gui_core::care::{Footprint, FOOTPRINT_IDS};
use deck_gui_core::i18n::Language;
use deck_gui_core::{TokioBridge, UiAction};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Window};
use gpui_component::{h_flex, v_flex};
use std::path::Path;

/// The outcome dialog's id, and how wide it is drawn.
pub(crate) const UNINSTALL_DIALOG: &str = "uninstall";
const UNINSTALL_WIDTH: f32 = 420.;

/// The table's columns, as the board draws them: the name, then the place
/// (whatever is left), the size, right-aligned, and the action.
const NAME_WIDTH: f32 = 160.;
const SIZE_WIDTH: f32 = 72.;
const ACTION_WIDTH: f32 = 104.;

/// What a row says the thing is where a path would not help.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A path on this Mac, as the pages print one (`present::home_short`).
    Path(String),
    /// Words in place of a path.
    Words(&'static str),
}

/// What a cache's **Delete…** deletes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    /// The Whisper model (`stt.remove`).
    Whisper,
    /// The Kokoro voice (`tts.remove`).
    Kokoro,
    /// The firmware downloads, the bootloader backups kept
    /// (`care.cache.clear firmware`).
    Firmware,
}

impl Removal {
    /// What the service is asked to do.
    pub fn action(self) -> UiAction {
        match self {
            Removal::Whisper => UiAction::SttRemove,
            Removal::Kokoro => UiAction::TtsRemove,
            Removal::Firmware => UiAction::CareCacheClear {
                what: "firmware".into(),
            },
        }
    }
}

/// What a row offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// **Remove…**: the installer's uninstall of this target, as a plan
    /// first.
    Remove(&'static str),
    /// **Delete…**, after a question.
    Delete(Removal),
    /// Goes with Uninstall.
    WithUninstall,
    /// Stays, Uninstall or not.
    Kept,
}

/// One row of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The service's id for it.
    pub id: &'static str,
    /// Its name, in the window's language.
    pub name: &'static str,
    pub place: Place,
    /// What the size column says: a size on disk, or a count (hooks,
    /// entries); `None` for nothing to weigh.
    pub size: Option<String>,
    pub action: Action,
}

/// The rows of `footprint`, in the board's order: one for each item the
/// service found on this Mac.
pub fn items(footprint: &Footprint, language: Language) -> Vec<Item> {
    FOOTPRINT_IDS
        .iter()
        .filter_map(|&id| {
            let item = footprint.get(id).filter(|item| item.present)?;
            let path = || {
                Place::Path(present::home_short(
                    item.path.as_deref().unwrap_or_default(),
                ))
            };
            let size = item.bytes.map(present::size_text);
            let count = item.count.map(|n| n.to_string());
            let (name, place, size, action) = match id {
                "service" => (
                    language.text("Background service"),
                    Place::Words(language.text(crate::voice::here(
                        "deckd, under Login Items in System Settings",
                        "deckd, under Startup apps in Settings",
                    ))),
                    None,
                    Action::WithUninstall,
                ),
                // The plugin is counted in the hook events it installs; a
                // plugin the service could not count is weighed.
                "claude_plugin" => (
                    language.text("Claude Code plugin"),
                    path(),
                    match count.as_deref() {
                        Some("1") => Some(language.text("1 hook").to_string()),
                        Some(n) => Some(language.format("{0} hooks", &[n])),
                        None => size,
                    },
                    Action::Remove("claude"),
                ),
                // The Codex setup is counted in this app's entries in
                // Codex's own file, which holds the owner's too.
                "codex_hooks" => (
                    language.text("Leftover Codex setup"),
                    path(),
                    match count.as_deref() {
                        Some("1") => Some(language.text("1 entry").to_string()),
                        Some(n) => Some(language.format("{0} entries", &[n])),
                        None => size,
                    },
                    Action::Remove("codex"),
                ),
                // The launcher is an app of its own, called by Codex
                // Desktop's name, which no language translates; its place
                // says which app it is.
                "codex_desktop" => (
                    "Codex Desktop",
                    path(),
                    size,
                    Action::Remove("codex-desktop"),
                ),
                "stt" => (
                    language.text("Whisper model"),
                    path(),
                    size,
                    Action::Delete(Removal::Whisper),
                ),
                "kokoro" => (
                    language.text("Kokoro voice"),
                    path(),
                    size,
                    Action::Delete(Removal::Kokoro),
                ),
                "firmware" => (
                    language.text("Firmware downloads"),
                    path(),
                    size,
                    Action::Delete(Removal::Firmware),
                ),
                "data" => (
                    language.text("Settings and backups"),
                    path(),
                    size,
                    Action::WithUninstall,
                ),
                "prefs" => (
                    language.text("Preferences"),
                    path(),
                    size,
                    Action::WithUninstall,
                ),
                // A Keychain item has no path to print and nothing to
                // weigh: its place says whose it is.
                "keychain" => (
                    language.text(crate::voice::here("Keychain item", "Credential Manager entry")),
                    Place::Words(language.text("Your cloud voice key")),
                    None,
                    Action::WithUninstall,
                ),
                _ => (language.text("Logs"), path(), size, Action::Kept),
            };
            Some(Item {
                id,
                name,
                place,
                size,
                action,
            })
        })
        .collect()
}

/// How many things there are and what they weigh together: the rows the
/// page draws, and the bytes of those that have a size.
pub fn total(footprint: &Footprint) -> (usize, u64) {
    FOOTPRINT_IDS
        .iter()
        .filter_map(|id| footprint.get(id).filter(|item| item.present))
        .fold((0, 0), |(count, bytes), item| {
            (count + 1, bytes + item.bytes.unwrap_or(0))
        })
}

/// The Service page's line about it: `10 items, 2.1 GB. Most can be removed
/// one at a time.`
pub fn summary(footprint: &Footprint, language: Language) -> String {
    let (count, bytes) = total(footprint);
    language.format(
        "{0} items, {1}. Most can be removed one at a time.",
        &[&count.to_string(), &present::size_text(bytes)],
    )
}

/// The command Uninstall runs: the bundled service's own uninstall, the data
/// directory and the caches with it, without its question (the sheet asked).
pub fn uninstall_argv(deckd: &Path) -> Vec<String> {
    vec![
        deckd.to_string_lossy().into_owned(),
        "uninstall".into(),
        "--purge".into(),
        "--yes".into(),
    ]
}

/// What `deckd uninstall` said went wrong: the lines under its `PROBLEMS:`
/// heading, joined. `None` when it names none.
pub fn uninstall_problems(report: &str) -> Option<String> {
    let problems: Vec<&str> = report
        .lines()
        .skip_while(|line| line.trim() != "PROBLEMS:")
        .skip(1)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Run `argv` on the bridge's blocking pool and answer on the returned
/// channel: `Ok` when it exits 0, else why not.
fn run_uninstall(
    bridge: &TokioBridge,
    argv: Vec<String>,
) -> async_channel::Receiver<Result<(), String>> {
    let (tx, rx) = async_channel::bounded(1);
    bridge.spawn(async move {
        let answer = tokio::task::spawn_blocking(move || uninstall_now(&argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(answer).await;
    });
    rx
}

/// The blocking half. The last guard is here: only the installed app, run
/// for real, ever starts the command.
fn uninstall_now(argv: &[String]) -> Result<(), String> {
    if let Some(why) = app::launch().hands_off() {
        return Err(why.to_string());
    }
    let (program, args) = argv.split_first().ok_or("no command")?;
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    Err(uninstall_problems(&String::from_utf8_lossy(&out.stdout))
        .unwrap_or_else(|| format!("deckd uninstall exited {}", out.status)))
}

/// Take back the service's launch agent and this window's login item, each
/// by its re-read status: what is still registered afterwards is a problem,
/// with what macOS said.
fn unregister_both() -> Result<(), String> {
    let mut problems = Vec::new();
    if let Err(why) = login_item::unregister_agent() {
        if login_item::agent_status().registered() {
            problems.push(why);
        }
    }
    if let Err(why) = login_item::unregister() {
        if login_item::status().registered() {
            problems.push(why);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

impl Shell {
    /// The page: its way back, its head, and the one card of rows with the
    /// note and **Uninstall…** under them.
    pub(crate) fn render_footprint(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let header = ui::Header::new(
            self.tr(Screen::Footprint.label()),
            self.tr("What this app has added to your Mac, and what Uninstall does with each."),
        )
        .back(
            "footprint-back",
            self.tr(Screen::Service.label()),
            cx.listener(|this, _, window, cx| this.go(Screen::Service, window, cx)),
        );
        let mut rows: Vec<AnyElement> = match self.service.footprint.got() {
            Some(footprint) => items(footprint, self.language())
                .into_iter()
                .enumerate()
                .map(|(i, item)| self.footprint_row(i, item, dim, &p, cx).into_any_element())
                .collect(),
            None if self.service.footprint.missing() => vec![Row::new()
                .lead(ui::bare(ui::icon(ic::INFO, 17., p.text2)))
                .title(self.tr("This needs a newer background service."))
                .into_any_element()],
            None if self.state.connected() => vec![Row::new()
                .lead(ui::bare(ui::spinner(13., p.text2)))
                .title(self.tr("Reading…"))
                .into_any_element()],
            None => Vec::new(),
        };
        rows.push(self.footprint_uninstall_row(dim, &p, cx).into_any_element());
        let body = v_flex()
            .gap(px(20.))
            .child(header)
            .child(ui::card(rows, &p));
        ui::page("footprint", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// One row of the table, in the board's fixed columns. Every row has
    /// a rule under it: the card ends with Uninstall's row.
    fn footprint_row(
        &self,
        i: usize,
        item: Item,
        dim: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> Div {
        let place = match item.place {
            Place::Path(path) => ui::mono(path, p.text2).truncate(),
            Place::Words(words) => div()
                .ts(12., 16.)
                .text_color(p.text2)
                .truncate()
                .child(words),
        };
        let kept = |words: &'static str| {
            div()
                .ts(12., 16.)
                .text_color(p.text3)
                .whitespace_nowrap()
                .child(words)
                .into_any_element()
        };
        let action = match item.action {
            Action::Remove(target) => Btn::new(("footprint-action", i), self.tr("Remove…"))
                .quiet()
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.plan_integration(Verb::Uninstall, target, window, cx);
                }))
                .into_any_element(),
            Action::Delete(what) => Btn::new(("footprint-action", i), self.tr("Delete…"))
                .quiet()
                .disabled(dim)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.ask_delete_cache(what, window, cx);
                }))
                .into_any_element(),
            Action::WithUninstall => kept(self.tr("With Uninstall")),
            Action::Kept => kept(self.tr("Kept")),
        };
        ui::table_row(ui::ROW_ONE_LINE, true)
            .child(
                ui::cell(NAME_WIDTH)
                    .ts(13., 18.)
                    .medium()
                    .text_color(p.text)
                    .child(ui::para(item.name)),
            )
            .child(div().flex_1().min_w_0().child(place))
            .child(ui::cell_end(SIZE_WIDTH).children(item.size.map(|size| ui::mono(size, p.text2))))
            .child(ui::cell_end(ACTION_WIDTH).child(action))
    }

    /// The card's last row: what Uninstall does, and the button. Outside the
    /// installed app it is drawn and cannot be pressed, and says why.
    fn footprint_uninstall_row(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let installed = self.service_installed();
        let button = Btn::new("footprint-uninstall", self.tr("Uninstall…"))
            .danger()
            .disabled(!installed || dim || self.service.uninstalling)
            .when(!installed, |button| {
                button.tooltip(self.tr("Only in the installed app."))
            })
            .on_click(cx.listener(|this, _, window, cx| this.ask_uninstall_app(window, cx)));
        h_flex()
            .w_full()
            .gap(px(16.))
            .min_h(px(64.))
            .py(px(8.))
            .px(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ts(13., 18.)
                    .text_color(p.text2)
                    .child(ui::para(self.tr(crate::voice::here(
                        "Uninstall removes all of these except the logs and your macOS permissions,\nand puts your CLI settings back. Move the app to the Trash yourself.",
                        "Uninstall removes all of these except the logs,\nand puts your CLI settings back. Delete the app's folder yourself.",
                    )))),
            )
            .child(button)
    }

    /// Read the footprint again, unless a read is on its way.
    pub(crate) fn read_footprint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.footprint_pending || self.service.staged() || !self.state.connected() {
            return;
        }
        self.service.footprint_pending = true;
        self.ask(
            UiAction::CareFootprint,
            window,
            cx,
            |this, result, _window, _cx| {
                this.service.footprint_pending = false;
                this.service.footprint.take(result, Footprint::parse);
            },
        );
    }

    /// **Show All**: the page, read afresh.
    pub(crate) fn show_footprint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.go(Screen::Footprint, window, cx);
        self.read_footprint(window, cx);
    }

    /// A cache's **Delete…**: the question, then the service deletes it, then
    /// the page reads what is left.
    fn ask_delete_cache(&mut self, what: Removal, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body) = match what {
            Removal::Whisper => (
                self.tr("Delete the Whisper model?"),
                self.tr("It is downloaded again when you choose Whisper."),
            ),
            Removal::Kokoro => (
                self.tr("Delete the Kokoro voice?"),
                self.tr("It is downloaded again when you choose Kokoro."),
            ),
            Removal::Firmware => (
                self.tr("Delete the firmware downloads?"),
                self.tr("Bootloader backups are kept."),
            ),
        };
        let sheet = Sheet {
            title: title.into(),
            body: body.into(),
            confirm: self.tr("Delete").into(),
            danger: true,
        };
        self.ask_first(sheet, window, cx, move |this, window, cx| {
            this.ask(what.action(), window, cx, |this, result, window, cx| {
                match result {
                    Ok(_) => {}
                    Err(e) if method_missing(&e) => {
                        this.toast(
                            this.tr("This needs a newer background service."),
                            window,
                            cx,
                        );
                    }
                    Err(e) => this.toast(
                        this.fmt("{0} failed: {1}", &[this.tr("Delete"), &e.to_string()]),
                        window,
                        cx,
                    ),
                }
                this.read_footprint(window, cx);
            });
        });
    }

    /// **Uninstall…**: the question first.
    pub(crate) fn ask_uninstall_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sheet = Sheet {
            title: self.tr("Uninstall OpenWalkieTalkie?").into(),
            body: self
                .tr(crate::voice::here(
                    "Removes everything in this list except the logs and your macOS permissions, and puts your CLI settings back. Then move the app to the Trash.",
                    "Removes everything in this list except the logs, and puts your CLI settings back. Then delete the app's folder.",
                ))
                .into(),
            confirm: self.tr("Uninstall").into(),
            danger: true,
        };
        self.ask_first(sheet, window, cx, |this, window, cx| {
            this.uninstall_app(window, cx)
        });
    }

    /// Run the bundled uninstall off the UI thread, then take back the two
    /// registrations and say how it went.
    fn uninstall_app(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.uninstalling {
            return;
        }
        let deckd = match (app::launch().hands_off(), app::deckd_path()) {
            (Some(why), _) => Err(why.to_string()),
            (None, None) => Err("this copy of the app carries no deckd".to_string()),
            (None, Some(deckd)) => Ok(deckd),
        };
        let deckd = match deckd {
            Ok(deckd) => deckd,
            Err(why) => {
                self.service.uninstalled = Some(Err(why));
                self.open_dialog(UNINSTALL_DIALOG, window);
                cx.notify();
                return;
            }
        };
        self.service.uninstalling = true;
        let done = run_uninstall(&cx.global::<app::Tokio>().0, uninstall_argv(&deckd));
        cx.spawn_in(window, async move |this, cx| {
            let answer = done
                .recv()
                .await
                .unwrap_or_else(|_| Err("the uninstall stopped without an answer".into()));
            this.update_in(cx, |this, window, cx| {
                this.service.uninstalling = false;
                this.service.uninstalled = Some(answer.and_then(|()| unregister_both()));
                this.open_dialog(UNINSTALL_DIALOG, window);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// How Uninstall went: done, with the one thing left to the owner and
    /// **Quit**; or what stopped it, and **Close**.
    pub(crate) fn render_uninstalled(&self, cx: &Context<Self>) -> Option<(AnyElement, f32)> {
        let outcome = self.service.uninstalled.as_ref()?;
        let (words, button) = match outcome {
            Ok(()) => (
                self.tr(crate::voice::here(
                    "OpenWalkieTalkie is uninstalled. Move the app to the Trash.",
                    "OpenWalkieTalkie is uninstalled. Delete the app's folder.",
                ))
                    .to_string(),
                Btn::new("uninstall-quit", self.tr("Quit OpenWalkieTalkie"))
                    .primary()
                    .on_click(|_, _, cx| cx.quit()),
            ),
            Err(why) => (
                self.fmt("Uninstall did not finish: {0}", &[why]),
                Btn::new("uninstall-close", self.tr("Close"))
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            ),
        };
        let body = v_flex()
            .p(px(20.))
            .gap(px(16.))
            .ts(13., 18.)
            .child(div().child(ui::para(words)))
            .child(h_flex().justify_end().child(button));
        Some((body.into_any_element(), UNINSTALL_WIDTH))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codex_desktop_launcher_has_a_row_the_installer_removes_and_is_counted() {
        crate::i18n::install();
        // Windows sets no HOME: its home is USERPROFILE (`present::home_short`).
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .expect("a home to print paths from");
        let footprint = |launcher: bool| {
            Footprint::parse(&serde_json::json!({"items": [
                {"id": "codex_hooks", "path": format!("{home}/.codex/hooks.json"),
                 "bytes": 2048, "count": 7, "present": true},
                {"id": "codex_desktop",
                 "path": format!("{home}/Applications/Codex with OpenWalkieTalkie.app"),
                 "bytes": 1536, "count": null, "present": launcher},
                {"id": "stt", "path": format!("{home}/Library/Caches/OpenWalkieTalkie/stt"),
                 "bytes": 1_624_555_275u64, "count": null, "present": true}
            ]}))
        };
        let here = footprint(true);
        let rows = items(&here, Language::English);
        let ids: Vec<&str> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, ["codex_hooks", "codex_desktop", "stt"]);
        let launcher = &rows[1];
        assert_eq!(
            launcher.place,
            Place::Path("~/Applications/Codex with OpenWalkieTalkie.app".into())
        );
        assert_eq!(launcher.size.as_deref(), Some("2 KB"));
        // Its Remove… is the installer's uninstall of the launcher, planned
        // first like the plugin's.
        assert_eq!(launcher.action, Action::Remove("codex-desktop"));
        // Codex Desktop's own name, in every language.
        for language in Language::ALL {
            assert_eq!(items(&here, language)[1].name, "Codex Desktop", "{language:?}");
        }
        // Counted and weighed with the rest, on the Service page too.
        assert_eq!(total(&here), (3, 1_624_558_859));
        assert_eq!(
            summary(&here, Language::English),
            "3 items, 1.6 GB. Most can be removed one at a time."
        );
        // Never set up: no row, and nothing counted for it.
        let absent = footprint(false);
        assert!(!items(&absent, Language::English)
            .iter()
            .any(|row| row.id == "codex_desktop"));
        assert_eq!(total(&absent), (2, 1_624_557_323));
    }

    #[test]
    fn uninstall_is_never_run_from_a_build_or_a_test() {
        // This process never booted: a build outside the bundle. The command
        // is refused before anything is started.
        let argv = uninstall_argv(Path::new("/bin/echo"));
        let refused = uninstall_now(&argv).expect_err("a test runs nothing");
        assert!(refused.contains("installed app"), "{refused}");
    }
}
