//! Remote access: letting another CodeShellManager connect to this one.
//!
//! Two parts, both a view of what the `peer_*` tools say and a way to call them:
//!
//! - **Settings > Remote access** ([`settings`]): off by default. Turning it on walks through the
//!   address (this machine's interfaces, the WireGuard one marked and pre-selected, no wildcard
//!   unless it is chosen under Advanced), the port and the firewall rule, then switches it on
//!   (`peer_server_enable`) and opens a pairing offer (`peer_pair_start`): the link, a QR code, and
//!   the four-digit check code with Allow and Deny when a device asks (`peer_pair_answer`). Paired
//!   devices are listed with Revoke and Revoke all (`peer_device_revoke`). The toggle at the top
//!   turns it off in one click (`peer_server_disable`).
//! - **The title bar indicator** ([`indicator`]): shown only while it is on. Quiet blue while it
//!   serves, green with the device's name while a client is connected, amber while a device waits
//!   for Allow or Deny or the chosen address is missing. A click opens this page.
//!
//! Nothing here can turn the server on by itself: the enabling tools are refused by the engine for
//! every caller but this window (`csm-engine`'s `peer` precheck), and nothing is sent until the
//! person presses the button. State read from the tools lives in [`RemoteState`]; the engine is
//! polled once a second while the server is on or this page is open, and never otherwise.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{Action, ConfirmState, CsmApp, SettingsSection};
use crate::backend::Call;
use crate::screens::settings::{caption, section};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The port suggested when none is typed (the engine's own default).
pub const DEFAULT_PORT: u16 = 47_300;
/// How often the engine is asked while the server is on or the page is open.
const POLL_S: f64 = 1.0;

/// The toggle's label: also what the tests and screen readers find.
pub const TOGGLE_LABEL: &str = "Let another CodeShellManager connect to this one";

/// `peer_status`, as the page reads it. Every field has a default, so a newer engine can add more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct StatusView {
    pub available: bool,
    pub enabled: bool,
    pub listening: Option<String>,
    pub waiting: Option<String>,
    pub pairing_open_secs: Option<u64>,
    pub pending_pair: Option<PendingView>,
    pub devices: usize,
    pub connected: usize,
    pub private_files_restricted: bool,
}

/// A device that finished the pairing handshake and waits for Allow or Deny.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct PendingView {
    pub device_name: String,
    pub check_code: String,
    pub expires_in_secs: u64,
}

/// One row of `peer_device_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub level: String,
    /// Unix seconds.
    pub paired_at: u64,
    pub last_seen: Option<u64>,
    pub connected: bool,
}

/// One row of `peer_interfaces`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IfaceView {
    pub name: String,
    pub address: String,
    /// The network it is on, `10.8.0.0/24`, when the adapter says.
    pub network: Option<String>,
    pub wireguard: bool,
    pub loopback: bool,
}

/// The steps before it is turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Address,
    Firewall,
}

/// The walk-through that turns remote access on. Ephemeral UI state: nothing here is a setting,
/// and nothing is sent until the last button.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub step: Step,
    /// The address picked in the list (`None`: the default pick, the WireGuard one if there is one).
    pub address: Option<String>,
    /// The port as typed; empty means [`DEFAULT_PORT`].
    pub port: String,
    pub advanced: bool,
    /// Advanced: an address that is not in the list (a tunnel that is not up yet).
    pub manual: String,
    /// Advanced: listen on every interface. Off unless chosen here on purpose.
    pub all_interfaces: bool,
}

/// What the `peer_*` tools last said, and the walk-through.
#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub status: StatusView,
    pub devices: Vec<DeviceView>,
    pub interfaces: Vec<IfaceView>,
    /// The pairing link of the open offer. A secret for ten minutes: dropped as soon as the offer
    /// is gone or remote access is turned off.
    pub link: Option<String>,
    /// The walk-through, while it is open.
    pub setup: Option<Setup>,
    /// The last refusal or failure of a `peer_*` call, shown on the page.
    pub error: Option<String>,
    /// The relay in use or saved (`peer_relay_status`). `None` when none is set.
    pub relay: Option<RelayView>,
    /// The relay's address and token fields, while they are open.
    pub relay_form: RelayForm,
    /// The last refusal of `peer_relay_set`, shown on the relay card.
    pub relay_error: Option<String>,
    polled_at: Option<f64>,
}

/// The relay as `peer_relay_status` reports it (also the reply of `peer_relay_set`). No secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RelayView {
    pub url: String,
    pub host_id: String,
    pub connected: bool,
    pub last_error: Option<String>,
    /// Whether an enrolment token is saved in the keychain (the token itself is never shown).
    pub access_set: bool,
}

/// The enrolment token as typed. It lives in the field only until it is sent, and is never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() { "Secret(empty)" } else { "Secret(<hidden>)" })
    }
}

/// The relay's address and token fields.
#[derive(Debug, Clone, Default)]
pub struct RelayForm {
    /// Open: the fields show (the relay is being turned on, or the saved one is being changed).
    pub editing: bool,
    pub url: String,
    pub token: Secret,
}

/// The `peer_relay_set` params for saving an address. The token goes in `access` only when it is
/// typed; left out, the engine keeps the saved token for the same address. Empty `url` turns the
/// relay off.
pub fn relay_set_params(url: &str, token: &str) -> Value {
    let mut params = json!({"url": url.trim()});
    let token = token.trim();
    if !token.is_empty() {
        params["access"] = json!(token);
    }
    params
}

/// The relay in a reply: `null` (or an empty address) means none is set.
fn relay_view(v: &Value) -> Option<RelayView> {
    if v.is_null() {
        return None;
    }
    let view: RelayView = parse(v);
    (!view.url.is_empty()).then_some(view)
}

// ---- replies and polling -----------------------------------------------------------------------

fn parse<T: for<'de> Deserialize<'de> + Default>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_default()
}

fn set_status(r: &mut RemoteState, status: StatusView) {
    // The offer ended (used, expired, or the server is off): its link is of no use any more.
    if status.pairing_open_secs.is_none() || !status.enabled {
        r.link = None;
    }
    r.status = status;
}

