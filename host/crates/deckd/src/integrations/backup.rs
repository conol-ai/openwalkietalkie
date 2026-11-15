//! Staged atomic writes and backups (design §5.7, ported from openmicrokbd
//! `agent_integrations.rs:293-763` with the corrections of C22).
//!
//! The port keeps three properties of the original, because each of them is
//! the reason a user's config survives a crash or a concurrent editor:
//!
//! 1. **Stage everything, then publish.** Every file is written to a private
//!    sibling temp file (`mode 0600` from the first byte — a staged settings
//!    file holds a bearer token and must never have a world-readable window)
//!    and `fsync`ed before any live file is touched. A multi-file target
//!    (Codex: `hooks.json` + `config.toml`) is one `apply` call.
//! 2. **Rename, don't copy, for the backup.** The live file is *moved* into
//!    the backups directory, so the backup keeps the original inode and mode
//!    and is bit-exact by construction. Only when the rename crosses a file
//!    system (`EXDEV`) does it fall back to a copy, and it says so.
//! 3. **Publish with `hard_link`, never `rename`.** A rename over the target
//!    would silently discard a file another process created in the gap
//!    between the last check and the publish; `hard_link` fails with
//!    `AlreadyExists` instead, and the backup is put back.
//!
//! What is **new** here (C22: the design asked for it, openmicrokbd never did
//! it): backups are named `backups/<file>.<ts>.bak` in deckd's own data
//! directory — not hidden sibling directories that accumulate inside
//! `~/.claude` forever — and are pruned to the ten newest per file name.
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Files bigger than this are not config files; refuse rather than rewrite.
pub const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
/// How many backups of one file name are kept (design §5.7).
pub const BACKUPS_PER_FILE: usize = 10;
/// A file this installer creates from nothing. Existing files keep their own
/// mode: the installer never chmods a file the user already had.
pub const NEW_FILE_MODE: u32 = 0o600;

/// What a file looked like when the plan was made: the bytes we merged into,
/// the digest and mode we will re-check before publishing, and the path with
/// symlinks followed (so editing a symlinked `settings.json` edits the file
/// it points at and leaves the symlink alone).
#[derive(Clone, Debug)]
pub struct FileSnapshot {
    logical: PathBuf,
    resolved: PathBuf,
    bytes: Option<Vec<u8>>,
    digest: Option<[u8; 32]>,
    mode: Option<u32>,
}

impl FileSnapshot {
    /// Read `logical` as it is now. A missing file is a snapshot with no
    /// bytes (the installer creates it); anything that is not a regular file,
    /// or is bigger than [`MAX_CONFIG_BYTES`], is refused.
    pub fn read(logical: &Path) -> Result<FileSnapshot, String> {
        let resolved = resolve(logical)?;
        let meta = match std::fs::symlink_metadata(&resolved) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FileSnapshot {
                    logical: logical.to_path_buf(),
                    resolved,
                    bytes: None,
                    digest: None,
                    mode: None,
                })
            }
            Err(e) => return Err(format!("cannot inspect {}: {e}", logical.display())),
        };
        if !meta.is_file() {
            return Err(format!("{} is not a regular file", logical.display()));
        }
        if meta.len() > MAX_CONFIG_BYTES {
            return Err(format!(
                "{} is {} bytes, larger than the {MAX_CONFIG_BYTES} byte limit for a config file",
                logical.display(),
                meta.len()
            ));
        }
        let bytes = std::fs::read(&resolved)
            .map_err(|e| format!("cannot read {}: {e}", logical.display()))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(format!("{} grew past the size limit", logical.display()));
        }
        Ok(FileSnapshot {
            logical: logical.to_path_buf(),
            resolved,
            digest: Some(*blake3::hash(&bytes).as_bytes()),
            bytes: Some(bytes),
            mode: mode_of(&meta),
        })
    }

    pub fn path(&self) -> &Path {
        &self.logical
    }

    pub fn resolved(&self) -> &Path {
        &self.resolved
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    /// The file's text, or `""` when it does not exist. Non-UTF-8 is an error
    /// the caller reports as a conflict rather than overwriting.
    pub fn text(&self) -> Result<String, String> {
        match &self.bytes {
            None => Ok(String::new()),
            Some(bytes) => String::from_utf8(bytes.clone())
                .map_err(|_| format!("{} is not UTF-8 text", self.logical.display())),
        }
    }

    pub fn exists(&self) -> bool {
        self.bytes.is_some()
    }

    pub fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Is the live file still exactly what the plan was made against?
    pub fn still_matches(&self) -> bool {
        let Ok(resolved) = resolve(&self.logical) else {
            return false;
        };
        if resolved != self.resolved {
            return false;
        }
        match std::fs::symlink_metadata(&resolved) {
            Err(_) => self.bytes.is_none(),
            Ok(meta) => {
                if !meta.is_file() || self.bytes.is_none() || mode_of(&meta) != self.mode {
                    return false;
                }
                match std::fs::read(&resolved) {
                    Ok(bytes) => Some(*blake3::hash(&bytes).as_bytes()) == self.digest,
                    Err(_) => false,
                }
            }
        }
    }
}

