//! Settings > Remote access and the title bar indicator, on the demo peer host (no socket): off by
//! default with no indicator, the enable walk-through, pairing Allow and Deny, the indicator's
//! states, revoke, and that the tools which open a way in stay refused for every caller but this
//! window.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_engine::Caller;
use csm_ui::demo::peer::DemoPeerHost;
use csm_ui::screens::remote::{PillKind, RELAY_TOGGLE_LABEL, Secret, TOGGLE_LABEL};
use csm_ui::theme::ThemeKind;
use csm_ui::{Action, CsmApp};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

type H = Harness<'static, CsmApp>;

fn rig(host: &DemoPeerHost) -> H {
    let mut h = harness(app_with_peer(backend(ThemeKind::Mocha), host));
    settle(&mut h);
    h
}

fn host() -> DemoPeerHost {
    DemoPeerHost::new(NOW_MS / 1000)
}

fn open_page(h: &mut H) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenRemoteAccess);
    wait_for(h, "the page's reads", |a| a.ui.remote.status.available && !a.ui.remote.interfaces.is_empty());
}

/// The indicator's accessible name, when it is in the title bar.
fn indicator(h: &H) -> Option<String> {
    h.query_all_by_role(egui::accesskit::Role::Button).filter_map(|n| n.accesskit_node().label()).find(|l| l.starts_with("Remote access: "))
}

fn peer_calls(h: &H, id: &str) -> usize {
    h.state().submitted().iter().filter(|c| c.id == id).count()
}

/// The walk-through, to the end: the WireGuard address as it is pre-selected.
fn turn_on(h: &mut H) {
    open_page(h);
    h.get_by_label(TOGGLE_LABEL).click();
    settle(h);
    h.get_by_label("Next").click();
    settle(h);
    h.get_by_label("Turn on and pair a device").click();
    wait_for(h, "listening with an open link", |a| a.ui.remote.status.enabled && a.ui.remote.link.is_some());
}

#[test]
fn off_by_default_nothing_is_called_and_nothing_is_shown() {
    let host = host();
    let mut h = rig(&host);
    assert!(!h.state().ui.remote.status.enabled && h.state().ui.remote.status.available);
    assert_eq!(indicator(&h), None, "no indicator while it is off");
    // Two seconds of frames with the page closed: only the one read at start-up, no polling.
    for _ in 0..150 {
        h.step();
    }
    assert_eq!(peer_calls(&h, "peer_status"), 1);
    assert_eq!(peer_calls(&h, "peer_device_list"), 0);
    assert!(host.calls().is_empty(), "the host was asked to do nothing: {:?}", host.calls());

    // The page: off, one switch, and no way in on its own.
    open_page(&mut h);
    assert!(h.query_by_label(TOGGLE_LABEL).is_some());
    assert!(h.query_by_label("Turn off remote access").is_none());
    assert!(h.query_by_label("Pair a device\u{2026}").is_none(), "pairing needs it to be on");
    assert!(host.calls().is_empty());
    for c in h.state().submitted() {
        assert!(!matches!(c.id.as_str(), "peer_server_enable" | "peer_pair_start" | "peer_pair_answer"), "{c:?}");
    }
}

