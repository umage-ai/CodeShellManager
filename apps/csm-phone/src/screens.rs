//! The add-on's three screens (design note sections 8 and 10.6): the pairing screen, the phones
//! screen and the settings screen.
//!
//! The screen is a tree the add-on builds and CSM draws. Every control sends an event
//! (`screen/event`), and [`event`] turns the event into commands for the host thread. There is no
//! CSM confirm on these buttons, so the two that change a device (Allow, Revoke) are guarded here:
//! Allow only when a check code is shown, Revoke only after its own "Yes, revoke" press.
//!
//! The tree is a pure function of [`Live`] (what the host last reported), the time and the screen's
//! own [`Ui`] state, so the tests can check every tree and every event without threads.

use std::sync::mpsc;

use csm_remote_proto::msg::Level;
use serde_json::{Value, json};

use crate::config::{MAX_PHONE_SESSIONS_RANGE, ProfileEntry};
use crate::host::{DeviceFlag, DeviceView, HostView, Link, Out};
use crate::policy::cut;
use crate::relay_client::Command;
use crate::settings::SettingsChange;

pub const PAIR: &str = "phone-remote.pair";
pub const DEVICES: &str = "phone-remote.devices";
pub const SETTINGS: &str = "phone-remote.settings";
/// The screens this add-on declares (at most 4 in `docs/addons.md`).
pub const SCREEN_IDS: [&str; 3] = [PAIR, DEVICES, SETTINGS];

/// Longest text a screen event may carry (a relay URL, a folder, a command). Longer is ignored.
const MAX_EVENT_TEXT: usize = 2048;

/// What the host last reported, as the screens read it. The shell keeps one, fed by the host
/// thread's outputs ([`Live::absorb`]) and by a periodic [`Command::View`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Live {
    /// The add-on is enabled in CSM (the first switch). Set by the shell at start.
    pub enabled: bool,
    pub link: Option<Link>,
    /// The offer link of the open pairing. It is shown only in the pairing screen's QR code.
    pub offer: Option<String>,
    /// A phone waiting for the person: its check code and the name it sent.
    pub pending: Option<(String, String)>,
    /// The last plain answer for the person's action.
    pub notice: Option<String>,
    pub host: Option<HostView>,
}

impl Live {
    /// Takes one output of the host thread.
    pub fn absorb(&mut self, out: &Out) {
        match out {
            Out::PairingUrl(url) => self.offer = Some(url.clone()),
            Out::Pending { check, name } => self.pending = Some((check.clone(), name.clone())),
            Out::PairingEnded => {
                self.offer = None;
                self.pending = None;
            }
            Out::Notice(text) => self.notice = Some(cut(text, 200)),
            Out::Link(link) => self.link = Some(link.clone()),
            Out::Frame(_) | Out::Control(_) | Out::Problem(_) | Out::DevicesChanged => {}
        }
    }

    /// The paired phones the host last reported (none while the host is not running).
    pub fn devices(&self) -> &[DeviceView] {
        self.host.as_ref().map_or(&[], |h| h.devices.as_slice())
    }
}

/// What the screens read: the live state, the time and the store problem, if the host cannot run.
pub struct View<'a> {
    pub live: &'a Live,
    pub now: u64,
    pub problem: Option<&'a str>,
}

impl View<'_> {
    fn host(&self) -> Option<&HostView> {
        self.live.host.as_ref()
    }
}

/// One screen's own state: what the person picked and typed, which the tree shows back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ui {
    /// The level the pairing screen will allow a phone at (viewer until the person picks operator).
    pub level: Level,
    /// The phone selected on the phones screen.
    pub device: Option<String>,
    /// The "Revoke this phone?" question is showing.
    pub confirm_revoke: bool,
    /// The folder selected on the settings screen (its path).
    pub folder: Option<String>,
    /// The profile selected on the settings screen (its name).
    pub profile: Option<String>,
    /// The profile form's fields, as typed.
    pub form: ProfileEntry,
}

impl Default for Ui {
    fn default() -> Self {
        Self { level: Level::Viewer, device: None, confirm_revoke: false, folder: None, profile: None, form: ProfileEntry::default() }
    }
}

