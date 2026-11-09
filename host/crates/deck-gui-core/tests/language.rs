use deck_gui_core::{ApiEvent, AppEvent, HostState, UiAction};
use deck_gui_core::i18n::Language;
use deck_model::{Event, EventKind, Principal, SetKey, Snapshot, Ts};
use serde_json::json;

#[test]
fn language_changes_from_either_side_update_the_same_mirror_and_survive_reconnect() {
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot: Snapshot::default() }));
    for (event_rev, principal, language, generation) in [
        (1, Principal::Device, Language::TraditionalChinese, 5),
        (2, Principal::Uid(501), Language::Japanese, 6),
    ] {
        state.handle_event(AppEvent::Api(ApiEvent::Event(Event {
            rev: event_rev,
            ts: Ts(0),
            kind: EventKind::DeviceSet,
            principal,
            data: json!({"k":"language","v":language as u8,"rev":generation}),
        })));
        assert_eq!(state.mirror().settings.language, language as u16);
        assert_eq!(state.mirror().settings.language_revision, generation);
        assert_eq!(state.mirror().device.settings.language, language as u16);
    }
    let snapshot = state.mirror().clone();
    let mut reconnected = HostState::detached();
    reconnected.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    assert_eq!(reconnected.mirror().settings.language, Language::Japanese as u16);
    assert_eq!(reconnected.mirror().settings.language_revision, 6);
}

#[test]
fn language_selection_uses_the_shared_device_setting_api_without_optimistic_state() {
    let state = HostState::detached();
    for language in Language::ALL {
        let action = UiAction::DeviceSet { key: SetKey::Language, value: language as u16 };
        assert_eq!(action.request(), ("device.set", json!({"k":"language","v":language as u8})));
        let _ = state.send(&action);
        assert_eq!(state.mirror().settings.language, 0);
    }
}

#[test]
fn an_older_state_get_reply_cannot_undo_a_live_language_change() {
    let mut state = HostState::detached();
    state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot: Snapshot::default() }));
    state.handle_event(AppEvent::Api(ApiEvent::Event(Event {
        rev: 1, ts: Ts(0), kind: EventKind::DeviceSet, principal: Principal::Device,
        data: json!({"k":"language","v":3,"rev":7}),
    })));
    state.handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot: Snapshot::default() }));
    assert_eq!(state.mirror().settings.language, 3);
    assert_eq!(state.mirror().settings.language_revision, 7);
    state.handle_event(AppEvent::Api(ApiEvent::Resync));
    assert_eq!(state.handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot: Snapshot::default() })), vec![deck_gui_core::HostEffect::StateGet]);
    assert!(state.resyncing());
    let current = state.mirror().clone();
    state.handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot: current }));
    assert!(!state.resyncing());
    assert_eq!(state.mirror().settings.language, 3);
}
