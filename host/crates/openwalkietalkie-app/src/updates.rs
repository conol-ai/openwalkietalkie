//! The Service page's updates: the app's own, which Sparkle installs in a
//! signed release, and the deck's firmware, which the service looks for,
//! downloads and installs by cable while this window asks and shows.
//!
//! **The looking is the service's.** It looks for a release by itself every
//! six hours while a deck is around, with this window closed or the app not
//! running, and a look opens the deck over USB and reads the release
//! manifest. So the page only reads `firmware.status`, asks for a
//! `firmware.check` when the owner does (**Check**, **Check for Updates…**:
//! [`firmware_request`]), shows what came back, and sends the one
//! `firmware.install` the owner confirmed in the review dialog. A build, a
//! harness walk or a window on a test service never has the service look:
//! the shell sends their check as the `firmware.status` read it would have
//! ended in ([`crate::app::Launch::outgoing`]).
//!
//! **What the service found.** The release manifest names the app's version
//! too, so the app's row says when a newer one is out, in every build
//! ([`app_update_line`]); Sparkle still installs it. And the service
//! compares the release with the firmware the deck named in its hello, so a
//! deck on Bluetooth alone, which a look cannot open, hears of a newer
//! firmware too ([`newer_firmware`]).
//!
//! **The deck has the last word.** On a linked deck the service does not
//! start the install it is sent: it asks on the deck first, and answers
//! `confirming` at once. The dialog then says to press ENTER there
//! ([`status_line`]) and follows the status as it does a download; a card
//! dismissed or left for a minute comes back as the service's reason, with
//! nothing installed. A deck in recovery cannot be asked, and installs as
//! it always has: there the dialog asks the owner for the board.
//!
//! The review dialog draws the status as it is now, every frame, so what the
//! owner confirms is what **Install** sends: a release or a deck that
//! changes under the open dialog changes its words before the button can be
//! pressed, and lets go of a board picked for the deck that was there
//! ([`confirmation_target`]).
//!
//! An app update replaces the service's executable on disk and not the
//! service running from it. Once a run the page asks the service which copy
//! it runs, and restarts an older copy of this bundle's own service when it
//! is idle ([`needs_restart`]).
use crate::shell::Shell;
use crate::style::palette;
use crate::ui::{self, Btn, Segmented, Type as _};
use crate::updater::UpdateCheck;
use deck_client::ClientError;
use deck_gui_core::i18n::Language;
use deck_gui_core::UiAction;
use gpui::{div, prelude::*, px, AnyElement, Context, Window};
use gpui_component::{h_flex, v_flex};
use serde_json::Value;
use std::time::Duration;

/// The review dialog's id, and how wide it is drawn.
pub(crate) const FIRMWARE_DIALOG: &str = "firmware";
const FIRMWARE_WIDTH: f32 = 460.;

/// How often the status is read again: every second while the service is
/// working on the deck, every half minute otherwise.
const POLL_BUSY: Duration = Duration::from_secs(1);
const POLL_IDLE: Duration = Duration::from_secs(30);

/// The deck's boards, as the firmware names them and as they are printed on
/// the board.
pub const BOARDS: [(&str, &str); 2] = [("hw-a", "Rev A"), ("hw-b", "Rev B")];

/// What the page keeps of the firmware. None of it is the mirror: the
/// status is a call's answer, and nothing folds it.
#[derive(Default)]
pub struct FirmwarePane {
    /// The service's last answer about the firmware, as it came.
    pub(crate) status: Value,
    /// A read or an install is on its way.
    pending: bool,
    /// The next read is waiting for its moment.
    polling: bool,
    /// The board the owner picked for a deck that cannot name its own.
    pub(crate) board: Option<String>,
    /// The service an app update replaced was asked about, once a run.
    restart_asked: bool,
}

// ---- what the firmware says, as pure functions -------------------------------

/// `value[key]` as a string; empty when it is not one.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Whether the service is working on the deck: checking, waiting for a
/// press on it, downloading, installing, waiting for it to come back.
pub fn busy(status: &Value) -> bool {
    matches!(
        text(status, "phase"),
        "checking" | "confirming" | "downloading" | "installing" | "reconnecting" | "bootloader"
    )
}

