//! One model download at a time, the way every local model's weights reach
//! the Mac: never at install time, only on the operator's opt-in
//! (`deckd stt install`, `deckd tts install`, the app's first-start prompt
//! and its Voice screen).
//!
//! The transfer goes through `/usr/bin/curl` like every other transfer the
//! daemon makes, with `-C -` so a dropped connection resumes the part file.
//! What happens to a finished download is the job's: a single file is
//! renamed into place once its size is the catalogue's; an archive is
//! unpacked and is "present" once its marker file exists. `stt` (Whisper)
//! and `kokoro` (the voice) build the jobs; this module runs them and
//! reports progress for `stt.status` / `tts.status`.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the download's progress is read while curl runs.
const POLL: Duration = Duration::from_millis(300);

/// What to do with a finished part file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Rename the part file to `path`; `total` bytes or it is not the model.
    Rename { path: PathBuf, total: u64 },
    /// Unpack a `.tar.bz2` into `into`, which is then present when `marker`
    /// exists.
    Extract { into: PathBuf, marker: PathBuf },
}

/// One download, as the catalogue modules describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// The model's name, for the log and the status.
    pub name: String,
    pub url: String,
    /// The file curl writes (and resumes).
    pub part: PathBuf,
    /// The catalogue's size, `0` when unknown.
    pub total: u64,
    pub finish: Finish,
}

impl Job {
    /// Whether this job's result is already on disk, whole.
    #[must_use]
    pub fn present(&self) -> bool {
        match &self.finish {
            Finish::Rename { path, total } => std::fs::metadata(path).is_ok_and(|m| {
                if *total > 0 {
                    m.len() == *total
                } else {
                    m.len() > 0
                }
            }),
            Finish::Extract { marker, .. } => marker.is_file(),
        }
    }

    /// Where the result lives: the file, or the unpacked directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        match &self.finish {
            Finish::Rename { path, .. } => path,
            Finish::Extract { into, .. } => into,
        }
    }

    /// Delete the result and any part file.
    pub fn remove(&self) -> Result<(), String> {
        let mut victims = vec![self.part.clone()];
        match &self.finish {
            Finish::Rename { path, .. } => victims.push(path.clone()),
            Finish::Extract { into, .. } => {
                if into.is_dir() {
                    std::fs::remove_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
                    tracing::info!(path = %into.display(), "download: removed");
                }
            }
        }
        for victim in victims {
            match std::fs::remove_file(&victim) {
                Ok(()) => tracing::info!(path = %victim.display(), "download: removed"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", victim.display())),
            }
        }
        Ok(())
    }
}

/// Where a download stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub model: String,
    pub path: PathBuf,
    /// The catalogue's size, `0` for a model it does not list.
    pub total: u64,
    pub bytes: u64,
    pub running: bool,
    pub done: bool,
    pub error: Option<String>,
}

impl Progress {
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "path": self.path.display().to_string(),
            "total": self.total,
            "bytes": self.bytes,
            "running": self.running,
            "done": self.done,
            "error": self.error,
        })
    }
}

struct Inner {
    progress: Option<Progress>,
    child: Option<Child>,
}

/// The daemon's one download at a time (per kind of model), driven from a
/// thread of its own and read by the status calls.
#[derive(Clone)]
pub struct Installer {
    inner: Arc<Mutex<Inner>>,
    curl: PathBuf,
    /// Never start curl: see [`Installer::offline`].
    offline: bool,
}

/// The error an offline installer reports for the download it did not start.
pub const OFFLINE: &str = "downloads are off in this daemon: nothing was fetched";

impl Default for Installer {
    fn default() -> Self {
        Installer {
            inner: Arc::new(Mutex::new(Inner {
                progress: None,
                child: None,
            })),
            curl: PathBuf::from("curl"),
            offline: false,
        }
    }
}

impl std::fmt::Debug for Installer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installer")
            .field("progress", &self.status())
            .finish_non_exhaustive()
    }
}