/// The status line for the indicator (`csm/status`): text and tone, as `docs/addons.md` names them.
pub fn status_line(view: &View) -> (String, &'static str) {
    if let Some(problem) = view.problem {
        return (cut(&format!("Phone remote stopped: {problem}"), 80), "error");
    }
    if !view.live.enabled {
        return ("Phone remote is off".into(), "idle");
    }
    let Some(host) = view.host() else { return ("Starting".into(), "idle") };
    if !host.cfg.connect {
        return ("Off: Connect to the relay is switched off in settings".into(), "idle");
    }
    if host.cfg.relay.is_empty() {
        return ("No relay set: add one in Phone remote settings".into(), "warn");
    }
    match &view.live.link {
        None | Some(Link::Connecting) => ("Connecting to the relay".into(), "warn"),
        Some(Link::Down(why)) => (cut(&format!("Relay unreachable, trying again: {why}"), 80), "warn"),
        Some(Link::Up) => {
            let n = host.devices.len();
            let phones = match n {
                0 => "no phones".to_owned(),
                1 => "1 phone".to_owned(),
                _ => format!("{n} phones"),
            };
            let waiting = view.live.pending.is_some();
            let mut text = format!("Connected to relay, {phones}");
            if waiting {
                text.push_str(", one waiting for you");
            }
            (cut(&text, 80), if waiting { "warn" } else { "ok" })
        }
    }
}

/// The tree a screen shows, or `None` for a screen this add-on does not declare.
pub fn tree(screen: &str, view: &View, ui: &Ui) -> Option<Value> {
    match screen {
        PAIR => Some(pair_tree(view, ui)),
        DEVICES => Some(devices_tree(view, ui)),
        SETTINGS => Some(settings_tree(view, ui)),
        _ => None,
    }
}

/// "5 min ago" and the like, from two Unix times.
pub fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else if s < 86_400 {
        format!("{} h ago", s / 3600)
    } else if s < 2 * 86_400 {
        "1 day ago".into()
    } else {
        format!("{} days ago", s / 86_400)
    }
}

fn level_word(level: Level) -> &'static str {
    match level {
        Level::Viewer => "Viewer",
        Level::Operator => "Operator",
    }
}

fn level_value(level: Level) -> &'static str {
    match level {
        Level::Viewer => "viewer",
        Level::Operator => "operator",
    }
}

// ---- node builders ----------------------------------------------------------------------------

fn text(body: &str) -> Value {
    json!({"type": "text", "text": cut(body, 300), "wrap": true, "tone": "default"})
}

fn text_toned(body: &str, tone: &str) -> Value {
    json!({"type": "text", "text": cut(body, 300), "wrap": true, "tone": tone})
}

fn mono(body: &str) -> Value {
    json!({"type": "text", "text": body, "style": "mono", "wrap": true})
}

fn heading(body: &str) -> Value {
    json!({"type": "text", "text": cut(body, 200), "style": "title"})
}

fn button(key: &str, label: &str, kind: &str) -> Value {
    json!({"type": "button", "key": key, "label": label, "kind": kind, "event": true})
}

fn disabled_button(key: &str, label: &str, kind: &str) -> Value {
    json!({"type": "button", "key": key, "label": label, "kind": kind, "event": true, "disabled": true})
}

fn column(children: Vec<Value>) -> Value {
    json!({"type": "column", "children": children, "gap": "md"})
}

fn row(children: Vec<Value>) -> Value {
    json!({"type": "row", "children": children, "gap": "sm"})
}

fn level_segmented(key: &str, selected: Level) -> Value {
    json!({
        "type": "segmented",
        "key": key,
        "options": [{"value": "viewer", "label": "Viewer"}, {"value": "operator", "label": "Operator"}],
        "selected": level_value(selected),
    })
}

fn checkbox(key: &str, label: &str, checked: bool) -> Value {
    json!({"type": "checkbox", "key": key, "label": label, "checked": checked})
}

fn text_input(key: &str, value: &str, placeholder: &str, submit_label: &str) -> Value {
    json!({"type": "text_input", "key": key, "value": value, "placeholder": placeholder, "max_len": MAX_EVENT_TEXT, "submit_label": submit_label})
}

/// The problem and the last notice, at the top of every screen.
fn status_rows(view: &View) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(problem) = view.problem {
        rows.push(text_toned(&format!("The phone remote cannot run: {problem}"), "danger"));
    }
    if let Some(notice) = &view.live.notice {
        rows.push(text_toned(notice, "info"));
    }
    rows
}

// ---- the pairing screen ----------------------------------------------------------------------