/// Whether the install waits for ENTER on the deck: nothing is downloaded
/// or written until the deck confirms it.
pub fn confirming(status: &Value) -> bool {
    text(status, "phase") == "confirming"
}

/// The release a check found newer than the deck's firmware, by its version.
pub fn available(status: &Value) -> Option<String> {
    let version = text(&status["release"]["firmware"], "version");
    (status["available"].as_bool() == Some(true) && !version.is_empty())
        .then(|| version.to_string())
}

/// The deck firmware to tell the owner about: the release a check found
/// newer than the deck on the cable ([`available`]), else newer than the
/// firmware the deck named in its hello, which the service compares
/// (`linked`). The second is how a deck on Bluetooth alone hears of one;
/// installing it still takes the cable.
pub fn newer_firmware(status: &Value) -> Option<String> {
    available(status).or_else(|| {
        let version = text(&status["release"]["firmware"], "version");
        (status["linked"]["available"].as_bool() == Some(true) && !version.is_empty())
            .then(|| version.to_string())
    })
}

/// What the page asks the service for: a look for a release only when the
/// owner asks for one (`force`: **Check**, **Check for Updates…**), and
/// otherwise the status as it is. The service looks by itself every six
/// hours while a deck is around, whether this window is open or not.
pub fn firmware_request(force: bool) -> UiAction {
    if force {
        UiAction::FirmwareCheck { force: true }
    } else {
        UiAction::FirmwareStatus
    }
}

/// The app row's line once the service found a newer app in the release
/// manifest (`firmware.status.app`): "OpenWalkieTalkie {0} is available.";
/// `None` before it has, and for this version or an older one. The
/// service's `available` compares the release with the service's own
/// version, and a service older than this app runs on after an update
/// until it restarts, so the release must be newer than this app too. It
/// says so in every build: Sparkle installs it in a signed release
/// (**Check**), and any other build says why it cannot.
pub fn app_update_line(status: &Value, language: Language) -> Option<String> {
    let app = &status["app"];
    let version = text(app, "version");
    (app["available"].as_bool() == Some(true) && newer_than(version, env!("CARGO_PKG_VERSION")))
        .then(|| language.format("OpenWalkieTalkie {0} is available.", &[version]))
}

/// Whether `announced` is a newer version than `own`: `a.b.c` compared as
/// numbers, a release newer than its own pre-release (`0.14.0` after
/// `0.14.0-rc.1`), build metadata ignored. `false` when either is not such
/// a version.
pub fn newer_than(announced: &str, own: &str) -> bool {
    fn parts(version: &str) -> Option<([u64; 3], bool)> {
        let version = version.split('+').next()?;
        let (core, pre) = match version.split_once('-') {
            Some((core, _)) => (core, true),
            None => (version, false),
        };
        let mut numbers = core.split('.');
        let mut out = [0u64; 3];
        for slot in &mut out {
            *slot = numbers.next()?.parse().ok()?;
        }
        numbers.next().is_none().then_some((out, pre))
    }
    match (parts(announced), parts(own)) {
        (Some((new, new_pre)), Some((current, current_pre))) => {
            new > current || (new == current && current_pre && !new_pre)
        }
        _ => false,
    }
}

/// A deck that stopped half way through an update: in its maintenance mode,
/// or in recovery. The review dialog takes it on from there.
pub fn interrupted(status: &Value) -> bool {
    status["device"]["maintenance"] == true || status["device"]["recovery"] == true
}

/// What the Deck firmware row offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareStep {
    /// **Check**: have the service look for a release.
    Check,
    /// **Review…**: the dialog, for a release that was found, an install
    /// under way, or one to take on.
    Review,
}

/// The row's step for `status`.
pub fn firmware_step(status: &Value) -> FirmwareStep {
    if available(status).is_some() || busy(status) || interrupted(status) {
        FirmwareStep::Review
    } else {
        FirmwareStep::Check
    }
}

