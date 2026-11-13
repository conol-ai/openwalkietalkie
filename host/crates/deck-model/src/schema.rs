//! The JSON Schema of everything on the API, from the one Rust source
//! (`GET /v1/schema`).
use crate::{ApiError, Command, Event, LinkEvent, Request, Snapshot};
use schemars::schema_for;

/// `{"snapshot": …, "event": …, "request": …, "link_event": …, "error": …}`.
pub fn schema() -> serde_json::Value {
    let _: Option<Command> = None; // Commands are internal; their payloads are below.
    serde_json::json!({
        "snapshot": schema_for!(Snapshot),
        "event": schema_for!(Event),
        "request": schema_for!(Request),
        "link_event": schema_for!(LinkEvent),
        "error": schema_for!(ApiError),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_names_the_top_level_types() {
        let s = schema();
        assert_eq!(s["snapshot"]["title"], "Snapshot");
        assert!(s["snapshot"]["properties"]["agents"].is_object());
        assert!(s["request"]["oneOf"].is_array() || s["request"]["anyOf"].is_array());
        assert!(s["event"]["properties"]["type"].is_object());
        assert_eq!(s["error"]["title"], "ApiError");
        assert!(s["error"]["properties"]["code"].is_object());
    }

    #[test]
    fn every_snapshot_and_request_type_has_a_schema_definition() {
        let s = schema();
        let defs = &s["snapshot"]["$defs"];
        for name in [
            "Agent",
            "Card",
            "Reply",
            "DeviceState",
            "VoiceState",
            "PttInfo",
            "Settings",
            "Usage",
            "Focus",
            "Principal",
            "Ts",
        ] {
            assert!(defs[name].is_object(), "{name}");
        }
        let defs = &s["request"]["$defs"];
        for name in [
            "AgentRegister",
            "CardSet",
            "OpAnswer",
            "OpOrder",
            "OrderOutcome",
            "FocusReport",
        ] {
            assert!(defs[name].is_object(), "{name}");
        }
    }
}