/// What the installer wants a file to become.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// These exact bytes.
    Write(Vec<u8>),
    /// Gone (an uninstall that empties a file deckd created).
    Remove,
}

/// One file of a plan.
#[derive(Clone, Debug)]
pub struct Planned {
    pub snapshot: FileSnapshot,
    pub action: Action,
    /// The mode to give the published file **when it does not exist yet**.
    /// `None` means [`NEW_FILE_MODE`]. A file that is already there always
    /// keeps its own mode, so this only ever decides what a *new* file gets.
    ///
    /// It exists for one case: putting a file back from its backup. The
    /// backup is a rename, so it still carries the original's mode, and
    /// without this the restored file would come back `0600` — which for
    /// an executable means "restored but no longer executable".
    mode: Option<u32>,
}

impl Planned {
    pub fn write(snapshot: FileSnapshot, contents: Vec<u8>) -> Planned {
        Planned {
            snapshot,
            action: Action::Write(contents),
            mode: None,
        }
    }

    /// [`Planned::write`], but a file that has to be created is created with
    /// `mode` instead of [`NEW_FILE_MODE`].
    pub fn write_with_mode(
        snapshot: FileSnapshot,
        contents: Vec<u8>,
        mode: Option<u32>,
    ) -> Planned {
        Planned {
            snapshot,
            action: Action::Write(contents),
            mode,
        }
    }

    pub fn remove(snapshot: FileSnapshot) -> Planned {
        Planned {
            snapshot,
            action: Action::Remove,
            mode: None,
        }
    }

    /// True when the file is already what the plan wants. This — and nothing
    /// else — is what makes `install` idempotent.
    pub fn is_noop(&self) -> bool {
        match &self.action {
            Action::Write(contents) => self.snapshot.bytes() == Some(contents.as_slice()),
            Action::Remove => !self.snapshot.exists(),
        }
    }
}

/// What `apply` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Logical paths whose content changed (the symlink, not its target).
    pub changed: Vec<PathBuf>,
    /// Absolute paths of the displaced originals. Shorter than `changed`
    /// whenever a file was created rather than replaced — use [`Applied::published`]
    /// to pair them.
    pub backups: Vec<PathBuf>,
    /// Every published file in publish order, for undoing the run.
    pub published: Vec<Published>,
    /// Non-fatal remarks, e.g. a backup that had to cross a file system.
    pub notes: Vec<String>,
}

/// One published file, as [`restore_all`](crate::integrations) needs it back.
///
/// `resolved` is the file the bytes actually went to. [`publish`] writes
/// through it precisely so that editing a symlinked `settings.json` edits its
/// target and leaves the symlink alone — so a rollback has to put the backup
/// back on to `resolved` too. Undoing the publish against `path` would delete
/// the user's symlink and drop a regular file in its place, while the file it
/// pointed at kept the content deckd wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    /// The path the plan named (the symlink, not its target).
    pub path: PathBuf,
    /// The same path with symlinks followed: where the bytes went.
    pub resolved: PathBuf,
    /// The displaced original, or `None` when the file did not exist and
    /// undoing the publish means deleting it again.
    pub backup: Option<PathBuf>,
}