/// Replies of the `peer_*` tools. Quiet about the reads (a build without a peer server answers
/// `available: false`); a refusal of an action is kept to show on the page.
pub fn on_reply(app: &mut CsmApp, _ctx: &egui::Context, call: &Call, reply: &csm_engine::Reply) {
    if !call.id.starts_with("peer_") {
        return;
    }
    let reads = matches!(call.id.as_str(), "peer_status" | "peer_device_list" | "peer_interfaces" | "peer_relay_status");
    let out = match reply {
        Ok(out) => out,
        Err(e) => {
            if call.id == "peer_relay_set" {
                app.ui.remote.relay_error = Some(e.to_string());
            } else if !reads {
                app.ui.remote.error = Some(e.to_string());
            }
            return;
        }
    };
    let r = &mut app.ui.remote;
    match call.id.as_str() {
        "peer_status" => set_status(r, parse(&out.data)),
        "peer_device_list" => r.devices = parse(&out.data),
        "peer_interfaces" => r.interfaces = parse(&out.data),
        "peer_server_enable" => {
            set_status(r, parse(&out.data));
            r.setup = None;
            r.error = None;
            // On: the person is here to pair a device, so the offer opens at once.
            app.dispatch.submit("peer_pair_start", json!({}));
            app.dispatch.submit("peer_device_list", json!({}));
        }
        "peer_server_disable" => {
            set_status(r, parse(&out.data));
            r.link = None;
            r.setup = None;
            r.error = None;
            app.dispatch.submit("peer_device_list", json!({}));
        }
        "peer_pair_start" => {
            r.link = out.data.get("link").and_then(Value::as_str).map(str::to_owned);
            r.error = None;
            app.dispatch.submit("peer_status", json!({}));
        }
        "peer_pair_answer" => {
            set_status(r, parse(&out.data));
            r.error = None;
            app.dispatch.submit("peer_device_list", json!({}));
        }
        "peer_device_revoke" => {
            r.error = None;
            app.dispatch.submit("peer_status", json!({}));
            app.dispatch.submit("peer_device_list", json!({}));
        }
        "peer_relay_status" => r.relay = relay_view(&out.data),
        "peer_relay_set" => {
            r.relay = relay_view(&out.data);
            r.relay_error = None;
            r.relay_form.editing = false;
            // An open pairing link names the relay it was made with: a new offer carries the new one.
            let open_link = r.link.is_some();
            if open_link {
                app.dispatch.submit("peer_pair_start", json!({}));
            }
            app.dispatch.submit("peer_relay_status", json!({}));
        }
        _ => {}
    }
}

/// The page opened (or the palette sent the person here): read everything once.
pub fn opened(app: &mut CsmApp, ctx: &egui::Context) {
    app.ui.remote.polled_at = Some(ctx.input(|i| i.time));
    for id in ["peer_status", "peer_device_list", "peer_interfaces", "peer_relay_status"] {
        app.dispatch.submit(id, json!({}));
    }
}

/// Once a frame: while the server is on, or this page is open, asks for the status and the devices
/// every second, so a device asking to pair, a connection and a missing address show up. Off and
/// closed: no calls at all.
pub fn poll(app: &mut CsmApp, ctx: &egui::Context) {
    let r = &app.ui.remote;
    if !r.status.available {
        return;
    }
    let on_page = app.ui.settings.open && app.ui.settings.section == SettingsSection::Remote;
    if !r.status.enabled && !on_page {
        return;
    }
    let now = ctx.input(|i| i.time);
    if r.polled_at.is_none_or(|t| now - t >= POLL_S) {
        app.ui.remote.polled_at = Some(now);
        app.dispatch.submit("peer_status", json!({}));
        app.dispatch.submit("peer_device_list", json!({}));
        app.dispatch.submit("peer_relay_status", json!({}));
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(POLL_S));
}

// ---- pure helpers --------------------------------------------------------------------------------

/// The port as typed: empty is the default, anything else must be 1-65535.
pub fn parse_port(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() {
        return Some(DEFAULT_PORT);
    }
    s.parse::<u16>().ok().filter(|p| *p != 0)
}

/// The firewall rule to show: Windows' `netsh` line, or a plain sentence elsewhere. It is text for
/// the person to read and copy; CodeShellManager adds no rule itself.
pub fn firewall_rule(address: &str, network: Option<&str>, port: u16, windows: bool) -> String {
    let scope = network.unwrap_or("<your WireGuard subnet, for example 10.8.0.0/24>");
    if windows {
        format!("netsh advfirewall firewall add rule name=\"CodeShellManager remote access\" dir=in action=allow protocol=TCP localip={address} localport={port} remoteip={scope}")
    } else {
        format!("Allow incoming TCP to {address} port {port} from {scope} in your firewall (for example: sudo ufw allow from {scope} to {address} port {port} proto tcp)")
    }
}

/// The address the walk-through would use now: the typed one, the picked one, else the WireGuard
/// address, else the first that is not loopback. With "every interface" it is the wildcard, which
/// only the Advanced switch can give. `(address, network, is_wireguard)`.
pub fn chosen(setup: &Setup, interfaces: &[IfaceView]) -> Option<(String, Option<String>, bool)> {
    if setup.all_interfaces {
        return Some(("0.0.0.0".to_owned(), None, false));
    }
    let typed = setup.manual.trim();
    if !typed.is_empty() {
        let known = interfaces.iter().find(|i| i.address == typed);
        return Some((typed.to_owned(), known.and_then(|i| i.network.clone()), known.is_some_and(|i| i.wireguard)));
    }
    let pick = match &setup.address {
        Some(a) => interfaces.iter().find(|i| &i.address == a),
        None => interfaces.iter().find(|i| i.wireguard).or_else(|| interfaces.iter().find(|i| !i.loopback)),
    };
    pick.map(|i| (i.address.clone(), i.network.clone(), i.wireguard))
}

/// What the title bar shows, and in which state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillKind {
    /// Listening, nobody connected.
    Serving,
    /// At least one device is connected.
    Connected,
    /// A device waits for Allow or Deny.
    Asking,
    /// On, but the chosen address is not there yet.
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    pub kind: PillKind,
    /// The whole text, also the accessible name.
    pub label: String,
    /// What is shown when the room is short.
    pub short: String,
}

