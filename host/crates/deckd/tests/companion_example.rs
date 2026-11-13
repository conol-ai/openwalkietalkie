//! The companion example the website offers for download finds the socket
//! where `deckd` listens.
//!
//! `host/examples/native_companion.py` is served as
//! `site/public/examples/native_companion.py` (`tools/release.py` refuses a
//! release whose download differs), and the companion guide beside it
//! documents `config::Paths::detect`'s rule: on Linux the socket is in
//! `$XDG_RUNTIME_DIR/openwalkietalkie/`, else in
//! `${XDG_CACHE_HOME:-~/.cache}/openwalkietalkie/`, and `DECKD_DATA_DIR`
//! overrides both. A client that follows the example must reach the same
//! file the daemon binds.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The repository root: this crate lives at `<repo>/host/crates/deckd`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the crate sits three levels under the repository root")
}

/// The example in the tree, which the website copies.
fn example() -> PathBuf {
    repo_root().join("host/examples/native_companion.py")
}

/// `python3` on PATH, or `None` (the row prints its reason and skips).
fn python3() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|p| p.is_file())
}

/// Loads the example as a module (its `__main__` block does not run; it is
/// registered first, as its dataclasses need), takes `sys.platform` from the
/// second argument when there is one, and prints `socket_path()` once per
/// line of stdin. Each line is the whole environment as a JSON object, or
/// `null` for the one it was started with.
const PROBE: &str = r#"
import importlib.util, json, os, sys
spec = importlib.util.spec_from_file_location('native_companion', sys.argv[1])
example = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = example
spec.loader.exec_module(example)
if len(sys.argv) > 2:
    sys.platform = sys.argv[2]
for line in sys.stdin:
    env = json.loads(line)
    if env is not None:
        os.environ.clear()
        os.environ.update(env)
    print(example.socket_path())
"#;

/// What the example's `socket_path()` answers on `platform` (this
/// machine's own when `None`), once per environment.
fn socket_paths(
    python: &Path,
    platform: Option<&str>,
    envs: &[Option<serde_json::Value>],
) -> Vec<String> {
    let mut command = Command::new(python);
    // Isolated (no user site, no cwd on the path) and no bytecode written
    // next to the example.
    command.args(["-I", "-B", "-c", PROBE]).arg(example());
    command.args(platform);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 runs");
    {
        let mut stdin = child.stdin.take().expect("the probe's stdin");
        for env in envs {
            let line = env
                .as_ref()
                .map_or_else(|| "null".into(), ToString::to_string);
            writeln!(stdin, "{line}").expect("the probe reads its environments");
        }
    }
    let out = child.wait_with_output().expect("the probe finishes");
    assert!(
        out.status.success(),
        "the probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let paths: Vec<String> = String::from_utf8(out.stdout)
        .expect("paths in UTF-8")
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(paths.len(), envs.len(), "one path per environment");
    paths
}

/// `vars`, with `HOME`, as the whole environment of one probe.
fn env(home: &str, vars: &[(&str, &str)]) -> Option<serde_json::Value> {
    let mut all = serde_json::Map::new();
    all.insert("HOME".into(), home.into());
    for (name, value) in vars {
        all.insert((*name).into(), (*value).into());
    }
    Some(serde_json::Value::Object(all))
}

#[test]
fn the_website_offers_the_example_in_the_tree() {
    let site = repo_root().join("site/public/examples/native_companion.py");
    let served = std::fs::read(&site).expect("the website's copy of the example");
    let ours = std::fs::read(example()).expect("the example");
    assert!(
        served == ours,
        "{} is not host/examples/native_companion.py byte for byte, and tools/release.py would \
         refuse the release: copy the example over it",
        site.display()
    );
}

#[test]
fn the_example_looks_where_this_deckd_listens() {
    let Some(python) = python3() else {
        eprintln!("skip: no python3 on PATH");
        return;
    };
    // An empty `DECKD_DATA_DIR` is set to `deckd` (a socket relative to its
    // cwd) and unset to the example; neither is a place a client looks.
    if std::env::var_os("DECKD_DATA_DIR").is_some_and(|dir| dir.is_empty()) {
        eprintln!("skip: DECKD_DATA_DIR is set but empty");
        return;
    }
    let paths = deckd::config::Paths::detect().expect("this process has a home");
    let found = socket_paths(&python, None, &[None]);
    assert_eq!(
        PathBuf::from(&found[0]),
        paths.socket,
        "the example and deckd disagree about the socket in this environment"
    );
}

#[test]
fn the_examples_linux_rule_is_the_one_deckd_follows() {
    let Some(python) = python3() else {
        eprintln!("skip: no python3 on PATH");
        return;
    };
    let home = "/home/owner";
    let cases: [(&[(&str, &str)], &str); 7] = [
        // Neither variable: the cache directory under the home.
        (&[], "/home/owner/.cache/openwalkietalkie/deckd.sock"),
        // No runtime directory: the cache directory.
        (
            &[("XDG_CACHE_HOME", "/var/cache/owner")],
            "/var/cache/owner/openwalkietalkie/deckd.sock",
        ),
        // The runtime directory comes first.
        (
            &[
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
                ("XDG_CACHE_HOME", "/var/cache/owner"),
            ],
            "/run/user/1000/openwalkietalkie/deckd.sock",
        ),
        // A relative or empty value is skipped, as `directories` skips it.
        (
            &[("XDG_RUNTIME_DIR", "run/user/1000")],
            "/home/owner/.cache/openwalkietalkie/deckd.sock",
        ),
        (
            &[("XDG_RUNTIME_DIR", ""), ("XDG_CACHE_HOME", "cache")],
            "/home/owner/.cache/openwalkietalkie/deckd.sock",
        ),
        // Never the state directory the example used to fall back to.
        (
            &[("XDG_STATE_HOME", "/home/owner/.local/state")],
            "/home/owner/.cache/openwalkietalkie/deckd.sock",
        ),
        // `DECKD_DATA_DIR` wins over both, as it does for the daemon.
        (
            &[
                ("DECKD_DATA_DIR", "/srv/owt"),
                ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ],
            "/srv/owt/deckd.sock",
        ),
    ];
    let envs: Vec<_> = cases.iter().map(|(vars, _)| env(home, vars)).collect();
    let found = socket_paths(&python, Some("linux"), &envs);
    for ((vars, want), got) in cases.iter().zip(&found) {
        assert_eq!(got, want, "on Linux with {vars:?}");
    }

    // macOS keeps the Application Support path, under `DECKD_DATA_DIR`
    // when that is set.
    let found = socket_paths(
        &python,
        Some("darwin"),
        &[
            env("/Users/owner", &[("XDG_RUNTIME_DIR", "/run/user/501")]),
            env("/Users/owner", &[("DECKD_DATA_DIR", "/tmp/owt")]),
        ],
    );
    assert_eq!(
        found,
        [
            "/Users/owner/Library/Application Support/OpenWalkieTalkie/deckd.sock",
            "/tmp/owt/deckd.sock",
        ]
    );
}