struct Staged {
    planned: Planned,
    temp: Option<PathBuf>,
}

/// Stage every write, re-check every snapshot, then publish one file at a
/// time — backing each live file up by moving it into `backups`.
pub fn apply(planned: Vec<Planned>, backups: &Path) -> Result<Applied, String> {
    let todo: Vec<Planned> = planned.into_iter().filter(|p| !p.is_noop()).collect();
    if todo.is_empty() {
        return Ok(Applied::default());
    }
    let mut staged: Vec<Staged> = Vec::new();
    for planned in todo {
        let temp = match &planned.action {
            Action::Write(contents) => match stage(&planned.snapshot, contents, planned.mode) {
                Ok(temp) => Some(temp),
                Err(e) => {
                    cleanup(&staged);
                    return Err(e);
                }
            },
            Action::Remove => None,
        };
        staged.push(Staged { planned, temp });
    }
    for entry in &staged {
        if !entry.planned.snapshot.still_matches() {
            cleanup(&staged);
            return Err(format!(
                "{} changed while deckd was preparing the installation; nothing was replaced",
                entry.planned.snapshot.path().display()
            ));
        }
    }
    let mut applied = Applied::default();
    for (at, entry) in staged.iter().enumerate() {
        match publish(entry, backups, &mut applied) {
            Ok(()) => {}
            Err(e) => {
                cleanup(&staged[at..]);
                return Err(partial_error(e, &applied));
            }
        }
    }
    for entry in &staged {
        if let Some(name) = entry.planned.snapshot.path().file_name() {
            prune(backups, &name.to_string_lossy(), &mut applied.notes);
        }
    }
    Ok(applied)
}

/// Publish one staged file: re-check, displace the live file into the
/// backups directory, then hard-link the staged copy into place.
fn publish(entry: &Staged, backups: &Path, applied: &mut Applied) -> Result<(), String> {
    let snapshot = &entry.planned.snapshot;
    if !snapshot.still_matches() {
        return Err(format!(
            "{} changed while deckd was installing",
            snapshot.path().display()
        ));
    }
    let mut backup = None;
    if snapshot.exists() {
        let (path, note) = displace(snapshot, backups)?;
        if let Some(note) = note {
            applied.notes.push(note);
        }
        backup = Some(path);
    }
    if let Some(temp) = &entry.temp {
        if let Err(e) = std::fs::hard_link(temp, snapshot.resolved()) {
            if let Some(backup) = &backup {
                restore(backup, snapshot, applied);
            }
            return Err(format!("cannot publish {}: {e}", snapshot.path().display()));
        }
        let _ = std::fs::remove_file(temp);
        sync_parent(snapshot.resolved());
    }
    applied.changed.push(snapshot.path().to_path_buf());
    if let Some(backup) = &backup {
        applied.backups.push(backup.clone());
    }
    applied.published.push(Published {
        path: snapshot.path().to_path_buf(),
        resolved: snapshot.resolved().to_path_buf(),
        backup,
    });
    Ok(())
}

/// Move the live file into `backups` as `<file>.<ts>.bak`. The rename keeps
/// the inode and the mode; a cross-device rename falls back to a copy and
/// says so, because then the inode property no longer holds.
fn displace(snapshot: &FileSnapshot, backups: &Path) -> Result<(PathBuf, Option<String>), String> {
    std::fs::create_dir_all(backups)
        .map_err(|e| format!("cannot create {}: {e}", backups.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(backups, std::fs::Permissions::from_mode(0o700));
    }
    let name = snapshot
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    for attempt in 0..32u32 {
        let target = backups.join(format!("{name}.{}.bak", stamp(attempt)));
        if target.exists() {
            continue;
        }
        match std::fs::rename(snapshot.resolved(), &target) {
            Ok(()) => return Ok((target, None)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) if is_cross_device(&e) => {
                let bytes = snapshot.bytes().unwrap_or_default();
                std::fs::write(&target, bytes)
                    .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
                #[cfg(unix)]
                if let Some(mode) = snapshot.mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
                }
                std::fs::remove_file(snapshot.resolved())
                    .map_err(|e| format!("cannot move {}: {e}", snapshot.path().display()))?;
                return Ok((
                    target,
                    Some(format!(
                        "the backup of {} crossed a file system, so it is a copy, not the original inode",
                        snapshot.path().display()
                    )),
                ));
            }
            Err(e) => return Err(format!("cannot back up {}: {e}", snapshot.path().display())),
        }
    }
    Err(format!(
        "cannot find a free backup name for {} in {}",
        name,
        backups.display()
    ))
}