/// The indicator for a status, or `None` while it is off (or not available): nothing is drawn then.
pub fn pill(status: &StatusView, devices: &[DeviceView]) -> Option<Pill> {
    if !status.available || !status.enabled {
        return None;
    }
    if let Some(p) = &status.pending_pair {
        return Some(Pill { kind: PillKind::Asking, label: format!("{} wants to pair", p.device_name), short: "Pair request".into() });
    }
    if status.connected > 0 {
        let name = devices.iter().find(|d| d.connected).map(|d| d.name.as_str()).unwrap_or("A device");
        let (label, short) = if status.connected == 1 {
            (format!("{name} connected"), "1 connected".to_owned())
        } else {
            (format!("{} devices connected", status.connected), format!("{} connected", status.connected))
        };
        return Some(Pill { kind: PillKind::Connected, label, short });
    }
    match (&status.listening, &status.waiting) {
        (Some(at), _) => Some(Pill { kind: PillKind::Serving, label: format!("Remote access on \u{b7} {at}"), short: "Remote access on".into() }),
        (None, Some(why)) => Some(Pill { kind: PillKind::Waiting, label: format!("Remote access waiting \u{b7} {why}"), short: "Remote access waiting".into() }),
        (None, None) => Some(Pill { kind: PillKind::Waiting, label: "Remote access starting".into(), short: "Remote access starting".into() }),
    }
}

/// "just now", "4m ago", "2h ago", "3d ago".
pub fn ago(now_ms: u64, secs: u64) -> String {
    let min = now_ms.saturating_sub(secs.saturating_mul(1000)) / 60_000;
    match min {
        0 => "just now".to_owned(),
        1..60 => format!("{min}m ago"),
        60..1440 => format!("{}h ago", min / 60),
        _ => format!("{}d ago", min / 1440),
    }
}

/// The QR code of a link: its side in modules and the dark ones row by row. `None` when the link
/// does not fit any code.
pub fn qr_modules(link: &str) -> Option<(usize, Vec<bool>)> {
    let qr = qrcodegen::QrCode::encode_text(link, qrcodegen::QrCodeEcc::Low).ok()?;
    let n = qr.size();
    let dark = (0..n).flat_map(|y| (0..n).map(move |x| (x, y))).map(|(x, y)| qr.get_module(x, y)).collect();
    usize::try_from(n).ok().map(|n| (n, dark))
}

// ---- title bar indicator -----------------------------------------------------------------------

fn pill_color(t: &Tokens, k: PillKind) -> egui::Color32 {
    match k {
        PillKind::Serving => t.blue,
        PillKind::Connected => t.green,
        PillKind::Asking => t.yellow,
        PillKind::Waiting => t.peach,
    }
}

/// The indicator in the title bar, between `left` and `right` (x), centred on `cy`. Draws nothing
/// while remote access is off. The text shrinks to its short form and then to the dot when there
/// is no room; the accessible name is always the whole text. A click opens the Remote access page.
pub fn indicator(ui: &Ui, app: &CsmApp, left: f32, right: f32, cy: f32, actions: &mut Vec<Action>) {
    let Some(p) = pill(&app.ui.remote.status, &app.ui.remote.devices) else { return };
    let t = Tokens::get(ui.ctx());
    let color = pill_color(&t, p.kind);
    let font = theme::medium(text::CAPTION);
    let room = (right - left).min(size::REMOTE_PILL_MAX_W);
    let pad = space::ROW_X;
    let dot = size::STATUS_DOT;
    let fit = |s: &str| widgets::text_width(ui, s, font.clone()) + pad * 2.0 + dot + space::SM;
    let (shown, width) = if fit(&p.label) <= room {
        (Some(p.label.as_str()), fit(&p.label))
    } else if fit(&p.short) <= room {
        (Some(p.short.as_str()), fit(&p.short))
    } else {
        (None, pad * 2.0 + dot)
    };
    if width > room.max(0.0) + f32::EPSILON {
        return;
    }
    let rect = Rect::from_min_size(pos2(left, cy - size::CONTROL_SM / 2.0), vec2(width, size::CONTROL_SM));
    let resp = ui.interact(rect, ui.id().with("tb-remote"), Sense::click());
    let fill = Tokens::tint(color, if resp.hovered() { alpha::PILL * 2.0 } else { alpha::PILL });
    ui.painter().rect(rect, CornerRadius::same(radius::PILL), fill, Stroke::new(size::HAIRLINE, Tokens::tint(color, alpha::GAUGE_BAND)), egui::StrokeKind::Inside);
    let dot_c = pos2(rect.min.x + pad + dot / 2.0, rect.center().y);
    ui.painter().circle_filled(dot_c, dot / 2.0, color);
    if let Some(s) = shown {
        ui.painter().text(pos2(dot_c.x + dot / 2.0 + space::SM, rect.center().y), Align2::LEFT_CENTER, s, font, t.fg);
    }
    let label = format!("Remote access: {}", p.label);
    resp.widget_info({
        let label = label.clone();
        move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
    });
    if resp.on_hover_text(format!("{} \u{2014} click for devices and to turn it off", p.label)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        actions.push(Action::OpenRemoteAccess);
    }
}

// ---- Settings > Remote access ----------------------------------------------------------------

