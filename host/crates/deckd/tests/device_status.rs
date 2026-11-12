//! The deck's status as a client of the service sees it (D1): what the hid
//! thread says about the linked deck reaches a `*` subscriber as
//! `device.status`, every one of the six keys in every event, and
//! `state.get` agrees. A detached in-process daemon on a private temp dir:
//! no deck, no network, nothing of the owner's.
mod common;

use common::Daemon;
use deck_model::{DeviceLine, DeviceReport, LinkEvent, Transport};
use deckd::hid::FromHid;
use serde_json::{json, Value};

/// The keys a `device.status` event always carries, null or `[]` when empty.
const KEYS: [&str; 6] = ["transport", "battery", "charging", "board", "last", "drops"];

fn is_status(line: &Value) -> bool {
    line["method"] == "event" && line["params"]["type"] == "device.status"
}

fn every_key(data: &Value) {
    for key in KEYS {
        assert!(data.get(key).is_some(), "{key} missing from {data}");
    }
}

#[tokio::test]
async fn a_linked_decks_status_reaches_a_subscriber_whole_and_the_snapshot_agrees() {
    let daemon = Daemon::start().await;
    let mut gui = daemon.connect().await;
    let hello = json!({"client": {"name": "status-gui", "version": "1"},
        "scopes": ["observe"], "subscribe": ["*"], "snapshot": true});
    gui.call("session.hello", hello).await;
    daemon.hid(FromHid::Link(LinkEvent::Linked));
    let bluetooth = DeviceReport::Transport(Some(Transport::Bluetooth));
    daemon.hid(FromHid::Report(bluetooth));
    daemon.device_input(DeviceLine::Hello {
        v: 1,
        fw: "0.13.0".into(),
        sid: 7,
        boot: false,
        rx: 1024,
        agents: Some(255),
        lang: None,
        lang_rev: None,
        faces: vec![],
    });
    daemon.hid(FromHid::Link(LinkEvent::Live { sid: 7 }));
    daemon.hid(FromHid::Report(DeviceReport::Board(Some("Rev B".into()))));
    daemon.hid(FromHid::Report(DeviceReport::Power {
        battery: Some(85),
        charging: Some(false),
    }));
    let event = gui
        .line_where("the charge", |line| {
            is_status(line) && line["params"]["data"]["battery"] == 85
        })
        .await;
    let data = &event["params"]["data"];
    every_key(data);
    assert_eq!(data["transport"], "bluetooth");
    assert_eq!(
        (&data["charging"], &data["board"]),
        (&json!(false), &json!("Rev B"))
    );
    assert_eq!(data["last"]["fw"], "0.13.0");
    assert_eq!(data["last"]["board"], "Rev B");
    assert_eq!(data["drops"], json!([]));
    let state = gui.call("state.get", json!({})).await;
    let device = &state["snapshot"]["device"];
    assert_eq!(
        (&device["battery"], &device["board"]),
        (&json!(85), &json!("Rev B"))
    );
    assert_eq!(device["transport"], "bluetooth");

    // The deck goes: why, then the `Down`. One more event clears what
    // described it and keeps the drop and the deck it was.
    daemon.hid(FromHid::Report(DeviceReport::Dropped {
        why: "removed".into(),
    }));
    daemon.hid(FromHid::Link(LinkEvent::Down));
    let event = gui
        .line_where("the drop", |line| {
            is_status(line) && line["params"]["data"]["drops"] != json!([])
        })
        .await;
    let data = &event["params"]["data"];
    every_key(data);
    for gone in ["transport", "battery", "charging", "board"] {
        assert_eq!(data[gone], Value::Null, "{gone}");
    }
    assert_eq!(data["drops"][0]["why"], "removed");
    assert_eq!(data["last"]["board"], "Rev B");
    let state = gui.call("state.get", json!({})).await;
    let device = &state["snapshot"]["device"];
    assert_eq!(device["link"], "offline");
    assert!(
        device.get("battery").is_none(),
        "unknown is absent from the snapshot"
    );
    assert_eq!(device["last"]["transport"], "bluetooth");
    daemon.shutdown().await;
}
