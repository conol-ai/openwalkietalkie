//! Release discovery and verified, board-specific USB application updates.
//! All network/USB work runs off the core. Only an explicit install call
//! acquires HID maintenance and writes flash; polling never installs anything.
//!
//! On a deck that can show a card, the install call itself does not start
//! one: the core asks the deck first, and the install waits in the phase
//! `confirming` ([`Updater::await_confirmation`]) for the press that starts
//! it ([`Updater::confirmed`]) or for the card to go unanswered
//! ([`Updater::not_confirmed`]). A deck in recovery cannot show a card and
//! installs as it always has (`Core::install_after_press`).
//!
//! The service looks for a release by itself (R8, [`schedule`]): every six
//! hours while a deck is around, with the window closed or the app not
//! running. Such a look is quiet ([`Updater::check_scheduled`]): one that
//! cannot reach the feed leaves the status as it was. The release feed's
//! manifest names the app's version too, so `firmware.status` says when a
//! newer app is out ([`Status::app`]), and when the release is newer than
//! the firmware the deck said it runs in its hello ([`Status::linked`]),
//! which is how a deck on Bluetooth alone hears of one.
mod bulk;
mod migration;
pub mod schedule;
mod transport;

pub use schedule::Schedule;

use crate::hid::Maintenance;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const REPOSITORY: &str = "conol-ai/openwalkietalkie";
pub const MANIFEST_URL: &str =
    "https://github.com/conol-ai/openwalkietalkie/releases/latest/download/release-manifest.json";
pub const APP_BASE: u32 = 0x12020000;
// Raw image span, including FF padding over the two preserved bond stores.
pub const APP_SIZE: usize = 16 * 1024 * 1024 - 128 * 1024;
pub const LEGACY_APP_SIZE: usize = 7 * 1024 * 1024 - 64 * 1024;
pub const BONDS_START: u32 = 0x12700000;
pub const BONDS_END: u32 = 0x12720000;
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// The updater's folder under the caches directory: the release manifest
/// and the images it downloaded.
pub const CACHE_DIR: &str = "firmware";
/// The folder inside [`CACHE_DIR`] that keeps each deck's bootloader as it
/// was before an install replaced it. It is not a download and nothing can
/// fetch it again, so clearing the cache (`care.cache.clear`) leaves it.
pub const BOOTLOADER_BACKUPS: &str = "bootloader-backups";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Firmware {
    pub version: String,
    pub protocol: u8,
    pub app_base: u32,
    pub app_size: usize,
    pub variants: BTreeMap<String, Asset>,
    #[serde(default)]
    pub migrations: BTreeMap<String, Migration>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Migration {
    pub bootstrap: Asset,
    pub bootloader: Asset,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema: u8,
    pub product: String,
    pub release_url: String,
    pub firmware: Firmware,
    /// The app the release ships, as `tools/release.py` writes it (its
    /// `version` and the Mac downloads), kept as it came: the service reads
    /// only the version ([`Manifest::app_version`]), so a section it cannot
    /// read never fails the firmware's check. The manifest the app bundle
    /// carries for its firmware has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<serde_json::Value>,
}

impl Manifest {
    /// The app version this release names (`app.version`), when it is a
    /// stable version; `None` for no app section or one the service cannot
    /// read.
    #[must_use]
    pub fn app_version(&self) -> Option<Version> {
        let version = self.app.as_ref()?.get("version")?.as_str()?;
        Version::parse(version)
            .ok()
            .filter(|version| version.pre.is_empty() && version.build.is_empty())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1
            || self.product != "openwalkietalkie"
            || self.firmware.app_base != APP_BASE
            || !matches!(
                (self.firmware.protocol, self.firmware.app_size),
                (1, LEGACY_APP_SIZE) | (2, APP_SIZE)
            )
        {
            return Err("This release targets another product or flash layout.".into());
        }
        let version =
            Version::parse(&self.firmware.version).map_err(|_| "Invalid firmware version.")?;
        if !version.pre.is_empty() || !version.build.is_empty() {
            return Err("The stable updater requires a stable firmware version.".into());
        }
        let prefix = format!("https://github.com/{REPOSITORY}/releases/tag/v");
        let tag = self
            .release_url
            .strip_prefix(&prefix)
            .ok_or("Unexpected release URL.")?;
        let tag_version = Version::parse(tag).map_err(|_| "Invalid release tag.")?;
        if !tag_version.pre.is_empty() || !tag_version.build.is_empty() {
            return Err("Invalid stable release tag.".into());
        }
        if self.firmware.variants.is_empty() {
            return Err("No firmware in this release.".into());
        }
        for (board, asset) in &self.firmware.variants {
            if !matches!(board.as_str(), "hw-a" | "hw-b") {
                return Err("Unknown board in release.".into());
            }
            let name = format!("OpenWalkieTalkie-{}-{board}.bin", self.firmware.version);
            let url = format!("https://github.com/{REPOSITORY}/releases/download/v{tag}/{name}");
            if asset.name != name
                || asset.url != url
                || !(8..=self.firmware.app_size).contains(&asset.size)
                || asset.sha256.len() != 64
                || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("Invalid firmware asset or download location.".into());
            }
            if self.firmware.protocol == 2 {
                let migration = self
                    .firmware
                    .migrations
                    .get(board)
                    .ok_or("This release is missing its bootloader migration images.")?;
                for (kind, asset, max) in [
                    (
                        "bootstrap",
                        &migration.bootstrap,
                        (BONDS_START - APP_BASE) as usize,
                    ),
                    ("recovery", &migration.bootloader, 65536),
                ] {
                    let name = format!(
                        "OpenWalkieTalkie-{}-{kind}-{board}.bin",
                        self.firmware.version
                    );
                    let url =
                        format!("https://github.com/{REPOSITORY}/releases/download/v{tag}/{name}");
                    if asset.name != name
                        || asset.url != url
                        || !(8..=max).contains(&asset.size)
                        || asset.sha256.len() != 64
                        || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return Err("Invalid bootloader migration asset.".into());
                    }
                }
            }
        }
        if self
            .firmware
            .migrations
            .keys()
            .any(|b| !self.firmware.variants.contains_key(b))
            || (self.firmware.protocol == 1 && !self.firmware.migrations.is_empty())
        {
            return Err("Unexpected bootloader migration images.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Device {
    pub serial: String,
    pub board: Option<String>,
    pub version: Option<String>,
    pub recovery: bool,
    #[serde(default)]
    pub recovery_protocol: Option<u8>,
    #[serde(default)]
    pub maintenance: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub phase: String,
    pub progress: u8,
    pub message: String,
    pub device: Option<Device>,
    pub release: Option<Manifest>,
    pub error: Option<String>,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub bootloader_warning: bool,
    /// The app release the feed named when it last answered ([`AppUpdate`]);
    /// `None` until it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppUpdate>,
    /// The firmware the deck said it runs, against the release
    /// ([`Linked`]). The core fills it in for the methods' replies
    /// ([`Status::with_linked`]): the updater has no hello to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked: Option<Linked>,
}

/// The app release the release feed names (R8): its version, whether it is
/// newer than this service, which ships in the same bundle as the app, and
/// the release's page. It only says so: Sparkle's signed feed stays the one
/// thing that installs an app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppUpdate {
    pub version: String,
    pub available: bool,
    pub release_url: String,
}

/// The firmware the deck named in its hello (the deck linked now, else the
/// last one linked here), against the release the last check found (R8). A
/// check opens only the deck on the cable ([`Status::device`]), so this is
/// how a deck on Bluetooth alone hears of a newer firmware. Installing it
/// still takes the cable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Linked {
    /// As the deck said it (`0.13.0-openwalkietalkie`).
    pub fw: String,
    /// The release's firmware is newer.
    pub available: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            progress: 0,
            message: "Connect your deck with USB to update its firmware.".into(),
            device: None,
            release: None,
            error: None,
            available: false,
            bootloader_warning: false,
            app: None,
            linked: None,
        }
    }
}

impl Status {
    /// This status with [`Status::linked`] for `fw`, the firmware a deck
    /// named in its hello: whether the release is newer. `None` for no
    /// deck that said.
    #[must_use]
    pub fn with_linked(mut self, fw: Option<&str>) -> Self {
        let release = self
            .release
            .as_ref()
            .and_then(|release| firmware_version(&release.firmware.version));
        self.linked = fw.map(|fw| Linked {
            fw: fw.to_string(),
            available: release
                .as_ref()
                .zip(firmware_version(fw))
                .is_some_and(|(new, current)| *new > current),
        });
        self
    }

    /// An update operation is under way: a check, an install waiting for
    /// the deck's press, or one writing to it. No other starts meanwhile.
    pub fn busy(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "checking"
                | "confirming"
                | "downloading"
                | "installing"
                | "reconnecting"
                | "bootloader"
        )
    }
}

/// An install, as `firmware.install` names it: the board, and the deck and
/// release the last check found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Install {
    pub board: String,
    pub serial: String,
    pub version: String,
    pub allow_bootloader_update: bool,
}

