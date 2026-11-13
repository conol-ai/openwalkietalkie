//! `care.checks`: the Service page's "Check my setup" as one answer —
//! `deckd doctor`'s rows as objects, then two the doctor does not have: a
//! launch agent left over from before the rename, and the Codex client.
//!
//! The doctor looks for the deck by enumerating HID. A service started
//! with `--no-device` may not do even that, and neither may a unit-test
//! build (O16); nor does a service while a firmware update holds the HID
//! probe thread. For them the `device` row says it did not look: its `ok`
//! is `null`, which the app reads as "not checked", neither a pass nor a
//! problem. The Codex row is `null` the same way while the owner has the
//! client switched off.
//!
//! The launchd rows and the legacy one ask launchd and read
//! `~/Library/LaunchAgents`, which describe the service launchd runs. They
//! are asked only by that service: anywhere else (a unit-test build, a test
//! service, one on its own `--data-dir`, a dev run from a terminal) they
//! would describe the installed service instead of the one answering, so
//! they are `null` with the reason, and nothing is asked.
use super::{Context, Homes};
use crate::launchd::{DECKD_LABEL, LEGACY_DECKD_LABEL};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The doctor's row about the deck.
pub const DEVICE: &str = "device";
/// The doctor's first launchd row, the one left when launchd is not asked.
pub const LAUNCHD: &str = "launchd deckd";
/// The two rows `care.checks` adds to the doctor's.
pub const LEGACY_LAUNCH_AGENT: &str = "legacy launch agent";
pub const CODEX_DAEMON: &str = "codex daemon";

/// One check, as the app reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    /// `None` (`null`): not checked.
    pub ok: Option<bool>,
    pub detail: String,
}

impl Check {
    fn new(name: &str, ok: Option<bool>, detail: impl Into<String>) -> Check {
        Check {
            name: name.to_string(),
            ok,
            detail: detail.into(),
        }
    }
}

/// A doctor's row, which is always judged.
impl From<(String, bool, String)> for Check {
    fn from((name, ok, detail): (String, bool, String)) -> Check {
        Check {
            name,
            ok: Some(ok),
            detail,
        }
    }
}

/// `{"checks": [{name, ok, detail}]}`, in the doctor's order with the two
/// new rows last.
pub fn run(context: &Context, homes: &Homes) -> serde_json::Value {
    // The `tcp` row asks how the door was decided only when the port file
    // names none, and this service already knows.
    let mut checks: Vec<Check> = crate::cli::setup_checks(&context.paths, || Some(context.tcp))
        .into_iter()
        .map(Check::from)
        .collect();
    checks.push(match not_looking(context, cfg!(test)) {
        None => crate::cli::device_check().into(),
        Some(why) => Check::new(DEVICE, None, format!("not looked for: {why}")),
    });
    match not_asking_launchd(cfg!(test), launchd_job()) {
        None => {
            checks.extend(crate::cli::launchd_checks().into_iter().map(Check::from));
            checks.push(legacy_launch_agent(&homes.home));
        }
        Some(why) => {
            for name in [LAUNCHD, LEGACY_LAUNCH_AGENT] {
                checks.push(Check::new(name, None, format!("not looked at: {why}")));
            }
        }
    }
    checks.push(codex_daemon(
        context.config.codex.daemon,
        context.codex_connected,
    ));
    serde_json::json!({ "checks": checks })
}

/// Why the `device` row may not enumerate HID, when it may not: this
/// service never looks for the deck, the build is a unit test's, or a
/// firmware update has the HID probe thread, where an install runs for
/// minutes and the row would wait it out.
fn not_looking(context: &Context, unit_test: bool) -> Option<&'static str> {
    if context.no_device {
        Some("this service runs with --no-device")
    } else if unit_test {
        Some("a unit-test build never enumerates HID")
    } else if context.firmware_busy {
        Some("a firmware check or install is using the deck")
    } else {
        None
    }
}

/// Why the launchd rows and the legacy row may not ask launchd or read
/// `~/Library/LaunchAgents`, when they may not: a unit-test build never
/// does, nor does any service but the one launchd runs (`job`), whose
/// launchd rows would describe the installed service and not itself.
fn not_asking_launchd(unit_test: bool, job: bool) -> Option<&'static str> {
    if unit_test {
        Some("a unit-test build never asks launchd")
    } else if !job {
        Some("launchd did not start this service, so its agent is not the one answering")
    } else {
        None
    }
}

/// Whether this process is the service launchd runs under [`DECKD_LABEL`].
fn launchd_job() -> bool {
    is_job(std::env::var("XPC_SERVICE_NAME").ok().as_deref())
}

/// launchd names the job in `XPC_SERVICE_NAME` for every process it starts
/// (the installed app's agent and a `deckd install --dev` plist alike). A
/// shell in Terminal has it as `0`, an app as its own `application.…` name,
/// and a test runner, a tool or a hand-started service never as ours.
fn is_job(xpc_service_name: Option<&str>) -> bool {
    xpc_service_name == Some(DECKD_LABEL)
}