/// The page.
pub fn settings(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if !app.ui.remote.status.available {
        caption(ui, "Remote access isn't available in this run: a test or isolated start has no peer server.");
        return;
    }
    caption(
        ui,
        "Lets a CodeShellManager on another machine, such as your laptop, use this one's sessions over your VPN. It is off until you turn it on here. Only you, in this window, can turn it on or pair a device: agents, the csm command and paired devices cannot.",
    );
    ui.add_space(space::SM);
    let on = app.ui.remote.status.enabled;
    let hint = if on {
        "On. One click turns it off: every connected device is disconnected and any open pairing link stops working."
    } else {
        "Off. Nothing is listening and nothing is announced."
    };
    if widgets::toggle(ui, on, TOGGLE_LABEL, Some(hint)).clicked() {
        if on {
            actions.push(Action::Tool("peer_server_disable", json!({})));
        } else if app.ui.remote.setup.is_some() {
            app.ui.remote.setup = None;
        } else {
            app.ui.remote.setup = Some(Setup::default());
            actions.push(Action::Tool("peer_interfaces", json!({})));
        }
    }
    if app.ui.remote.setup.is_some() && !on {
        setup_card(ui, app, actions);
    }
    if on {
        status_card(ui, app, actions);
        pairing_card(ui, app, actions);
    }
    relay_card(ui, app);
    devices_card(ui, app, actions);
    if let Some(e) = app.ui.remote.error.clone() {
        ui.add_space(space::SM);
        ui.label(egui::RichText::new(e).font(theme::regular(text::BODY)).color(t.red));
    }
    ui.add_space(space::SM);
    caption(ui, "Every connection, pairing and change a device makes is written to remote-access.log beside the state, never what is typed.");
}

fn dot(ui: &mut Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size::STATUS_DOT), Sense::hover());
    ui.painter().circle_filled(r.center(), size::STATUS_DOT / 2.0, color);
}

/// The toggle of the relay card.
pub const RELAY_TOGGLE_LABEL: &str = "Also reachable through a relay";

/// The relay card: for a laptop that cannot reach this PC over a VPN. The toggle opens the address
/// and token fields (on) or removes the relay (off). A saved relay shows its status, and can be
/// changed or turned off. Every change goes to the engine through the dispatch (background), and
/// the token is cleared from the field as soon as it is sent.
fn relay_card(ui: &mut Ui, app: &mut CsmApp) {
    let t = Tokens::get(ui.ctx());
    let relay = app.ui.remote.relay.clone();
    let editing = app.ui.remote.relay_form.editing;
    let on = relay.is_some() || editing;
    // What the card asked for this frame; applied after the frame so no state is borrowed twice.
    let (mut toggled, mut save, mut change, mut turn_off) = (false, false, false, false);
    let mut cancel = false;
    ui.add_space(space::SM);
    widgets::card_frame(&t).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = space::SM;
        let hint = if on {
            "On. Turning it off removes the address and the token. The relay copies encrypted frames only."
        } else {
            "Off. Only the direct address is offered. Turn this on where there is no VPN."
        };
        toggled = widgets::toggle(ui, on, RELAY_TOGGLE_LABEL, Some(hint)).clicked();
        caption(
            ui,
            "A relay copies encrypted frames between this PC and a laptop that cannot reach it directly. It cannot read them. It runs only while remote access is on, and a pairing link then names it too.",
        );
        if editing {
            relay_fields(ui, app, &mut save, &mut cancel);
        } else if let Some(r) = &relay {
            relay_status_line(ui, &t, r, &mut change, &mut turn_off);
        }
        if let Some(e) = app.ui.remote.relay_error.clone() {
            ui.label(egui::RichText::new(e).font(theme::regular(text::BODY)).color(t.red));
        }
    });
    if toggled {
        if on && relay.is_some() {
            turn_off = true;
        } else if on {
            cancel = true;
        } else {
            app.ui.remote.relay_form.editing = true;
        }
    }
    if cancel {
        app.ui.remote.relay_form = RelayForm::default();
        app.ui.remote.relay_error = None;
    }
    if change {
        let form = &mut app.ui.remote.relay_form;
        form.editing = true;
        if form.url.trim().is_empty()
            && let Some(r) = &relay
        {
            form.url = r.url.clone();
        }
        app.ui.remote.relay_error = None;
    }
    if turn_off {
        app.ui.remote.relay_form = RelayForm::default();
        app.dispatch.submit("peer_relay_set", relay_set_params("", ""));
    }
    if save {
        let form = &mut app.ui.remote.relay_form;
        let params = relay_set_params(&form.url, &form.token.0);
        // The token leaves the field now, sent or not: it is not kept in the page state.
        form.token = Secret::default();
        if form.url.trim().is_empty() {
            app.ui.remote.relay_error = Some("Type the relay's address, for example wss://relay.example.".to_owned());
        } else {
            app.ui.remote.relay_error = None;
            app.dispatch.submit("peer_relay_set", params);
        }
    }
}

/// The address and token fields of the relay card, and Save and Cancel.
fn relay_fields(ui: &mut Ui, app: &mut CsmApp, save: &mut bool, cancel: &mut bool) {
    let t = Tokens::get(ui.ctx());
    let saved_token = app.ui.remote.relay.as_ref().is_some_and(|r| r.access_set);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Address").font(theme::regular(text::BODY)).color(t.fg));
        let edit = egui::TextEdit::singleline(&mut app.ui.remote.relay_form.url)
            .id(Id::new("remote-relay-url"))
            .hint_text(egui::RichText::new("wss://relay.example").color(t.faint))
            .font(theme::mono(text::MONO))
            .desired_width(size::FIELD_W);
        ui.add(edit).widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Relay address"));
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Enrolment token").font(theme::regular(text::BODY)).color(t.fg));
        let hint = if saved_token { "saved: leave empty to keep it" } else { "only if the relay asks for one" };
        let edit = egui::TextEdit::singleline(&mut app.ui.remote.relay_form.token.0)
            .id(Id::new("remote-relay-token"))
            .password(true)
            .hint_text(egui::RichText::new(hint).color(t.faint))
            .font(theme::mono(text::MONO))
            .desired_width(size::FIELD_W);
        ui.add(edit).widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Relay enrolment token"));
    });
    caption(ui, "The token is kept in the OS keychain, never in a file. Leave it empty to keep the saved token for the same address.");
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Primary, None, "Save relay", None).clicked() {
            *save = true;
        }
        if widgets::button(ui, ButtonKind::Ghost, None, "Cancel", None).clicked() {
            *cancel = true;
        }
    });
}