#[derive(Clone, Default)]
pub struct Updater {
    state: Arc<Mutex<Status>>,
    /// The last check that found a release: an unforced check within
    /// [`CHECK_INTERVAL`] of it answers the status as it is.
    checked: Arc<Mutex<Option<Instant>>>,
    /// What the release feed said, and when ([`Feed`]).
    feed: Arc<Mutex<Feed>>,
    /// The owner asked for a look while the service's own was under way:
    /// that look answers the owner then, failure and all.
    asked: Arc<AtomicBool>,
    /// The install that waits for the deck's press while the phase is
    /// `confirming` ([`Updater::await_confirmation`]).
    awaiting: Arc<Mutex<Option<Install>>>,
    /// Never probe USB or fetch: see [`Updater::offline`].
    offline: bool,
    /// A test's updater ([`testkit::checked`]): an install that passed
    /// every check is kept here instead of started, so no thread, USB or
    /// download is ever reached (a started install that fails probes USB).
    #[cfg(test)]
    started: Option<Arc<Mutex<Vec<Install>>>>,
}

/// What the release feed said, and when: what the service's own looks go by
/// ([`Schedule`]), and the app release it names ([`Status::app`]).
#[derive(Clone, Debug, Default)]
struct Feed {
    /// When it last answered a look with a manifest the service accepts.
    answered: Option<Instant>,
    /// When a look last could not reach it, or failed before it asked.
    failed: Option<Instant>,
    /// The app release it named at its last answer: the version, and the
    /// release's page.
    app: Option<(Version, String)>,
}

/// What one look for a release came to ([`Updater::conclude`]).
struct Found {
    /// The release to offer, the newer of the one the app carries and the
    /// one the feed names, or why there is none.
    release: Result<Manifest, String>,
    /// The release feed answered with a manifest the service accepts.
    answered: bool,
    /// The app release that manifest names ([`Manifest::app_version`]),
    /// with the release's page.
    app: Option<(Version, String)>,
}

impl Found {
    /// What a look found from the release the app carries (`bundled`) and
    /// the feed's answer (`remote`): the newer firmware of the two, and
    /// whether the feed answered and with which app release, whichever
    /// firmware wins.
    fn of(bundled: Option<Manifest>, remote: Result<Manifest, String>) -> Found {
        let answered = remote.is_ok();
        let app = remote
            .as_ref()
            .ok()
            .and_then(|remote| Some((remote.app_version()?, remote.release_url.clone())));
        Found {
            release: select_release(bundled, remote),
            answered,
            app,
        }
    }

    /// A look that failed before it asked the feed, for `why`.
    fn failed(why: String) -> Found {
        Found {
            release: Err(why),
            answered: false,
            app: None,
        }
    }
}

/// The app release `version` the feed names at `release_url`, against
/// `own`, the version of this service (and so of the app it ships with).
#[must_use]
pub fn app_update(version: &Version, release_url: &str, own: &str) -> AppUpdate {
    AppUpdate {
        version: version.to_string(),
        available: Version::parse(own).is_ok_and(|own| *version > own),
        release_url: release_url.to_string(),
    }
}

/// Why an offline updater refuses to install (see [`Updater::offline`]).
pub const NO_DEVICE: &str =
    "This service runs with --no-device: it never opens the deck, so it cannot install firmware.";

/// Why an install does not start while someone talks: it pauses the deck's
/// link under the press, the transcription and the speech.
pub const VOICE_BUSY: &str = "Finish talking and wait for transcription before updating.";

/// Why an install is refused for a deck that runs its firmware (the last
/// check did not find it in recovery) but has no session with the service:
/// it cannot be asked for the press yet, and nothing installs without one.
/// Once it links, the same install asks it.
pub const WAIT_FOR_LINK: &str = "Wait for the deck to link, then install again.";

/// Why an install does not start while another update operation runs.
const ALREADY_RUNNING: &str = "An update operation is already running.";

impl Updater {
    /// The updater of a daemon that may not open the deck: `deckd run
    /// --no-device`, and every unit-test core. `check` answers with the
    /// status it already has and `install` refuses, so neither probes USB
    /// nor runs curl.
    ///
    /// The hid thread's `hardware: false` never covered these: a check
    /// opens the deck from a thread of its own, beside the hid thread, and
    /// then fetches the release manifest. The app sent one on every
    /// connection then, so a throwaway service it was pointed at answered by
    /// opening the operator's own deck and downloading the manifest (B2).
    /// The service's own looks ([`Updater::check_scheduled`]) answer the
    /// same way here.
    #[must_use]
    pub fn offline() -> Self {
        Updater {
            offline: true,
            ..Updater::default()
        }
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().unwrap().clone();
        self.derived(state)
    }

    /// `state` with what is worked out from it: whether the release is newer
    /// than the deck, the bootloader warning, and the app release the feed
    /// named against this service's version. Every status the updater
    /// answers goes through here, so a check and an install answer what
    /// `firmware.status` says. Called with no lock held: it takes the feed's.
    fn derived(&self, mut state: Status) -> Status {
        state.available = state
            .device
            .as_ref()
            .zip(state.release.as_ref())
            .is_some_and(|(d, r)| {
                d.recovery
                    || d.maintenance
                    || d.version
                        .as_deref()
                        .and_then(firmware_version)
                        .zip(firmware_version(&r.firmware.version))
                        .is_some_and(|(current, new)| new > current)
            });
        state.bootloader_warning = state
            .device
            .as_ref()
            .zip(state.release.as_ref())
            .is_some_and(|(d, r)| {
                r.firmware.protocol == 2 && (d.recovery_protocol != Some(2) || d.maintenance)
            });
        state.app = self
            .feed
            .lock()
            .unwrap()
            .app
            .as_ref()
            .map(|(version, page)| app_update(version, page, env!("CARGO_PKG_VERSION")));
        state
    }

    /// An update operation is under way ([`Status::busy`]).
    pub fn busy(&self) -> bool {
        self.state.lock().unwrap().busy()
    }

    /// When the release feed last answered a look: the service's own looks
    /// come six hours after it ([`Schedule`]).
    pub fn last_answered(&self) -> Option<Instant> {
        self.feed.lock().unwrap().answered
    }

    /// When a look last could not reach the release feed, or failed before
    /// it asked: the service's next look waits an hour after it.
    pub fn last_failed(&self) -> Option<Instant> {
        self.feed.lock().unwrap().failed
    }