/// The agent installed before the project was renamed: loaded in launchd,
/// or its plist still in `~/Library/LaunchAgents`. Read only: `deckd
/// uninstall` takes it out, and so does the app's Uninstall.
pub fn legacy_launch_agent(home: &Path) -> Check {
    legacy_row(
        crate::launchd::print(LEGACY_DECKD_LABEL).is_some(),
        legacy_plist(home),
    )
}

/// The leftover agent's plist in `home`, if it is there.
fn legacy_plist(home: &Path) -> Option<PathBuf> {
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{LEGACY_DECKD_LABEL}.plist"));
    plist.exists().then_some(plist)
}

/// The legacy row from what launchd and the home showed.
fn legacy_row(loaded: bool, plist: Option<PathBuf>) -> Check {
    let found = match (loaded, plist) {
        (false, None) => return Check::new(LEGACY_LAUNCH_AGENT, Some(true), "none"),
        (true, None) => format!(
            "{LEGACY_DECKD_LABEL} is loaded in gui/{}",
            crate::launchd::uid()
        ),
        (false, Some(plist)) => format!("{} is on disk", plist.display()),
        (true, Some(plist)) => format!(
            "{LEGACY_DECKD_LABEL} is loaded in gui/{} and {} is on disk",
            crate::launchd::uid(),
            plist.display()
        ),
    };
    Check::new(
        LEGACY_LAUNCH_AGENT,
        Some(false),
        format!("{found}; `deckd uninstall` takes it out"),
    )
}