/// The board an install is for: the one the owner picked, else the one the
/// deck names.
pub fn install_board<'a>(status: &'a Value, picked: Option<&'a str>) -> &'a str {
    picked.unwrap_or_else(|| text(&status["device"], "board"))
}

/// A board as it is printed on it: `Rev A`, `Rev B`; `None` for a name this
/// build does not know.
pub fn board_name(board: &str) -> Option<&'static str> {
    BOARDS
        .iter()
        .find(|(id, _)| *id == board)
        .map(|(_, name)| *name)
}

/// Whether **Install** may be pressed for `board`: the service is idle, a
/// deck is on the cable with its serial, and a release was found with a
/// build for that board.
pub fn can_install(status: &Value, board: &str) -> bool {
    !busy(status)
        && !text(&status["device"], "serial").is_empty()
        && available(status).is_some()
        && status["release"]["firmware"]["variants"]
            .get(board)
            .is_some()
}

/// Whether the install may replace the bootloader: the service says so, or
/// a release of the second protocol meets a deck whose recovery is older,
/// or one already in maintenance.
pub fn requires_bootloader_warning(status: &Value) -> bool {
    status["bootloader_warning"].as_bool() == Some(true)
        || (status["release"]["firmware"]["protocol"] == 2
            && (status["device"]["recovery_protocol"] != 2
                || status["device"]["maintenance"] == true))
}

/// What the owner is confirming: the deck, the release and the warning. A
/// change to any of them lets go of a board picked for what was there.
pub fn confirmation_target(status: &Value) -> Value {
    serde_json::json!({
        "device": status["device"],
        "release": status["release"],
        "warning": requires_bootloader_warning(status),
    })
}

/// What the review dialog says the service is doing: the status's message
/// in the window's language, with the progress while it works on the deck;
/// while the install waits for the deck, what to press there; before the
/// service has said anything, how to begin.
///
/// The service writes its firmware status in a fixed set of English
/// sentences (`deckd`'s `firmware/mod.rs` and `firmware/migration.rs`), and
/// each has a row in the tables, looked up here by its English; a sentence
/// a newer service adds is shown as it came until it has one. The
/// message this page writes itself when the service cannot be asked is
/// already in the window's language, and no table has it as a key, so the
/// lookup leaves it as it is. Why an install the deck did not confirm
/// installed nothing is one of the service's sentences, and so is what a
/// confirmed install waits for before it starts.
pub fn status_line(status: &Value, language: Language) -> String {
    let message = language.text(text(status, "message"));
    // Waiting for the deck is no progress, and has no percentage: what to
    // press there, or once it is pressed, what the install still waits for
    // (the service says which).
    if confirming(status) {
        return if message.is_empty() {
            language.text("Press ENTER on the deck to start the install.")
        } else {
            message
        }
        .to_string();
    }
    if busy(status) {
        let progress = status["progress"].as_u64().unwrap_or(0).to_string();
        format!("{message} {}", language.format("{0}%", &[&progress]))
    } else if message.is_empty() {
        language
            .text("Check for firmware updates with your deck connected by USB.")
            .to_string()
    } else {
        message.to_string()
    }
}

/// Why the service refused an install, in the window's language: its
/// reason is one of its fixed English sentences, looked up like the status
/// line's; one this build has no row for is shown as it came.
pub fn refusal_line(error: &ClientError, language: Language) -> String {
    language.text(&failure_text(error)).to_string()
}

