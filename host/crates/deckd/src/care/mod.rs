//! What the companion app reads about how the deck is fed and kept, as the
//! service knows it: counts, times and states, never a session (the
//! companion redesign, PLAN §3.2).
//!
//! - [`sources`]: `sources.list`, and the owner's `[sources]` switches that
//!   mute a paused source's sessions.
//! - [`test`]: `care.test.*`, the test agent: a question or a voice card the
//!   daemon puts on the deck from its own care session, a spoken sample, and
//!   how the last of each went; and, from the same session, the card that
//!   asks for a press on the deck before a firmware install.
//! - [`checks`]: `care.checks`, the Service page's setup checks.
//! - [`footprint`]: `care.footprint`, what the app has put on this Mac, item
//!   by item, and `care.cache.clear`.
//! - [`export`]: `care.support_file`, `deckd diagnostics` as a method.
//!
//! The core answers the first two itself, from its own tables and the
//! events. The other four methods walk directories, run the doctor and write
//! or delete files, so the door answers them on the blocking pool the way it
//! answers `integrations.*` ([`api_dispatch`]), never on the core thread.
//! The core only says where this daemon keeps its things and how it was
//! started ([`Context`]), and the process environment says where the rest
//! is ([`Homes`]), read the way the installer reads it.
pub mod checks;
pub mod export;
pub mod footprint;
pub mod sources;
pub mod test;

use crate::config::{Config, Paths};
use crate::core::handle::CoreHandle;
use deck_model::ApiError;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The methods [`api_dispatch`] answers on the blocking pool.
pub const POOLED: [&str; 4] = [
    "care.checks",
    "care.footprint",
    "care.support_file",
    "care.cache.clear",
];

/// Whether the door hands `method` to [`api_dispatch`].
#[must_use]
pub fn pooled(method: &str) -> bool {
    POOLED.contains(&method)
}

/// The core's side of a care method: where this daemon keeps its things,
/// how it was started and how its clients stand. Cheap to make, and all
/// the core does for these methods.
#[derive(Clone, Debug)]
pub struct Context {
    /// The daemon's own directories. Never `Paths::detect()`, which names
    /// another data dir for a daemon started with `--data-dir`, and the
    /// operator's own for a unit test.
    pub paths: Paths,
    /// The executable the service runs from, as it was at start.
    pub executable: Option<PathBuf>,
    /// The configuration in force.
    pub config: Arc<Config>,
    /// `--no-device`: nothing may enumerate HID for this service.
    pub no_device: bool,
    /// The TCP door as this service resolves `[api] tcp` (`Core::tcp_door`),
    /// so the doctor's `tcp` row can tell a door that is off from one that
    /// is degraded without reading Claude's settings again.
    pub tcp: crate::config::TcpDoor,
    /// The Codex app-server client is connected now (`crate::codexd`).
    pub codex_connected: bool,
    /// A firmware check or install is running (`firmware.status`).
    pub firmware_busy: bool,
}

/// Where the process environment puts what lives outside the data dir,
/// read as `integrations` reads it, so the footprint and the checks look
/// where the installer writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Homes {
    /// `HOME`.
    pub home: PathBuf,
    /// `$CODEX_HOME`, else `~/.codex`.
    pub codex: PathBuf,
    /// The app's own preferences file.
    pub prefs: PathBuf,
}

impl Homes {
    pub fn detect() -> Result<Homes, String> {
        let home = crate::integrations::home_dir()?;
        let codex = crate::integrations::nonempty_env_path("CODEX_HOME")?
            .unwrap_or_else(|| home.join(".codex"));
        let prefs = prefs_file(&home);
        Ok(Homes { home, codex, prefs })
    }
}

/// The app's preferences (the Footprint page's "Preferences"): its
/// `NSUserDefaults` domain.
#[cfg(target_os = "macos")]
fn prefs_file(home: &Path) -> PathBuf {
    home.join("Library/Preferences/ai.cohdl.openwalkietalkie.plist")
}

/// The app's preferences: the JSON file it keeps under `$XDG_CONFIG_HOME`
/// when that is absolute, else `~/.config` (the app's `prefs.rs`).
#[cfg(not(target_os = "macos"))]
fn prefs_file(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("openwalkietalkie/app-prefs.json")
}

/// One pooled method, its params read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Checks,
    Footprint,
    SupportFile(export::Target),
    CacheClear(footprint::Cache),
}

/// `care.checks` and `care.footprint` take nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}

/// `care.support_file{to}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportFile {
    to: String,
}

/// `care.cache.clear{what}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheClear {
    what: String,
}

fn read<T: DeserializeOwned>(params: serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(params).map_err(|e| ApiError::invalid_params(&e.to_string()))
}