    pub fn check(&self, cache: PathBuf, force: bool) -> Result<Status, String> {
        // Offline: no deck to read and no release to fetch, so the status
        // it already has is the whole answer.
        if self.offline {
            return Ok(self.status());
        }
        let throttled = !force
            && self
                .checked
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < CHECK_INTERVAL);
        self.look(cache, throttled, false)
    }

    /// The service's own look for a release (R8, [`Schedule`],
    /// `Core::check_for_updates`): the look **Check** starts, but quiet.
    /// Nobody asked for it, so one that cannot reach the deck or the feed
    /// leaves the status as it was instead of saying the update did not
    /// complete ([`Updater::conclude`]), and the schedule tries again an
    /// hour later. The schedule decides when, so an unforced check's
    /// six-hour throttle does not apply. Offline, or while an update
    /// operation is under way, the status as it is, and nothing starts.
    pub fn check_scheduled(&self, cache: PathBuf) -> Result<Status, String> {
        if self.offline {
            return Ok(self.status());
        }
        self.look(cache, false, true)
    }

    /// Start a look for a release on a thread of its own, answering the
    /// status it starts from (`checking`). While an update operation is
    /// under way, or when `throttled`, nothing starts and the answer is the
    /// status as it is. `quiet`: see [`Updater::check_scheduled`].
    fn look(&self, cache: PathBuf, throttled: bool, quiet: bool) -> Result<Status, String> {
        let mut state = self.state.lock().unwrap();
        if throttled {
            let now = state.clone();
            drop(state);
            return Ok(self.derived(now));
        }
        if state.busy() {
            // The owner asked while a look is under way: it answers them.
            if !quiet && state.phase == "checking" {
                self.asked.store(true, Ordering::Release);
            }
            let now = state.clone();
            drop(state);
            return Ok(self.derived(now));
        }
        self.asked.store(false, Ordering::Release);
        let before = state.clone();
        state.phase = "checking".into();
        state.message = "Checking for firmware updates…".into();
        state.error = None;
        let initial = state.clone();
        drop(state);
        let this = self.clone();
        let from = before.clone();
        let spawned = std::thread::Builder::new()
            .name("firmware-check".into())
            .spawn(move || {
                let found = this.find_release(&cache);
                this.conclude(&from, found, quiet, Instant::now());
            });
        if let Err(e) = spawned {
            let why = e.to_string();
            self.conclude(&before, Found::failed(why.clone()), quiet, Instant::now());
            return Err(why);
        }
        Ok(self.derived(initial))
    }

    /// One look, on the check's own thread: the deck on the cable, then the
    /// release the app carries and the one the feed names, the newer of the
    /// two. What the feed says of the app counts whichever firmware wins.
    fn find_release(&self, cache: &Path) -> Found {
        let device = match crate::hid::probe_mut(transport::inspect) {
            Ok(Ok(device)) => device,
            Ok(Err(why)) => return Found::failed(why),
            Err(e) => return Found::failed(e.to_string()),
        };
        self.state.lock().unwrap().device = device;
        let bundled = match bundled_firmware_dir()
            .map(|dir| read_bundled_manifest(&dir))
            .transpose()
        {
            Ok(bundled) => bundled.flatten(),
            Err(why) => return Found::failed(why),
        };
        let path = cache.join("release-manifest.json");
        // A packaged release can update a device offline, including
        // before publication. Bound the optional network check so
        // an unreachable release feed does not block that path.
        let remote = fetch_with_timeout(
            MANIFEST_URL,
            &path,
            128 * 1024,
            if bundled.is_some() { 10 } else { 120 },
        )
        .and_then(|()| read_manifest(&path));
        Found::of(bundled, remote)
    }

    /// The end of a look, at `now`. A release found is `ready`, as before.
    /// None: an owner's check says why (`error`, "Update did not
    /// complete…"), and so does a quiet look the owner asked for while it
    /// ran; any other quiet look leaves the phase, the message and the error
    /// as they were `before` it, and logs why. The feed's answer and the app
    /// release it named, or its silence, are kept for the schedule either
    /// way: a release the app carries can be found with the feed
    /// unreachable, and that still counts as a failure to reach it.
    fn conclude(&self, before: &Status, found: Found, quiet: bool, now: Instant) {
        {
            let mut feed = self.feed.lock().unwrap();
            if found.answered {
                feed.answered = Some(now);
                feed.app = found.app;
            }
            if !found.answered || found.release.is_err() {
                feed.failed = Some(now);
            }
        }
        // Under the state's lock, so the owner's check either asked before
        // this look ended or finds it ended and starts its own.
        let mut state = self.state.lock().unwrap();
        let quiet = quiet && !self.asked.swap(false, Ordering::AcqRel);
        match found.release {
            Ok(release) => {
                state.release = Some(release);
                state.phase = "ready".into();
                state.message =
                    "Firmware release checked. Installation requires your confirmation.".into();
                *self.checked.lock().unwrap() = Some(now);
            }
            Err(why) if quiet => {
                tracing::info!(%why, "updates: no release found; the status stays as it was");
                state.phase.clone_from(&before.phase);
                state.progress = before.progress;
                state.message.clone_from(&before.message);
                state.error.clone_from(&before.error);
            }
            Err(why) => failed(&mut state, why),
        }
    }

    pub fn install(
        &self,
        board: &str,
        serial: &str,
        version: &str,
        allow_bootloader_update: bool,
        cache: PathBuf,
        maintenance: Maintenance,
    ) -> Result<Status, String> {
        // Before anything else: whatever the state holds, an offline
        // updater neither pauses the link nor opens the deck.
        if self.offline {
            return Err(NO_DEVICE.into());
        }
        let state = self.state.lock().unwrap();
        if state.busy() {
            return Err(ALREADY_RUNNING.into());
        }
        let install = Install {
            board: board.into(),
            serial: serial.into(),
            version: version.into(),
            allow_bootloader_update,
        };
        self.start(state, &install, cache, maintenance)
    }

    /// Every refusal [`Updater::install`] would answer `install` with, and
    /// nothing else: no phase changes, nothing starts. What the core asks
    /// before it puts the install's card on the deck, so a card only ever
    /// asks for an install that would start.
    pub fn vet(&self, install: &Install) -> Result<(), String> {
        if self.offline {
            return Err(NO_DEVICE.into());
        }
        let state = self.state.lock().unwrap();
        if state.busy() {
            return Err(ALREADY_RUNNING.into());
        }
        plan(&state, install).map(drop)
    }

    /// Hold `install`, which [`Updater::vet`] passed, for a press on the
    /// deck. The phase is `confirming`, as busy as an install: no check
    /// starts and no other install is taken until the deck confirms it
    /// ([`Updater::confirmed`]) or does not ([`Updater::not_confirmed`]).
    pub fn await_confirmation(&self, install: Install) -> Status {
        *self.awaiting.lock().unwrap() = Some(install);
        {
            let mut state = self.state.lock().unwrap();
            state.phase = "confirming".into();
            state.progress = 0;
            state.error = None;
            state.message = "Press ENTER on the deck to start the install.".into();
        }
        self.status()
    }

    /// The deck confirmed the install that waits: it starts now, checked
    /// again as [`Updater::install`] checks it. Nobody is waiting for this
    /// answer, so a refusal stays in the status, as a failed install's
    /// reason does.
    pub fn confirmed(&self, cache: PathBuf, maintenance: Maintenance) -> Result<Status, String> {
        let Some(install) = self.awaiting.lock().unwrap().take() else {
            return Err("No install is waiting for the deck.".into());
        };
        let state = self.state.lock().unwrap();
        let started = self.start(state, &install, cache, maintenance);
        if let Err(why) = &started {
            self.fail(why.clone());
        }
        started
    }

    /// The deck confirmed the install that waits, and it starts once the
    /// voice is quiet ([`Updater::confirmed`]): the phase stays
    /// `confirming`, as busy as before and with nothing downloaded yet, and
    /// the message says the press was taken and what the install waits for.
    /// Nothing changes when no install waits.
    pub fn waits_for_quiet(&self) {
        if self.awaiting.lock().unwrap().is_none() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.phase == "confirming" {
            state.message = "Starting the install when the deck stops talking…".into();
        }
    }

    /// The deck did not confirm the install that waits: its card was
    /// dismissed, answered some other way, or its minute ran out. Nothing
    /// was installed; the status is idle again and says so.
    pub fn not_confirmed(&self) {
        if self.awaiting.lock().unwrap().take().is_none() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.phase = "idle".into();
        state.progress = 0;
        state.error = None;
        state.message = "Not confirmed on the deck. Nothing was installed.".into();
    }

    /// The install the deck confirmed cannot start, for `why`: it stays in
    /// the status, as a failed install's reason does.
    pub fn abandon(&self, why: &str) {
        if self.awaiting.lock().unwrap().take().is_some() {
            self.fail(why.into());
        }
    }

    /// Start `install` from `state`: every check of [`plan`], then the
    /// download and the flash on a thread of their own. The caller made
    /// sure nothing else is under way.
    fn start(
        &self,
        mut state: MutexGuard<'_, Status>,
        install: &Install,
        cache: PathBuf,
        maintenance: Maintenance,
    ) -> Result<Status, String> {
        // Whoever asks: an offline updater never pauses the link or opens
        // the deck.
        if self.offline {
            return Err(NO_DEVICE.into());
        }
        let Plan {
            device,
            release,
            asset,
            migration,
        } = plan(&state, install)?;
        state.phase = "downloading".into();
        state.progress = 0;
        state.error = None;
        state.message = "Downloading and verifying firmware…".into();
        let initial = state.clone();
        drop(state);
        #[cfg(test)]
        if let Some(started) = &self.started {
            started.lock().unwrap().push(install.clone());
            return Ok(self.derived(initial));
        }
        let this = self.clone();
        let board = install.board.clone();
        let version = install.version.clone();
        let allow_bootloader_update = install.allow_bootloader_update;
        std::thread::Builder::new()
            .name("firmware-install".into())
            .spawn(move || {
                let selected_serial = device.serial.clone();
                let selected_board = board.clone();
                let result = (|| {
                    let _pause = Pause::acquire(maintenance)?;
                    let _awake = KeepAwake::acquire()?;
                    // Verify every image before entering recovery or erasing flash.
                    let bytes = load_asset(&cache, &asset)?;
                    validate_image(&bytes, &asset, &board)?;
                    if has_marker(&bytes, "Bootstrap 1", &board) {
                        return Err(
                            "A temporary updater cannot be installed as normal firmware.".into(),
                        );
                    }
                    let migration = migration
                        .map(|assets| {
                            let bootstrap = load_asset(&cache, &assets.bootstrap)?;
                            let bootloader = load_asset(&cache, &assets.bootloader)?;
                            validate_migration(&bootstrap, &bootloader, &assets, &board)?;
                            Ok::<_, String>(migration::MigrationImages {
                                bootstrap,
                                bootloader,
                            })
                        })
                        .transpose()?;
                    let images = migration::Images {
                        application: bytes,
                        migration,
                    };
                    let progress = this.clone();
                    let updated = crate::hid::probe_mut(move |api| {
                        transport::install(
                            api,
                            migration::Request {
                                expected: &device,
                                board: &board,
                                version: &version,
                                protocol: release.firmware.protocol,
                                allow_bootloader_update,
                            },
                            &images,
                            &cache.join(BOOTLOADER_BACKUPS),
                            |phase, percent, message| progress.progress(phase, percent, message),
                        )
                    })
                    .map_err(|e| e.to_string())??;
                    let mut state = this.state.lock().unwrap();
                    state.device = Some(updated);
                    state.phase = "complete".into();
                    state.progress = 100;
                    state.message = "Firmware updated. Your deck is ready.".into();
                    Ok(())
                })();
                if let Err(e) = result {
                    // Refresh without rebooting: a retry must know whether the
                    // device is the temporary app or interrupted recovery.
                    if let Ok(Ok(observed)) = crate::hid::probe_mut(transport::inspect) {
                        let observed = observed.map(|mut d| {
                            if d.serial == selected_serial && d.recovery && d.board.is_none() {
                                d.board = Some(selected_board.clone());
                            }
                            d
                        });
                        this.state.lock().unwrap().device = observed;
                    }
                    this.fail(e);
                }
            })
            .map_err(|e| {
                self.fail(e.to_string());
                e.to_string()
            })?;
        Ok(self.derived(initial))
    }

    fn progress(&self, phase: &str, progress: u8, message: &str) {
        let mut state = self.state.lock().unwrap();
        // The log keeps every step the status overwrites: a failed install
        // is diagnosed from where it got to, not only its last message.
        if state.phase != phase {
            tracing::info!(phase, progress, message, "firmware: install step");
        }
        state.phase = phase.into();
        state.progress = progress;
        state.message = message.into();
    }
    /// The status keeps only the latest failure, and a retry replaces it;
    /// the log keeps each one.
    fn fail(&self, error: String) {
        tracing::warn!(%error, "firmware: update did not complete");
        failed(&mut self.state.lock().unwrap(), error);
    }
}

