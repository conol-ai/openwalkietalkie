//! Release discovery and verified, board-specific USB application updates.
//! All network/USB work runs off the core. Only an explicit install call
//! acquires HID maintenance and writes flash; polling never installs anything.
mod bulk;
mod migration;
mod transport;

use crate::hid::Maintenance;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{atomic::Ordering, Arc, Mutex};
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
}

impl Manifest {
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
        }
    }
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "checking" | "downloading" | "installing" | "reconnecting" | "bootloader"
        )
    }
}

#[derive(Clone, Default)]
pub struct Updater {
    state: Arc<Mutex<Status>>,
    checked: Arc<Mutex<Option<Instant>>>,
    /// Never probe USB or fetch: see [`Updater::offline`].
    offline: bool,
}

/// Why an offline updater refuses to install (see [`Updater::offline`]).
pub const NO_DEVICE: &str =
    "This service runs with --no-device: it never opens the deck, so it cannot install firmware.";

impl Updater {
    /// The updater of a daemon that may not open the deck: `deckd run
    /// --no-device`, and every unit-test core. `check` answers with the
    /// status it already has and `install` refuses, so neither probes USB
    /// nor runs curl.
    ///
    /// The hid thread's `hardware: false` never covered these: a check
    /// opens the deck from a thread of its own, beside the hid thread, and
    /// then fetches the release manifest. The app sends one on every
    /// connection, so a throwaway service it was pointed at answered by
    /// opening the operator's own deck and downloading the manifest (B2).
    #[must_use]
    pub fn offline() -> Self {
        Updater {
            offline: true,
            ..Updater::default()
        }
    }

    pub fn status(&self) -> Status {
        let mut state = self.state.lock().unwrap().clone();
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
        state
    }

    pub fn check(&self, cache: PathBuf, force: bool) -> Result<Status, String> {
        // Offline: no deck to read and no release to fetch, so the status
        // it already has is the whole answer.
        if self.offline {
            return Ok(self.status());
        }
        let mut state = self.state.lock().unwrap();
        if state.busy() {
            return Ok(state.clone());
        }
        if !force
            && self
                .checked
                .lock()
                .unwrap()
                .is_some_and(|t| t.elapsed() < CHECK_INTERVAL)
        {
            return Ok(state.clone());
        }
        state.phase = "checking".into();
        state.message = "Checking for firmware updates…".into();
        state.error = None;
        let initial = state.clone();
        drop(state);
        let this = self.clone();
        std::thread::Builder::new()
            .name("firmware-check".into())
            .spawn(move || {
                let result = (|| {
                    let device =
                        crate::hid::probe_mut(transport::inspect).map_err(|e| e.to_string())??;
                    this.state.lock().unwrap().device = device;
                    let bundled = bundled_firmware_dir()
                        .map(|dir| read_bundled_manifest(&dir))
                        .transpose()?
                        .flatten();
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
                    let release = select_release(bundled, remote)?;
                    let mut state = this.state.lock().unwrap();
                    state.release = Some(release);
                    state.phase = "ready".into();
                    state.message =
                        "Firmware release checked. Installation requires your confirmation.".into();
                    *this.checked.lock().unwrap() = Some(Instant::now());
                    Ok(())
                })();
                if let Err(e) = result {
                    this.fail(e);
                }
            })
            .map_err(|e| {
                self.fail(e.to_string());
                e.to_string()
            })?;
        Ok(initial)
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
        let mut state = self.state.lock().unwrap();
        if state.busy() {
            return Err("An update operation is already running.".into());
        }
        let device = state
            .device
            .clone()
            .ok_or("Check for updates with your deck connected by USB first.")?;
        let release = state
            .release
            .clone()
            .ok_or("Check for a firmware release first.")?;
        release.validate()?;
        if device.serial != serial || serial.is_empty() || release.firmware.version != version {
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
                "Read and accept the bootloader power-loss warning before updating this device."
                    .into(),
            );
        }
        let migration = release.firmware.migrations.get(board).cloned();
        state.phase = "downloading".into();
        state.progress = 0;
        state.error = None;
        state.message = "Downloading and verifying firmware…".into();
        let initial = state.clone();
        drop(state);
        let this = self.clone();
        let board = board.to_string();
        let version = version.to_string();
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
        Ok(initial)
    }

    fn progress(&self, phase: &str, progress: u8, message: &str) {
        let mut state = self.state.lock().unwrap();
        state.phase = phase.into();
        state.progress = progress;
        state.message = message.into();
    }
    fn fail(&self, error: String) {
        let mut state = self.state.lock().unwrap();
        state.phase = "error".into();
        state.error = Some(error);
        state.message = "Update did not complete. You can check again and retry.".into();
    }
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

fn fetch(url: &str, path: &Path, max: usize) -> Result<(), String> {
    fetch_with_timeout(url, path, max, 120)
}

fn fetch_with_timeout(url: &str, path: &Path, max: usize, seconds: u32) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid cache path.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let part = path.with_extension("part");
    let result = std::process::Command::new("/usr/bin/curl")
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
        #[cfg(not(target_os = "macos"))]
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn fixture() -> (Manifest, Vec<u8>) {
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
        };
        (release, image)
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