impl Installer {
    /// An installer that never downloads. `start` answers the way a failed
    /// fetch ends (not running, not done, the reason as its error), so a
    /// `stt.status`/`tts.status` poll stops instead of waiting, and nothing
    /// is spawned or written. Every unit-test core has one (O16): the API
    /// tests call `stt.install` and `tts.install` with no model named,
    /// which is the 1.6 GB recogniser and the 365 MB voice.
    #[must_use]
    pub fn offline() -> Self {
        Installer {
            offline: true,
            ..Installer::default()
        }
    }

    /// The curl to run (tests point this at a fake).
    #[must_use]
    pub fn with_curl(mut self, curl: impl Into<PathBuf>) -> Self {
        self.curl = curl.into();
        self
    }

    #[must_use]
    pub fn status(&self) -> Option<Progress> {
        self.inner.lock().unwrap().progress.clone()
    }

    /// Start `job`, unless a download is already running (then that one's
    /// progress is returned). A result already on disk is reported `done`
    /// at once; an [offline](Installer::offline) installer starts nothing.
    pub fn start(&self, job: &Job) -> Result<Progress, String> {
        {
            let st = self.inner.lock().unwrap();
            if let Some(progress) = st.progress.as_ref().filter(|p| p.running) {
                return Ok(progress.clone());
            }
        }
        if job.present() {
            let progress = Progress {
                model: job.name.clone(),
                path: job.path().to_path_buf(),
                total: job.total,
                bytes: job.total,
                running: false,
                done: true,
                error: None,
            };
            self.inner.lock().unwrap().progress = Some(progress.clone());
            return Ok(progress);
        }
        // What is on disk needed no fetch; anything else would.
        if self.offline {
            let progress = Progress {
                model: job.name.clone(),
                path: job.path().to_path_buf(),
                total: job.total,
                bytes: std::fs::metadata(&job.part).map_or(0, |m| m.len()),
                running: false,
                done: false,
                error: Some(OFFLINE.into()),
            };
            self.inner.lock().unwrap().progress = Some(progress.clone());
            return Ok(progress);
        }
        if let Some(dir) = job.part.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let child = spawn_curl(&self.curl, &job.url, &job.part)?;
        let progress = Progress {
            model: job.name.clone(),
            path: job.path().to_path_buf(),
            total: job.total,
            bytes: std::fs::metadata(&job.part).map_or(0, |m| m.len()),
            running: true,
            done: false,
            error: None,
        };
        {
            let mut st = self.inner.lock().unwrap();
            st.child = Some(child);
            st.progress = Some(progress.clone());
        }
        let inner = self.inner.clone();
        let job = job.clone();
        std::thread::Builder::new()
            .name("deckd-download".into())
            .spawn(move || {
                let outcome = loop {
                    std::thread::sleep(POLL);
                    let mut st = inner.lock().unwrap();
                    let bytes = std::fs::metadata(&job.part).map_or(0, |m| m.len());
                    if let Some(p) = st.progress.as_mut() {
                        p.bytes = bytes;
                    }
                    let Some(child) = st.child.as_mut() else {
                        break Err("cancelled".to_string());
                    };
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let child = st.child.take();
                            drop(st);
                            let err = child.and_then(|c| c.wait_with_output().ok()).map(|o| {
                                String::from_utf8_lossy(&o.stderr).trim().to_string()
                            });
                            break if status.success() {
                                finish(&job)
                            } else {
                                Err(format!(
                                    "curl exit {}: {}",
                                    status.code().unwrap_or(-1),
                                    err.unwrap_or_default()
                                ))
                            };
                        }
                        Ok(None) => {}
                        Err(e) => {
                            st.child = None;
                            break Err(format!("curl: {e}"));
                        }
                    }
                };
                let mut st = inner.lock().unwrap();
                st.child = None;
                if let Some(p) = st.progress.as_mut() {
                    p.running = false;
                    match outcome {
                        Ok(()) => {
                            p.done = true;
                            p.bytes = p.total.max(p.bytes);
                            tracing::info!(model = %p.model, bytes = p.bytes, "download: done");
                        }
                        Err(why) => {
                            tracing::warn!(model = %p.model, why, "download: failed");
                            p.error = Some(why);
                        }
                    }
                }
            })
            .map_err(|e| format!("download thread: {e}"))?;
        Ok(progress)
    }

    /// Stop a running download. The part file stays, so a later start
    /// resumes it.
    pub fn cancel(&self) {
        let mut st = self.inner.lock().unwrap();
        if let Some(mut child) = st.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(p) = st.progress.as_mut().filter(|p| p.running) {
            p.running = false;
            p.error = Some("cancelled".into());
        }
    }
}

