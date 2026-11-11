//! The week-4 audio pins are a fact of the manifests, and the two that bite
//! are pins nobody would guess (HOST-CONTRACT-W4 §2 V20, §3 B11):
//!
//! * `rtrb` — 0.4.0 is latest, but **0.3.5 was published after it** as a
//!   backport on the 0.3 line, so "take the newest release" lands on an older
//!   API. A caret range would resolve to 0.4.x today and to whatever the 0.3
//!   line does tomorrow.
//! * `rubato` — 5.0.0 is a clean break from 0.x. A loosened pin that resolved
//!   back to 0.15 would compile a *different* library under the same name.
//!
//! And every one of them is macOS-only: CoreAudio, AVFoundation and
//! Speech.framework exist nowhere else, so they are declared under
//! `[target.'cfg(target_os = "macos")'.dependencies]` exactly like
//! `security-framework` — a plain `[dependencies]` entry breaks the Linux
//! `cargo check` of §13's port plan without anybody noticing on a Mac.
//!
//! This row reads the manifests rather than the resolved graph so that it
//! fails on the *declaration*, which is what a future edit gets wrong.

use std::path::PathBuf;

/// `<repo>/host` — this crate lives at `<repo>/host/crates/deckd`.
fn host_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate sits two levels under the host workspace")
}

fn read(relative: &str) -> String {
    let path = host_dir().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every `[workspace.dependencies]` row of the host workspace, as
/// `name -> the rest of the line`.
fn workspace_deps() -> Vec<(String, String)> {
    let manifest = read("Cargo.toml");
    let at = manifest
        .find("[workspace.dependencies]")
        .expect("the workspace still pins its dependencies in one place");
    let body = &manifest[at..];
    let end = body[1..].find("\n[").map_or(body.len(), |i| i + 1);
    body[..end]
        .lines()
        .skip(1)
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| l.split_once('='))
        .map(|(name, rest)| (name.trim().to_string(), rest.trim().to_string()))
        .collect()
}

/// The audio crates of B11, with the version each must be pinned to.
const AUDIO_PINS: [(&str, &str); 10] = [
    ("cpal", "0.18.2"),
    ("rubato", "5.0.0"),
    ("rtrb", "0.4.0"),
    ("objc2", "0.6.4"),
    ("block2", "0.6.2"),
    ("objc2-foundation", "0.3.2"),
    ("objc2-core-audio", "0.3.2"),
    ("objc2-core-foundation", "0.3.2"),
    ("objc2-speech", "0.3.2"),
    ("objc2-avf-audio", "0.3.2"),
];

#[test]
fn the_voice_pins_are_exact_macos_gated_and_resolve_to_one_objc2() {
    let deps = workspace_deps();
    let row = |name: &str| {
        deps.iter()
            .find(|(n, _)| n == name)
            .map(|(_, rest)| rest.clone())
    };
    for (name, version) in AUDIO_PINS {
        let rest = row(name).unwrap_or_else(|| panic!("{name} is not in [workspace.dependencies]"));
        assert!(
            rest.contains(&format!("\"={version}\"")),
            "{name} must be pinned `=\"{version}\"` (W4 V20), not {rest}"
        );
    }
    // `objc2-avf-audio` without these features has no `AVSpeechSynthesizer`,
    // no buffer/format types to read the per-voice rate off (V16) and no
    // `block2` for the writeUtterance callback: it compiles and then cannot
    // speak.
    let avf = row("objc2-avf-audio").expect("checked above");
    for feature in [
        "AVSpeechSynthesis",
        "AVAudioBuffer",
        "AVAudioFormat",
        "AVAudioTypes",
        "block2",
    ] {
        assert!(
            avf.contains(&format!("\"{feature}\"")),
            "objc2-avf-audio needs the {feature} feature: {avf}"
        );
    }
    // Every pin the workspace declares is an exact one: the whole point of
    // the `--locked` gate is that two machines build the same bytes.
    for (name, rest) in &deps {
        if name.starts_with("deck-") {
            continue; // path dependencies carry no version
        }
        assert!(
            rest.contains("\"=") || rest.contains("version = \"="),
            "{name} is not pinned exactly: {rest}"
        );
    }

    // The gating, in the crate that names them. `security-framework` is the
    // precedent the contract points at, so it has to still be in the same
    // section — a test that only checked the new rows would go green if the
    // section were renamed out from under them.
    let deckd = read("crates/deckd/Cargo.toml");
    // The macOS section, and the one macOS shares with Linux (the voice engine's
    // `lanes`/`ble` pieces: cpal, rubato, rtrb). Neither is a plain `[dependencies]`.
    let section = |header: &str| {
        let at = deckd
            .find(header)
            .unwrap_or_else(|| panic!("deckd still gates its dependencies under {header}"));
        let body = &deckd[at..];
        let end = body[1..].find("\n[").map_or(body.len(), |i| i + 1);
        body[..end].to_string()
    };
    let gated = format!(
        "{}\n{}",
        section("[target.'cfg(target_os = \"macos\")'.dependencies]"),
        section("[target.'cfg(any(target_os = \"macos\", target_os = \"linux\"))'.dependencies]"),
    );
    for name in AUDIO_PINS
        .iter()
        .map(|(n, _)| *n)
        .chain(["security-framework"])
    {
        assert!(
            gated.contains(&format!("\n{name} = ")),
            "{name} must be declared under cfg(target_os = \"macos\") (or macos+linux)"
        );
    }

    // V20's resolution check, kept honest by the lock itself: two `objc2`
    // majors in one graph means two incompatible `Retained<T>`s and error
    // messages nobody can read.
    let lock = read("Cargo.lock");
    let objc2s = lock
        .lines()
        .filter(|l| l.trim() == "name = \"objc2\"")
        .count();
    assert_eq!(objc2s, 1, "the lock carries {objc2s} objc2 versions");
    for (name, version) in AUDIO_PINS {
        assert!(
            lock.contains(&format!("name = \"{name}\"\nversion = \"{version}\"")),
            "{name} {version} is not in Cargo.lock; regenerate it in the same commit"
        );
    }
}