/// Put a displaced original back after a failed publish. A newer file at the
/// path wins (someone else got there first) and the backup is kept.
fn restore(backup: &Path, snapshot: &FileSnapshot, applied: &mut Applied) {
    if snapshot.resolved().exists() {
        applied.notes.push(format!(
            "{} was re-created by another process; its backup is {}",
            snapshot.path().display(),
            backup.display()
        ));
        return;
    }
    if std::fs::rename(backup, snapshot.resolved()).is_err()
        && std::fs::hard_link(backup, snapshot.resolved()).is_err()
    {
        applied.notes.push(format!(
            "could not restore {} — the original is at {}",
            snapshot.path().display(),
            backup.display()
        ));
    }
}

/// Write `contents` to a private sibling temp file and sync it.
///
/// `new_mode` is what a file that does not exist yet is created with; an
/// existing file always keeps its own mode, so the installer never chmods a
/// file the user already had.
fn stage(
    snapshot: &FileSnapshot,
    contents: &[u8],
    new_mode: Option<u32>,
) -> Result<PathBuf, String> {
    let parent = snapshot
        .resolved()
        .parent()
        .ok_or_else(|| format!("{} has no parent", snapshot.path().display()))?;
    let fresh = !parent.exists();
    // File modes are Unix's: on Windows a new file takes its folder's ACL.
    #[cfg(not(unix))]
    let _ = (fresh, new_mode);
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    #[cfg(unix)]
    if fresh {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let name = snapshot
        .resolved()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    for attempt in 0..32u32 {
        let temp = parent.join(format!(
            ".{name}.openwalkietalkie-tmp-{}-{}",
            std::process::id(),
            stamp(attempt)
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            // A staged settings file carries a bearer token. Never leave a
            // world-readable window before the chmod below.
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = match opts.open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot stage {}: {e}", snapshot.path().display())),
        };
        use std::io::Write;
        let write = file.write_all(contents).and_then(|()| file.sync_all());
        drop(file);
        if let Err(e) = write {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("cannot stage {}: {e}", snapshot.path().display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = snapshot.mode().or(new_mode).unwrap_or(NEW_FILE_MODE);
            if let Err(e) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode)) {
                let _ = std::fs::remove_file(&temp);
                return Err(format!("cannot set the mode of the staged file: {e}"));
            }
        }
        return Ok(temp);
    }
    Err(format!(
        "cannot find a free staging name next to {}",
        snapshot.path().display()
    ))
}

fn cleanup(staged: &[Staged]) {
    for entry in staged {
        if let Some(temp) = &entry.temp {
            let _ = std::fs::remove_file(temp);
        }
    }
}

fn partial_error(error: String, applied: &Applied) -> String {
    if applied.changed.is_empty() {
        return error;
    }
    let changed: Vec<String> = applied
        .changed
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    format!(
        "{error}; partial installation changed: {}; run install again to finish",
        changed.join(", ")
    )
}

/// Keep the ten newest backups of one file name and delete the rest.
fn prune(backups: &Path, name: &str, notes: &mut Vec<String>) {
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(backups) else {
        return;
    };
    let mut mine: Vec<(u128, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = file.strip_prefix(&prefix) else {
            continue;
        };
        let Some(stamp) = rest.strip_suffix(".bak") else {
            continue;
        };
        // `<file>.<ts>.bak` and nothing else: a user's own `.bak` file next
        // to ours is not ours to delete.
        let Ok(stamp) = stamp.parse::<u128>() else {
            continue;
        };
        mine.push((stamp, entry.path()));
    }
    if mine.len() <= BACKUPS_PER_FILE {
        return;
    }
    mine.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path) in mine.into_iter().skip(BACKUPS_PER_FILE) {
        if std::fs::remove_file(&path).is_err() {
            notes.push(format!("could not prune the backup {}", path.display()));
        }
    }
}