fn pair_tree(view: &View, ui: &Ui) -> Value {
    let mut kids = status_rows(view);
    if view.host().is_some_and(|h| h.cfg.relay.is_empty()) {
        kids.push(text_toned("Set the relay in Phone remote settings before you pair a phone.", "warning"));
    }
    let can_start = view.host().is_some_and(|h| !h.cfg.relay.is_empty()) && view.problem.is_none();
    match &view.live.offer {
        Some(offer) => {
            kids.push(text("Scan this code with the phone, or open the link on it."));
            kids.push(json!({"type": "qr", "key": "offer", "text": offer, "label": "Scan with the phone", "size": "md"}));
            kids.push(mono(offer));
            let left = view.host().and_then(|h| h.slot_expires).map_or(0, |e| e.saturating_sub(view.now));
            kids.push(text(&format!("Expires in {}:{:02} · the code can be used once", left / 60, left % 60)));
            kids.push(button("stop", "Stop pairing", "secondary"));
            match &view.live.pending {
                Some((check, name)) => {
                    kids.push(text(&format!("Device: {name}      Check code: {check}")));
                    kids.push(text("Read the same four digits on the phone. If they differ, press Deny."));
                    kids.push(level_segmented("level", ui.level));
                    kids.push(row(vec![button("allow", "Allow", "primary"), button("deny", "Deny", "danger")]));
                }
                None => kids.push(text("Waiting for a phone to scan the code.")),
            }
        }
        None => {
            kids.push(text("Press \"Start pairing\", then scan the code with the phone or open the link on it. The code can be used once and expires after 10 minutes."));
            let start = if can_start { button("start", "Start pairing", "primary") } else { disabled_button("start", "Start pairing", "primary") };
            kids.push(start);
        }
    }
    column(kids)
}

// ---- the phones screen -----------------------------------------------------------------------

fn devices_tree(view: &View, ui: &Ui) -> Value {
    let mut kids = status_rows(view);
    let devices = view.live.devices();
    if devices.is_empty() {
        kids.push(json!({"type": "empty", "title": "No phones paired yet", "body": "Open Pair a phone to pair one."}));
        return column(kids);
    }
    let items: Vec<Value> = devices
        .iter()
        .take(200)
        .map(|d| {
            let mut item = json!({
                "key": d.device_id,
                "title": cut(&d.name, 200),
                "subtitle": format!("{} · last seen {}", level_word(d.level), ago(view.now, d.last_seen)),
            });
            if d.notify && !d.push {
                item["badges"] = json!([{"text": "needs push", "tone": "warning"}]);
            }
            item
        })
        .collect();
    let mut list = json!({"type": "list", "key": "devices", "items": items});
    if let Some(id) = &ui.device {
        list["selected"] = json!(id);
    }
    kids.push(list);
    if let Some(d) = ui.device.as_ref().and_then(|id| devices.iter().find(|d| &d.device_id == id)) {
        kids.push(heading(&cut(&d.name, 200)));
        kids.push(level_segmented("level", d.level));
        kids.push(checkbox("notify", "Notify me about sessions that need me", d.notify));
        kids.push(checkbox("hide_names", "Hide session names in notifications", d.hide_names));
        if ui.confirm_revoke {
            kids.push(text_toned(&format!("Revoke {}? It is disconnected at once and must pair again.", cut(&d.name, 80)), "warning"));
            kids.push(row(vec![button("revoke_yes", "Yes, revoke", "danger"), button("revoke_no", "Cancel", "secondary")]));
        } else {
            kids.push(button("revoke", "Revoke this phone", "danger"));
        }
    }
    column(kids)
}

// ---- the settings screen ---------------------------------------------------------------------