/// The saved relay, while the fields are closed: its address, whether it is connected, and the
/// change and turn-off buttons.
fn relay_status_line(ui: &mut Ui, t: &Tokens, r: &RelayView, change: &mut bool, turn_off: &mut bool) {
    let (color, status) = if r.connected {
        (t.green, "Connected to the relay.".to_owned())
    } else {
        (t.peach, format!("Not connected: {}", r.last_error.as_deref().unwrap_or("starting")))
    };
    ui.horizontal(|ui| {
        dot(ui, color);
        ui.label(egui::RichText::new(format!("Also reachable through {}", r.url)).font(theme::medium(text::BODY)).color(t.fg));
    });
    caption(ui, &status);
    if r.access_set {
        caption(ui, "An enrolment token is saved in the OS keychain.");
    }
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, None, "Change\u{2026}", None).clicked() {
            *change = true;
        }
        if widgets::button(ui, ButtonKind::Danger, None, "Turn off the relay", None).clicked() {
            *turn_off = true;
        }
    });
}

fn status_card(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let s = app.ui.remote.status.clone();
    ui.add_space(space::SM);
    widgets::card_frame(&t).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            match (&s.listening, &s.waiting) {
                (Some(at), _) => {
                    dot(ui, t.blue);
                    ui.label(egui::RichText::new(format!("Listening on {at}")).font(theme::medium(text::BODY)).color(t.fg));
                }
                (None, why) => {
                    dot(ui, t.peach);
                    let why = why.clone().unwrap_or_else(|| "starting".to_owned());
                    ui.label(egui::RichText::new(format!("Waiting: {why}")).font(theme::medium(text::BODY)).color(t.fg));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Danger, Some("power"), "Turn off remote access", None).clicked() {
                    actions.push(Action::Tool("peer_server_disable", json!({})));
                }
            });
        });
        if !s.private_files_restricted {
            caption(ui, "The key and device files could not be limited to your user account. Keep this machine's profile private.");
        }
    });
}

fn setup_card(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let interfaces = app.ui.remote.interfaces.clone();
    let Some(setup) = app.ui.remote.setup.clone() else { return };
    let mut next = setup.clone();
    let mut close = false;
    ui.add_space(space::SM);
    widgets::card_frame(&t).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = space::SM;
        let title = match setup.step {
            Step::Address => "Step 1 of 2: choose the address",
            Step::Firewall => "Step 2 of 2: let the port through your firewall",
        };
        ui.label(egui::RichText::new(title).font(semibold(text::BODY)).color(t.fg));
        let pick = chosen(&setup, &interfaces);
        let port = parse_port(&setup.port);
        match setup.step {
            Step::Address => {
                caption(ui, "Remote access listens on one address of this machine, never on all of them. Pick the WireGuard address if there is one: only devices on the VPN can reach it.");
                if interfaces.is_empty() {
                    caption(ui, "No network addresses were found. Type one under Advanced.");
                }
                let selected = pick.as_ref().map(|p| p.0.clone());
                for i in &interfaces {
                    let on = !setup.all_interfaces && setup.manual.trim().is_empty() && selected.as_deref() == Some(i.address.as_str());
                    if iface_row(ui, i, on).clicked() {
                        next.address = Some(i.address.clone());
                        next.manual.clear();
                        next.all_interfaces = false;
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Port").font(theme::regular(text::BODY)).color(t.fg));
                    let mut buf = setup.port.clone();
                    let edit = egui::TextEdit::singleline(&mut buf)
                        .id(Id::new("remote-port"))
                        .hint_text(egui::RichText::new(DEFAULT_PORT.to_string()).color(t.faint))
                        .font(theme::mono(text::MONO))
                        .desired_width(size::DATE_FIELD_W);
                    let r = ui.add(edit);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Remote access port"));
                    next.port = buf;
                    if port.is_none() {
                        ui.label(egui::RichText::new("A port from 1 to 65535").font(theme::regular(text::CAPTION)).color(t.red));
                    }
                });
                let label = if setup.advanced { "Hide advanced" } else { "Advanced\u{2026}" };
                if widgets::button(ui, ButtonKind::Ghost, None, label, None).clicked() {
                    next.advanced = !setup.advanced;
                }
                if setup.advanced {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Other address").font(theme::regular(text::BODY)).color(t.fg));
                        let mut buf = setup.manual.clone();
                        let edit = egui::TextEdit::singleline(&mut buf)
                            .id(Id::new("remote-manual"))
                            .hint_text(egui::RichText::new("a tunnel that is not up yet").color(t.faint))
                            .font(theme::mono(text::MONO))
                            .desired_width(size::FIELD_W / 2.0);
                        let r = ui.add(edit);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Other address to listen on"));
                        next.manual = buf;
                    });
                    if widgets::toggle(
                        ui,
                        setup.all_interfaces,
                        "Listen on every interface",
                        Some("Not recommended: every network this machine is on can then reach the port, not only the VPN. A pairing link names one address, so pair first on a single address."),
                    )
                    .clicked()
                    {
                        next.all_interfaces = !setup.all_interfaces;
                    }
                }
                ui.horizontal(|ui| {
                    let ok = pick.is_some() && port.is_some();
                    ui.add_enabled_ui(ok, |ui| {
                        if widgets::button(ui, ButtonKind::Primary, None, "Next", None).clicked() {
                            next.step = Step::Firewall;
                        }
                    });
                    if widgets::button(ui, ButtonKind::Ghost, None, "Cancel", None).clicked() {
                        close = true;
                    }
                });
            }
            Step::Firewall => {
                let (address, network, _) = pick.clone().unwrap_or_default();
                let port = port.unwrap_or(DEFAULT_PORT);
                caption(
                    ui,
                    "The first time it listens, the system may ask, or the VPN adapter may be in a profile that blocks incoming connections. This is the rule that lets the port through for the VPN's addresses only. CodeShellManager does not change your firewall: run it yourself, as administrator, if you need it.",
                );
                if network.is_none() && !setup.all_interfaces {
                    caption(ui, "The subnet is not known for this address: replace it with yours.");
                }
                let rule = firewall_rule(&address, network.as_deref(), port, cfg!(windows));
                widgets::card_frame(&t).fill(t.field).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(egui::RichText::new(&rule).font(theme::mono(text::MONO)).color(t.fg2)).wrap().selectable(true));
                });
                ui.horizontal(|ui| {
                    if widgets::button(ui, ButtonKind::Secondary, Some("copy"), "Copy the rule", None).clicked() {
                        ui.ctx().copy_text(rule.clone());
                    }
                });
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    if widgets::button(ui, ButtonKind::Ghost, None, "Back", None).clicked() {
                        next.step = Step::Address;
                    }
                    let params = json!({"address": address, "port": port, "all_interfaces": setup.all_interfaces});
                    if widgets::button(ui, ButtonKind::Primary, Some("wifi"), "Turn on and pair a device", None).clicked() {
                        actions.push(Action::Tool("peer_server_enable", params));
                    }
                });
            }
        }
    });
    if close {
        app.ui.remote.setup = None;
        app.ui.remote.error = None;
    } else {
        app.ui.remote.setup = Some(next);
    }
}

