//! `config.set{patch}` end to end (§4.11, week 6): the call writes
//! `config.toml`, and what it reports applied is in force in the **running
//! daemon** by the time it answers: the core takes the file it wrote, as
//! `config.reload` would. The notify watcher of §3.7 still reads the same
//! file one debounce later, swaps the cell the doors read and raises
//! `config.changed{keys}`, exactly as for an editor's save.
//!
//! That is the whole point of the row. A method that wrote the file and
//! left the core to the watcher answered "applied" while `config.get` and
//! the pauses still read the old value for a debounce, and every switch
//! page reads back at once; a method that wrote the file and did not wire
//! the watcher would leave the doors and the disk disagreeing until a
//! restart. Both legs take the file, so there is one idea of what is in
//! force, and the assertions here are not "the file changed" but
//! "`config.get`, answered by the core, reads back what the file says" at
//! once, then "the watcher's cell and event follow".
//!
//! `core::run` hands the watcher's cell to the doors, so the bearer checks
//! and the CORS list follow by the same swap; this rig holds a watcher of
//! its own on the daemon's directory, which is the leg with a thread
//! boundary in it.
//!
//! Nothing here binds 47201 or touches the real data dir: the daemon is
//! `common::Daemon` on a temp dir with `[api] port = 0`.
mod common;

use common::{Daemon, UdsClient};
use deck_model::ApiError;
use deckd::config::{Config, Paths};
use std::time::{Duration, Instant};

/// The contract's bound on one reload (300 ms debounce + FSEvents), with
/// room for a loaded machine.
const WAIT: Duration = Duration::from_secs(5);

async fn admin(daemon: &Daemon) -> UdsClient {
    let mut client = daemon.connect().await;
    let hello = client
        .call(
            "session.hello",
            serde_json::json!({
                "client": {"name": "gui", "version": "1"},
                "scopes": ["admin", "observe"],
            }),
        )
        .await;
    assert!(
        hello["principal"]["scopes"]
            .as_array()
            .is_some_and(|s| s.contains(&"admin".into())),
        "the UDS door's `uds_max_scope` is admin: {hello}"
    );
    client
}