/// `<path>.part`.
#[must_use]
pub fn part_path(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// `curl` fetching `url` into `part`, resuming whatever is there, following
/// redirects (the hubs serve from a CDN), failing on an HTTP error, quiet
/// except for errors.
fn spawn_curl(curl: &Path, url: &str, part: &Path) -> Result<Child, String> {
    Command::new(curl)
        .arg("-sS")
        .arg("-L")
        .arg("--fail")
        .arg("--retry")
        .arg("3")
        .arg("-C")
        .arg("-")
        .arg("-o")
        .arg(part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", curl.display()))
}

/// curl finished: the part file is the model only if it is whole, and an
/// archive only once it is unpacked.
fn finish(job: &Job) -> Result<(), String> {
    let len = std::fs::metadata(&job.part).map_err(|e| format!("{}: {e}", job.part.display()))?.len();
    if job.total > 0 && len != job.total {
        return Err(format!("the download is {len} bytes, the model is {}", job.total));
    }
    match &job.finish {
        Finish::Rename { path, .. } => {
            std::fs::rename(&job.part, path).map_err(|e| format!("{}: {e}", path.display()))
        }
        Finish::Extract { into, marker } => {
            // Unpack beside the target, then move into place, so a half
            // unpacked directory is never mistaken for the model.
            let staging = PathBuf::from(format!("{}.unpacking", into.display()));
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
            let out = Command::new("tar")
                .arg("-xjf")
                .arg(&job.part)
                .arg("-C")
                .arg(&staging)
                .arg("--strip-components=1")
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("tar: {e}"))?;
            if !out.status.success() {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(format!(
                    "tar exit {}: {}",
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let _ = std::fs::remove_dir_all(into);
            std::fs::rename(&staging, into).map_err(|e| format!("{}: {e}", into.display()))?;
            let _ = std::fs::remove_file(&job.part);
            if marker.is_file() {
                Ok(())
            } else {
                Err(format!("the archive did not contain {}", marker.display()))
            }
        }
    }
}

/// Run `job` on this thread (the CLI), reporting `(bytes, total)` as it goes.
pub fn fetch(job: &Job, mut on_progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let installer = Installer::default();
    let progress = installer.start(job)?;
    if progress.done {
        return Ok(progress.path);
    }
    loop {
        std::thread::sleep(POLL);
        let Some(p) = installer.status() else {
            return Err("the download vanished".into());
        };
        on_progress(p.bytes, p.total);
        if !p.running {
            return match p.error {
                None => Ok(p.path),
                Some(why) => Err(why),
            };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fake curl that writes a file of the size the URL asks for and
    /// exits as told, so the installer's thread, progress, rename, unpack
    /// and failure paths run without a network. `tarball` in the URL makes
    /// it write a real `.tar.bz2` holding `model.onnx`.
    #[cfg(unix)]
    const FAKE: &str = r#"
import sys, time, io, tarfile
args = sys.argv[1:]
out = args[args.index("-o") + 1]
url = args[-1]
if "fail" in url:
    sys.stderr.write("curl: (22) The requested URL returned error: 404\n")
    sys.exit(22)
if "tarball" in url:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:bz2") as t:
        for name in ("model.onnx", "voices.bin", "tokens.txt"):
            data = b"x" * 1000
            info = tarfile.TarInfo("pkg/" + name)
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
    with open(out, "wb") as f:
        f.write(buf.getvalue())
    sys.exit(0)
size = 147964211 if "base.en" in url else 64
with open(out, "ab") as f:
    have = f.tell()
    chunk = max(1, (size - have) // 8)
    while have < size:
        n = min(chunk, size - have)
        f.write(b"\0" * n)
        f.flush()
        have += n
        time.sleep(0.15)
"#;

    #[cfg(unix)]
    pub(crate) fn fake_curl(dir: &Path) -> PathBuf {
        let script = dir.join("fake_curl.py");
        std::fs::write(&script, FAKE).unwrap();
        let curl = dir.join("curl");
        std::fs::write(
            &curl,
            format!("#!/bin/sh\nexec /usr/bin/python3 -I {} \"$@\"\n", script.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
        curl
    }

    fn wait(installer: &Installer) -> Progress {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let p = installer.status().unwrap();
            if !p.running {
                break p;
            }
        }
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn a_file_job_downloads_reports_progress_and_renames_a_whole_file() {
        let dir = std::env::temp_dir().join(format!("deckd-download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let installer = Installer::default().with_curl(fake_curl(&dir));
        let job = crate::stt::job(&dir.join("models"), "base.en");
        let first = installer.start(&job).unwrap();
        assert!(first.running);
        assert_eq!(first.total, 147_964_211);
        let mut saw_partial = false;
        let progress = loop {
            std::thread::sleep(Duration::from_millis(50));
            let p = installer.status().unwrap();
            if p.running && p.bytes > 0 && p.bytes < p.total {
                saw_partial = true;
            }
            if !p.running {
                break p;
            }
        };
        assert!(saw_partial, "progress was readable while curl ran");
        assert!(progress.done, "{progress:?}");
        assert_eq!(progress.error, None);
        assert!(job.present());
        assert!(!job.part.exists());
        let again = installer.start(&job).unwrap();
        assert!(again.done && !again.running, "answered from the disk");
        // An uncatalogued name with a failing fetch reports curl's words.
        let failed = installer.start(&crate::stt::job(&dir.join("models"), "fail-me")).unwrap();
        assert!(failed.running);
        let progress = wait(&installer);
        assert!(!progress.done);
        assert!(progress.error.as_deref().is_some_and(|e| e.contains("404")), "{progress:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn an_archive_job_is_unpacked_and_present_only_with_its_marker() {
        let dir = std::env::temp_dir().join(format!("deckd-download-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let installer = Installer::default().with_curl(fake_curl(&dir));
        let into = dir.join("voice").join("pkg");
        let job = Job {
            name: "pkg".into(),
            url: "https://example.invalid/tarball/pkg.tar.bz2".into(),
            part: part_path(&dir.join("voice").join("pkg.tar.bz2")),
            total: 0,
            finish: Finish::Extract {
                into: into.clone(),
                marker: into.join("model.onnx"),
            },
        };
        assert!(!job.present());
        assert!(installer.start(&job).unwrap().running);
        let progress = wait(&installer);
        assert!(progress.done, "{progress:?}");
        assert!(job.present());
        assert!(into.join("voices.bin").is_file(), "the archive's top directory was stripped");
        assert!(!job.part.exists());
        job.remove().unwrap();
        assert!(!job.present() && !into.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// O16: an offline installer answers `start` the way a failed fetch
    /// ends and spawns nothing; a model already on disk is still `done`,
    /// because reporting it needs no fetch.
    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn an_offline_installer_never_starts_curl_and_says_why() {
        let dir = std::env::temp_dir()
            .join(format!("deckd-download-offline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The fake, not the real curl: had this regressed, it would write
        // the part file rather than reach the network.
        let installer = Installer::offline().with_curl(fake_curl(&dir));
        let job = crate::stt::job(&dir.join("models"), "tiny");
        let progress = installer.start(&job).unwrap();
        assert!(!progress.running && !progress.done, "{progress:?}");
        assert_eq!(progress.error.as_deref(), Some(OFFLINE));
        assert_eq!(installer.status(), Some(progress), "what `stt.status` reports");
        assert!(!dir.join("models").exists(), "no part file, not even its directory");
        let local = crate::stt::job(&dir.join("local"), "custom");
        std::fs::create_dir_all(dir.join("local")).unwrap();
        std::fs::write(local.path(), b"weights").unwrap();
        let progress = installer.start(&local).unwrap();
        assert!(progress.done && progress.error.is_none(), "{progress:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