/// One address in the picker: a radio dot, the address, its adapter, and what it is.
fn iface_row(ui: &mut Ui, i: &IfaceView, on: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), Sense::click());
    if resp.hovered() || on {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), if on { t.row_sel } else { t.row_hover });
    }
    let c = pos2(rect.min.x + space::ROW_X + icon::XS / 2.0, rect.center().y);
    ui.painter().circle_stroke(c, icon::XS / 2.0, Stroke::new(size::HAIRLINE, if on { t.blue } else { t.surface2 }));
    if on {
        ui.painter().circle_filled(c, icon::XS / 2.0 - space::XS / 2.0, t.blue);
    }
    let r = widgets::paint_text(ui, pos2(c.x + icon::XS, rect.center().y), Align2::LEFT_CENTER, &i.address, theme::mono(text::MONO), t.fg, rect.width() / 2.0);
    let r = widgets::paint_text(ui, pos2(r.max.x + space::SM, rect.center().y), Align2::LEFT_CENTER, &i.name, theme::regular(text::CAPTION), t.muted, rect.max.x - r.max.x - space::XXL * 4.0);
    let tag = if i.wireguard {
        Some(("WireGuard", t.green))
    } else if i.loopback {
        Some(("this machine only", t.muted))
    } else {
        None
    };
    if let Some((tag, color)) = tag {
        let w = widgets::text_width(ui, tag, theme::medium(text::TINY)) + space::MD;
        let chip = Rect::from_min_size(pos2(r.max.x + space::SM, rect.center().y - size::CHIP / 2.0), vec2(w, size::CHIP));
        ui.painter().rect_filled(chip, CornerRadius::same(radius::PILL), Tokens::tint(color, alpha::PILL));
        ui.painter().text(chip.center(), Align2::CENTER_CENTER, tag, theme::medium(text::TINY), color);
    }
    let label = format!("Address: {} ({})", i.address, i.name);
    resp.widget_info(move || egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, label.clone()));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn pairing_card(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let s = app.ui.remote.status.clone();
    if s.listening.is_none() {
        return;
    }
    ui.add_space(space::SM);
    section(ui, "Pair a device");
    if let Some(p) = &s.pending_pair {
        let border = Stroke::new(size::HAIRLINE, t.yellow);
        widgets::card_frame(&t).stroke(border).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = space::SM;
            ui.label(egui::RichText::new(format!("{} wants to pair", p.device_name)).font(semibold(text::TITLE)).color(t.fg));
            ui.label(
                egui::RichText::new(p.check_code.chars().map(String::from).collect::<Vec<_>>().join(" "))
                    .font(theme::mono(text::DISPLAY))
                    .color(t.yellow),
            )
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Check code"));
            caption(ui, "Allow only if the other screen shows the same four digits. A paired device has full access: it can type into your sessions, like an SSH key to this machine.");
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Primary, Some("check"), "Allow", None).clicked() {
                    actions.push(Action::Tool("peer_pair_answer", json!({"allow": true, "check_code": p.check_code, "level": "full"})));
                }
                if widgets::button(ui, ButtonKind::Danger, Some("x"), "Deny", None).clicked() {
                    actions.push(Action::Tool("peer_pair_answer", json!({"allow": false})));
                }
            });
        });
        return;
    }
    let Some(link) = app.ui.remote.link.clone() else {
        caption(ui, "No pairing link is open. A link works once and for ten minutes.");
        if widgets::button(ui, ButtonKind::Secondary, Some("link"), "Pair a device\u{2026}", None).clicked() {
            actions.push(Action::Tool("peer_pair_start", json!({})));
        }
        return;
    };
    widgets::card_frame(&t).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::LG;
            if let Some((n, dark)) = qr_modules(&link) {
                qr(ui, &t, n, &dark);
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::SM;
                ui.label(egui::RichText::new("On the other CodeShellManager").font(semibold(text::BODY)).color(t.fg));
                caption(ui, "Choose Connect to another CSM and paste this link, or scan the code. The other screen then shows a four-digit code, and it appears here for you to compare.");
                widgets::card_frame(&t).fill(t.field).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(egui::RichText::new(&link).font(theme::mono(text::MONO_SM)).color(t.fg2)).wrap().selectable(true));
                });
                if widgets::button(ui, ButtonKind::Secondary, Some("copy"), "Copy link", None).clicked() {
                    ui.ctx().copy_text(link.clone());
                }
                let left = s.pairing_open_secs.map_or_else(String::new, |secs| format!("Valid for {} more minutes, one use. ", secs.div_ceil(60)));
                caption(ui, &format!("{left}Anyone with the link can ask to pair, so keep it to yourself."));
            });
        });
    });
}

/// The QR code, dark modules on a light ground with the quiet zone around it. The two colours are
/// the theme's darkest and lightest, so a scanner always sees dark on light.
fn qr(ui: &mut Ui, t: &Tokens, n: usize, dark: &[bool]) {
    let luma = |c: egui::Color32| u32::from(c.r()) * 299 + u32::from(c.g()) * 587 + u32::from(c.b()) * 114;
    let ground = [t.crust, t.mantle, t.bg, t.fg];
    let ink = ground.into_iter().min_by_key(|c| luma(*c)).unwrap_or(t.fg);
    let paper = ground.into_iter().max_by_key(|c| luma(*c)).unwrap_or(t.bg);
    let n = n as i32;
    let cells = n + 2 * size::QR_QUIET;
    let cell = (size::QR_SIDE / cells as f32).floor().max(1.0);
    let side = cell * cells as f32;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(radius::MD), paper);
    let origin = rect.min + Vec2::splat(cell * size::QR_QUIET as f32);
    for y in 0..n {
        // One rectangle per run of dark modules.
        let mut x = 0;
        while x < n {
            let at = |x: i32| dark.get((y * n + x) as usize).copied().unwrap_or(false);
            if !at(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < n && at(x) {
                x += 1;
            }
            p.rect_filled(Rect::from_min_size(origin + vec2(start as f32 * cell, y as f32 * cell), vec2((x - start) as f32 * cell, cell)), 0.0, ink);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, "Pairing QR code"));
}