#[tokio::test]
async fn config_set_writes_the_file_and_the_watcher_puts_it_in_force() {
    let daemon = Daemon::start().await;
    let paths = Paths::from_data_dir(
        daemon.dir.clone(),
        daemon.dir.join("logs"),
        daemon.dir.join("caches"),
    );
    // The file an operator keeps by hand, comments and all.
    let original = "\
# OpenWalkieTalkie — edited by hand, please keep the comments
[api]
port = 47201

[voice.stt]
locale = \"en-US\"
";
    std::fs::write(&paths.config, original).unwrap();
    let watch = deckd::config::watch(
        &paths,
        Config::load(&paths.config).unwrap(),
        daemon.api.core.clone(),
        daemon.api.bus.clone(),
    );
    assert!(watch.watching(), "the recommended watcher is up");
    let mut client = admin(&daemon).await;
    let mut events = daemon.api.bus.subscribe();

    // A hot key. The reply describes the write, and the write is in force.
    let ok = client
        .call(
            "config.set",
            serde_json::json!({"patch": {"voice.stt.locale": "en-GB"}}),
        )
        .await;
    assert_eq!(ok["keys"], serde_json::json!(["voice.stt.locale"]));
    assert_eq!(ok["changed"], serde_json::json!(["voice.stt.locale"]));
    assert_eq!(ok["applied"], serde_json::json!(["voice.stt.locale"]));
    let text = std::fs::read_to_string(&paths.config).unwrap();
    assert!(
        text.starts_with("# OpenWalkieTalkie — edited by hand"),
        "the operator's file is edited, not regenerated: {text:?}"
    );
    assert!(text.contains("locale = \"en-GB\""), "{text:?}");
    assert!(
        !text.contains("mic_warm"),
        "no default was materialised: {text:?}"
    );

    // …and the running daemon agrees with it at once: the very next read.
    let config = client.call("config.get", serde_json::json!({})).await;
    assert_eq!(config["voice"]["stt.locale"], "en-GB", "{config}");
    // `config.changed{keys}` is on the bus exactly as an editor's save
    // raises it: one event, whoever did the writing.
    let deadline = Instant::now() + WAIT;
    let changed = loop {
        match events.try_recv() {
            Ok(published)
                if published.event.kind == deck_model::EventKind::ConfigChanged
                    && published.event.data["keys"][0] == "voice.stt.locale" =>
            {
                break published.event;
            }
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "no config.changed on the bus");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    };
    assert_eq!(
        changed.data["keys"],
        serde_json::json!(["voice.stt.locale"])
    );
    // The watcher raises it after swapping its cell, so the doors' view has
    // moved with the core's by now.
    assert_eq!(
        *watch.current().voice.stt.locale,
        *"en-GB",
        "the cell the doors read moved with it"
    );

    // A key that needs a restart is written and **named** as such rather
    // than reported applied: §4.1's "change port and re-install hooks" is
    // this patch, then `integrations.repair claude`, then `service.restart`.
    let ok = client
        .call(
            "config.set",
            serde_json::json!({"patch": {"api.port": 47_299}}),
        )
        .await;
    assert_eq!(ok["applied"], serde_json::json!([]));
    assert_eq!(ok["restart_needed"], serde_json::json!(["api.port"]));
    assert_eq!(
        Config::load(&paths.config).unwrap().api.port,
        47_299,
        "written, even though it cannot be applied without a restart"
    );

    // Several managed keys in one call, which is the shape §4.1's
    // Integrations card uses.
    let ok = client
        .call(
            "config.set",
            serde_json::json!({"patch": {
                "voice.commands": false, "voice.tts.announce_max_chars": 80
            }}),
        )
        .await;
    assert_eq!(
        ok["changed"],
        serde_json::json!(["voice.commands", "voice.tts.announce_max_chars"])
    );
    let config = client.call("config.get", serde_json::json!({})).await;
    assert_eq!(config["voice"]["commands"], false, "{config}");
    assert_eq!(config["voice"]["tts.announce_max_chars"], 80, "{config}");

    // An invalid key or value is `-32602` and **writes nothing**, which is
    // checked against the bytes and not against the reply — and so is a key
    // outside `dispatch::MANAGED`. The trust-boundary rows are the point:
    // an `admin` caller that could write `[trust]` would not be changing a
    // setting, it would be declaring itself trusted, and `[trust]` is hot
    // so it would take effect on the next connection.
    let before = std::fs::read_to_string(&paths.config).unwrap();
    for params in [
        serde_json::json!({"patch": {"api.prot": 1}}),
        serde_json::json!({"patch": {"api.port": "47299"}}),
        serde_json::json!({"patch": {"voice.stt.locale": 7}}),
        serde_json::json!({"patch": {}}),
        serde_json::json!({}),
        serde_json::json!({"patch": {"trust.require_trusted_for_allow": false}}),
        serde_json::json!({"patch": {"trust.dev_requirement": "anchor apple"}}),
        serde_json::json!({"patch": {"privacy.log_content": true}}),
        // The Ghostty injector's old key is no key at all now.
        serde_json::json!({"patch": {"desktop.inject": ["ghostty", "Terminal"]}}),
        serde_json::json!({"patch": {"api.allowed_origins": ["https://evil.example"]}}),
        serde_json::json!({"patch": {"voice.stt.allow_server": true}}),
        // Decision 11: the Mac microphone fallback is gone, and asking for
        // it is refused by name rather than written for nothing to read.
        serde_json::json!({"patch": {"voice.fallback_input": "system_default"}}),
        // All or nothing: the good key in a patch with a bad one is not
        // written either. A port that moved while `[trust]` was refused
        // would leave the hooks pointing at a port nothing answers on.
        serde_json::json!({"patch": {
            "api.port": 47_310, "trust.require_trusted_for_allow": false
        }}),
        serde_json::json!({"patch": {"api.port": 47_310, "voice.stt.locale": 7}}),
    ] {
        let err = client.call_err("config.set", params.clone()).await;
        assert_eq!(err["code"], ApiError::INVALID_PARAMS, "{params}");
        assert_eq!(
            std::fs::read_to_string(&paths.config).unwrap(),
            before,
            "{params}"
        );
    }
    // Nothing partial was left where the watcher could find it either.
    assert!(
        std::fs::read_dir(&daemon.dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().starts_with("config.toml.")),
        "a refused write left a temp file in the watched directory"
    );
    // …and the daemon still holds what it held before any of them.
    let config = client.call("config.get", serde_json::json!({})).await;
    assert_eq!(config["trust"]["require_trusted_for_allow"], true);
    assert_eq!(config["privacy"]["log_content"], false);
    assert_eq!(config["voice"]["stt.allow_server"], false);

    // `config.set` is `admin`, and it is in `/v1/schema` because it answers.
    let auth = daemon.bearer("schema", &[deck_model::Scope::Observe]);
    let (status, schema) = daemon
        .http("GET", "/v1/schema", &[("Authorization", &auth)], "")
        .await;
    assert_eq!(status, 200, "{schema}");
    let row = schema["methods"]
        .as_array()
        .expect("the method table")
        .iter()
        .find(|m| m["name"] == "config.set")
        .expect("config.set is advertised");
    assert_eq!(row["scope"], "admin");
    assert_eq!(row["reducer"], false);
    let mut observer = daemon.connect().await;
    observer
        .call(
            "session.hello",
            serde_json::json!({"client": {"name": "ro", "version": "1"}, "scopes": ["observe"]}),
        )
        .await;
    let err = observer
        .call_err("config.set", serde_json::json!({"patch": {"api.port": 1}}))
        .await;
    assert_eq!(err["code"], ApiError::FORBIDDEN);
    assert_eq!(err["data"]["need"], "admin");

    drop(watch);
    daemon.shutdown().await;
}
