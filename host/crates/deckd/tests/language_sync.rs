//! Real API calls against a detached in-process daemon and private temp dir.
//! No device, production service, or user preferences are accessed.
mod common;

use common::{Daemon, PATIENCE, WALL};
use deck_model::{DeviceLine, Scope, Store, StoreConfig};
use deckd::{config::Paths, state::{Loaded, State}};
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test]
async fn app_language_api_updates_offline_state_and_persists_its_revision_for_restart() {
    let daemon = Daemon::start().await;
    let auth = daemon.bearer("language", &[Scope::Observe, Scope::Device]);
    let changed = daemon.rpc(&auth, &json!({"id":1,"method":"device.set","params":{"k":"language","v":3}}).to_string()).await;
    assert!(changed["error"].is_null(), "{changed}");
    let state = daemon.rpc(&auth, &json!({"id":2,"method":"state.get"}).to_string()).await;
    let snapshot = &state["result"]["snapshot"];
    assert_eq!(snapshot["device"]["link"], "offline");
    assert_eq!(snapshot["settings"]["language"], 3);
    assert_eq!(snapshot["settings"]["language_revision"], 1);

    let paths = Paths::from_data_dir(daemon.dir.clone(), daemon.dir.join("logs"), daemon.dir.join("caches"));
    let deadline = Instant::now() + PATIENCE;
    let saved = loop {
        let (saved, status) = State::load(&paths.state);
        if status == Loaded::Read && saved.settings.language == 3 && saved.settings.language_revision == 1 {
            break saved;
        }
        assert!(Instant::now() < deadline, "the offline preference was not persisted");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    // Exercise the same loaded-state contribution a fresh daemon receives,
    // without opening another process, hardware link, or production port.
    let mut config = StoreConfig::new(Instant::now(), WALL, 8);
    config.persisted = saved.persisted();
    let restarted = Store::new(config);
    assert_eq!((restarted.snapshot().settings.language, restarted.snapshot().settings.language_revision), (3, 1));

    let invalid = daemon.rpc(&auth, &json!({"id":3,"method":"device.set","params":{"k":"language","v":4}}).to_string()).await;
    assert!(invalid["error"].is_object());
    daemon.shutdown().await;
}

#[tokio::test]
async fn device_language_changes_reach_the_live_gui_subscription_and_snapshot() {
    let daemon = Daemon::start().await;
    let mut observer = daemon.connect().await;
    observer.call("session.hello", json!({"client":{"name":"language-gui","version":"1"},"scopes":["observe"],"subscribe":["*"],"snapshot":true})).await;
    daemon.device_input(DeviceLine::Hello { v: 1, fw: "language".into(), sid: 7, boot: false, rx: 1024,
        agents: None, lang: Some(1), lang_rev: Some(1), faces: vec![] });
    let first = observer.line_where("initial device language", |line| line["method"] == "event" && line["params"]["type"] == "device.set").await;
    assert_eq!(first["params"]["data"], json!({"k":"language","v":1,"rev":1}));
    daemon.device_input(DeviceLine::Language { value: 2, revision: 2 });
    let changed = observer.line_where("live device language", |line| line["method"] == "event" && line["params"]["type"] == "device.set").await;
    assert_eq!(changed["params"]["data"], json!({"k":"language","v":2,"rev":2}));
    let state = observer.call("state.get", json!({})).await;
    assert_eq!(state["snapshot"]["settings"]["language"], 2);
    assert_eq!(state["snapshot"]["settings"]["language_revision"], 2);
    daemon.shutdown().await;
}