/// The backups of one file, newest first.
pub fn list_backups(backups: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(backups) else {
        return Vec::new();
    };
    let mut mine: Vec<(u128, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().to_string();
            let stamp = file.strip_prefix(&prefix)?.strip_suffix(".bak")?;
            Some((stamp.parse::<u128>().ok()?, entry.path()))
        })
        .collect();
    mine.sort_by_key(|a| std::cmp::Reverse(a.0));
    mine.into_iter().map(|(_, path)| path).collect()
}

/// The mode of a file on disk, for a caller putting a backup back: a backup
/// is a rename, so it still carries the original's permissions and they are
/// the ones the restored file must come back with.
pub fn mode_of_path(path: &Path) -> Option<u32> {
    std::fs::metadata(path).ok().as_ref().and_then(mode_of)
}

/// A unique, sortable stamp: nanoseconds since the epoch plus the attempt.
fn stamp(attempt: u32) -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .saturating_add(attempt as u128)
}

/// The path with symlinks followed.
///
/// A path whose directories do not exist yet resolves through its deepest
/// *existing* ancestor, so a snapshot taken before `create_dir_all` and the
/// re-check taken after it agree. (On macOS `/var` is a symlink to
/// `/private/var`, so a `$TMPDIR` path resolves differently the moment its
/// parent exists — exactly the drift this avoids.)
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return Ok(resolved);
    }
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut base = path;
    loop {
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                base = parent;
            }
            _ => return Ok(path.to_path_buf()),
        }
        if let Ok(resolved) = std::fs::canonicalize(base) {
            let mut out = resolved;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return Ok(out);
        }
    }
}

