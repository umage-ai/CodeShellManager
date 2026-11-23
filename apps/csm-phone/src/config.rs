//! The add-on's own settings (`config.json` in its data folder): the relay, the folders a phone
//! may open sessions in, the profiles, the slash commands it may send, push and the launch cap.
//!
//! Every value that reaches a policy decision is checked here once, when the file loads, so the
//! rest of the program can trust it. A value that fails a check falls back to its default and is
//! listed in [`Config::problems`]; nothing is refused silently.

use serde::{Deserialize, Serialize};

use crate::policy::{Folder, canon_folder, clean_name, cut};

/// The origin the offer's link points at when `config.json` names none. A placeholder until the
/// owner fixes the umage static domain (design note, section 13, decision 11).
pub const DEFAULT_APP_ORIGIN: &str = "https://phone.umage.example";
/// Push services that may receive a push (section 4.1; the relay keeps its own list).
pub const DEFAULT_PUSH_HOSTS: &[&str] = &["fcm.googleapis.com", "updates.push.services.mozilla.com", "web.push.apple.com", "*.notify.windows.com"];
pub const MAX_PHONE_SESSIONS_RANGE: std::ops::RangeInclusive<u32> = 1..=20;
pub const DEFAULT_MAX_PHONE_SESSIONS: u32 = 5;
pub const MAX_HOST_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderEntry {
    pub path: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileEntry {
    /// The name a phone sends. Short, plain, unique.
    pub name: String,
    /// What a phone shows.
    pub label: String,
    /// The command on this PC (never sent to a phone).
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `wss://host/...`, or `ws://127.0.0.1...` only with a 127.0.0.1 `app_origin` (dev setup).
    pub relay: String,
    /// The origin of the phone web app whose link the QR code carries.
    pub app_origin: String,
    /// The second switch: nothing connects unless this is true as well as the add-on being on.
    pub connect: bool,
    pub folders: Vec<FolderEntry>,
    pub profiles: Vec<ProfileEntry>,
    /// Slash commands a phone may send (exact spellings, each starting with a slash).
    pub commands: Vec<String>,
    /// Whether "needs you" pushes are sent at all.
    pub push: bool,
    pub push_hosts: Vec<String>,
    pub max_phone_sessions: u32,
    pub host_name: String,
    /// Problems found when the file loaded or validated (not written back).
    #[serde(skip)]
    pub problems: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            relay: String::new(),
            app_origin: DEFAULT_APP_ORIGIN.to_owned(),
            connect: false,
            folders: Vec::new(),
            profiles: Vec::new(),
            commands: vec!["/remote-control".to_owned()],
            push: false,
            push_hosts: DEFAULT_PUSH_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
            max_phone_sessions: DEFAULT_MAX_PHONE_SESSIONS,
            host_name: String::new(),
            problems: Vec::new(),
        }
    }
}

/// A `config.json` that does not parse is replaced by defaults for the run (section 5).
pub fn parse(text: &str) -> Config {
    let mut cfg: Config = match serde_json::from_str(text) {
        Ok(c) => c,
        Err(e) => {
            let mut d = Config::default();
            d.problems.push(format!("config.json does not parse ({:?}); the defaults are in use", e.classify()));
            return d;
        }
    };
    cfg.validate();
    cfg
}

impl Config {
    /// Checks each value, resetting the ones that fail to their defaults and recording why.
    pub fn validate(&mut self) {
        let mut problems = Vec::new();
        if !self.app_origin.starts_with("https://") && !is_loopback_origin(&self.app_origin) {
            problems.push("app_origin must be https://, or http://127.0.0.1:<port>; the default is in use".to_owned());
            self.app_origin = DEFAULT_APP_ORIGIN.to_owned();
        }
        if !self.relay.is_empty() && !self.relay_allowed() {
            problems.push("relay must be wss://, or ws://127.0.0.1 with a 127.0.0.1 app_origin; the relay is not used".to_owned());
            self.relay.clear();
        }
        if !MAX_PHONE_SESSIONS_RANGE.contains(&self.max_phone_sessions) {
            problems.push(format!("max_phone_sessions must be 1 to 20; {DEFAULT_MAX_PHONE_SESSIONS} is in use"));
            self.max_phone_sessions = DEFAULT_MAX_PHONE_SESSIONS;
        }
        let mut folders = Vec::new();
        for f in std::mem::take(&mut self.folders) {
            match canon_folder(&f.path) {
                Some(_) => folders.push(f),
                None => problems.push(format!("folder {:?} is not an absolute path without .. and is left out", cut(&f.path, 80))),
            }
        }
        self.folders = folders;
        self.commands.retain(|c| {
            let ok = c.starts_with('/') && c.len() <= 64 && !c.contains(char::is_whitespace);
            if !ok {
                problems.push(format!("command {:?} is not a slash command and is left out", cut(c, 40)));
            }
            ok
        });
        self.push_hosts.retain(|h| !h.is_empty() && h.len() <= 253 && !h.contains(['/', ':', ' ']));
        self.host_name = clean_name(&self.host_name, MAX_HOST_NAME_CHARS);
        self.problems.extend(problems);
    }