fn settings_tree(view: &View, ui: &Ui) -> Value {
    let mut kids = status_rows(view);
    let Some(host) = view.host() else {
        kids.push(text("The settings appear once the phone remote is running."));
        return column(kids);
    };
    let cfg = &host.cfg;
    if cfg.damaged {
        kids.push(text_toned(
            "config.json is damaged, so the defaults are in use. The first change keeps the damaged file as config.json.damaged-<time> and writes a new one.",
            "danger",
        ));
    }
    if host.restart_needed {
        kids.push(text_toned("The relay and connect changes apply when the add-on restarts: turn Phone remote off and on in Settings > Add-ons.", "warning"));
    }
    // The "Use the umage relay" button is not drawn until the owner gives the URL (design note
    // section 13, decision 2). It goes here, with the URL, when that is decided.
    kids.push(heading("Relay"));
    kids.push(text_input("relay", &cfg.relay, "wss://relay.example/", "Save relay"));
    kids.push(checkbox("connect", "Connect to the relay", cfg.connect));
    kids.push(text_input("origin", &cfg.app_origin, "https://phone.example", "Save app address"));
    kids.push(text("A phone's link is made with this app address. Use the default unless you run your own copy of the phone app."));
    kids.push(text("The relay's access token is not supported in this version: the relay connection sends none."));

    kids.push(heading("Folders a phone may open sessions in"));
    if cfg.folders.is_empty() {
        kids.push(text("No folders yet: a phone cannot start a session until you add one."));
    } else {
        let items: Vec<Value> = cfg
            .folders
            .iter()
            .take(200)
            .enumerate()
            .map(|(i, f)| json!({"key": format!("f{i}"), "title": cut(&f.path, 200), "subtitle": if f.pinned { "Pinned" } else { "Not pinned" }}))
            .collect();
        let mut list = json!({"type": "list", "key": "folders", "items": items});
        if let Some(i) = ui.folder.as_ref().and_then(|p| cfg.folders.iter().position(|f| &f.path == p)) {
            list["selected"] = json!(format!("f{i}"));
            let pinned = cfg.folders.get(i).is_some_and(|f| f.pinned);
            kids.push(list);
            kids.push(checkbox("folder_pinned", "Pinned in the phone's folder list", pinned));
            kids.push(button("folder_remove", "Remove this folder", "danger"));
        } else {
            kids.push(list);
        }
    }
    kids.push(text_input("folder_add", "", "Full path of a folder on this PC", "Add folder"));

    kids.push(heading("Profiles a phone may pick"));
    if cfg.profiles.is_empty() {
        kids.push(text("No profiles yet. A profile is a name, a label and the command the PC runs."));
    } else {
        let items: Vec<Value> = cfg
            .profiles
            .iter()
            .take(200)
            .enumerate()
            .map(|(i, p)| json!({"key": format!("p{i}"), "title": cut(&p.name, 64), "subtitle": cut(&p.label, 64)}))
            .collect();
        let mut list = json!({"type": "list", "key": "profiles", "items": items});
        if let Some(i) = ui.profile.as_ref().and_then(|n| cfg.profiles.iter().position(|p| &p.name == n)) {
            list["selected"] = json!(format!("p{i}"));
            kids.push(list);
            kids.push(button("profile_remove", "Remove this profile", "danger"));
        } else {
            kids.push(list);
        }
    }
    kids.push(text("Add a profile (the phone sees the name and label; the command stays on this PC):"));
    kids.push(text_input("profile_name", &ui.form.name, "Name: letters, digits, hyphens", ""));
    kids.push(text_input("profile_label", &ui.form.label, "Label a phone shows", ""));
    kids.push(text_input("profile_command", &ui.form.command, "Command on this PC", ""));
    kids.push(button("profile_add", "Add profile", "primary"));

    kids.push(heading("Slash commands a phone may send"));
    kids.push(text("A command is sent only when this PC also lists it for Claude sessions."));
    if cfg.commands.is_empty() {
        kids.push(text("No commands are allowed."));
    }
    for (i, c) in cfg.commands.iter().enumerate().take(200) {
        kids.push(checkbox(&format!("cmd{i}"), &cut(c, 64), true));
    }
    kids.push(text_input("command_add", "", "A slash command, for example /clear", "Allow command"));

    kids.push(heading("Notifications and limits"));
    kids.push(checkbox("push", "Send push notifications for sessions that need me", cfg.push));
    let all_hide = !host.devices.is_empty() && host.devices.iter().all(|d| d.hide_names);
    kids.push(checkbox("hide_names", "Hide session names in notifications, for every phone", all_hide));
    kids.push(text("A phone may start, wake or restart at most 6 sessions a minute and 30 an hour. Fixed for this version."));
    let options: Vec<Value> = MAX_PHONE_SESSIONS_RANGE.map(|n| json!({"value": n.to_string(), "label": n.to_string()})).collect();
    kids.push(json!({"type": "select", "key": "max_sessions", "options": options, "selected": cfg.max_phone_sessions.to_string()}));
    kids.push(text("The most live sessions the phones may have started at once (1 to 20)."));
    column(kids)
}

// ---- events ----------------------------------------------------------------------------------

fn str_field(v: &Value, name: &str) -> Option<String> {
    let s = v.get(name)?.as_str()?;
    (s.len() <= MAX_EVENT_TEXT).then(|| s.to_owned())
}

fn bool_field(v: &Value, name: &str) -> Option<bool> {
    v.get(name)?.as_bool()
}

fn fresh_reply<T>() -> mpsc::Sender<T> {
    // The answer is not waited for on the screen: the host reports through its notices.
    mpsc::channel().0
}