/// `state` after an update operation that did not complete, for `error`.
fn failed(state: &mut Status, error: String) {
    state.phase = "error".into();
    state.error = Some(error);
    state.message = "Update did not complete. You can check again and retry.".into();
}

/// What an install writes, once [`plan`] found it sound.
struct Plan {
    device: Device,
    release: Manifest,
    asset: Asset,
    migration: Option<Migration>,
}

/// Every check an install makes before it starts, against the deck and the
/// release in `state`: the same deck and release the last check found, the
/// board, no downgrade, and the bootloader's consent where it may be
/// replaced.
fn plan(state: &Status, install: &Install) -> Result<Plan, String> {
    let Install {
        board,
        serial,
        version,
        allow_bootloader_update,
    } = install;
    let device = state
        .device
        .clone()
        .ok_or("Check for updates with your deck connected by USB first.")?;
    let release = state
        .release
        .clone()
        .ok_or("Check for a firmware release first.")?;
    release.validate()?;
    if device.serial != *serial || serial.is_empty() || release.firmware.version != *version {
        return Err("The device or release changed. Check for updates again.".into());
    }
    if let Some(actual) = &device.board {
        if actual != board {
            return Err("This firmware is for a different board.".into());
        }
    }
    if !device.recovery && !device.maintenance {
        if let Some(current) = device.version.as_deref().and_then(firmware_version) {
            let new = Version::parse(version).map_err(|_| "Invalid version.")?;
            if new <= current {
                return Err("The deck already has this version or a newer one.".into());
            }
        }
    }
    let asset = release
        .firmware
        .variants
        .get(board)
        .cloned()
        .ok_or("No firmware for the selected board.")?;
    if release.firmware.protocol == 2
        && (device.recovery_protocol != Some(2) || device.maintenance)
        && !allow_bootloader_update
    {
        return Err(
            "Read and accept the bootloader power-loss warning before updating this device.".into(),
        );
    }
    let migration = release.firmware.migrations.get(board).cloned();
    Ok(Plan {
        device,
        release,
        asset,
        migration,
    })
}

/// Versions reported by the running firmware append the product name.
pub fn firmware_version(s: &str) -> Option<Version> {
    Version::parse(s.strip_suffix("-openwalkietalkie").unwrap_or(s)).ok()
}

pub fn validate_image(bytes: &[u8], asset: &Asset, board: &str) -> Result<(), String> {
    if bytes.len() != asset.size || !(8..=APP_SIZE).contains(&bytes.len()) {
        return Err("Firmware length does not match the release.".into());
    }
    if format!("{:x}", Sha256::digest(bytes)) != asset.sha256.to_ascii_lowercase() {
        return Err("Firmware checksum does not match the release.".into());
    }
    validate_layout(bytes)?;
    let sp = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let pc = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if !(0x20000000..=0x20080000).contains(&sp)
        || pc & 1 != 1
        || !(APP_BASE..APP_BASE + bytes.len() as u32).contains(&(pc & !1))
        || (BONDS_START..BONDS_END).contains(&(pc & !1))
    {
        return Err("Invalid application vector table.".into());
    }
    let marker = match board {
        "hw-a" => b"OpenWalkieTalkie Rev A (".as_slice(),
        "hw-b" => b"OpenWalkieTalkie Rev B (".as_slice(),
        _ => return Err("Unknown board.".into()),
    };
    if !bytes.windows(marker.len()).any(|w| w == marker) {
        return Err("Firmware board identity does not match.".into());
    }
    Ok(())
}

fn bundled_firmware_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent().map(|c| c.join("Resources/firmware")))
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > 128 * 1024 {
        return Err("Oversized release manifest.".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Invalid release manifest: {e}"))?;
    manifest.validate()?;
    Ok(manifest)
}

fn read_bundled_manifest(directory: &Path) -> Result<Option<Manifest>, String> {
    let path = directory.join("firmware-manifest.json");
    if !path.exists() {
        return Ok(None); // Older apps did not include a firmware manifest.
    }
    let manifest = read_manifest(&path)?;
    for asset in manifest.firmware.variants.values().chain(
        manifest
            .firmware
            .migrations
            .values()
            .flat_map(|m| [&m.bootstrap, &m.bootloader]),
    ) {
        let bytes = std::fs::read(directory.join(&asset.name))
            .map_err(|e| format!("Missing bundled firmware {}: {e}", asset.name))?;
        validate_checksum(&bytes, asset)?;
    }
    Ok(Some(manifest))
}

fn select_release(
    bundled: Option<Manifest>,
    remote: Result<Manifest, String>,
) -> Result<Manifest, String> {
    match (bundled, remote) {
        (Some(local), Ok(remote)) => {
            // Both manifests were validated before selection. Equal versions
            // prefer the signed app's own bundle and its verified assets.
            if firmware_version(&remote.firmware.version)
                > firmware_version(&local.firmware.version)
            {
                Ok(remote)
            } else {
                Ok(local)
            }
        }
        (Some(local), Err(_)) => Ok(local),
        (None, remote) => remote,
    }
}

fn load_asset(cache: &Path, asset: &Asset) -> Result<Vec<u8>, String> {
    let bundled = bundled_firmware_dir().map(|dir| dir.join(&asset.name));
    let bytes = if let Some(path) = bundled.filter(|p| p.is_file()) {
        std::fs::read(path).map_err(|e| e.to_string())?
    } else {
        let path = cache.join(&asset.name);
        fetch(&asset.url, &path, asset.size)?;
        std::fs::read(path).map_err(|e| e.to_string())?
    };
    validate_checksum(&bytes, asset)?;
    Ok(bytes)
}

fn validate_checksum(bytes: &[u8], asset: &Asset) -> Result<(), String> {
    if bytes.len() != asset.size
        || format!("{:x}", Sha256::digest(bytes)) != asset.sha256.to_ascii_lowercase()
    {
        return Err("Update image length or checksum does not match the release.".into());
    }
    Ok(())
}

fn has_marker(bytes: &[u8], kind: &str, board: &str) -> bool {
    let marker = format!(
        "OpenWalkieTalkie {kind} Rev {}",
        if board == "hw-b" { "B" } else { "A" }
    );
    bytes.windows(marker.len()).any(|w| w == marker.as_bytes())
}

fn validate_migration(
    bootstrap: &[u8],
    bootloader: &[u8],
    assets: &Migration,
    board: &str,
) -> Result<(), String> {
    validate_image(bootstrap, &assets.bootstrap, board)?;
    let other = if board == "hw-b" { "hw-a" } else { "hw-b" };
    if bootstrap.len() > (BONDS_START - APP_BASE) as usize
        || !has_marker(bootstrap, "Bootstrap 1", board)
        || has_marker(bootstrap, "Bootstrap 1", other)
    {
        return Err("Invalid temporary updater image for this board.".into());
    }
    validate_checksum(bootloader, &assets.bootloader)?;
    if !(8..=65536).contains(&bootloader.len()) {
        return Err("Invalid bootloader size.".into());
    }
    let sp = u32::from_le_bytes(bootloader[..4].try_into().unwrap());
    let pc = u32::from_le_bytes(bootloader[4..8].try_into().unwrap());
    if !(0x20000000..=0x20080000).contains(&sp)
        || pc & 1 != 1
        || !(0x20020000..0x20020000 + bootloader.len() as u32).contains(&(pc & !1))
        || !has_marker(bootloader, "Recovery 2", board)
        || has_marker(bootloader, "Recovery 2", other)
    {
        return Err("Invalid recovery bootloader for this board.".into());
    }
    Ok(())
}

fn validate_layout(bytes: &[u8]) -> Result<(), String> {
    if !(8..=APP_SIZE).contains(&bytes.len()) {
        return Err("Invalid application image size.".into());
    }
    let start = ((BONDS_START - APP_BASE) as usize).min(bytes.len());
    let end = ((BONDS_END - APP_BASE) as usize).min(bytes.len());
    if bytes[start..end].iter().any(|b| *b != 255) {
        return Err("Firmware overlaps protected Bluetooth storage.".into());
    }
    Ok(())
}

/// The system's own curl, by absolute path (never a `PATH` lookup): the
/// one in `/usr/bin` on macOS and Linux, and the one Windows 10 and later
/// ship in `%SystemRoot%\System32`.
fn curl() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join(r"System32\curl.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/bin/curl")
    }
}

fn fetch(url: &str, path: &Path, max: usize) -> Result<(), String> {
    fetch_with_timeout(url, path, max, 120)
}

fn fetch_with_timeout(url: &str, path: &Path, max: usize, seconds: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid cache path.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let part = path.with_extension("part");
    let result = std::process::Command::new(curl())
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            &seconds.to_string(),
            "--max-filesize",
            &max.to_string(),
            "--output",
        ])
        .arg(&part)
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err("Could not download the release. Check your connection; the first release may not be published yet.".into());
    }
    if std::fs::metadata(&part).map_err(|e| e.to_string())?.len() > max as u64 {
        let _ = std::fs::remove_file(&part);
        return Err("Release download exceeded its size limit.".into());
    }
    std::fs::rename(part, path).map_err(|e| e.to_string())
}

