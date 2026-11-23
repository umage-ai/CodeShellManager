//! The changes the person makes on the settings screen (`phone-remote.settings`, design note
//! section 10.6), as pure functions over the config.
//!
//! Each change is applied to a copy of the config. The copy is written out and read back through
//! [`config::parse`]: only a copy that comes back with no problems is accepted. So a value the rules
//! refuse never reaches `config.json`, and a refused change writes nothing.

use std::path::Path;

use crate::config::{self, Config, FolderEntry, ProfileEntry, MAX_PHONE_SESSIONS_RANGE};
use crate::policy::canon_folder;

/// Profile names: 1 to 32 characters, letters, digits and `-` (section 10.6).
const MAX_PROFILE_NAME: usize = 32;
const MAX_PROFILE_LABEL: usize = 64;
const MAX_PROFILE_COMMAND: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsChange {
    SetRelay(String),
    SetAppOrigin(String),
    SetConnect(bool),
    SetPush(bool),
    SetMaxSessions(u32),
    AddFolder(String),
    RemoveFolder(String),
    SetFolderPinned { path: String, pinned: bool },
    AddProfile(ProfileEntry),
    RemoveProfile(String),
    /// Adds or removes one slash command from the add-on's own allow list.
    SetCommand { command: String, on: bool },
}

/// The config after `change`, or the reason it is refused. Nothing is changed on a refusal.
pub fn apply(cfg: &Config, change: &SettingsChange) -> Result<Config, String> {
    let mut next = cfg.clone();
    match change {
        SettingsChange::SetRelay(s) => next.relay = s.trim().to_owned(),
        SettingsChange::SetAppOrigin(s) => next.app_origin = s.trim().to_owned(),
        SettingsChange::SetConnect(on) => next.connect = *on,
        SettingsChange::SetPush(on) => next.push = *on,
        SettingsChange::SetMaxSessions(n) => {
            if !MAX_PHONE_SESSIONS_RANGE.contains(n) {
                return Err("the live-session limit must be 1 to 20".into());
            }
            next.max_phone_sessions = *n;
        }
        SettingsChange::AddFolder(p) => add_folder(&mut next, p.trim())?,
        SettingsChange::RemoveFolder(p) => next.folders.retain(|f| f.path != *p),
        SettingsChange::SetFolderPinned { path, pinned } => {
            for f in next.folders.iter_mut().filter(|f| f.path == *path) {
                f.pinned = *pinned;
            }
        }
        SettingsChange::AddProfile(p) => add_profile(&mut next, p)?,
        SettingsChange::RemoveProfile(name) => next.profiles.retain(|p| p.name != *name),
        SettingsChange::SetCommand { command, on } => {
            if *on {
                if !next.commands.contains(command) {
                    next.commands.push(command.clone());
                }
            } else {
                next.commands.retain(|c| c != command);
            }
        }
    }
    finish(next)
}

fn add_folder(next: &mut Config, path: &str) -> Result<(), String> {
    let key = canon_folder(path).ok_or("a folder must be an absolute path with no .. in it")?;
    if !Path::new(path).is_dir() {
        return Err("that folder does not exist on this PC".into());
    }
    if next.folders.iter().any(|f| canon_folder(&f.path).is_some_and(|k| k.key == key.key)) {
        return Err("that folder is already on the list".into());
    }
    next.folders.push(FolderEntry { path: path.to_owned(), pinned: false });
    Ok(())
}

fn add_profile(next: &mut Config, p: &ProfileEntry) -> Result<(), String> {
    let name_ok = !p.name.is_empty() && p.name.chars().count() <= MAX_PROFILE_NAME && p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !name_ok {
        return Err("a profile name is 1 to 32 letters, digits or hyphens".into());
    }
    let label = p.label.trim();
    if label.is_empty() || label.chars().count() > MAX_PROFILE_LABEL || label.chars().any(char::is_control) {
        return Err("a profile label is 1 to 64 characters with no control characters".into());
    }
    let command = p.command.trim();
    if command.is_empty() || command.len() > MAX_PROFILE_COMMAND || command.chars().any(char::is_control) {
        return Err("a profile command is 1 to 512 characters with no control characters".into());
    }
    if next.profiles.iter().any(|x| x.name == p.name) {
        return Err("a profile with that name already exists".into());
    }
    next.profiles.push(ProfileEntry { name: p.name.clone(), label: label.to_owned(), command: command.to_owned() });
    Ok(())
}