#[test]
fn the_enable_walk_through_picks_the_wireguard_address_and_shows_the_firewall_rule() {
    let host = host();
    let mut h = rig(&host);
    open_page(&mut h);
    h.get_by_label(TOGGLE_LABEL).click();
    settle(&mut h);

    // Step 1: this machine's addresses, WireGuard marked and pre-selected, no wildcard row.
    let wg = h.get_by_label("Address: 10.8.0.2 (wg)");
    assert_eq!(wg.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "the WireGuard address is pre-selected");
    assert!(h.query_by_label("Address: 192.168.1.20 (Ethernet)").is_some());
    assert!(h.query_all_by_label_contains("0.0.0.0").next().is_none(), "no wildcard in the list");
    assert!(h.state().ui.remote.setup.as_ref().is_some_and(|s| !s.all_interfaces));
    assert!(host.calls().is_empty(), "nothing is turned on by opening the walk-through");

    // Pick the other address and a port, then on to the firewall.
    h.get_by_label("Address: 192.168.1.20 (Ethernet)").click();
    settle(&mut h);
    h.get_by_label("Remote access port").click();
    h.get_by_label("Remote access port").type_text("48000");
    settle(&mut h);
    h.get_by_label("Next").click();
    settle(&mut h);
    assert!(host.calls().is_empty(), "still nothing: the firewall step comes first");
    assert!(h.query_all_by_label_contains("48000").next().is_some(), "the rule names the port");
    assert!(h.query_all_by_label_contains("192.168.1.0/24").next().is_some(), "and the subnet it comes from");

    // Back keeps the choice; then turn on.
    h.get_by_label("Back").click();
    settle(&mut h);
    assert_eq!(h.get_by_label("Address: 192.168.1.20 (Ethernet)").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
    h.get_by_label("Next").click();
    settle(&mut h);
    h.get_by_label("Turn on and pair a device").click();
    wait_for(&mut h, "listening with an open link", |a| a.ui.remote.status.enabled && a.ui.remote.link.is_some());
    assert_eq!(host.calls(), ["enable 192.168.1.20:48000 by ui", "pair_start by ui"]);
    let link = h.state().ui.remote.link.clone().unwrap();
    assert!(link.starts_with("csm://pair#"));
    assert!(h.state().ui.remote.setup.is_none(), "the walk-through closed");
    assert!(h.query_by_label("Pairing QR code").is_some(), "the QR code is on the page");
    assert!(h.query_by_label("Copy link").is_some());
    assert_eq!(indicator(&h).as_deref(), Some("Remote access: Remote access on \u{b7} 192.168.1.20:48000"));
}

#[test]
fn a_bad_port_cannot_go_on_and_cancel_leaves_it_off() {
    let host = host();
    let mut h = rig(&host);
    open_page(&mut h);
    h.get_by_label(TOGGLE_LABEL).click();
    settle(&mut h);
    h.get_by_label("Remote access port").click();
    h.get_by_label("Remote access port").type_text("99999");
    settle(&mut h);
    h.get_by_label("Next").click();
    settle(&mut h);
    assert_eq!(h.state().ui.remote.setup.as_ref().map(|s| s.step), Some(csm_ui::screens::remote::Step::Address), "Next is disabled for a bad port");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(h.state().ui.remote.setup.is_none());
    assert!(host.calls().is_empty());
    assert_eq!(indicator(&h), None);
}

#[test]
fn pairing_allow_adds_the_device_and_deny_does_not() {
    let host = host();
    let mut h = rig(&host);
    turn_on(&mut h);

    // A device asks. The page polls, shows its name and the check code, and waits for the person.
    host.ask_to_pair("Laptop", "4821");
    wait_for(&mut h, "the question", |a| a.ui.remote.status.pending_pair.is_some());
    assert!(h.query_by_label("Check code").is_some());
    assert!(h.query_all_by_label_contains("Laptop wants to pair").next().is_some());
    assert_eq!(indicator(&h).as_deref(), Some("Remote access: Laptop wants to pair"), "the indicator asks too");
    assert_eq!(host.devices_len(), 0, "nothing is paired before Allow");

    // Deny: nobody is added.
    h.get_by_label("Deny").click();
    settle(&mut h);
    assert_eq!(host.devices_len(), 0);
    assert!(h.state().ui.remote.status.pending_pair.is_none());
    assert!(h.state().ui.remote.devices.is_empty());
    assert!(host.calls().iter().any(|c| c == "pair_answer allow=false by ui"));

    // Allow: the check code goes with it, and the device is on the list.
    host.ask_to_pair("Laptop", "7305");
    wait_for(&mut h, "the second question", |a| a.ui.remote.status.pending_pair.as_ref().is_some_and(|p| p.check_code == "7305"));
    h.get_by_label("Allow").click();
    wait_for(&mut h, "the device on the list", |a| a.ui.remote.devices.len() == 1);
    assert_eq!(host.devices_len(), 1);
    assert_eq!(h.state().ui.remote.devices[0].name, "Laptop");
    assert!(host.calls().iter().any(|c| c == "pair_answer allow=true by ui"));
    assert!(h.query_by_label("Revoke Laptop").is_some());
    assert_eq!(h.state().ui.remote.link, None, "the used link is gone");
}

#[test]
fn the_indicator_shows_serving_then_connected_and_a_click_opens_the_page() {
    let host = host().with_device("Laptop", 3 * 86_400);
    let mut h = rig(&host);
    turn_on(&mut h);
    let ui = &h.state().ui;
    let p = csm_ui::screens::remote::pill(&ui.remote.status, &ui.remote.devices).unwrap();
    assert_eq!(p.kind, PillKind::Serving);
    assert!(indicator(&h).unwrap().ends_with("10.8.0.2:47300"));

    host.set_connected("Laptop", true);
    wait_for(&mut h, "connected", |a| a.ui.remote.status.connected == 1);
    assert_eq!(indicator(&h).as_deref(), Some("Remote access: Laptop connected"));
    let ui = &h.state().ui;
    assert_eq!(csm_ui::screens::remote::pill(&ui.remote.status, &ui.remote.devices).unwrap().kind, PillKind::Connected);

    // From any other page, a click on it opens Settings on Remote access.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::CloseSettings);
    settle(&mut h);
    assert!(!h.state().ui.settings.open);
    h.get_by_label("Remote access: Laptop connected").click();
    settle(&mut h);
    assert!(h.state().ui.settings.open);
    assert_eq!(h.state().ui.settings.section, csm_ui::app::SettingsSection::Remote);
    assert!(h.query_by_label("Revoke Laptop").is_some());

    host.set_connected("Laptop", false);
    wait_for(&mut h, "disconnected", |a| a.ui.remote.status.connected == 0);
    assert!(indicator(&h).unwrap().contains("Remote access on"));
}

#[test]
fn one_click_turns_it_off_and_the_indicator_and_the_link_go() {
    let host = host();
    let mut h = rig(&host);
    turn_on(&mut h);
    assert!(indicator(&h).is_some());
    h.get_by_label(TOGGLE_LABEL).click();
    wait_for(&mut h, "off", |a| !a.ui.remote.status.enabled);
    assert!(host.calls().iter().any(|c| c == "disable by ui"));
    assert_eq!(indicator(&h), None);
    assert_eq!(h.state().ui.remote.link, None, "the pairing link is dropped with it");
    assert!(h.query_by_label("Turn off remote access").is_none());

    // And the other button does the same.
    turn_on(&mut h);
    h.get_by_label("Turn off remote access").click();
    wait_for(&mut h, "off again", |a| !a.ui.remote.status.enabled);
    assert_eq!(host.calls().iter().filter(|c| c.starts_with("disable")).count(), 2);
}

#[test]
fn revoke_one_and_revoke_all() {
    let host = host().with_device("Laptop", 86_400).with_device("Phone", 7_200).with_device("Tablet", 600);
    let mut h = rig(&host);
    open_page(&mut h);
    wait_for(&mut h, "three devices", |a| a.ui.remote.devices.len() == 3);
    // Listed while it is off, with their ages.
    assert!(h.query_by_label("Revoke Laptop").is_some() && h.query_by_label("Revoke Phone").is_some());

    h.get_by_label("Revoke Phone").click();
    wait_for(&mut h, "two devices", |a| a.ui.remote.devices.len() == 2);
    assert!(h.query_by_label("Revoke Phone").is_none() && h.query_by_label("Revoke Laptop").is_some());
    assert!(host.calls().iter().any(|c| c.starts_with("revoke One(") && c.ends_with("by ui")));

    // Revoke all asks first; Cancel changes nothing.
    h.get_by_label("Revoke all").click();
    settle(&mut h);
    assert!(h.query_all_by_label_contains("Revoke every device?").next().is_some(), "a question first");
    assert_eq!(host.devices_len(), 2);
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(host.devices_len(), 2);
    h.get_by_label("Revoke all").click();
    settle(&mut h);
    h.get_all_by_label("Revoke all").last().unwrap().click();
    wait_for(&mut h, "none left", |a| a.ui.remote.devices.is_empty());
    assert_eq!(host.devices_len(), 0);
    assert!(host.calls().iter().any(|c| c == "revoke All by ui"));
    assert!(h.query_by_label("Revoke Laptop").is_none() && h.query_all_by_label("Revoke all").next().is_none(), "the list is gone with its last device while it is off");
}

#[test]
fn the_tools_that_open_a_way_in_stay_refused_for_everyone_but_this_window() {
    let host = host();
    let h = rig(&host);
    let engine = h.state().dispatch.engine().expect("the demo has an engine");
    let callers = [Caller::Cli, Caller::External { client: "claude".into(), session_id: None }];
    for caller in callers {
        for (tool, params) in [
            ("peer_server_enable", json!({"address": "10.8.0.2"})),
            ("peer_pair_start", json!({})),
            ("peer_pair_answer", json!({"allow": true, "check_code": "0000"})),
        ] {
            let reply = engine.submit(caller.clone(), tool, params).recv_timeout(std::time::Duration::from_secs(5)).expect("an answer");
            assert!(matches!(reply, Err(csm_engine::ToolError::Refused(_))), "{tool} for {caller:?}: {reply:?}");
        }
    }
    assert!(host.calls().is_empty(), "the host never saw them: {:?}", host.calls());
    // Turning it off is allowed for everyone.
    let off = engine.submit(Caller::Cli, "peer_server_disable", json!({})).recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(off.is_ok());
}

#[test]
fn the_palette_sends_the_enabling_commands_to_the_page() {
    let host = host();
    let mut h = rig(&host);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
    h.state_mut().ui.palette.query = "Turn on remote".into();
    settle(&mut h);
    h.get_by_label("Palette: Turn on remote access\u{2026}").click();
    settle(&mut h);
    assert!(h.state().ui.settings.open);
    assert_eq!(h.state().ui.settings.section, csm_ui::app::SettingsSection::Remote);
    assert!(host.calls().is_empty(), "the palette opens the page; it does not turn anything on");
    assert_eq!(peer_calls(&h, "peer_server_enable"), 0);
}

/// The relay card: the toggle opens the fields, the token is a password that leaves the page state
/// once sent, the engine gets it (the demo host records only that one was given), no log or call
/// history keeps it, and a refused address shows on the card while the saved one stays.
#[test]
fn the_relay_card_sends_the_token_once_and_keeps_it_out_of_the_logs() {
    const TOKEN: &str = "hunter2-enrolment-token";
    let host = host();
    let mut h = rig(&host);
    open_page(&mut h);
    assert!(h.state().ui.remote.relay.is_none(), "no relay is set by default");

    h.get_by_label(RELAY_TOGGLE_LABEL).click();
    settle(&mut h);
    assert!(h.state().ui.remote.relay_form.editing, "the toggle opens the fields");
    assert!(h.query_by_label("Relay enrolment token").is_some(), "the token field is there");
    assert!(host.calls().is_empty(), "opening the fields calls nothing");

    {
        let form = &mut h.state_mut().ui.remote.relay_form;
        form.url = "wss://relay.example".into();
        form.token = Secret(TOKEN.into());
    }
    settle(&mut h);
    h.get_by_label("Save relay").click();
    wait_for(&mut h, "the relay is saved", |a| a.ui.remote.relay.is_some());

    let relay = h.state().ui.remote.relay.clone().unwrap();
    assert_eq!((relay.url.as_str(), relay.access_set, relay.connected), ("wss://relay.example", true, false), "saved, and off while remote access is off");
    assert!(h.state().ui.remote.relay_form.token.0.is_empty(), "the token leaves the page state once sent");
    assert!(!h.state().ui.remote.relay_form.editing, "the fields close after saving");
    assert_eq!(host.calls().iter().filter(|c| c.starts_with("relay_set url=wss://relay.example access=true")).count(), 1, "{:?}", host.calls());
    assert!(host.calls().iter().all(|c| !c.contains(TOKEN)), "the host never sees the token in its calls: {:?}", host.calls());

    // Neither the call history nor the debug view of the page state keeps the token.
    let saved_call = h.state().submitted().iter().find(|c| c.id == "peer_relay_set").cloned().expect("the call was made");
    assert_eq!(saved_call.params["access"], "<redacted>");
    assert!(!format!("{:?}", h.state().submitted()).contains(TOKEN));
    assert!(!format!("{:?}", h.state().ui.remote).contains(TOKEN));

    // A refused address shows on the card; the saved relay is not changed by it.
    {
        let form = &mut h.state_mut().ui.remote.relay_form;
        form.editing = true;
        form.url = "http://relay.example".into();
    }
    settle(&mut h);
    h.get_by_label("Save relay").click();
    wait_for(&mut h, "the refusal is shown", |a| a.ui.remote.relay_error.is_some());
    assert_eq!(h.state().ui.remote.relay.as_ref().map(|r| r.url.as_str()), Some("wss://relay.example"), "the saved relay stays");

    // Turning it off removes the address and the token, and the card goes back to its toggle.
    h.state_mut().ui.remote.relay_form = Default::default();
    settle(&mut h);
    h.get_by_label("Also reachable through a relay").click();
    wait_for(&mut h, "the relay is off", |a| a.ui.remote.relay.is_none());
    assert!(host.calls().iter().any(|c| c.starts_with("relay_set url= access=false")), "{:?}", host.calls());
}