struct Pause(Maintenance);

/// Prevent idle sleep during USB flashing; manual shutdown still needs the warning.
struct KeepAwake(Option<std::process::Child>);
impl KeepAwake {
    fn acquire() -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let child = std::process::Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .spawn()
                .map_err(|e| format!("Cannot keep the computer awake for this update: {e}"))?;
            Ok(Self(Some(child)))
        }
        // Windows: the calling thread's execution state, which the install
        // holds from acquire to drop on the one blocking thread it runs on
        // (and which the OS drops by itself if that thread ends first).
        #[cfg(windows)]
        {
            // SAFETY: flags only; no pointers.
            if unsafe { win::SetThreadExecutionState(win::ES_CONTINUOUS | win::ES_SYSTEM_REQUIRED) }
                == 0
            {
                return Err("Cannot keep the computer awake for this update.".into());
            }
            Ok(Self(None))
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            Ok(Self(None))
        }
    }
}
impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        // SAFETY: flags only: back to the thread's ordinary state.
        unsafe {
            win::SetThreadExecutionState(win::ES_CONTINUOUS);
        }
    }
}

/// The one power call `KeepAwake` needs, declared here rather than through
/// a windows-sys feature: every crate that depends on windows-sys would be
/// rebuilt for a feature added to it.
#[cfg(windows)]
mod win {
    pub const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    pub const ES_CONTINUOUS: u32 = 0x8000_0000;
    #[link(name = "kernel32")]
    extern "system" {
        pub fn SetThreadExecutionState(flags: u32) -> u32;
    }
}
impl Pause {
    fn acquire(m: Maintenance) -> Result<Self, String> {
        m.ready.store(false, Ordering::Release);
        if m.requested.swap(true, Ordering::AcqRel) {
            return Err("USB maintenance is already active.".into());
        }
        let guard = Self(m);
        let until = Instant::now() + Duration::from_secs(10);
        while !guard.0.ready.load(Ordering::Acquire) {
            if Instant::now() >= until {
                return Err(
                    "The deck connection could not be paused. No firmware was written.".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(guard)
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.requested.store(false, Ordering::Release);
    }
}

/// An updater that has checked, for the tests of what starts an install
/// (`Core::install_after_press`): no deck, no network.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;

    /// The deck on the cable: Rev A, firmware 0.1.0, its recovery already
    /// the second protocol, so the test release needs no bootloader
    /// consent.
    pub fn deck() -> Device {
        Device {
            serial: "USB-A".into(),
            board: Some("hw-a".into()),
            version: Some("0.1.0-openwalkietalkie".into()),
            recovery_protocol: Some(2),
            ..Device::default()
        }
    }

    /// The same deck in recovery, which names neither its board nor its
    /// firmware.
    pub fn deck_in_recovery() -> Device {
        Device {
            serial: "USB-A".into(),
            recovery: true,
            recovery_protocol: Some(2),
            ..Device::default()
        }
    }

    /// The install of the test release (0.2.0) on [`deck`], as the app
    /// sends it.
    pub fn install() -> Install {
        Install {
            board: "hw-a".into(),
            serial: "USB-A".into(),
            version: "0.2.0".into(),
            allow_bootloader_update: false,
        }
    }

    /// An updater whose last check found `device` on the cable and the test
    /// release, and the list every install it starts goes to instead of a
    /// thread of its own.
    pub fn checked(device: Device) -> (Updater, Arc<Mutex<Vec<Install>>>) {
        let started = Arc::default();
        let updater = Updater {
            started: Some(Arc::clone(&started)),
            ..Updater::default()
        };
        {
            let mut state = updater.state.lock().unwrap();
            state.phase = "ready".into();
            state.device = Some(device);
            state.release = Some(super::tests::fixture().0);
        }
        (updater, started)
    }

    /// The release feed named app `version` at its last answer.
    pub fn found_app(updater: &Updater, version: &str) {
        updater.feed.lock().unwrap().app = Some((
            Version::parse(version).unwrap(),
            format!("https://github.com/{REPOSITORY}/releases/tag/v{version}"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A check and an install answer the status as `firmware.status` says
    /// it: whether the release is newer than the deck, the bootloader
    /// warning and the app release the feed named are worked out on every
    /// answer. A check within six hours of the last answers the status as
    /// it is, and so does a check while an update operation is under way;
    /// an install answers the status it starts from. None of them spawns
    /// anything here: the test updater keeps an install instead of starting
    /// it, and a throttled or busy check starts nothing.
    #[test]
    fn a_check_and_an_install_answer_what_firmware_status_says() {
        let (updater, started) = testkit::checked(testkit::deck());
        testkit::found_app(&updater, "99.0.0");
        *updater.checked.lock().unwrap() = Some(Instant::now());
        let cache = std::env::temp_dir().join("deckd-firmware-answers-never-written");
        let said = |status: &Status| {
            (
                status.available,
                status
                    .app
                    .as_ref()
                    .map(|app| (app.version.clone(), app.available)),
            )
        };
        let want = (true, Some(("99.0.0".to_string(), true)));
        assert_eq!(said(&updater.status()), want, "firmware.status");
        let throttled = updater.check(cache.clone(), false).unwrap();
        assert_eq!(throttled.phase, "ready");
        assert_eq!(said(&throttled), want, "a throttled check");
        let installing = updater
            .install(
                "hw-a",
                "USB-A",
                "0.2.0",
                false,
                cache.clone(),
                Maintenance::default(),
            )
            .unwrap();
        assert_eq!(installing.phase, "downloading");
        assert_eq!(started.lock().unwrap().len(), 1);
        assert_eq!(said(&installing), want, "an install that starts");
        let busy = updater.check(cache.clone(), true).unwrap();
        assert_eq!(busy.phase, "downloading");
        assert_eq!(said(&busy), want, "a check while it installs");
        assert!(!cache.exists());
    }

    #[test]
    #[ignore = "requires both boards' built release artifacts in OWT_MIGRATION_ARTIFACTS"]
    fn built_migration_artifacts_pass_host_validation() {
        let directory = PathBuf::from(std::env::var("OWT_MIGRATION_ARTIFACTS").unwrap());
        let manifest: Manifest = serde_json::from_slice(
            &std::fs::read(directory.join("firmware-manifest.json")).unwrap(),
        )
        .unwrap();
        manifest.validate().unwrap();
        assert!(read_bundled_manifest(&directory).unwrap().is_some());
        for board in ["hw-a", "hw-b"] {
            let asset = &manifest.firmware.variants[board];
            let application = std::fs::read(directory.join(&asset.name)).unwrap();
            validate_image(&application, asset, board).unwrap();
            assert!(!has_marker(&application, "Bootstrap 1", board));
            let migration = &manifest.firmware.migrations[board];
            let bootstrap = std::fs::read(directory.join(&migration.bootstrap.name)).unwrap();
            let bootloader = std::fs::read(directory.join(&migration.bootloader.name)).unwrap();
            validate_migration(&bootstrap, &bootloader, migration, board).unwrap();
        }
    }

    pub(super) fn fixture() -> (Manifest, Vec<u8>) {
        let mut image = vec![0; 128];
        image[..4].copy_from_slice(&0x20040000u32.to_le_bytes());
        image[4..8].copy_from_slice(&(APP_BASE + 9).to_le_bytes());
        let marker = b"OpenWalkieTalkie Rev A (test)";
        image[16..16 + marker.len()].copy_from_slice(marker);
        let asset = Asset { name: "OpenWalkieTalkie-0.2.0-hw-a.bin".into(), url: format!("https://github.com/{REPOSITORY}/releases/download/v0.3.0/OpenWalkieTalkie-0.2.0-hw-a.bin"), sha256: format!("{:x}", Sha256::digest(&image)), size: image.len() };
        let (migration, _, _) = migration_fixture("hw-a");
        let release = Manifest {
            schema: 1,
            product: "openwalkietalkie".into(),
            release_url: format!("https://github.com/{REPOSITORY}/releases/tag/v0.3.0"),
            firmware: Firmware {
                version: "0.2.0".into(),
                protocol: 2,
                app_base: APP_BASE,
                app_size: APP_SIZE,
                variants: BTreeMap::from([("hw-a".into(), asset)]),
                migrations: BTreeMap::from([("hw-a".into(), migration)]),
            },
            app: None,
        };
        (release, image)
    }

    /// The test release as the feed would serve it, naming the app
    /// `version` beside its Mac downloads, as `tools/release.py` writes it.
    fn feed_release(version: &str) -> Manifest {
        let mut value = serde_json::to_value(fixture().0).unwrap();
        value["app"] = serde_json::json!({
            "version": version,
            "macos": {"aarch64": {"name": format!("OpenWalkieTalkie-{version}-macos-aarch64.dmg")}},
        });
        serde_json::from_value(value).unwrap()
    }

    /// The release feed names the app beside the firmware. A stable version
    /// is compared with this build's own; a section the service cannot
    /// read is no version at all, and the firmware's check goes on.
    #[test]
    fn the_manifests_app_version_is_compared_with_this_build() {
        let own = env!("CARGO_PKG_VERSION");
        let newer = feed_release("99.0.0");
        newer.validate().unwrap();
        let version = newer.app_version().unwrap();
        assert_eq!(version, Version::new(99, 0, 0));
        let update = app_update(&version, &newer.release_url, own);
        assert_eq!(
            update,
            AppUpdate {
                version: "99.0.0".into(),
                available: true,
                release_url: format!("https://github.com/{REPOSITORY}/releases/tag/v0.3.0"),
            }
        );
        let same = feed_release(own).app_version().unwrap();
        assert!(!app_update(&same, "", own).available, "this build itself");
        assert!(!app_update(&Version::new(0, 0, 1), "", own).available);
        assert!(!app_update(&version, "", "not a version").available);
        // No app section (the manifest the app bundle carries), a
        // pre-release, and sections that are not what the release writes.
        assert_eq!(fixture().0.app_version(), None);
        assert_eq!(feed_release("1.0.0-beta.1").app_version(), None);
        for app in [
            serde_json::json!("1.0.0"),
            serde_json::json!(5),
            serde_json::json!({"version": 1}),
        ] {
            let mut value = serde_json::to_value(fixture().0).unwrap();
            value["app"] = app.clone();
            let odd: Manifest = serde_json::from_value(value).unwrap();
            odd.validate().unwrap();
            assert_eq!(odd.app_version(), None, "{app}");
        }
        // A manifest without one says nothing of an app when it is passed on.
        assert!(serde_json::to_value(fixture().0)
            .unwrap()
            .get("app")
            .is_none());
    }

    /// The release the app bundle carries may win the firmware, but what the
    /// feed said of the app counts all the same, and a later look that
    /// cannot reach the feed keeps it.
    #[test]
    fn the_feeds_app_version_outlives_a_bundled_firmware_win() {
        let (old, _) = fixture();
        let bundled: Manifest = serde_json::from_str(
            &serde_json::to_string(&old)
                .unwrap()
                .replace("0.2.0", "0.4.0"),
        )
        .unwrap();
        let found = Found::of(Some(bundled.clone()), Ok(feed_release("99.0.0")));
        assert!(found.answered);
        let release = found.release.as_ref().unwrap();
        assert_eq!(
            release.firmware.version, "0.4.0",
            "the bundle's firmware wins"
        );
        assert_eq!(release.app, None);
        let updater = Updater::default();
        let at = Instant::now();
        updater.conclude(&Status::default(), found, true, at);
        let status = updater.status();
        assert_eq!(status.phase, "ready");
        assert_eq!(
            status.app.as_ref().map(|app| app.version.as_str()),
            Some("99.0.0")
        );
        assert!(status.app.as_ref().unwrap().available);
        assert_eq!(updater.last_answered(), Some(at));
        assert_eq!(updater.last_failed(), None);
        // The feed is unreachable now: the bundle's release is still found,
        // the app release the feed named last time stays, and the look counts
        // as one that could not reach the feed.
        let later = at + Duration::from_secs(60);
        let found = Found::of(Some(bundled), Err("offline".into()));
        assert!(!found.answered && found.app.is_none());
        updater.conclude(&status, found, true, later);
        let status = updater.status();
        assert_eq!(status.phase, "ready");
        assert_eq!(status.app.map(|app| app.version), Some("99.0.0".into()));
        assert_eq!(updater.last_answered(), Some(at));
        assert_eq!(updater.last_failed(), Some(later));
    }

    /// The service's own look that cannot reach the deck or the feed was
    /// asked for by nobody: the status stays as it was before it started,
    /// with no error and no "Update did not complete", and the schedule
    /// hears of the failure.
    #[test]
    fn a_scheduled_check_that_cannot_reach_the_feed_leaves_the_status_as_it_was() {
        let (updater, _) = testkit::checked(testkit::deck());
        updater.state.lock().unwrap().message =
            "Firmware release checked. Installation requires your confirmation.".into();
        let before = updater.state.lock().unwrap().clone();
        // As `look` leaves it while the thread works.
        updater.state.lock().unwrap().phase = "checking".into();
        let at = Instant::now();
        let why = "Could not download the release.";
        updater.conclude(&before, Found::failed(why.into()), true, at);
        let status = updater.status();
        assert_eq!(status.phase, "ready");
        assert_eq!(status.message, before.message);
        assert_eq!(status.error, None);
        assert!(status.available, "the release it had is still there");
        assert_eq!(status.release.unwrap().firmware.version, "0.2.0");
        assert_eq!(updater.last_failed(), Some(at));
        assert_eq!(updater.last_answered(), None);
        assert!(updater.checked.lock().unwrap().is_none());
    }

    /// The owner's own **Check** that fails says so, as it always has.
    #[test]
    fn an_owner_check_that_fails_still_says_so() {
        let (updater, _) = testkit::checked(testkit::deck());
        let before = updater.state.lock().unwrap().clone();
        updater.state.lock().unwrap().phase = "checking".into();
        let at = Instant::now();
        updater.conclude(&before, Found::failed("no feed".into()), false, at);
        let status = updater.status();
        assert_eq!(status.phase, "error");
        assert_eq!(status.error.as_deref(), Some("no feed"));
        assert_eq!(
            status.message,
            "Update did not complete. You can check again and retry."
        );
        assert_eq!(updater.last_failed(), Some(at));
    }

    /// The owner's **Check** that lands while the service's own look is
    /// under way is answered by that look, so its failure says so; the
    /// service's next look is quiet again. A check while an install waits
    /// for the deck is no look to hand over.
    #[test]
    fn an_owner_check_during_the_services_own_look_hears_how_it_went() {
        let (updater, _) = testkit::checked(testkit::deck());
        let before = updater.state.lock().unwrap().clone();
        let under_way = |phase: &str| updater.state.lock().unwrap().phase = phase.into();
        let now = Instant::now();
        under_way("checking");
        // Busy, so this starts nothing: it is handed to the look.
        let status = updater.check(PathBuf::from("/unused"), true).unwrap();
        assert_eq!(status.phase, "checking");
        updater.conclude(&before, Found::failed("no feed".into()), true, now);
        let status = updater.status();
        assert_eq!(
            (status.phase.as_str(), status.error.as_deref()),
            ("error", Some("no feed"))
        );
        *updater.state.lock().unwrap() = before.clone();
        under_way("checking");
        updater.conclude(&before, Found::failed("no feed".into()), true, now);
        assert_eq!(updater.status().phase, "ready", "nobody asked this time");
        under_way("confirming");
        updater.check(PathBuf::from("/unused"), true).unwrap();
        assert!(!updater.asked.load(Ordering::Acquire));
    }

    /// `--no-device` and every unit-test core: the service's own look
    /// answers the status it has, and reaches for nothing.
    #[test]
    fn an_offline_updater_never_starts_a_scheduled_check() {
        let cache =
            std::env::temp_dir().join(format!("owt-offline-schedule-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let updater = Updater::offline();
        let status = updater.check_scheduled(cache.clone()).unwrap();
        assert_eq!(status.phase, "idle");
        assert!(status.release.is_none() && status.app.is_none() && !status.busy());
        assert_eq!(
            (updater.last_answered(), updater.last_failed()),
            (None, None)
        );
        assert!(updater.checked.lock().unwrap().is_none());
        assert!(!cache.exists(), "no manifest, not even its directory");
    }

    /// The firmware a deck named in its hello is compared with the release:
    /// how a deck on Bluetooth alone hears of a newer one.
    #[test]
    fn the_hellos_firmware_is_compared_with_the_release() {
        let (updater, _) = testkit::checked(testkit::deck());
        let status = updater.status();
        let linked = |fw: Option<&str>| status.clone().with_linked(fw).linked;
        assert_eq!(
            linked(Some("0.1.0-openwalkietalkie")),
            Some(Linked {
                fw: "0.1.0-openwalkietalkie".into(),
                available: true
            })
        );
        for fw in ["0.2.0-openwalkietalkie", "0.3.0", "not a version"] {
            assert_eq!(linked(Some(fw)).map(|l| l.available), Some(false), "{fw}");
        }
        assert_eq!(linked(None), None, "no deck said");
        // No release found yet: nothing is newer.
        let found_nothing = Status::default().with_linked(Some("0.1.0"));
        assert_eq!(found_nothing.linked.map(|l| l.available), Some(false));
        // Left out of a reply when there is none.
        let value = serde_json::to_value(Status::default()).unwrap();
        assert!(value.get("linked").is_none() && value.get("app").is_none());
    }

    #[test]
    fn bundled_release_works_offline_and_newest_valid_release_wins() {
        let (old, _) = fixture();
        let newer: Manifest = serde_json::from_str(
            &serde_json::to_string(&old)
                .unwrap()
                .replace("0.2.0", "0.4.0"),
        )
        .unwrap();
        old.validate().unwrap();
        newer.validate().unwrap();
        for remote in [Err("offline".into()), Ok(old.clone())] {
            assert_eq!(
                select_release(Some(newer.clone()), remote)
                    .unwrap()
                    .firmware
                    .version,
                "0.4.0"
            );
        }
        assert_eq!(
            select_release(Some(old), Ok(newer))
                .unwrap()
                .firmware
                .version,
            "0.4.0"
        );
        assert!(select_release(None, Err("offline".into())).is_err());
    }

    #[test]
    fn bundled_manifest_requires_complete_untampered_assets() {
        let dir = std::env::temp_dir().join(format!(
            "owt-bundled-firmware-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_bundled_manifest(&dir).unwrap().is_none());
        let (manifest, image) = fixture();
        let (migration, bootstrap, bootloader) = migration_fixture("hw-a");
        let path = dir.join("firmware-manifest.json");
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let app = dir.join(&manifest.firmware.variants["hw-a"].name);
        std::fs::write(&app, &image).unwrap();
        std::fs::write(dir.join(&migration.bootstrap.name), bootstrap).unwrap();
        assert!(read_bundled_manifest(&dir).is_err());
        std::fs::write(dir.join(&migration.bootloader.name), bootloader).unwrap();
        assert!(read_bundled_manifest(&dir).unwrap().is_some());
        let mut corrupt = image;
        corrupt[20] ^= 1;
        std::fs::write(app, corrupt).unwrap();
        assert!(read_bundled_manifest(&dir).is_err());
        std::fs::write(path, b"{}").unwrap();
        assert!(read_bundled_manifest(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn migration_fixture(board: &str) -> (Migration, Vec<u8>, Vec<u8>) {
        let rev = if board == "hw-b" { "B" } else { "A" };
        let mut bootstrap = vec![0; 128];
        bootstrap[..4].copy_from_slice(&0x20040000u32.to_le_bytes());
        bootstrap[4..8].copy_from_slice(&(APP_BASE + 9).to_le_bytes());
        bootstrap.extend_from_slice(
            format!("OpenWalkieTalkie Rev {rev} (test) OpenWalkieTalkie Bootstrap 1 Rev {rev}")
                .as_bytes(),
        );
        let mut bootloader = vec![0; 128];
        bootloader[..4].copy_from_slice(&0x20042000u32.to_le_bytes());
        bootloader[4..8].copy_from_slice(&0x20020009u32.to_le_bytes());
        bootloader.extend_from_slice(format!("OpenWalkieTalkie Recovery 2 Rev {rev}").as_bytes());
        let asset = |kind: &str, bytes: &[u8]| {
            let name = format!("OpenWalkieTalkie-0.2.0-{kind}-{board}.bin");
            Asset {
                url: format!("https://github.com/{REPOSITORY}/releases/download/v0.3.0/{name}"),
                name,
                size: bytes.len(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
            }
        };
        (
            Migration {
                bootstrap: asset("bootstrap", &bootstrap),
                bootloader: asset("recovery", &bootloader),
            },
            bootstrap,
            bootloader,
        )
    }

    #[test]
    fn migration_images_are_bound_to_board_vectors_size_and_checksums() {
        for board in ["hw-a", "hw-b"] {
            let (assets, bootstrap, bootloader) = migration_fixture(board);
            validate_migration(&bootstrap, &bootloader, &assets, board).unwrap();
            let other = if board == "hw-b" { "hw-a" } else { "hw-b" };
            assert!(validate_migration(&bootstrap, &bootloader, &assets, other).is_err());
            let mut corrupt = bootloader.clone();
            corrupt[25] ^= 1;
            assert!(validate_migration(&bootstrap, &corrupt, &assets, board).is_err());
            corrupt[..8].fill(0);
            let mut assets = assets.clone();
            assets.bootloader.sha256 = format!("{:x}", Sha256::digest(&corrupt));
            assert!(validate_migration(&bootstrap, &corrupt, &assets, board).is_err());
            let mut too_large = bootstrap.clone();
            too_large.resize((BONDS_START - APP_BASE) as usize + 1, 255);
            assets.bootstrap.size = too_large.len();
            assets.bootstrap.sha256 = format!("{:x}", Sha256::digest(&too_large));
            assert!(validate_migration(&too_large, &bootloader, &assets, board).is_err());
        }
    }

    #[test]
    fn migration_manifest_rejects_missing_and_crossed_assets() {
        let (mut release, _) = fixture();
        release.firmware.migrations.clear();
        assert!(release.validate().is_err());
        let (mut release, _) = fixture();
        release
            .firmware
            .migrations
            .get_mut("hw-a")
            .unwrap()
            .bootloader
            .name = "OpenWalkieTalkie-0.2.0-recovery-hw-b.bin".into();
        assert!(release.validate().is_err());
    }

    #[test]
    fn bootloader_consent_is_required_by_daemon_and_resume_is_available() {
        let (release, _) = fixture();
        let updater = Updater::default();
        {
            let mut state = updater.state.lock().unwrap();
            state.release = Some(release);
            state.device = Some(Device {
                serial: "USB-A".into(),
                board: Some("hw-a".into()),
                version: Some("0.2.0-openwalkietalkie".into()),
                maintenance: true,
                ..Device::default()
            });
        }
        assert!(updater.status().available);
        assert!(updater.status().bootloader_warning);
        let error = updater
            .install(
                "hw-a",
                "USB-A",
                "0.2.0",
                false,
                PathBuf::from("/unused"),
                Maintenance::default(),
            )
            .unwrap_err();
        assert!(error.contains("power-loss warning"));
        assert_eq!(updater.status().phase, "idle");
    }

    #[test]
    fn manifest_rejects_other_products_layouts_and_untrusted_locations() {
        let (release, _) = fixture();
        release.validate().unwrap();
        let mut bad = release.clone();
        bad.product = "openmicrokbd".into();
        assert!(bad.validate().is_err());
        let mut bad = release.clone();
        bad.firmware.app_base -= 65536;
        assert!(bad.validate().is_err());
        let mut bad = release.clone();
        bad.firmware.protocol = 3;
        assert!(bad.validate().is_err());
        let mut bad = release.clone();
        bad.firmware.variants.get_mut("hw-a").unwrap().url =
            "https://evil.example/image.bin".into();
        assert!(bad.validate().is_err());
        let mut bad = release.clone();
        bad.firmware.variants.get_mut("hw-a").unwrap().name = "../firmware.bin".into();
        assert!(bad.validate().is_err());
        let mut bad = release;
        bad.firmware.variants.get_mut("hw-a").unwrap().size = APP_SIZE + 1;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn legacy_manifests_remain_supported_and_storage_is_protected() {
        let (mut release, image) = fixture();
        release.firmware.migrations.clear();
        release.firmware.protocol = 1;
        release.firmware.app_size = LEGACY_APP_SIZE;
        release.validate().unwrap();
        release.firmware.app_size = APP_SIZE;
        assert!(release.validate().is_err());
        let mut large = vec![255; (BONDS_END - APP_BASE) as usize + 17];
        large[..image.len()].copy_from_slice(&image);
        let mut asset = release.firmware.variants["hw-a"].clone();
        asset.size = large.len();
        asset.sha256 = format!("{:x}", Sha256::digest(&large));
        validate_image(&large, &asset, "hw-a").unwrap();
        large[(BONDS_START - APP_BASE) as usize] = 0;
        asset.sha256 = format!("{:x}", Sha256::digest(&large));
        assert!(validate_image(&large, &asset, "hw-a").is_err());
    }

    #[test]
    fn corruption_wrong_board_and_invalid_vector_are_rejected_before_flash() {
        let (release, image) = fixture();
        let mut asset = release.firmware.variants["hw-a"].clone();
        validate_image(&image, &asset, "hw-a").unwrap();
        assert!(validate_image(&image, &asset, "hw-b").is_err());
        assert!(validate_image(&image[..127], &asset, "hw-a").is_err());
        let mut bad = image;
        bad[5] ^= 255;
        assert!(validate_image(&bad, &asset, "hw-a").is_err());
        asset.sha256 = format!("{:x}", Sha256::digest(&bad));
        assert!(validate_image(&bad, &asset, "hw-a").is_err());
    }

    #[test]
    fn install_refuses_stale_device_wrong_board_and_downgrades_without_spawning() {
        let (release, _) = fixture();
        let updater = Updater::default();
        let cache = PathBuf::from("/unused");
        let mut state = updater.state.lock().unwrap();
        state.release = Some(release);
        state.device = Some(Device {
            serial: "USB-A".into(),
            board: Some("hw-a".into()),
            version: Some("0.1.0-openwalkietalkie".into()),
            recovery: false,
            ..Device::default()
        });
        drop(state);
        assert!(updater.status().available);
        for (board, serial, version) in [
            ("hw-b", "USB-A", "0.2.0"),
            ("hw-a", "USB-B", "0.2.0"),
            ("hw-a", "USB-A", "0.1.0"),
        ] {
            assert!(updater
                .install(
                    board,
                    serial,
                    version,
                    false,
                    cache.clone(),
                    Maintenance::default()
                )
                .is_err());
        }
        updater
            .state
            .lock()
            .unwrap()
            .device
            .as_mut()
            .unwrap()
            .version = Some("0.3.0-openwalkietalkie".into());
        assert!(!updater.status().available);
        assert!(updater
            .install(
                "hw-a",
                "USB-A",
                "0.2.0",
                false,
                cache,
                Maintenance::default()
            )
            .is_err());
        assert_eq!(updater.status().phase, "idle");
    }

    /// B2: an offline updater (`--no-device`, every unit-test core) answers
    /// a check with the idle status it has, and refuses an install that an
    /// online one would start (same deck, newer version, consent given)
    /// without asking the link to pause: nothing is probed, fetched or
    /// written.
    #[test]
    fn an_offline_updater_answers_its_status_and_refuses_to_install_without_probing() {
        let cache = std::env::temp_dir()
            .join(format!("owt-offline-firmware-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let updater = Updater::offline();
        for force in [false, true] {
            let status = updater.check(cache.clone(), force).unwrap();
            assert_eq!(status.phase, "idle");
            assert!(status.device.is_none() && status.release.is_none() && !status.available);
        }
        assert!(updater.checked.lock().unwrap().is_none(), "nothing was checked");
        assert!(!cache.exists(), "no manifest, not even its directory");
        let (release, _) = fixture();
        {
            let mut state = updater.state.lock().unwrap();
            state.release = Some(release);
            state.device = Some(Device {
                serial: "USB-A".into(),
                board: Some("hw-a".into()),
                version: Some("0.1.0-openwalkietalkie".into()),
                recovery_protocol: Some(2),
                ..Device::default()
            });
        }
        assert!(updater.status().available, "an online updater would install this");
        let maintenance = Maintenance::default();
        let error = updater
            .install("hw-a", "USB-A", "0.2.0", true, cache.clone(), maintenance.clone())
            .unwrap_err();
        assert_eq!(error, NO_DEVICE);
        assert_eq!(updater.status().phase, "idle");
        assert!(
            !maintenance.requested.load(Ordering::Acquire),
            "the link was never asked to pause"
        );
        assert!(!cache.exists());
    }

    /// `vet` answers exactly the refusals `install` answers — the board,
    /// the deck and release the check found, a downgrade, the bootloader's
    /// consent, an offline service — and starts nothing, whatever it says.
    #[test]
    fn vet_refuses_what_install_refuses_and_starts_nothing() {
        let (updater, started) = testkit::checked(testkit::deck());
        let good = testkit::install();
        let same_refusal = |install: &Install| {
            let vetted = updater.vet(install).unwrap_err();
            let installed = updater
                .install(
                    &install.board,
                    &install.serial,
                    &install.version,
                    install.allow_bootloader_update,
                    PathBuf::from("/unused"),
                    Maintenance::default(),
                )
                .unwrap_err();
            assert_eq!(vetted, installed, "{install:?}");
            vetted
        };
        let refused = [
            Install {
                board: "hw-b".into(),
                ..good.clone()
            },
            Install {
                serial: "USB-B".into(),
                ..good.clone()
            },
            Install {
                version: "0.1.0".into(),
                ..good.clone()
            },
        ];
        for install in &refused {
            same_refusal(install);
        }
        let set_device = |edit: &dyn Fn(&mut Device)| {
            edit(updater.state.lock().unwrap().device.as_mut().unwrap());
        };
        set_device(&|deck| deck.version = Some("0.3.0-openwalkietalkie".into()));
        assert!(same_refusal(&good).contains("newer one"));
        set_device(&|deck| {
            deck.version = Some("0.1.0-openwalkietalkie".into());
            deck.recovery_protocol = Some(1);
        });
        let consent = same_refusal(&good);
        assert!(consent.contains("power-loss warning"), "{consent}");
        let given = Install {
            allow_bootloader_update: true,
            ..good.clone()
        };
        updater.vet(&given).unwrap();
        assert_eq!(updater.status().phase, "ready");
        assert!(started.lock().unwrap().is_empty(), "vet starts nothing");
        assert_eq!(Updater::offline().vet(&good).unwrap_err(), NO_DEVICE);
    }

    /// An install that waits for the deck is `confirming`: busy, so no
    /// other install or check starts; the press starts the one it waited
    /// for, once, and only then.
    #[test]
    fn an_install_waiting_for_the_deck_starts_when_it_is_confirmed() {
        let (updater, started) = testkit::checked(testkit::deck());
        let cache = PathBuf::from("/unused");
        updater.vet(&testkit::install()).unwrap();
        let status = updater.await_confirmation(testkit::install());
        assert_eq!(status.phase, "confirming");
        assert_eq!(
            status.message,
            "Press ENTER on the deck to start the install."
        );
        assert!(status.busy() && status.available && status.error.is_none());
        assert!(
            started.lock().unwrap().is_empty(),
            "nothing before the press"
        );
        assert_eq!(
            updater.vet(&testkit::install()).unwrap_err(),
            ALREADY_RUNNING
        );
        let again = updater.install(
            "hw-a",
            "USB-A",
            "0.2.0",
            false,
            cache.clone(),
            Maintenance::default(),
        );
        assert_eq!(again.unwrap_err(), ALREADY_RUNNING);
        let check = updater.check(cache.clone(), true).unwrap();
        assert_eq!(check.phase, "confirming", "a check waits too");
        assert!(updater.checked.lock().unwrap().is_none());

        let maintenance = Maintenance::default();
        let status = updater
            .confirmed(cache.clone(), maintenance.clone())
            .unwrap();
        assert_eq!(status.phase, "downloading");
        assert_eq!(*started.lock().unwrap(), [testkit::install()]);
        assert!(
            !maintenance.requested.load(Ordering::Acquire),
            "a test's install never reaches the link"
        );
        let twice = updater
            .confirmed(cache, Maintenance::default())
            .unwrap_err();
        assert_eq!(twice, "No install is waiting for the deck.");
        assert_eq!(started.lock().unwrap().len(), 1, "one press, one install");
        assert_eq!(updater.status().phase, "downloading");
    }

    /// After the press, while the voice holds the install back, the status
    /// says what it waits for and stays `confirming`: busy, nothing
    /// downloaded. With no install waiting it changes nothing — the press
    /// that started one, a refusal, an idle updater.
    #[test]
    fn a_confirmed_install_held_back_by_the_voice_says_what_it_waits_for() {
        const WAITS: &str = "Starting the install when the deck stops talking…";
        let (updater, started) = testkit::checked(testkit::deck());
        updater.waits_for_quiet();
        assert_eq!(updater.status().phase, "ready", "nothing waits yet");
        assert_ne!(updater.status().message, WAITS);
        updater.await_confirmation(testkit::install());
        updater.waits_for_quiet();
        let status = updater.status();
        assert_eq!((status.phase.as_str(), status.message.as_str()), ("confirming", WAITS));
        assert!(status.busy() && status.error.is_none());
        assert!(started.lock().unwrap().is_empty(), "nothing before quiet");
        let status = updater
            .confirmed(PathBuf::from("/unused"), Maintenance::default())
            .unwrap();
        assert_eq!(status.phase, "downloading");
        updater.waits_for_quiet();
        let status = updater.status();
        assert_eq!(status.phase, "downloading");
        assert_ne!(status.message, WAITS, "a started install is not waiting");
    }

    /// An install the deck did not confirm is idle again with the reason,
    /// nothing started; the release is still there to install.
    #[test]
    fn an_install_the_deck_did_not_confirm_is_idle_with_the_reason() {
        let (updater, started) = testkit::checked(testkit::deck());
        updater.await_confirmation(testkit::install());
        updater.not_confirmed();
        let status = updater.status();
        assert_eq!(status.phase, "idle");
        assert_eq!(
            status.message,
            "Not confirmed on the deck. Nothing was installed."
        );
        assert!(status.error.is_none() && status.available && !status.busy());
        assert!(updater
            .confirmed(PathBuf::from("/unused"), Maintenance::default())
            .is_err());
        assert!(started.lock().unwrap().is_empty());
        assert_eq!(updater.status().phase, "idle", "no late press starts it");
        // Nothing waits any more: a second call changes nothing.
        updater.not_confirmed();
        updater.abandon(VOICE_BUSY);
        assert_eq!(updater.status().phase, "idle");
        updater.vet(&testkit::install()).unwrap();
    }

    /// The press checks the install again, and one that can no longer
    /// start, or that the voice held back for good, ends in `error` with
    /// its reason, nothing started.
    #[test]
    fn a_confirmed_install_that_cannot_start_keeps_its_reason() {
        let (updater, started) = testkit::checked(testkit::deck());
        updater.await_confirmation(testkit::install());
        let mut state = updater.state.lock().unwrap();
        state.device.as_mut().unwrap().serial = "USB-B".into();
        drop(state);
        let why = updater
            .confirmed(PathBuf::from("/unused"), Maintenance::default())
            .unwrap_err();
        let status = updater.status();
        assert_eq!(status.phase, "error");
        assert_eq!(status.error.as_deref(), Some(why.as_str()));
        assert!(why.contains("changed"), "{why}");

        let (updater, _) = testkit::checked(testkit::deck());
        updater.await_confirmation(testkit::install());
        updater.abandon(VOICE_BUSY);
        let status = updater.status();
        assert_eq!(
            (status.phase.as_str(), status.error.as_deref()),
            ("error", Some(VOICE_BUSY))
        );
        assert!(updater
            .confirmed(PathBuf::from("/unused"), Maintenance::default())
            .is_err());
        assert!(started.lock().unwrap().is_empty());
    }

    #[test]
    fn maintenance_is_released_on_failure() {
        let maintenance = Maintenance::default();
        maintenance.requested.store(true, Ordering::Release);
        {
            let _guard = Pause(maintenance.clone());
        }
        assert!(!maintenance.requested.load(Ordering::Acquire));
    }

    #[test]
    fn firmware_version_compares_numerically() {
        assert!(
            firmware_version("0.10.0-openwalkietalkie")
                > firmware_version("0.9.0-openwalkietalkie")
        );
        assert_eq!(firmware_version("garbage"), None);
    }
}