    /// Whether the configured relay URL is one this add-on may connect to.
    pub fn relay_allowed(&self) -> bool {
        if self.relay.starts_with("wss://") {
            return true;
        }
        self.relay.starts_with("ws://127.0.0.1") && is_loopback_origin(&self.app_origin)
    }

    /// The folder a phone's request names, if it is on the allow list. Exact match on canonical
    /// paths: a prefix, a `..` path or a different case (on Windows, the same folder) does not match.
    pub fn folder_for(&self, requested: &str) -> Option<Folder> {
        let key = canon_folder(requested)?;
        self.folders.iter().find_map(|f| {
            let k = canon_folder(&f.path)?;
            (k.key == key.key).then(|| Folder { spelling: f.path.clone(), pinned: f.pinned, key: k.key })
        })
    }

    /// The commands a phone may send: the PC's list (`settings_get ClaudeCommands`) intersected
    /// with the add-on's allow list.
    pub fn allowed_commands(&self, pc_commands: &[String]) -> Vec<String> {
        self.commands.iter().filter(|c| pc_commands.iter().any(|p| p == *c)).cloned().collect()
    }
}

fn is_loopback_origin(origin: &str) -> bool {
    origin.strip_prefix("http://127.0.0.1:").is_some_and(|port| !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_off() {
        let c = Config::default();
        assert!(!c.connect);
        assert!(c.relay.is_empty());
        assert!(c.folders.is_empty());
        assert_eq!(c.commands, vec!["/remote-control".to_owned()]);
        assert_eq!(c.max_phone_sessions, 5);
    }

    #[test]
    fn plain_ws_relay_is_refused_unless_loopback_dev_origin() {
        let mut c = Config { relay: "ws://relay.example/".into(), ..Config::default() };
        c.validate();
        assert!(c.relay.is_empty(), "a remote ws:// relay must never be used");
        assert!(!c.problems.is_empty());

        let mut dev = Config { relay: "ws://127.0.0.1:9000".into(), app_origin: "http://127.0.0.1:8080".into(), ..Config::default() };
        dev.validate();
        assert_eq!(dev.relay, "ws://127.0.0.1:9000");
    }

    #[test]
    fn bad_values_fall_back_and_are_reported() {
        let c = parse(r#"{"max_phone_sessions": 99, "app_origin": "http://evil.example", "commands": ["/x", "help"]}"#);
        assert_eq!(c.max_phone_sessions, 5);
        assert_eq!(c.app_origin, DEFAULT_APP_ORIGIN);
        assert_eq!(c.commands, vec!["/x".to_owned()]);
        assert_eq!(c.problems.len(), 3);
    }

    #[test]
    fn damaged_text_gives_defaults_not_a_panic() {
        for text in ["", "{", "[]", "{\"folders\": 3}", "\u{0}\u{1}"] {
            let c = parse(text);
            assert!(!c.connect);
        }
    }

    #[test]
    fn folder_matching_is_exact() {
        let c = Config { folders: vec![FolderEntry { path: "/work/app".into(), pinned: true }], ..Config::default() };
        assert_eq!(c.folder_for("/work/app").map(|f| f.spelling), Some("/work/app".to_owned()));
        assert_eq!(c.folder_for("/work/app/").map(|f| f.spelling), Some("/work/app".to_owned()));
        assert!(c.folder_for("/work/app/sub").is_none(), "a prefix never matches");
        assert!(c.folder_for("/work/ap").is_none());
        assert!(c.folder_for("/work/app/../other").is_none(), "a .. path is refused");
        assert!(c.folder_for("work/app").is_none(), "a relative path is refused");
    }

    #[test]
    fn allowed_commands_intersect_the_pc_list() {
        let c = Config { commands: vec!["/remote-control".into(), "/clear".into()], ..Config::default() };
        let pc = vec!["/remote-control".to_owned(), "/help".to_owned()];
        assert_eq!(c.allowed_commands(&pc), vec!["/remote-control".to_owned()]);
    }
}
