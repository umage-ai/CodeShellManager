//! The keyboard map through the engine: `keymap_list`, `keymap_set` and `keymap_reset`, the
//! settings they write, the event the UI follows, and damaged or unknown entries in the settings.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_engine::{Event, ToolError};
use serde_json::{Value, json};

fn chords_of(list: &Value, id: &str) -> Vec<String> {
    let actions = list["actions"].as_array().cloned().unwrap_or_default();
    let entry = actions.iter().find(|a| a["id"] == id).unwrap_or_else(|| panic!("no action {id}"));
    entry["chords"].as_array().map(|c| c.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

#[test]
fn the_defaults_list_every_action_with_no_conflicts() {
    let h = H::start(test_app(), |_| {});
    let list = h.ok("keymap_list", json!({})).data;
    assert_eq!(chords_of(&list, "session_new"), ["Ctrl+T"]);
    assert_eq!(chords_of(&list, "ui.help"), ["F1", "Ctrl+/"]);
    assert_eq!(list["conflicts"], json!([]));
    assert_eq!(list["skipped"], json!([]));
    assert_eq!(list["unknown"], json!([]));
    let actions = list["actions"].as_array().map_or(0, Vec::len);
    assert!(actions >= 40, "{actions}");
}

#[test]
fn setting_a_chord_changes_the_list_the_settings_and_tells_the_ui() {
    let h = H::start(test_app(), |_| {});
    let events = h.engine.subscribe();
    let set = h.ok("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+P"]}));
    assert_eq!(set.data["chords"], json!(["Ctrl+P"]));
    assert!(set.message.contains("Ctrl+P"), "{}", set.message);

    let list = h.ok("keymap_list", json!({})).data;
    assert_eq!(chords_of(&list, "ui.palette"), ["Ctrl+P"]);
    let shown = h.ok("settings_get", json!({"key": "Keymap"})).data;
    assert_eq!(shown["value"], json!({"ui.palette": ["Ctrl+P"]}));

    let mut told = false;
    while let Ok(e) = events.try_recv() {
        if let Event::SettingChanged { key, value } = e {
            told |= key == "Keymap" && value == json!({"ui.palette": ["Ctrl+P"]});
        }
    }
    assert!(told, "the UI is told the new map");
}

#[test]
fn a_chord_another_action_uses_is_refused_with_the_owner_named() {
    let h = H::start(test_app(), |_| {});
    let err = h.ui("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+T"]})).err();
    match err {
        Some(ToolError::Refused(m)) => assert!(m.contains("New session"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Nothing changed.
    assert_eq!(chords_of(&h.ok("keymap_list", json!({})).data, "ui.palette"), ["Ctrl+K"]);
}

#[test]
fn bad_requests_are_refused_with_the_reason() {
    let h = H::start(test_app(), |_| {});
    assert!(matches!(h.ui("keymap_set", json!({"action": "no.such", "chords": []})), Err(ToolError::NotFound(_))));
    assert!(matches!(h.ui("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+Nope"]})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("keymap_set", json!({"action": "ui.palette", "chords": ["T"]})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("keymap_reset", json!({"action": "no.such"})), Err(ToolError::NotFound(_))));
    // The settings are not a back door to the map.
    assert!(matches!(h.ui("settings_set", json!({"key": "Keymap", "value": {}})), Err(ToolError::Refused(_))));
}

#[test]
fn an_empty_list_removes_the_chords_and_reset_brings_them_back() {
    let h = H::start(test_app(), |_| {});
    h.ok("keymap_set", json!({"action": "session_new", "chords": []}));
    assert!(chords_of(&h.ok("keymap_list", json!({})).data, "session_new").is_empty());
    h.ok("keymap_reset", json!({"action": "session_new"}));
    assert_eq!(chords_of(&h.ok("keymap_list", json!({})).data, "session_new"), ["Ctrl+T"]);

    h.ok("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+P"]}));
    h.ok("keymap_set", json!({"action": "ui.help", "chords": ["F3"]}));
    h.ok("keymap_reset", json!({}));
    let shown = h.ok("settings_get", json!({"key": "Keymap"})).data;
    assert_eq!(shown["value"], json!({}), "every override is gone");
    assert_eq!(chords_of(&h.ok("keymap_list", json!({})).data, "ui.help"), ["F1", "Ctrl+/"]);
}

#[test]
fn setting_the_defaults_leaves_no_override_behind() {
    let h = H::start(test_app(), |_| {});
    h.ok("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+P"]}));
    h.ok("keymap_set", json!({"action": "ui.palette", "chords": ["Ctrl+K"]}));
    let shown = h.ok("settings_get", json!({"key": "Keymap"})).data;
    assert_eq!(shown["value"], json!({}));
}

#[test]
fn damaged_and_unknown_entries_in_the_settings_are_reported_and_the_rest_works() {
    let mut app = test_app();
    app.settings.keymap = csm_core::keymap::overrides_from_value(&json!({
        "ui.palette": ["Ctrl+P", "T", "Ctrl+Hyper+X"],
        "session_new": "Ctrl+N",
        "from.a.newer.build": ["Ctrl+G"],
    }));
    let h = H::start(app, |_| {});
    let list = h.ok("keymap_list", json!({})).data;
    assert_eq!(chords_of(&list, "ui.palette"), ["Ctrl+P"]);
    assert_eq!(chords_of(&list, "session_new"), ["Ctrl+T"], "a damaged entry keeps the default");
    assert_eq!(list["unknown"], json!(["from.a.newer.build"]));
    assert_eq!(list["skipped"].as_array().map_or(0, Vec::len), 2, "{list}");
}

#[test]
fn a_saved_override_is_read_back_at_start() {
    let mut app = test_app();
    app.settings.keymap = csm_core::keymap::overrides_from_value(&json!({"ui.palette": ["Ctrl+P"]}));
    let h = H::start(app, |_| {});
    assert_eq!(chords_of(&h.ok("keymap_list", json!({})).data, "ui.palette"), ["Ctrl+P"]);
}