fn devices_card(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let devices = app.ui.remote.devices.clone();
    if devices.is_empty() && !app.ui.remote.status.enabled {
        return;
    }
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        section(ui, "Paired devices");
        if !devices.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Danger, None, "Revoke all", None).clicked() {
                    actions.push(Action::Confirm(ConfirmState {
                        also: None,
                        title: "Revoke every device?".into(),
                        body: format!(
                            "Their keys are deleted and any connection is closed at once. Each device needs a fresh pairing to come back. {} devices will be revoked.",
                            devices.len()
                        ),
                        yes_label: "Revoke all".into(),
                        no_label: "Cancel".into(),
                        yes: Box::new(Action::Tool("peer_device_revoke", json!({"all": true}))),
                    }));
                }
            });
        }
    });
    let now_ms = app.model.now_ms;
    widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::MD as i8, space::XS as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        if devices.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), Sense::hover());
            ui.painter().text(pos2(r.min.x, r.center().y), Align2::LEFT_CENTER, "No device is paired.", theme::regular(text::BODY), t.muted);
        }
        for (i, d) in devices.iter().enumerate() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::INBOX_ROW), Sense::hover());
            if i > 0 {
                ui.painter().hline(r.x_range(), r.min.y, Stroke::new(size::HAIRLINE, t.surface0));
            }
            let c = pos2(r.min.x + size::STATUS_DOT / 2.0, r.center().y);
            ui.painter().circle_filled(c, size::STATUS_DOT / 2.0, if d.connected { t.green } else { t.surface2 });
            let x = c.x + size::STATUS_DOT / 2.0 + space::SM;
            ui.painter().text(pos2(x, r.center().y - space::SM), Align2::LEFT_CENTER, &d.name, theme::medium(text::BODY), t.fg);
            let seen = if d.connected {
                "connected now".to_owned()
            } else {
                d.last_seen.map_or_else(|| "not connected yet".to_owned(), |s| format!("last seen {}", ago(now_ms, s)))
            };
            let level = if d.level == "full" { "Full access".to_owned() } else { d.level.clone() };
            let meta = format!("{level} \u{b7} paired {} \u{b7} {seen}", ago(now_ms, d.paired_at));
            ui.painter().text(pos2(x, r.center().y + space::SM), Align2::LEFT_CENTER, meta, theme::regular(text::CAPTION), t.muted);
            let b = Rect::from_min_max(pos2(r.max.x - size::MENU_W / 2.0 + space::XL, r.min.y + space::SM), pos2(r.max.x, r.max.y - space::SM));
            let resp = ui
                .scope_builder(egui::UiBuilder::new().max_rect(b).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                    widgets::button(ui, ButtonKind::Danger, None, "Revoke", None)
                })
                .inner;
            let label = format!("Revoke {}", d.name);
            resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
            if resp.clicked() {
                actions.push(Action::Tool("peer_device_revoke", json!({"id": d.id})));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(enabled: bool) -> StatusView {
        StatusView { available: true, enabled, listening: enabled.then(|| "10.8.0.2:47300".to_owned()), ..StatusView::default() }
    }

    fn device(name: &str, connected: bool) -> DeviceView {
        DeviceView { id: format!("ID{name}"), name: name.into(), level: "full".into(), paired_at: 0, last_seen: None, connected }
    }

    #[test]
    fn off_has_no_indicator_and_each_state_its_own() {
        assert_eq!(pill(&StatusView::default(), &[]), None, "unavailable");
        assert_eq!(pill(&status(false), &[]), None, "off");
        let serving = pill(&status(true), &[]).unwrap();
        assert_eq!((serving.kind, serving.label.as_str()), (PillKind::Serving, "Remote access on \u{b7} 10.8.0.2:47300"));
        let one = StatusView { connected: 1, ..status(true) };
        let c = pill(&one, &[device("Laptop", true)]).unwrap();
        assert_eq!((c.kind, c.label.as_str()), (PillKind::Connected, "Laptop connected"));
        let two = StatusView { connected: 2, ..status(true) };
        assert_eq!(pill(&two, &[]).unwrap().label, "2 devices connected");
        let asking = StatusView { pending_pair: Some(PendingView { device_name: "Laptop".into(), check_code: "1234".into(), expires_in_secs: 5 }), connected: 1, ..status(true) };
        assert_eq!(pill(&asking, &[]).unwrap().kind, PillKind::Asking, "a question outranks a connection");
        let waiting = StatusView { listening: None, waiting: Some("10.8.0.2 is not on this machine".into()), ..status(true) };
        assert_eq!(pill(&waiting, &[]).unwrap().kind, PillKind::Waiting);
    }

    #[test]
    fn the_port_is_one_to_65535_or_empty() {
        assert_eq!(parse_port(""), Some(47_300));
        assert_eq!(parse_port(" 8080 "), Some(8080));
        for bad in ["0", "65536", "-1", "http", "47300x"] {
            assert_eq!(parse_port(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_firewall_rule_names_the_subnet_or_admits_it_does_not_know() {
        let w = firewall_rule("10.8.0.2", Some("10.8.0.0/24"), 47300, true);
        assert!(w.starts_with("netsh advfirewall firewall add rule"), "{w}");
        assert!(w.contains("localip=10.8.0.2 localport=47300 remoteip=10.8.0.0/24"), "{w}");
        let unknown = firewall_rule("10.8.0.2", None, 47300, true);
        assert!(unknown.contains("<your WireGuard subnet"), "no guess at the subnet: {unknown}");
        assert!(firewall_rule("10.8.0.2", Some("10.8.0.0/24"), 47300, false).contains("47300"));
    }

    fn ifaces() -> Vec<IfaceView> {
        let i = |name: &str, address: &str, wireguard: bool, loopback: bool| IfaceView { name: name.into(), address: address.into(), network: Some("10.8.0.0/24".into()), wireguard, loopback };
        vec![i("Ethernet", "192.168.1.20", false, false), i("wg", "10.8.0.2", true, false), i("lo", "127.0.0.1", false, true)]
    }

    #[test]
    fn the_wireguard_address_is_the_default_and_a_wildcard_needs_the_switch() {
        let l = ifaces();
        assert_eq!(chosen(&Setup::default(), &l).map(|c| c.0), Some("10.8.0.2".into()), "WireGuard is pre-selected");
        let eth = Setup { address: Some("192.168.1.20".into()), ..Setup::default() };
        assert_eq!(chosen(&eth, &l).map(|c| c.0), Some("192.168.1.20".into()));
        let no_wg: Vec<IfaceView> = l.iter().filter(|i| !i.wireguard).cloned().collect();
        assert_eq!(chosen(&Setup::default(), &no_wg).map(|c| c.0), Some("192.168.1.20".into()), "else the first that is not loopback");
        assert_eq!(chosen(&Setup::default(), &[]), None, "nothing is chosen for the person");
        let lo_only = vec![l[2].clone()];
        assert_eq!(chosen(&Setup::default(), &lo_only), None, "loopback is never picked by default");
        let all = Setup { all_interfaces: true, ..Setup::default() };
        assert_eq!(chosen(&all, &l).map(|c| c.0), Some("0.0.0.0".into()));
        let typed = Setup { manual: " 10.9.0.2 ".into(), ..Setup::default() };
        assert_eq!(chosen(&typed, &l).map(|c| c.0), Some("10.9.0.2".into()));
    }

    #[test]
    fn ages_read_plainly() {
        let now = 10_000_000_000;
        let s = now / 1000;
        assert_eq!(ago(now, s - 10), "just now");
        assert_eq!(ago(now, s - 4 * 60), "4m ago");
        assert_eq!(ago(now, s - 3 * 3600), "3h ago");
        assert_eq!(ago(now, s - 3 * 86_400), "3d ago");
        assert_eq!(ago(now, s + 100), "just now", "a clock behind is not negative");
    }

    #[test]
    fn the_qr_code_encodes_a_link() {
        let link = format!("csm://pair#{}", "A".repeat(250));
        let (n, dark) = qr_modules(&link).unwrap();
        assert_eq!(dark.len(), n * n);
        assert!((21..=177).contains(&n) && (n - 17) % 4 == 0, "a QR version's size: {n}");
        // The three finder patterns have a dark corner module.
        assert!(dark[0] && dark[n - 1] && dark[(n - 1) * n]);
        assert!(qr_modules(&"x".repeat(4000)).is_none(), "too long for any code");
    }

    #[test]
    fn a_status_with_unknown_fields_still_reads() {
        let v = json!({"available": true, "enabled": true, "listening": "10.8.0.2:47300", "future": [1, 2], "devices": 2});
        let s: StatusView = parse(&v);
        assert!(s.available && s.enabled && s.devices == 2);
        let junk: StatusView = parse(&json!("not an object"));
        assert!(!junk.available, "a bad reply reads as unavailable, never a panic");
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;

    #[test]
    fn the_relay_params_carry_the_token_only_when_typed() {
        assert_eq!(relay_set_params(" wss://relay.example ", ""), json!({"url": "wss://relay.example"}), "left out: the saved token is kept by the engine");
        assert_eq!(relay_set_params("wss://relay.example", "  tok-0123456789abcdef  "), json!({"url": "wss://relay.example", "access": "tok-0123456789abcdef"}));
        assert_eq!(relay_set_params("", "tok"), json!({"url": "", "access": "tok"}), "the engine turns it off whatever the token");
        assert_eq!(relay_set_params("", ""), json!({"url": ""}), "turning off sends no token");
    }

    #[test]
    fn a_relay_reply_is_none_or_a_view_without_a_secret() {
        assert_eq!(relay_view(&Value::Null), None, "no relay set");
        assert_eq!(relay_view(&json!({"url": ""})), None, "an empty address is no relay");
        let v = relay_view(&json!({"url": "wss://relay.example", "host_id": "abc", "connected": false, "last_error": "remote access is off", "access_set": true, "future": 1})).unwrap();
        assert_eq!((v.url.as_str(), v.connected, v.access_set), ("wss://relay.example", false, true));
        assert_eq!(v.last_error.as_deref(), Some("remote access is off"));
        assert!(relay_view(&json!("not an object")).is_none(), "a bad reply reads as no relay, never a panic");
    }

    #[test]
    fn the_typed_token_never_shows_in_debug_output() {
        let mut r = RemoteState::default();
        r.relay_form.editing = true;
        r.relay_form.url = "wss://relay.example".into();
        r.relay_form.token = Secret("hunter2-enrolment-token".into());
        let shown = format!("{r:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("wss://relay.example"), "the address may show: {shown}");
        assert_eq!(format!("{:?}", Secret::default()), "Secret(empty)");
    }

    #[test]
    fn a_logged_relay_call_has_its_token_redacted_and_nothing_else_changed() {
        let params = json!({"url": "wss://relay.example", "access": "hunter2-enrolment-token"});
        let shown = crate::backend::redacted_params("peer_relay_set", &params);
        assert_eq!(shown, json!({"url": "wss://relay.example", "access": "<redacted>"}));
        assert_eq!(params["access"], "hunter2-enrolment-token", "the original is not changed");
        // Other tools are logged as they are, and a relay call without a token has nothing to hide.
        assert_eq!(crate::backend::redacted_params("peer_pair_answer", &json!({"access": "x"})), json!({"access": "x"}));
        assert_eq!(crate::backend::redacted_params("peer_relay_set", &json!({"url": ""})), json!({"url": ""}));
    }
}