fn mode_of(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn is_cross_device(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::testdir::TestDir;

    fn snapshot(path: &Path) -> FileSnapshot {
        FileSnapshot::read(path).expect("snapshot")
    }

    #[test]
    fn a_no_op_write_changes_nothing_and_a_real_one_backs_the_original_up() {
        let dir = TestDir::new("backup-noop");
        let target = dir.path().join("settings.json");
        std::fs::write(&target, b"{\"a\":1}\n").unwrap();
        let backups = dir.path().join("backups");
        // Same bytes: filtered out, no backup, no write.
        let applied = apply(
            vec![Planned::write(snapshot(&target), b"{\"a\":1}\n".to_vec())],
            &backups,
        )
        .unwrap();
        assert_eq!(applied, Applied::default());
        assert!(!backups.exists(), "a no-op never creates a backup");
        // Different bytes: one change, one backup that is byte-exact.
        let applied = apply(
            vec![Planned::write(snapshot(&target), b"{\"a\":2}\n".to_vec())],
            &backups,
        )
        .unwrap();
        assert_eq!(applied.changed, vec![target.clone()]);
        assert_eq!(applied.backups.len(), 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"{\"a\":2}\n");
        assert_eq!(std::fs::read(&applied.backups[0]).unwrap(), b"{\"a\":1}\n");
        assert!(applied.backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("settings.json."));
    }

    /// The backup is a *rename*, so it is the very same inode and mode — the
    /// property openmicrokbd's test `symlinked_settings_keep_the_symlink_…`
    /// pins and the reason a backup can be trusted byte for byte.
    #[cfg(unix)]
    #[test]
    fn the_backup_keeps_the_original_inode_and_mode_and_a_symlink_stays_a_symlink() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = TestDir::new("backup-inode");
        let real = dir.path().join("real.json");
        std::fs::write(&real, b"original\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        let inode = std::fs::metadata(&real).unwrap().ino();
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let backups = dir.path().join("backups");
        let applied = apply(
            vec![Planned::write(snapshot(&link), b"replaced\n".to_vec())],
            &backups,
        )
        .unwrap();
        assert_eq!(applied.changed, vec![link.clone()]);
        assert!(applied.notes.is_empty(), "{:?}", applied.notes);
        let backup = &applied.backups[0];
        assert_eq!(std::fs::metadata(backup).unwrap().ino(), inode);
        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read(&real).unwrap(), b"replaced\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640,
            "an existing file keeps its own mode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_is_private_and_its_parent_is_created() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("backup-new");
        let target = dir.path().join("fresh/settings.json");
        let applied = apply(
            vec![Planned::write(snapshot(&target), b"{}\n".to_vec())],
            &dir.path().join("backups"),
        )
        .unwrap();
        assert_eq!(applied.changed, vec![target.clone()]);
        assert!(applied.backups.is_empty(), "nothing to back up");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            NEW_FILE_MODE
        );
    }

    #[test]
    fn a_concurrent_edit_stops_the_whole_plan_and_leaves_no_residue() {
        let dir = TestDir::new("backup-drift");
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        std::fs::write(&a, b"a0").unwrap();
        std::fs::write(&b, b"b0").unwrap();
        let plan = vec![
            Planned::write(snapshot(&a), b"a1".to_vec()),
            Planned::write(snapshot(&b), b"b1".to_vec()),
        ];
        std::fs::write(&b, b"someone else").unwrap();
        let error = apply(plan, &dir.path().join("backups")).unwrap_err();
        assert!(error.contains("changed while deckd"), "{error}");
        assert_eq!(std::fs::read(&a).unwrap(), b"a0", "nothing was published");
        assert_eq!(std::fs::read(&b).unwrap(), b"someone else");
        let residue: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("openwalkietalkie-tmp"))
            .collect();
        assert!(residue.is_empty(), "{residue:?}");
    }

    #[test]
    fn backups_are_pruned_to_ten_per_file_newest_first() {
        let dir = TestDir::new("backup-prune");
        let target = dir.path().join("settings.json");
        let backups = dir.path().join("backups");
        std::fs::write(&target, b"v0").unwrap();
        for n in 1..=14u32 {
            apply(
                vec![Planned::write(
                    snapshot(&target),
                    format!("v{n}").into_bytes(),
                )],
                &backups,
            )
            .unwrap();
        }
        let kept = list_backups(&backups, "settings.json");
        assert_eq!(kept.len(), BACKUPS_PER_FILE, "{kept:?}");
        // Newest first, and the newest holds the last content we replaced.
        assert_eq!(std::fs::read(&kept[0]).unwrap(), b"v13");
        assert_eq!(std::fs::read(&kept[9]).unwrap(), b"v4");
        // A file that is not ours is never pruned.
        std::fs::write(backups.join("settings.json.keep-me.bak"), b"x").unwrap();
        apply(
            vec![Planned::write(snapshot(&target), b"v15".to_vec())],
            &backups,
        )
        .unwrap();
        assert!(backups.join("settings.json.keep-me.bak").exists());
    }

    #[test]
    fn remove_backs_the_file_up_and_a_missing_file_is_a_no_op() {
        let dir = TestDir::new("backup-remove");
        let target = dir.path().join("plist");
        let backups = dir.path().join("backups");
        std::fs::write(&target, b"<plist/>").unwrap();
        let applied = apply(vec![Planned::remove(snapshot(&target))], &backups).unwrap();
        assert_eq!(applied.changed, vec![target.clone()]);
        assert_eq!(std::fs::read(&applied.backups[0]).unwrap(), b"<plist/>");
        assert!(!target.exists());
        let applied = apply(vec![Planned::remove(snapshot(&target))], &backups).unwrap();
        assert_eq!(applied, Applied::default(), "already gone");
    }

    #[test]
    fn an_oversized_or_irregular_file_is_refused_rather_than_rewritten() {
        let dir = TestDir::new("backup-refuse");
        let sub = dir.path().join("a-directory");
        std::fs::create_dir(&sub).unwrap();
        let error = FileSnapshot::read(&sub).unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");
        let big = dir.path().join("big.json");
        std::fs::write(&big, vec![b'x'; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        let error = FileSnapshot::read(&big).unwrap_err();
        assert!(error.contains("larger than"), "{error}");
    }
}