/// Applies one screen event to the screen's state and returns the commands for the host.
/// `key` and `kind` are the event's (the kinds of `docs/addons.md`, "Events and buttons"). An event
/// the screen does not know, or one that does not fit the state, returns no commands.
pub fn event(screen: &str, view: &View, ui: &mut Ui, key: &str, kind: &str, value: &Value) -> Vec<Command> {
    match screen {
        PAIR => pair_event(view, ui, key, kind, value),
        DEVICES => devices_event(ui, key, kind, value),
        SETTINGS => settings_event(view, ui, key, kind, value),
        _ => Vec::new(),
    }
}

fn pair_event(view: &View, ui: &mut Ui, key: &str, kind: &str, value: &Value) -> Vec<Command> {
    match (key, kind) {
        ("start", "click") => vec![Command::StartPairing { app_version: env!("CARGO_PKG_VERSION").to_owned(), reply: fresh_reply() }],
        ("stop", "click") => vec![Command::StopPairing],
        ("level", "select") => {
            if let Some(level) = str_field(value, "value").and_then(|v| level_from(&v)) {
                ui.level = level;
            }
            Vec::new()
        }
        // Allow and Deny only mean something while a phone waits for an answer.
        ("allow", "click") if view.live.pending.is_some() => vec![Command::Decide(Some(ui.level))],
        ("deny", "click") if view.live.pending.is_some() => vec![Command::Decide(None)],
        _ => Vec::new(),
    }
}