impl Call {
    /// The params, read before anything else happens: a call refused here
    /// never reaches the core, and the two that write or delete refuse
    /// `{}` (an `{}` sweep must never export or clear anything).
    fn parse(method: &str, params: serde_json::Value) -> Result<Call, ApiError> {
        let params = if params.is_null() {
            serde_json::json!({})
        } else {
            params
        };
        match method {
            "care.checks" => read::<Nothing>(params).map(|_| Call::Checks),
            "care.footprint" => read::<Nothing>(params).map(|_| Call::Footprint),
            "care.support_file" => {
                export::Target::parse(&read::<SupportFile>(params)?.to).map(Call::SupportFile)
            }
            "care.cache.clear" => {
                footprint::Cache::parse(&read::<CacheClear>(params)?.what).map(Call::CacheClear)
            }
            other => Err(ApiError::method_not_found(other)),
        }
    }

    /// The answer, off the core thread.
    fn answer(self, context: &Context) -> Result<serde_json::Value, ApiError> {
        let homes = || Homes::detect().map_err(|e| ApiError::internal(&e));
        let now = crate::core::wall_now();
        match self {
            Call::Checks => Ok(checks::run(context, &homes()?)),
            Call::Footprint => Ok(footprint::measure(context, &homes()?)),
            Call::SupportFile(export::Target::Desktop) => {
                export::support_file(context, &export::desktop(&homes()?.home), now)
            }
            Call::SupportFile(export::Target::Dir(dir)) => {
                export::support_file(context, &dir, now)
            }
            Call::CacheClear(cache) => footprint::clear(context, cache),
        }
    }
}

/// [`POOLED`], answered on the blocking pool: the params first, then the
/// core's [`Context`], then the disk.
pub async fn api_dispatch(
    core: &CoreHandle,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let call = Call::parse(method, params)?;
    let context = core.care_context().await?;
    match tokio::task::spawn_blocking(move || call.answer(&context)).await {
        Ok(result) => result,
        Err(e) => Err(ApiError::internal(&format!("the care task failed: {e}"))),
    }
}

/// A [`Context`] and [`Homes`] on a temp dir, for the tests of the three
/// pooled modules: the data dir apart from the logs and the caches, as
/// macOS lays them out, and a home of its own.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::integrations::testdir::TestDir;

    pub fn context(dir: &TestDir) -> Context {
        Context {
            paths: Paths::from_data_dir(
                dir.sub("data"),
                dir.path().join("logs"),
                dir.path().join("caches"),
            ),
            executable: Some(dir.path().join("bin/deckd")),
            config: Arc::new(Config::default()),
            no_device: false,
            tcp: Config::default().api.tcp_door(|| false),
            codex_connected: true,
            firmware_busy: false,
        }
    }

    pub fn homes(dir: &TestDir) -> Homes {
        let home = dir.sub("home");
        Homes {
            codex: home.join(".codex"),
            prefs: home.join("Library/Preferences/ai.cohdl.openwalkietalkie.plist"),
            home,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every pooled method's params are read before the core is asked
    /// anything. The two that take nothing take `{}` and `null` and refuse
    /// a key they do not know; the two that write or delete refuse `{}`,
    /// a relative directory and a cache this service does not keep.
    #[test]
    fn the_params_are_read_first_and_the_two_that_write_refuse_nothing() {
        for method in ["care.checks", "care.footprint"] {
            assert!(Call::parse(method, json!({})).is_ok(), "{method}");
            assert!(Call::parse(method, serde_json::Value::Null).is_ok(), "{method}");
            let error = Call::parse(method, json!({"detect": true})).unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{method}");
        }
        for method in ["care.support_file", "care.cache.clear"] {
            let error = Call::parse(method, json!({})).unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{method}: {error:?}");
        }
        assert_eq!(
            Call::parse("care.support_file", json!({"to": "desktop"})),
            Ok(Call::SupportFile(export::Target::Desktop))
        );
        assert_eq!(
            Call::parse("care.support_file", json!({"to": "/tmp/support"})),
            Ok(Call::SupportFile(export::Target::Dir("/tmp/support".into())))
        );
        for to in ["", "Desktop", "support/here"] {
            let error = Call::parse("care.support_file", json!({ "to": to })).unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{to:?}");
        }
        assert_eq!(
            Call::parse("care.cache.clear", json!({"what": "firmware"})),
            Ok(Call::CacheClear(footprint::Cache::Firmware))
        );
        for what in ["stt", "everything", ""] {
            let error = Call::parse("care.cache.clear", json!({ "what": what })).unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{what:?}");
        }
        let error = Call::parse(
            "care.cache.clear",
            json!({"what": "firmware", "also": "stt"}),
        )
        .unwrap_err();
        assert_eq!(error.code, ApiError::INVALID_PARAMS);
        assert!(POOLED.iter().all(|method| pooled(method)));
        assert!(!pooled("care.test.status") && !pooled("sources.list"));
    }
}