/// Why a firmware call failed: the service's own reason when it gave one.
pub fn failure_text(error: &ClientError) -> String {
    let details = match error {
        ClientError::Rpc { data, .. } => data.as_ref(),
        ClientError::Refused(error) => Some(&error.data),
        _ => None,
    };
    details
        .and_then(|data| data.get("why"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

/// The service inside this copy of the app, when this copy is a bundle:
/// `Contents/MacOS/deckd`, beside the app's own executable.
fn bundled_daemon() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let directory = exe.parent()?;
    if directory.file_name()? != "MacOS" || directory.parent()?.file_name()? != "Contents" {
        return None;
    }
    Some(directory.join("deckd").to_string_lossy().into_owned())
}

/// Whether the service `info` describes is this bundle's own (`expected`)
/// running an older version than this app's: the one an app update replaced
/// on disk and that is still running from memory.
pub fn needs_restart(info: &Value, expected: &str, version: &str) -> bool {
    text(info, "executable") == expected
        && !text(info, "daemon").is_empty()
        && text(info, "daemon") != version
}

impl Shell {
    /// Read the firmware's status; `force` has the service look for a
    /// release first ([`firmware_request`]). A release that turns up is said
    /// once, whatever page is open, whoever looked for it.
    pub(crate) fn refresh_firmware(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let firmware = &self.service.firmware;
        if firmware.pending || !self.state.connected() || self.service.staged() {
            return;
        }
        self.service.firmware.pending = true;
        let action = firmware_request(force);
        self.ask(action, window, cx, move |this, result, window, cx| {
            this.service.firmware.pending = false;
            match result {
                Ok(status) => {
                    let firmware = &mut this.service.firmware;
                    let found =
                        available(&status).is_some() && available(&firmware.status).is_none();
                    if confirmation_target(&status) != confirmation_target(&firmware.status) {
                        firmware.board = None;
                    }
                    firmware.status = status;
                    if found {
                        this.toast(
                            this.tr("A firmware update is available in Service → Updates."),
                            window,
                            cx,
                        );
                    }
                }
                Err(error) => {
                    let message = this.tr("Firmware updates are unavailable from this service.");
                    this.service.firmware.status = serde_json::json!({
                        "phase": "error",
                        "error": failure_text(&error),
                        "message": message,
                    });
                }
            }
            this.schedule_firmware_poll(window, cx);
        });
    }

    /// Read again in a moment: every second while the service works on the
    /// deck, else every half minute. A service that went away ends the
    /// round ([`Shell::refresh_firmware`] asks nothing then); the next
    /// connection starts it again.
    fn schedule_firmware_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.firmware.polling {
            return;
        }
        self.service.firmware.polling = true;
        let delay = if busy(&self.service.firmware.status) {
            POLL_BUSY
        } else {
            POLL_IDLE
        };
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update_in(cx, |this, window, cx| {
                this.service.firmware.polling = false;
                this.refresh_firmware(false, window, cx);
                this.restart_updated_service(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Restart a running service that an app update has replaced on disk,
    /// once it is idle: asked once a run, of this bundle's own service only,
    /// and never by a build outside the bundle. A refusal (it is busy with
    /// the deck or a recognition) is asked again on the next round.
    pub(crate) fn restart_updated_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.service.firmware.restart_asked || !self.state.connected() || self.service.staged() {
            return;
        }
        let Some(expected) = bundled_daemon() else {
            return;
        };
        self.service.firmware.restart_asked = true;
        self.ask(
            UiAction::ServiceInfo,
            window,
            cx,
            move |this, result, window, cx| {
                let Ok(info) = result else {
                    this.service.firmware.restart_asked = false;
                    return;
                };
                if !needs_restart(&info, &expected, env!("CARGO_PKG_VERSION")) {
                    return;
                }
                this.ask(
                    UiAction::ServiceRestartWhenIdle,
                    window,
                    cx,
                    |this, result, _, _| {
                        // Busy is asked again by the poll; a failed call never
                        // forces an exit or stops the service.
                        if result.is_err() {
                            this.service.firmware.restart_asked = false;
                        }
                    },
                );
            },
        );
    }

    /// **Check for Updates…**: the app's, then the deck's firmware.
    pub(crate) fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.check_app_update(window, cx);
        self.refresh_firmware(true, window, cx);
    }

    /// The app's own update, which Sparkle checks for, downloads, installs
    /// and relaunches in a signed release. Any other build says why not.
    pub(crate) fn check_app_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self
            .service
            .updater
            .check_for_updates_localized(self.language())
        {
            Ok(UpdateCheck::Started) => {}
            Ok(UpdateCheck::Busy) => {
                self.toast(self.tr("An app update is already in progress."), window, cx);
            }
            Err(reason) => self.toast(reason, window, cx),
        }
    }

    /// The firmware's review dialog.
    pub(crate) fn review_firmware(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(FIRMWARE_DIALOG, window);
        cx.notify();
    }

    /// Install what the dialog shows, as it is now. The service answers with
    /// its status at once and works on; the poll follows it.
    fn install_firmware(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let firmware = &self.service.firmware;
        let status = &firmware.status;
        let board = install_board(status, firmware.board.as_deref()).to_string();
        if firmware.pending || !can_install(status, &board) {
            return;
        }
        let action = UiAction::FirmwareInstall {
            serial: text(&status["device"], "serial").into(),
            version: text(&status["release"]["firmware"], "version").into(),
            allow_bootloader_update: requires_bootloader_warning(status),
            board,
        };
        self.service.firmware.pending = true;
        self.ask(action, window, cx, |this, result, window, cx| {
            this.service.firmware.pending = false;
            match result {
                Ok(status) => this.service.firmware.status = status,
                Err(error) => this.toast(refusal_line(&error, this.language()), window, cx),
            }
            this.refresh_firmware(false, window, cx);
        });
    }

    /// The review dialog: what the service says it is doing, the deck on the
    /// cable and the release, the board when the deck cannot name it, the
    /// bootloader's warning when it applies, and **Install** — or only
    /// **Close** while there is nothing to install.
    pub(crate) fn render_firmware_dialog(&self, cx: &Context<Self>) -> (AnyElement, f32) {
        let p = *palette(cx);
        let firmware = &self.service.firmware;
        let status = &firmware.status;
        let device = &status["device"];
        let board = install_board(status, firmware.board.as_deref());
        let release = available(status);
        let serial = text(device, "serial");
        let warning = requires_bootloader_warning(status);
        let working = busy(status) || firmware.pending;
        let ready = !working && self.state.connected() && can_install(status, board);
        let doing = status_line(status, self.language());
        let deck = (!serial.is_empty()).then(|| {
            let named = match board {
                "" => self.tr("Select your board below"),
                board => board_name(board).unwrap_or(board),
            };
            let installed = if device["recovery"] == true {
                self.tr("Recovery mode")
            } else {
                text(device, "version")
            };
            let version = text(&status["release"]["firmware"], "version");
            self.fmt(
                "{0} · USB {1} · Installed {2} · Available {3}",
                &[named, serial, installed, version],
            )
        });
        // A deck that cannot say which board it is: the owner reads it off
        // the board.
        let pick = (!serial.is_empty() && text(device, "board").is_empty()).then(|| {
            let picked = BOARDS
                .iter()
                .position(|(id, _)| Some(*id) == firmware.board.as_deref())
                .unwrap_or(usize::MAX);
            v_flex()
                .gap(px(8.))
                .child(ui::hint(
                    self.tr("The device cannot report its board. Choose the revision printed on your board."),
                    &p,
                ))
                .child(
                    Segmented::new(
                        "firmware-board",
                        BOARDS.iter().map(|(_, name)| (*name).into()).collect(),
                        picked,
                    )
                    .disabled(working)
                    .on_select(cx.listener(|this, at: &usize, _, cx| {
                        this.service.firmware.board =
                            BOARDS.get(*at).map(|(id, _)| (*id).to_string());
                        cx.notify();
                    })),
                )
        });
        let caution = warning.then(|| {
            v_flex()
                .gap(px(4.))
                .p(px(12.))
                .rounded(px(8.))
                .bg(p.warn_wash)
                .border_1()
                .border_color(p.warn_line)
                .child(
                    div()
                        .semibold()
                        .text_color(p.warn_text)
                        .child(ui::para(self.tr("Bootloader update — power must stay connected"))),
                )
                .child(ui::hint(
                    self.tr("This update may replace the bootloader, the software that starts your device. Keep USB-C firmly connected and your computer awake until the update finishes. Do not unplug or power off the device. Losing power during bootloader replacement can leave it unable to start or update over USB and require hardware repair. Bluetooth pairings are preserved."),
                    &p,
                ))
        });
        let confirm = release.as_ref().map(|version| {
            div().child(ui::para(self.fmt(
                "Install firmware {0}? Keep USB connected until your deck restarts.",
                &[version],
            )))
        });
        let error = status["error"]
            .as_str()
            .filter(|error| !error.is_empty())
            .map(|error| {
                div()
                    .text_color(p.bad_text)
                    .child(ui::para(error.to_string()))
            });
        let close = Btn::new(
            "firmware-close",
            if release.is_some() {
                self.tr("Cancel")
            } else {
                self.tr("Close")
            },
        )
        .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx)));
        let install = release.is_some().then(|| {
            Btn::new(
                "firmware-install",
                if warning {
                    self.tr("I understand — update device")
                } else {
                    self.tr("Install")
                },
            )
            .primary()
            .disabled(!ready)
            .on_click(cx.listener(|this, _, window, cx| this.install_firmware(window, cx)))
        });
        let close = if install.is_some() {
            close
        } else {
            close.primary()
        };
        let body = v_flex()
            .p(px(20.))
            .gap(px(12.))
            .ts(13., 18.)
            .child(
                v_flex()
                    .gap(px(4.))
                    .child(div().semibold().child(self.tr("Install firmware")))
                    .child(div().text_color(p.text2).child(ui::para(doing))),
            )
            .children(deck.map(|line| {
                div()
                    .ts(12., 16.)
                    .tnum()
                    .text_color(p.text2)
                    .child(ui::para(line))
            }))
            .children(pick)
            .children(caution)
            .children(confirm)
            .children(error)
            .child(
                h_flex()
                    .mt(px(4.))
                    .gap(px(8.))
                    .justify_end()
                    .child(close)
                    .children(install),
            );
        (body.into_any_element(), FIRMWARE_WIDTH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bootloader_warning_covers_unknown_old_and_interrupted_devices() {
        for device in [
            json!({}),
            json!({"recovery_protocol": 1}),
            json!({"recovery_protocol": 2, "maintenance": true}),
        ] {
            assert!(requires_bootloader_warning(
                &json!({"device": device, "release": {"firmware": {"protocol": 2}}})
            ));
        }
        assert!(!requires_bootloader_warning(
            &json!({"device": {"recovery_protocol": 2}, "release": {"firmware": {"protocol": 2}}})
        ));
        // The service's own say-so is enough.
        assert!(requires_bootloader_warning(
            &json!({"bootloader_warning": true})
        ));
        assert!(busy(&json!({"phase": "bootloader"})));
        assert!(!busy(&json!({"phase": "ready"})));
    }

    #[test]
    fn changing_release_or_device_invalidates_confirmation() {
        let status = json!({"device": {"serial": "A"}, "release": {"firmware": {"protocol": 2, "version": "0.3.1"}}});
        let mut changed = status.clone();
        changed["release"]["firmware"]["version"] = "0.3.2".into();
        assert_ne!(confirmation_target(&status), confirmation_target(&changed));
        changed = status.clone();
        changed["device"]["serial"] = "B".into();
        assert_ne!(confirmation_target(&status), confirmation_target(&changed));
        // Progress is not a different thing to confirm.
        changed = status.clone();
        changed["progress"] = 50.into();
        assert_eq!(confirmation_target(&status), confirmation_target(&changed));
    }

    /// R8: the service looks for a release by itself, so the window asks
    /// for a look only when the owner does, and otherwise reads.
    #[test]
    fn the_window_only_asks_for_a_check_when_told() {
        assert_eq!(firmware_request(false), UiAction::FirmwareStatus);
        assert_eq!(firmware_request(false).request().0, "firmware.status");
        assert_eq!(
            firmware_request(true),
            UiAction::FirmwareCheck { force: true }
        );
    }

    #[test]
    fn the_app_row_names_a_newer_app_once_the_service_found_one() {
        crate::i18n::install();
        let found = json!({"app": {
            "version": "99.0.0",
            "available": true,
            "release_url": "https://github.com/conol-ai/openwalkietalkie/releases/tag/v99.0.0",
        }});
        let said = [
            Language::English,
            Language::SimplifiedChinese,
            Language::TraditionalChinese,
            Language::Japanese,
        ]
        .map(|language| app_update_line(&found, language));
        assert_eq!(
            said,
            [
                "OpenWalkieTalkie 99.0.0 is available.",
                "OpenWalkieTalkie 99.0.0 现已可用。",
                "OpenWalkieTalkie 99.0.0 現已可用。",
                "OpenWalkieTalkie 99.0.0 が利用可能です。",
            ]
            .map(|line| Some(line.to_string()))
        );
        // Nothing found yet, this version or an older one, and a service
        // that predates the field.
        for status in [
            json!({}),
            json!({"app": null}),
            json!({"app": {"version": "0.13.0", "available": false}}),
            json!({"app": {"available": true}}),
            // A service older than this app (it runs on after an update until
            // it restarts) says its own version is behind the release: this
            // app is that release, or newer still.
            json!({"app": {"version": env!("CARGO_PKG_VERSION"), "available": true}}),
            json!({"app": {"version": "0.1.0", "available": true}}),
            json!({"app": {"version": "next", "available": true}}),
        ] {
            assert_eq!(
                app_update_line(&status, Language::English),
                None,
                "{status}"
            );
        }
    }

    /// Versions compared as numbers, a release after its own pre-release,
    /// and nothing that is not `a.b.c` newer than anything.
    #[test]
    fn a_version_is_newer_by_its_numbers() {
        assert!(newer_than("0.14.0", "0.13.0"));
        assert!(newer_than("0.10.0", "0.9.9"), "numbers, not text");
        assert!(newer_than("1.0.0", "0.99.99"));
        assert!(newer_than("0.14.0", "0.14.0-rc.1"));
        assert!(newer_than("0.14.0+build.7", "0.13.0"));
        for (announced, own) in [
            ("0.13.0", "0.13.0"),
            ("0.12.9", "0.13.0"),
            ("0.14.0-rc.1", "0.14.0"),
            ("0.14", "0.13.0"),
            ("0.14.0.1", "0.13.0"),
            ("", "0.13.0"),
            ("0.14.0", "dev"),
        ] {
            assert!(!newer_than(announced, own), "{announced} vs {own}");
        }
    }

    /// A deck on Bluetooth alone cannot be opened by a look, so the service
    /// compares the release with the firmware the deck named in its hello;
    /// the owner hears of it, and the row still asks for the cable.
    #[test]
    fn a_deck_on_bluetooth_alone_hears_of_a_newer_firmware_from_its_hello() {
        let release = json!({"firmware": {"version": "0.14.0"}});
        let cable = json!({"available": true, "release": release});
        assert_eq!(newer_firmware(&cable).as_deref(), Some("0.14.0"));
        let hello = json!({
            "available": false,
            "release": release,
            "linked": {"fw": "0.13.0-openwalkietalkie", "available": true},
        });
        assert_eq!(newer_firmware(&hello).as_deref(), Some("0.14.0"));
        assert_eq!(
            available(&hello),
            None,
            "nothing to install without the cable"
        );
        assert_eq!(firmware_step(&hello), FirmwareStep::Check);
        // Up to date, nothing found yet, and a service that predates the
        // field.
        for status in [
            json!({"release": release, "linked": {"fw": "0.14.0", "available": false}}),
            json!({"linked": {"fw": "0.13.0", "available": true}}),
            json!({"release": release}),
        ] {
            assert_eq!(newer_firmware(&status), None, "{status}");
        }
    }

    #[test]
    fn only_an_older_copy_of_this_bundled_daemon_is_restarted() {
        let exe = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";
        assert!(needs_restart(
            &json!({"daemon": "0.1.0", "executable": exe}),
            exe,
            "0.2.0"
        ));
        assert!(!needs_restart(
            &json!({"daemon": "0.2.0", "executable": exe}),
            exe,
            "0.2.0"
        ));
        assert!(!needs_restart(
            &json!({"daemon": "0.1.0", "executable": "/tmp/deckd"}),
            exe,
            "0.2.0"
        ));
        assert!(!needs_restart(&json!({}), exe, "0.2.0"));
        // A test binary is no bundle, and restarts nothing.
        assert_eq!(bundled_daemon(), None);
    }
}
