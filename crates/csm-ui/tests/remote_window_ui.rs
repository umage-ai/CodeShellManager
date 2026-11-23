//! A remote window (`docs/design/remote-csm.md`, section 6, P1 step 7), on the demo backend standing
//! for a remote host (no socket): the link strip in each state, the local-only items hidden and
//! refused, Quit that disconnects and the office's own quit that asks first, links that open on the
//! laptop (a loopback one with a warning), and the connect dialog's states.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use csm_ui::app::{Action, Overlay};
use csm_ui::remote_window::{ConnectDialog, HostRow, RemoteLink, RemoteStatus};
use csm_ui::screens::menus::{self, hidden_in_remote};
use csm_ui::screens::palette::{self, PalRow};
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, csm_ui::CsmApp>;

fn status(link: RemoteLink) -> RemoteStatus {
    RemoteStatus { host_name: "OFFICE".into(), address: "10.8.0.2:47300".into(), link }
}

/// A remote window in this link state, with the strip and the local-only items as they are there.
fn remote(link: RemoteLink) -> H {
    let mut b = backend(ThemeKind::Mocha);
    b.set_remote(Some(status(link)));
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

fn local() -> H {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn run(h: &mut H, action: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, action);
    settle(h);
}

fn submitted_ids(h: &H) -> Vec<String> {
    h.state().submitted().iter().map(|c| c.id.clone()).collect()
}

fn tool_ids(rows: &[PalRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            PalRow::Tool { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

type Seen = Arc<Mutex<Vec<String>>>;
type Hook = Arc<dyn Fn(String) + Send + Sync>;

/// A recorder for the hook a remote window calls (the browser, a new window).
fn recorder() -> (Seen, Hook) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    (seen, Arc::new(move |s: String| rec.lock().unwrap().push(s)))
}

#[test]
fn the_strip_names_the_host_and_the_address_in_every_link_state() {
    let cases = [
        (RemoteLink::Connected, "Connected to OFFICE · 10.8.0.2:47300"),
        (RemoteLink::Connecting, "Connecting to OFFICE · 10.8.0.2:47300"),
        (RemoteLink::Offline, "OFFICE offline · reconnecting · 10.8.0.2:47300"),
        (RemoteLink::Refused("revoked".into()), "OFFICE refused this laptop: revoked"),
    ];
    for (link, text) in cases {
        let mut h = remote(link);
        assert!(h.query_by_label(text).is_some(), "the strip reads `{text}`");
        h.step();
    }
    let h = local();
    assert!(h.query_by_label("Connected to OFFICE · 10.8.0.2:47300").is_none(), "a local window has no strip");
}

#[test]
fn the_local_only_items_are_not_offered_by_a_remote_window() {
    let h = remote(RemoteLink::Connected);
    let rows = palette::rows(h.state(), "");
    let ids = tool_ids(&rows);
    for id in ["open_folder", "open_with", "state_export", "state_import", "peer_connect_open", "app_quit"] {
        assert!(!ids.iter().any(|i| i == id), "`{id}` is hidden in a remote window: {ids:?}");
    }
    assert!(ids.iter().any(|i| i == "remote_disconnect") && ids.iter().any(|i| i == "remote_quit_office"), "{ids:?}");
    let all = tool_ids(&palette::rows(h.state(), ">"));
    assert!(all.iter().any(|i| i == "session_list"), "the office's own tools are still listed");
    assert!(!all.iter().any(|i| i == "open_folder" || i == "app_quit"), "and the laptop's are not, even as commands");

    let l = local();
    let ids = tool_ids(&palette::rows(l.state(), ">"));
    assert!(ids.iter().any(|i| i == "app_quit") && ids.iter().any(|i| i == "peer_connect_open"), "the laptop lists them");
    assert!(!ids.iter().any(|i| i == "remote_disconnect"), "the laptop has no Disconnect");

    // The menus: what the laptop would offer for a session, and which of it a remote window drops.
    let sid = h.state().model.sessions.first().expect("the demo has sessions").id.clone();
    let entries = menus::pane_menu(&h.state().model, &sid);
    let hidden: Vec<&menus::Entry> = entries.iter().filter(|e| hidden_in_remote(e)).collect();
    assert!(!hidden.is_empty(), "the pane menu has laptop-only items (open folder, open with)");
    assert!(
        hidden.iter().all(|e| matches!(e, menus::Entry::Item { label, .. } | menus::Entry::ItemTip { label, .. } | menus::Entry::Sub { label, .. } if !label.is_empty())),
        "every hidden entry is a named item"
    );
}

#[test]
fn a_refused_local_action_never_reaches_the_office() {
    let mut h = remote(RemoteLink::Connected);
    let sid = h.state().model.sessions.first().expect("sessions").id.clone();
    run(&mut h, Action::Tool("open_folder", json!({"session_id": sid})));
    run(&mut h, Action::ToolNamed("open_with".into(), json!({"session_id": sid})));
    run(&mut h, Action::OpenWithEditor);
    run(&mut h, Action::Tool("state_export", json!({"path": "x.csm"})));
    run(&mut h, Action::PaletteTool("state_import".into()));
    run(&mut h, Action::OpenConnectRemote);
    let ids = submitted_ids(&h);
    for id in ["open_folder", "open_with", "state_export", "state_import", "peer_connect_open"] {
        assert!(!ids.iter().any(|i| i == id), "`{id}` was not sent: {ids:?}");
    }
    assert!(h.state().ui.overlay != Overlay::ConnectRemote, "the connect dialog does not open in a remote window");
}

#[test]
fn quit_in_a_remote_window_disconnects_and_never_quits_the_office() {
    let mut h = remote(RemoteLink::Connected);
    run(&mut h, Action::AskQuit);
    assert!(h.state().disconnect, "Quit disconnects");
    assert!(!submitted_ids(&h).iter().any(|i| i == "app_quit"), "the office is not asked to quit");

    let mut h = remote(RemoteLink::Connected);
    run(&mut h, Action::Tool("app_quit", json!({})));
    assert!(h.state().disconnect);
    assert!(!submitted_ids(&h).iter().any(|i| i == "app_quit"));

    // The office's own quit is a separate, confirmed item.
    run(&mut h, Action::RemoteQuitOffice);
    let c = h.state().ui.confirm.clone().expect("the office quit asks first");
    assert_eq!(c.title, "Quit CodeShellManager on OFFICE?");
    assert!(!submitted_ids(&h).iter().any(|i| i == "app_quit"), "nothing is sent before the answer");
    run(&mut h, *c.yes);
    assert!(submitted_ids(&h).iter().any(|i| i == "app_quit"), "the confirmed answer asks the office to quit");
}

#[test]
fn a_local_window_quit_is_not_a_disconnect() {
    let mut h = local();
    run(&mut h, Action::AskQuit);
    assert!(!h.state().disconnect, "the laptop's own quit is not a disconnect");
}

#[test]
fn links_open_on_the_laptop_and_a_loopback_link_says_it_points_to_the_office() {
    let (opened, open) = recorder();
    let mut b = backend(ThemeKind::Mocha);
    b.set_remote(Some(status(RemoteLink::Connected)));
    let mut app = app_with(b);
    app.hooks.open_link = open;
    let mut h = harness(app);
    settle(&mut h);

    run(&mut h, Action::Tool("open_url", json!({"url": "https://example.com/docs"})));
    assert_eq!(opened.lock().unwrap().as_slice(), ["https://example.com/docs"], "an https link opens on the laptop");
    assert!(!submitted_ids(&h).iter().any(|i| i == "open_url"), "the office does not open it");

    run(&mut h, Action::Tool("open_url", json!({"url": "http://localhost:3000/"})));
    assert_eq!(opened.lock().unwrap().len(), 1, "a loopback link asks first");
    let c = h.state().ui.confirm.clone().expect("the warning");
    assert_eq!(c.title, "This link points to OFFICE");
    assert!(c.body.contains("OFFICE's own machine"));
    run(&mut h, *c.yes);
    assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some("http://localhost:3000/"), "confirmed: it opens on the laptop");

    run(&mut h, Action::Tool("open_url", json!({"url": "file:///C:/secret.txt"})));
    run(&mut h, Action::Tool("open_url", json!({"url": "javascript:alert(1)"})));
    assert_eq!(opened.lock().unwrap().len(), 2, "only http and https open");
    h.step();
}

#[test]
fn the_connect_dialog_follows_the_pairing_states() {
    let mut h = local();
    run(&mut h, Action::OpenConnectRemote);
    assert!(h.state().ui.overlay == Overlay::ConnectRemote);
    assert!(h.state().submitted().iter().any(|c| c.id == "peer_hosts_list"), "the paired offices are read");
    h.step();

    // Pair is off without a link.
    let before = submitted_ids(&h).len();
    h.get_by_label("Pair").click();
    settle(&mut h);
    assert!(!submitted_ids(&h)[before..].iter().any(|i| i == "peer_client_pair"), "no link, no pairing");

    // A pairing the engine refuses (no pairing client in the demo) shows why.
    h.state_mut().ui.connect.link = "csm://pair#demo".into();
    h.step();
    h.get_by_label("Pair").click();
    wait_for(&mut h, "the refusal", |a| matches!(a.ui.connect.phase, csm_ui::remote_window::PairPhase::Failed(_)));
    h.step();

    // The check code, allowed, cancelled and busy states draw.
    for answer in [
        json!({"state": "connecting"}),
        json!({"state": "check_code", "check_code": "4821"}),
        json!({"state": "paired", "host_name": "OFFICE"}),
        json!({"state": "cancelled"}),
    ] {
        h.state_mut().ui.connect.apply_status(&answer);
        h.step();
    }
    h.state_mut().ui.connect.apply_status(&json!({"state": "check_code", "check_code": "4821"}));
    h.step();
    assert!(h.query_by_label("4821").is_some(), "the check code is shown to compare");
    assert!(h.query_by_label("Cancel pairing").is_some(), "a running pairing can be cancelled");
}

#[test]
fn the_paired_offices_connect_forget_and_open_a_remote_window() {
    let (opened, open) = recorder();
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.hooks.open_window = Some(open);
    let mut h = harness(app);
    settle(&mut h);
    run(&mut h, Action::OpenConnectRemote);
    h.state_mut().ui.connect = ConnectDialog {
        hosts: vec![HostRow { host_id: "HOST1".into(), name: "Office".into(), address: "10.8.0.2:47300".into(), last_connected: Some(5) }],
        hosts_loaded: true,
        ..ConnectDialog::default()
    };
    h.step();
    assert!(h.query_by_label("Office").is_some(), "the office is listed");
    h.get_by_label("Connect").click();
    settle(&mut h);
    assert_eq!(opened.lock().unwrap().as_slice(), ["HOST1"], "Connect starts a remote window to that host");

    run(&mut h, Action::OpenConnectRemote);
    h.state_mut().ui.connect.hosts = vec![HostRow { host_id: "HOST1".into(), name: "Office".into(), address: String::new(), last_connected: None }];
    h.state_mut().ui.connect.hosts_loaded = true;
    h.step();
    h.get_by_label("Forget").click();
    settle(&mut h);
    let c = h.state().ui.confirm.clone().expect("forgetting asks first");
    assert_eq!(c.title, "Forget Office?");
    assert!(!submitted_ids(&h).iter().any(|i| i == "peer_host_forget"), "nothing is forgotten before the answer");
    run(&mut h, *c.yes);
    assert!(submitted_ids(&h).iter().any(|i| i == "peer_host_forget"));
}

#[test]
fn an_opened_remote_window_needs_a_host_to_open() {
    // The laptop's window without the hook: the dialog says it cannot open one (no panic, no process).
    let mut h = local();
    run(&mut h, Action::ConnectRemoteOpen { host_id: "HOST1".into(), name: "Office".into() });
    assert!(h.state().hooks.open_window.is_none());
    h.step();
}

// ── Close, the panes the frame draws, and the stale mark ─────────────────────────────────────

/// The viewport commands of the last frame.
fn viewport_commands(h: &H) -> Vec<egui::ViewportCommand> {
    h.output().viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

/// What eframe delivers for a close (the title bar's ✕, Alt+F4), and again while it is pending.
fn deliver_close(h: &mut H) {
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
}

#[test]
fn a_remote_windows_close_disconnects_and_is_never_cancelled() {
    let mut h = remote(RemoteLink::Connected);
    deliver_close(&mut h);
    assert!(h.state().disconnect, "the close disconnects the window");
    // A CancelClose would be re-queued by the event loop every pass, and the window would never go.
    assert!(!viewport_commands(&h).contains(&egui::ViewportCommand::CancelClose), "{:?}", viewport_commands(&h));
    for _ in 0..3 {
        deliver_close(&mut h);
    }
    assert!(!viewport_commands(&h).contains(&egui::ViewportCommand::CancelClose), "{:?}", viewport_commands(&h));
    assert!(!submitted_ids(&h).iter().any(|id| id == "app_quit"), "a close never quits the office");
}

#[test]
fn a_remote_windows_quit_disconnects_and_the_close_after_it_is_not_cancelled() {
    let mut h = remote(RemoteLink::Connected);
    run(&mut h, Action::AskQuit);
    assert!(h.state().disconnect, "Quit disconnects");
    deliver_close(&mut h);
    assert!(!viewport_commands(&h).contains(&egui::ViewportCommand::CancelClose), "{:?}", viewport_commands(&h));
    assert!(!submitted_ids(&h).iter().any(|id| id == "app_quit"), "Quit never quits the office");
}

#[test]
fn every_frame_reports_the_panes_it_drew_and_the_focused_one() {
    let b = backend(ThemeKind::Mocha);
    let log = b.panes_log();
    let mut h = harness(app_with(b));
    settle(&mut h);
    let (shown, driver) = log.lock().unwrap().clone().expect("a frame reported its panes");
    assert!(!shown.is_empty(), "the workspace shows panes");
    assert_eq!(driver.as_deref(), h.state().model.active.as_deref(), "the focused pane is the driver");
    for id in &shown {
        assert!(h.state().model.session(id).is_some_and(|s| s.status.is_live()), "only live sessions are subscribed: {id}");
    }
    // The board draws cards, not panes: a remote window subscribes nothing.
    run(&mut h, Action::Tool("layout_set", json!({"mode": "board"})));
    let (shown, _) = log.lock().unwrap().clone().expect("the board frame reported");
    assert!(shown.is_empty(), "the board shows no panes: {shown:?}");
}

#[test]
fn an_office_out_of_reach_marks_its_snapshot_stale() {
    assert!(remote(RemoteLink::Offline).state().model.stale, "reconnecting is stale");
    assert!(!remote(RemoteLink::Connected).state().model.stale, "a live window is not");
    assert!(!remote(RemoteLink::Connecting).state().model.stale, "nothing to be stale yet");
}