/// Writes the copy out and reads it back. Any problem the rules find (a relay that is not wss, a
/// command that is not a slash command, ...) refuses the whole change.
fn finish(next: Config) -> Result<Config, String> {
    let text = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    let parsed = config::parse(&text);
    if parsed.problems.is_empty() { Ok(parsed) } else { Err(parsed.problems.join("; ")) }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn base() -> Config {
        Config::default()
    }

    #[test]
    fn a_wss_relay_is_accepted_and_a_plain_remote_one_is_refused_with_nothing_changed() {
        let cfg = base();
        let ok = apply(&cfg, &SettingsChange::SetRelay("wss://relay.example/".into())).unwrap();
        assert_eq!(ok.relay, "wss://relay.example/");
        let err = apply(&cfg, &SettingsChange::SetRelay("ws://relay.example/".into())).unwrap_err();
        assert!(err.contains("relay must be wss"), "{err}");
    }

    #[test]
    fn the_app_origin_must_be_https_or_loopback() {
        assert!(apply(&base(), &SettingsChange::SetAppOrigin("https://phone.example".into())).is_ok());
        assert!(apply(&base(), &SettingsChange::SetAppOrigin("http://127.0.0.1:8080".into())).is_ok());
        assert!(apply(&base(), &SettingsChange::SetAppOrigin("http://phone.example".into())).is_err());
    }

    #[test]
    fn the_live_session_limit_stays_in_its_range() {
        assert_eq!(apply(&base(), &SettingsChange::SetMaxSessions(20)).unwrap().max_phone_sessions, 20);
        assert!(apply(&base(), &SettingsChange::SetMaxSessions(0)).is_err());
        assert!(apply(&base(), &SettingsChange::SetMaxSessions(21)).is_err());
    }

    #[test]
    fn a_folder_must_exist_and_be_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        let cfg = apply(&base(), &SettingsChange::AddFolder(path.clone())).unwrap();
        assert_eq!(cfg.folders.len(), 1);
        let again = apply(&cfg, &SettingsChange::AddFolder(path.clone()));
        assert!(again.is_err(), "the same folder twice is refused");
        let missing = dir.path().join("not-there").to_string_lossy().to_string();
        assert!(apply(&base(), &SettingsChange::AddFolder(missing)).is_err());
        assert!(apply(&base(), &SettingsChange::AddFolder(format!("{path}/../x"))).is_err(), "a .. path is refused");
    }

    #[test]
    fn pinning_and_removing_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        let cfg = apply(&base(), &SettingsChange::AddFolder(path.clone())).unwrap();
        let pinned = apply(&cfg, &SettingsChange::SetFolderPinned { path: path.clone(), pinned: true }).unwrap();
        assert!(pinned.folders[0].pinned);
        let removed = apply(&pinned, &SettingsChange::RemoveFolder(path)).unwrap();
        assert!(removed.folders.is_empty());
    }

    #[test]
    fn profiles_are_checked_and_unique_by_name() {
        let p = ProfileEntry { name: "claude-1".into(), label: "Claude".into(), command: "claude".into() };
        let cfg = apply(&base(), &SettingsChange::AddProfile(p.clone())).unwrap();
        assert_eq!(cfg.profiles.len(), 1);
        assert!(apply(&cfg, &SettingsChange::AddProfile(p)).is_err(), "a second profile with the name is refused");
        let bad_name = ProfileEntry { name: "has space".into(), label: "x".into(), command: "x".into() };
        assert!(apply(&base(), &SettingsChange::AddProfile(bad_name)).is_err());
        let bad_cmd = ProfileEntry { name: "ok".into(), label: "x".into(), command: "a\nb".into() };
        assert!(apply(&base(), &SettingsChange::AddProfile(bad_cmd)).is_err());
        let removed = apply(&cfg, &SettingsChange::RemoveProfile("claude-1".into())).unwrap();
        assert!(removed.profiles.is_empty());
    }

    #[test]
    fn commands_are_slash_commands_only() {
        let on = apply(&base(), &SettingsChange::SetCommand { command: "/clear".into(), on: true }).unwrap();
        assert!(on.commands.contains(&"/clear".to_owned()));
        assert!(apply(&base(), &SettingsChange::SetCommand { command: "clear".into(), on: true }).is_err(), "not a slash command");
        let off = apply(&on, &SettingsChange::SetCommand { command: "/clear".into(), on: false }).unwrap();
        assert!(!off.commands.contains(&"/clear".to_owned()));
    }

    #[test]
    fn a_change_to_a_damaged_config_starts_from_the_defaults() {
        let damaged = config::parse("{not json");
        assert!(damaged.damaged);
        let next = apply(&damaged, &SettingsChange::SetPush(true)).unwrap();
        assert!(next.push);
        assert!(!next.damaged, "the written copy is a fresh, undamaged file");
    }
}