fn devices_event(ui: &mut Ui, key: &str, kind: &str, value: &Value) -> Vec<Command> {
    match (key, kind) {
        ("devices", "select") => {
            ui.device = str_field(value, "key");
            ui.confirm_revoke = false;
            Vec::new()
        }
        ("level", "select") => match (ui.device.clone(), str_field(value, "value").and_then(|v| level_from(&v))) {
            (Some(id), Some(level)) => vec![Command::SetLevel(id, level)],
            _ => Vec::new(),
        },
        ("notify", "toggle") => match (ui.device.clone(), bool_field(value, "checked")) {
            (Some(id), Some(on)) => vec![Command::SetDeviceFlag(Some(id), DeviceFlag::Notify(on))],
            _ => Vec::new(),
        },
        ("hide_names", "toggle") => match (ui.device.clone(), bool_field(value, "checked")) {
            (Some(id), Some(on)) => vec![Command::SetDeviceFlag(Some(id), DeviceFlag::HideNames(on))],
            _ => Vec::new(),
        },
        ("revoke", "click") if ui.device.is_some() => {
            ui.confirm_revoke = true;
            Vec::new()
        }
        ("revoke_no", "click") => {
            ui.confirm_revoke = false;
            Vec::new()
        }
        ("revoke_yes", "click") => match ui.device.take() {
            Some(id) => {
                ui.confirm_revoke = false;
                vec![Command::Revoke(id)]
            }
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn settings_event(view: &View, ui: &mut Ui, key: &str, kind: &str, value: &Value) -> Vec<Command> {
    let Some(host) = view.host() else { return Vec::new() };
    let cfg = &host.cfg;
    let change = |c: SettingsChange| vec![Command::Settings(c)];
    match (key, kind) {
        ("relay", "submit") => str_field(value, "value").map_or_else(Vec::new, |v| change(SettingsChange::SetRelay(v))),
        ("origin", "submit") => str_field(value, "value").map_or_else(Vec::new, |v| change(SettingsChange::SetAppOrigin(v))),
        ("connect", "toggle") => bool_field(value, "checked").map_or_else(Vec::new, |on| change(SettingsChange::SetConnect(on))),
        ("push", "toggle") => bool_field(value, "checked").map_or_else(Vec::new, |on| change(SettingsChange::SetPush(on))),
        ("hide_names", "toggle") => match bool_field(value, "checked") {
            Some(on) => vec![Command::SetDeviceFlag(None, DeviceFlag::HideNames(on))],
            None => Vec::new(),
        },
        ("max_sessions", "select") => match str_field(value, "value").and_then(|v| v.parse::<u32>().ok()) {
            Some(n) => change(SettingsChange::SetMaxSessions(n)),
            None => Vec::new(),
        },
        ("folders", "select") => {
            ui.folder = str_field(value, "key").and_then(|k| index_of(&k, "f")).and_then(|i| cfg.folders.get(i)).map(|f| f.path.clone());
            Vec::new()
        }
        ("folder_add", "submit") => str_field(value, "value").map_or_else(Vec::new, |v| change(SettingsChange::AddFolder(v))),
        ("folder_remove", "click") => match ui.folder.take() {
            Some(path) => change(SettingsChange::RemoveFolder(path)),
            None => Vec::new(),
        },
        ("folder_pinned", "toggle") => match (ui.folder.clone(), bool_field(value, "checked")) {
            (Some(path), Some(pinned)) => change(SettingsChange::SetFolderPinned { path, pinned }),
            _ => Vec::new(),
        },
        ("profiles", "select") => {
            ui.profile = str_field(value, "key").and_then(|k| index_of(&k, "p")).and_then(|i| cfg.profiles.get(i)).map(|p| p.name.clone());
            Vec::new()
        }
        ("profile_name", "change") => {
            if let Some(v) = str_field(value, "value") {
                ui.form.name = v;
            }
            Vec::new()
        }
        ("profile_label", "change") => {
            if let Some(v) = str_field(value, "value") {
                ui.form.label = v;
            }
            Vec::new()
        }
        ("profile_command", "change") => {
            if let Some(v) = str_field(value, "value") {
                ui.form.command = v;
            }
            Vec::new()
        }
        ("profile_add", "click") => change(SettingsChange::AddProfile(ui.form.clone())),
        ("profile_remove", "click") => match ui.profile.take() {
            Some(name) => change(SettingsChange::RemoveProfile(name)),
            None => Vec::new(),
        },
        ("command_add", "submit") => str_field(value, "value").map_or_else(Vec::new, |v| change(SettingsChange::SetCommand { command: v.trim().to_owned(), on: true })),
        (k, "toggle") => match (index_of(k, "cmd"), bool_field(value, "checked")) {
            (Some(i), Some(on)) => match cfg.commands.get(i) {
                Some(command) => change(SettingsChange::SetCommand { command: command.clone(), on }),
                None => Vec::new(),
            },
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn level_from(word: &str) -> Option<Level> {
    match word {
        "viewer" => Some(Level::Viewer),
        "operator" => Some(Level::Operator),
        _ => None,
    }
}

/// The index in a key such as `f3` or `cmd0`, for a prefix.
fn index_of(key: &str, prefix: &str) -> Option<usize> {
    key.strip_prefix(prefix)?.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::config::Config;

    fn live_with_host(cfg: Config, devices: Vec<DeviceView>) -> Live {
        Live {
            enabled: true,
            link: Some(Link::Up),
            host: Some(HostView { cfg, devices, slot_expires: None, restart_needed: false }),
            ..Live::default()
        }
    }

    fn view(live: &Live) -> View<'_> {
        View { live, now: 1_000, problem: None }
    }

    fn device(id: &str, name: &str, level: Level) -> DeviceView {
        DeviceView { device_id: id.into(), name: name.into(), level, paired_at: 1, last_seen: 900, notify: true, hide_names: false, push: false }
    }

    /// Every key in a tree, found by walking every object.
    fn keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(k)) = map.get("key") {
                    out.push(k.clone());
                }
                for child in map.values() {
                    keys(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|c| keys(c, out)),
            _ => {}
        }
    }

    fn assert_tree_is_sound(t: &Value) {
        let mut all = Vec::new();
        keys(t, &mut all);
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "keys are unique in a tree: {all:?}");
        for k in &all {
            assert!(!k.is_empty() && k.chars().count() <= 64 && !k.chars().any(char::is_control), "key {k:?}");
        }
    }

    fn all_screens_for(live: &Live) -> Vec<Value> {
        let v = view(live);
        let ui = Ui::default();
        SCREEN_IDS.iter().map(|s| tree(s, &v, &ui).expect("declared screen")).collect()
    }

    #[test]
    fn every_tree_has_unique_short_keys_for_every_state() {
        let mut cfg = Config { relay: "wss://relay.example/".into(), connect: true, ..Config::default() };
        cfg.folders.push(crate::config::FolderEntry { path: "/work/app".into(), pinned: true });
        cfg.profiles.push(ProfileEntry { name: "claude".into(), label: "Claude".into(), command: "claude".into() });
        let devices = vec![device("d_1", "Pixel 9", Level::Operator), device("d_2", "Old phone", Level::Viewer)];
        let mut live = live_with_host(cfg, devices);
        for step in 0..4 {
            for t in all_screens_for(&live) {
                assert_tree_is_sound(&t);
            }
            match step {
                0 => live.offer = Some("https://phone.example/v/0.1.0/#p=abc".into()),
                1 => live.pending = Some(("8216".into(), "Pixel 9".into())),
                2 => live.notice = Some("Saved.".into()),
                _ => live.link = Some(Link::Down("refused".into())),
            }
        }
    }

    #[test]
    fn the_pairing_tree_shows_the_code_only_while_a_slot_is_open() {
        let live = live_with_host(Config { relay: "wss://relay.example/".into(), ..Config::default() }, Vec::new());
        let closed = tree(PAIR, &view(&live), &Ui::default()).unwrap().to_string();
        assert!(closed.contains("Start pairing"), "{closed}");
        assert!(!closed.contains("\"qr\""), "no code without a slot");

        let mut open = live.clone();
        open.offer = Some("https://phone.example/v/0.1.0/#p=SECRET".into());
        let shown = tree(PAIR, &view(&open), &Ui::default()).unwrap();
        let text = shown.to_string();
        assert!(text.contains("\"type\":\"qr\""), "{text}");
        assert!(text.contains("#p=SECRET"), "the code carries the offer, on this screen only");
        assert!(!text.contains("Allow"), "Allow waits for a check code");
    }

    #[test]
    fn allow_shows_only_with_a_check_code_and_sends_the_level_chosen() {
        let mut live = live_with_host(Config { relay: "wss://relay.example/".into(), ..Config::default() }, Vec::new());
        live.offer = Some("https://phone.example/v/0.1.0/#p=x".into());
        live.pending = Some(("8216".into(), "Pixel 9".into()));
        let t = tree(PAIR, &view(&live), &Ui::default()).unwrap().to_string();
        assert!(t.contains("8216") && t.contains("Allow") && t.contains("Deny"));

        let mut ui = Ui::default();
        let cmds = event(PAIR, &view(&live), &mut ui, "level", "select", &json!({"value": "operator"}));
        assert!(cmds.is_empty());
        let cmds = event(PAIR, &view(&live), &mut ui, "allow", "click", &Value::Null);
        assert!(matches!(cmds.as_slice(), [Command::Decide(Some(Level::Operator))]), "{cmds:?}");
        let cmds = event(PAIR, &view(&live), &mut ui, "deny", "click", &Value::Null);
        assert!(matches!(cmds.as_slice(), [Command::Decide(None)]));
    }

    #[test]
    fn allow_and_deny_do_nothing_without_a_waiting_phone() {
        let live = live_with_host(Config { relay: "wss://relay.example/".into(), ..Config::default() }, Vec::new());
        let mut ui = Ui::default();
        assert!(event(PAIR, &view(&live), &mut ui, "allow", "click", &Value::Null).is_empty());
        assert!(event(PAIR, &view(&live), &mut ui, "deny", "click", &Value::Null).is_empty());
    }

    #[test]
    fn start_is_disabled_without_a_relay() {
        let live = live_with_host(Config::default(), Vec::new());
        let t = tree(PAIR, &view(&live), &Ui::default()).unwrap().to_string();
        assert!(t.contains("\"disabled\":true"), "{t}");
    }

    #[test]
    fn revoke_needs_its_own_second_press() {
        let live = live_with_host(Config::default(), vec![device("d_1", "Pixel 9", Level::Operator)]);
        let mut ui = Ui::default();
        assert!(event(DEVICES, &view(&live), &mut ui, "revoke", "click", &Value::Null).is_empty(), "nothing selected");
        event(DEVICES, &view(&live), &mut ui, "devices", "select", &json!({"key": "d_1"}));
        assert!(event(DEVICES, &view(&live), &mut ui, "revoke", "click", &Value::Null).is_empty(), "the first press only asks");
        assert!(ui.confirm_revoke);
        let t = tree(DEVICES, &view(&live), &ui).unwrap().to_string();
        assert!(t.contains("Yes, revoke"), "{t}");
        let cmds = event(DEVICES, &view(&live), &mut ui, "revoke_yes", "click", &Value::Null);
        assert!(matches!(cmds.as_slice(), [Command::Revoke(id)] if id == "d_1"));
        assert!(ui.device.is_none());
    }

    #[test]
    fn a_level_or_flag_change_names_the_selected_phone() {
        let live = live_with_host(Config::default(), vec![device("d_1", "Pixel 9", Level::Viewer)]);
        let mut ui = Ui::default();
        assert!(event(DEVICES, &view(&live), &mut ui, "level", "select", &json!({"value": "operator"})).is_empty());
        event(DEVICES, &view(&live), &mut ui, "devices", "select", &json!({"key": "d_1"}));
        let cmds = event(DEVICES, &view(&live), &mut ui, "level", "select", &json!({"value": "operator"}));
        assert!(matches!(cmds.as_slice(), [Command::SetLevel(id, Level::Operator)] if id == "d_1"));
        let cmds = event(DEVICES, &view(&live), &mut ui, "notify", "toggle", &json!({"checked": false}));
        assert!(matches!(cmds.as_slice(), [Command::SetDeviceFlag(Some(id), DeviceFlag::Notify(false))] if id == "d_1"));
    }

    #[test]
    fn settings_events_become_validated_changes_not_raw_writes() {
        let live = live_with_host(Config::default(), Vec::new());
        let mut ui = Ui::default();
        let cmds = event(SETTINGS, &view(&live), &mut ui, "relay", "submit", &json!({"value": "wss://relay.example/"}));
        assert!(matches!(cmds.as_slice(), [Command::Settings(SettingsChange::SetRelay(u))] if u == "wss://relay.example/"));
        let cmds = event(SETTINGS, &view(&live), &mut ui, "max_sessions", "select", &json!({"value": "7"}));
        assert!(matches!(cmds.as_slice(), [Command::Settings(SettingsChange::SetMaxSessions(7))]));
        let cmds = event(SETTINGS, &view(&live), &mut ui, "relay", "submit", &json!({"value": "x".repeat(MAX_EVENT_TEXT + 1)}));
        assert!(cmds.is_empty(), "an oversize value is ignored");
        let cmds = event(SETTINGS, &view(&live), &mut ui, "nonsense", "click", &Value::Null);
        assert!(cmds.is_empty());
    }

    #[test]
    fn a_command_checkbox_maps_to_the_command_it_shows() {
        let cfg = Config { commands: vec!["/remote-control".into(), "/clear".into()], ..Config::default() };
        let live = live_with_host(cfg, Vec::new());
        let mut ui = Ui::default();
        let cmds = event(SETTINGS, &view(&live), &mut ui, "cmd1", "toggle", &json!({"checked": false}));
        assert!(matches!(cmds.as_slice(), [Command::Settings(SettingsChange::SetCommand { command, on: false })] if command == "/clear"));
    }

    #[test]
    fn a_damaged_config_is_said_on_the_settings_screen() {
        let cfg = Config { damaged: true, ..Config::default() };
        let live = live_with_host(cfg, Vec::new());
        let t = tree(SETTINGS, &view(&live), &Ui::default()).unwrap().to_string();
        assert!(t.contains("config.json is damaged"), "{t}");
    }

    #[test]
    fn a_store_problem_is_the_screens_message_and_no_list_is_faked() {
        let live = Live::default();
        let v = View { live: &live, now: 0, problem: Some("devices.json is damaged") };
        let t = tree(DEVICES, &v, &Ui::default()).unwrap().to_string();
        assert!(t.contains("devices.json is damaged"), "{t}");
    }

    #[test]
    fn the_status_line_says_what_is_true() {
        let mut live = live_with_host(Config { relay: "wss://relay.example/".into(), connect: true, ..Config::default() }, vec![device("d_1", "A", Level::Viewer), device("d_2", "B", Level::Viewer)]);
        assert_eq!(status_line(&view(&live)), ("Connected to relay, 2 phones".to_owned(), "ok"));
        live.pending = Some(("1234".into(), "C".into()));
        assert_eq!(status_line(&view(&live)).1, "warn");
        live.link = Some(Link::Down("refused".into()));
        assert!(status_line(&view(&live)).0.starts_with("Relay unreachable"));
        live.enabled = false;
        assert_eq!(status_line(&view(&live)), ("Phone remote is off".to_owned(), "idle"));
        let stopped = View { live: &live, now: 0, problem: Some("devices.json is damaged") };
        assert_eq!(status_line(&stopped).1, "error");
    }

    #[test]
    fn the_status_line_is_within_csm_limits() {
        let live = live_with_host(Config { relay: "wss://relay.example/".into(), connect: true, ..Config::default() }, Vec::new());
        let long = View { live: &live, now: 0, problem: Some(&"x".repeat(500)) };
        let (text, _) = status_line(&long);
        assert!(text.chars().count() <= 80);
    }

    #[test]
    fn absorb_follows_the_offer_and_the_waiting_phone() {
        let mut live = Live::default();
        live.absorb(&Out::PairingUrl("u".into()));
        live.absorb(&Out::Pending { check: "1".into(), name: "n".into() });
        assert!(live.offer.is_some() && live.pending.is_some());
        live.absorb(&Out::PairingEnded);
        assert!(live.offer.is_none() && live.pending.is_none());
    }

    #[test]
    fn ago_reads_in_plain_words() {
        assert_eq!(ago(1000, 990), "just now");
        assert_eq!(ago(1000, 700), "5 min ago");
        assert_eq!(ago(50_000, 1000), "13 h ago");
        assert_eq!(ago(100_000, 1000), "1 day ago");
        assert_eq!(ago(300_000, 1000), "3 days ago");
    }
}
