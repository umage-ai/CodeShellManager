//! The `qr` node and the add-on status dot, through the real UI (egui_kittest, AccessKit, the demo
//! backend): a screen with a code and a refused text, the title-bar dot for the worst `csm/status`,
//! its absence when every status is idle or none is set, and that a click on the dot opens
//! Settings > Add-ons.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use csm_core::addons::screen::{ScreenScope, ScreenTree};
use csm_core::addons::status::{AddonStatus, Tone};
use csm_engine::{DeclaredScreen, PluginRuntime, RunState, ScreenInfo, ScreenPhase};
use csm_ui::app::SettingsSection;
use csm_ui::demo::{DemoBackend, DemoScreens};
use csm_ui::terminal::lock;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

type H = Harness<'static, csm_ui::CsmApp>;
type Handle = Arc<Mutex<DemoScreens>>;

fn status(tone: Tone, text: &str) -> AddonStatus {
    AddonStatus { text: text.into(), tone }
}

/// A demo backend whose one add-on is running and has set `status` (or none).
fn backend_with_status(statuses: &[Option<AddonStatus>]) -> DemoBackend {
    let mut b = backend(ThemeKind::Mocha);
    let addons = statuses
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut a = csm_ui::demo::demo_claude_usage(true);
            a.id = format!("status-test-{i}");
            let mut rt = PluginRuntime::starting();
            rt.state = RunState::Running;
            rt.status = s.clone();
            a.process = Some(rt);
            a
        })
        .collect();
    b.set_addons(addons);
    b.publish();
    b
}

fn info() -> ScreenInfo {
    ScreenInfo {
        instance: "i1".into(),
        addon: "echo-test".into(),
        addon_name: "Echo test plugin".into(),
        screen: "echo-test.main".into(),
        title: "Pairing".into(),
        icon: "layers".into(),
        scope: ScreenScope::Global,
        phase: ScreenPhase::Ready,
        popped_out: false,
        error: None,
        rev: 1,
        context: Default::default(),
    }
}

/// An app with one open screen showing `root` (the qr tests' screen).
fn app_with_screen(root: Value) -> (H, Handle) {
    let b = backend(ThemeKind::Mocha);
    let handle = b.screens_handle();
    {
        let mut g = handle.lock().unwrap();
        g.declared = vec![DeclaredScreen {
            addon: "echo-test".into(),
            addon_name: "Echo test plugin".into(),
            id: "echo-test.main".into(),
            title: "Pairing".into(),
            icon: "layers".into(),
            scope: ScreenScope::Global,
            summary: String::new(),
        }];
        g.instances = vec![info()];
        g.trees.insert("i1".into(), Arc::new(ScreenTree::from_render(&json!({"rev": 1, "root": root}), "echo-test", None).unwrap_or_else(|e| panic!("{e}"))));
    }
    let mut h = harness(app_with(b));
    settle(&mut h);
    lock(&h.state().ui.screens).active = Some("i1".into());
    settle(&mut h);
    (h, handle)
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

#[test]
fn a_qr_node_draws_its_code_and_a_refused_text_draws_a_notice_beside_the_rest() {
    let (mut h, _handle) = app_with_screen(json!({"type": "column", "children": [
        {"type": "qr", "text": "https://phone.example.org/v/0.1.0/#p=abc", "label": "Scan with the phone", "size": "lg"},
        {"type": "qr", "text": "bad\u{7}text", "label": "Refused code"},
        {"type": "text", "text": "The link stays as text: https://phone.example.org/v/0.1.0/#p=abc"}
    ]}));
    settle(&mut h);
    assert!(has(&h, "Refused code"), "the refused node keeps its label under the notice");
    assert!(has(&h, "This code cannot be shown."), "the refused text draws its notice");
    assert!(has(&h, "The link stays as text: https://phone.example.org/v/0.1.0/#p=abc"), "the rest of the screen is drawn");
}

#[test]
fn the_title_dot_shows_the_worst_status_and_a_click_opens_settings_on_add_ons() {
    let b = backend_with_status(&[Some(status(Tone::Ok, "Connected")), Some(status(Tone::Error, "Relay down")), Some(status(Tone::Warn, "Slow"))]);
    let mut h = harness(app_with(b));
    settle(&mut h);
    assert!(has(&h, "Add-on status: Relay down"), "the dot names the worst text");
    assert!(!has(&h, "Add-on status: Connected"), "only the worst one is the dot");
    h.get_by_label("Add-on status: Relay down").click();
    settle(&mut h);
    assert!(h.state().ui.settings.open, "the dot opens Settings");
    assert!(matches!(h.state().ui.settings.section, SettingsSection::Addons), "on the Add-ons section");
}

#[test]
fn no_dot_when_no_status_is_set_or_every_status_is_idle() {
    for statuses in [vec![None], vec![Some(status(Tone::Idle, "Quiet"))]] {
        let mut h = harness(app_with(backend_with_status(&statuses)));
        settle(&mut h);
        assert!(!has(&h, "Add-on status: Quiet"), "idle draws no dot");
    }
}