/// The Codex app-server client (`crate::codexd`), not checked while the
/// owner has it off (`[codex] daemon = false`).
pub fn codex_daemon(enabled: bool, connected: bool) -> Check {
    match (enabled, connected) {
        (false, _) => Check::new(CODEX_DAEMON, None, "off ([codex] daemon = false)"),
        (true, true) => Check::new(CODEX_DAEMON, Some(true), "connected"),
        (true, false) => Check::new(
            CODEX_DAEMON,
            Some(false),
            "not connected: no Codex app-server daemon is answering",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::care::testkit::{context, homes};
    use crate::integrations::testdir::TestDir;
    use serde_json::Value;
    use std::collections::BTreeSet;

    fn keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn rows(reply: &Value) -> &Vec<Value> {
        reply["checks"].as_array().unwrap()
    }

    fn row<'a>(reply: &'a Value, name: &str) -> &'a Value {
        rows(reply)
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("no {name:?} row in {reply}"))
    }

    /// B3.3: the reply, for a daemon on a temp data dir, has the shape of
    /// `GC/tests/fixtures/daemon/care_checks.json` — `{checks: [{name, ok,
    /// detail}]}`, strings and a boolean, or `null` for "not checked",
    /// which the app's parser reads as `None` — and every row the fixture
    /// names, the two new ones among them. Three rows are not checked: this
    /// build never enumerates HID and never asks launchd.
    #[test]
    fn the_reply_has_the_shape_and_every_row_of_the_apps_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../deck-gui-core/tests/fixtures/daemon/care_checks.json"
        ))
        .unwrap();
        let dir = TestDir::new("care-checks");
        let reply = run(&context(&dir), &homes(&dir));
        assert_eq!(keys(&reply), keys(&fixture));
        let want = &fixture["checks"][0];
        for got in rows(&reply) {
            assert_eq!(keys(got), keys(want), "{got}");
            assert!(got["name"].is_string(), "{got}");
            assert!(got["ok"].is_boolean() || got["ok"].is_null(), "{got}");
            let detail = got["detail"].as_str().unwrap_or_default();
            assert!(!detail.is_empty(), "a row with nothing to act on: {got}");
        }
        for want in rows(&fixture) {
            row(&reply, want["name"].as_str().unwrap());
        }
        let unchecked: Vec<&str> = rows(&reply)
            .iter()
            .filter(|row| row["ok"].is_null())
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(unchecked, [DEVICE, LAUNCHD, LEGACY_LAUNCH_AGENT]);
        assert!(row(&reply, DEVICE)["detail"]
            .as_str()
            .unwrap()
            .starts_with("not looked for"));
        for name in [LAUNCHD, LEGACY_LAUNCH_AGENT] {
            assert_eq!(
                row(&reply, name)["detail"],
                "not looked at: a unit-test build never asks launchd"
            );
        }
        assert_eq!(row(&reply, CODEX_DAEMON)["ok"], true);
        assert_eq!(
            row(&reply, LEGACY_LAUNCH_AGENT)["name"],
            LEGACY_LAUNCH_AGENT
        );
        // The doctor's own rows come first, in its order.
        assert_eq!(rows(&reply)[0]["name"], "data dir");
        assert_eq!(rows(&reply)[0]["ok"], true);
    }

    /// O16 and T1.1d: neither a `--no-device` service nor a unit-test
    /// build enumerates HID for the `device` row, nor does a service whose
    /// firmware update has the probe thread, and the row says which held it
    /// back; any other service looks.
    #[test]
    fn the_deck_is_looked_for_only_by_a_service_that_may_open_it() {
        let dir = TestDir::new("care-checks-no-device");
        let mut no_device = context(&dir);
        no_device.no_device = true;
        let reply = run(&no_device, &homes(&dir));
        let device = row(&reply, DEVICE);
        assert_eq!(device["ok"], Value::Null);
        assert_eq!(
            device["detail"],
            "not looked for: this service runs with --no-device"
        );
        let mut installed = context(&dir);
        assert_eq!(
            not_looking(&installed, true),
            Some("a unit-test build never enumerates HID")
        );
        assert_eq!(
            not_looking(&installed, false),
            None,
            "the installed service looks"
        );
        installed.firmware_busy = true;
        assert_eq!(
            not_looking(&installed, false),
            Some("a firmware check or install is using the deck")
        );
        assert_eq!(
            not_looking(&no_device, false),
            Some("this service runs with --no-device")
        );
    }

    /// R3: launchd is asked, and `~/Library/LaunchAgents` read, by the
    /// service launchd runs and by nothing else: never by a unit-test build,
    /// never by a service started any other way (a test service, a dev
    /// run), whose launchd rows would describe the installed one. launchd
    /// names its job in `XPC_SERVICE_NAME`.
    #[test]
    fn launchd_is_asked_only_by_the_service_it_runs() {
        assert_eq!(
            not_asking_launchd(true, true),
            Some("a unit-test build never asks launchd")
        );
        assert_eq!(
            not_asking_launchd(false, false),
            Some("launchd did not start this service, so its agent is not the one answering")
        );
        assert_eq!(not_asking_launchd(false, true), None, "the installed service asks");
        assert!(is_job(Some(DECKD_LABEL)));
        for other in [
            None,
            Some("0"),
            Some(""),
            Some(LEGACY_DECKD_LABEL),
            Some("application.ai.cohdl.openwalkietalkie.1234.5678"),
        ] {
            assert!(!is_job(other), "{other:?}");
        }
    }

    /// The leftover agent is found by its plist in the home it is given — a
    /// fake one here — and the row says where it is and what removes it.
    /// launchd itself is not asked: the rows are built from what it would
    /// have said.
    #[test]
    fn a_legacy_plist_in_the_home_fails_its_row() {
        let dir = TestDir::new("care-legacy");
        let home = dir.sub("home");
        assert_eq!(legacy_plist(&home), None, "an empty home has none");
        let plist = dir.write(
            &format!("home/Library/LaunchAgents/{LEGACY_DECKD_LABEL}.plist"),
            "<plist/>",
        );
        let found = legacy_row(false, legacy_plist(&home));
        assert_eq!(found.name, LEGACY_LAUNCH_AGENT);
        assert_eq!(found.ok, Some(false), "{found:?}");
        assert!(
            found.detail.contains(&plist.display().to_string()),
            "{found:?}"
        );
        assert!(found.detail.ends_with("`deckd uninstall` takes it out"));
        // The whole table, without asking launchd.
        assert_eq!(
            legacy_row(false, None),
            Check::new(LEGACY_LAUNCH_AGENT, Some(true), "none")
        );
        let loaded = legacy_row(true, None);
        assert_eq!(loaded.ok, Some(false));
        assert!(loaded.detail.starts_with(LEGACY_DECKD_LABEL), "{loaded:?}");
        let both = legacy_row(true, Some(plist.clone()));
        assert_eq!(both.ok, Some(false));
        assert!(both.detail.contains("loaded") && both.detail.contains("on disk"));
    }

    /// The Codex client's row: connected passes, waiting fails, and off is
    /// not checked at all (the page leaves its row out then).
    #[test]
    fn the_codex_row_follows_the_client_and_is_not_checked_while_off() {
        assert_eq!(codex_daemon(true, true).ok, Some(true));
        assert_eq!(codex_daemon(true, true).detail, "connected");
        let waiting = codex_daemon(true, false);
        assert_eq!(waiting.ok, Some(false));
        assert!(waiting.detail.starts_with("not connected"));
        for connected in [false, true] {
            let off = codex_daemon(false, connected);
            assert_eq!((off.name.as_str(), off.ok), (CODEX_DAEMON, None));
            assert!(off.detail.contains("[codex] daemon = false"));
        }
        let dir = TestDir::new("care-checks-codex-off");
        let mut off = context(&dir);
        let mut config = (*off.config).clone();
        config.codex.daemon = false;
        off.config = std::sync::Arc::new(config);
        assert_eq!(
            row(&run(&off, &homes(&dir)), CODEX_DAEMON)["ok"],
            Value::Null
        );
    }
}
