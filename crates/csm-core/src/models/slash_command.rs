//! The Claude slash commands a Claude pane offers (WPF `CommandPresetsService.InSessionShortcuts`,
//! the ⚡ Commands panel), kept in `AppSettings::claude_commands` (`ClaudeCommands`).

use serde::{Deserialize, Serialize};

use super::serde_util::null_default as nd;
use crate::program_status_payload::is_format_char;

/// One click-to-send command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct SlashCommand {
    /// What is typed (`/compact`).
    #[serde(deserialize_with = "nd")]
    pub command: String,
    /// The menu's hint (`Compact conversation`).
    #[serde(deserialize_with = "nd")]
    pub description: String,
}

impl SlashCommand {
    /// Most entries kept.
    pub const MAX: usize = 40;
    /// Longest command, in characters.
    pub const MAX_COMMAND: usize = 200;
    const MAX_DESCRIPTION: usize = 120;

    pub fn new(command: &str, description: &str) -> Self {
        SlashCommand { command: command.to_owned(), description: description.to_owned() }
    }

    /// The WPF app's in-session shortcuts, without its `--continue` row (a launch flag with no
    /// command to type).
    pub fn defaults() -> Vec<SlashCommand> {
        [
            ("/help", "Show Claude help"),
            ("/clear", "Clear conversation"),
            ("/compact", "Compact conversation"),
            ("/status", "Show status"),
            ("/memory", "Show memory"),
            ("/init", "Initialize project"),
            ("exit", "Exit the shell"),
        ]
        .iter()
        .map(|(c, d)| SlashCommand::new(c, d))
        .collect()
    }

    /// `serde` helper: the list is written only when it differs from [`Self::defaults`].
    #[allow(clippy::ptr_arg)] // serde's skip_serializing_if passes &Vec
    pub fn is_default_list(v: &Vec<SlashCommand>) -> bool {
        *v == Self::defaults()
    }

    /// The command as it may be typed into a session: trimmed, one line, no control or
    /// invisible format characters, at most [`Self::MAX_COMMAND`] characters. `None` when it
    /// is unusable. (It is app-authored text typed into a terminal: a newline in it would be
    /// an extra Enter.)
    pub fn clean_command(command: &str) -> Option<String> {
        let c = command.trim();
        let bad = |ch: char| ch.is_control() || is_format_char(ch) || matches!(ch, '\u{2028}' | '\u{2029}');
        (!c.is_empty() && c.chars().count() <= Self::MAX_COMMAND && !c.chars().any(bad)).then(|| c.to_owned())
    }

    /// A list from a settings file or a tool call, cleaned: unusable commands and duplicates
    /// dropped, descriptions made one plain line, at most [`Self::MAX`] entries.
    pub fn clean_list(list: &[SlashCommand]) -> Vec<SlashCommand> {
        let mut out: Vec<SlashCommand> = Vec::new();
        for e in list {
            let Some(command) = Self::clean_command(&e.command) else { continue };
            if out.iter().any(|x| x.command == command) {
                continue;
            }
            let description: String = e.description.chars().filter(|c| !c.is_control() && !is_format_char(*c)).take(Self::MAX_DESCRIPTION).collect();
            out.push(SlashCommand { command, description: description.trim().to_owned() });
            if out.len() >= Self::MAX {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    #[test]
    fn defaults_follow_the_wpf_panel() {
        let d = SlashCommand::defaults();
        let cmds: Vec<&str> = d.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(cmds, ["/help", "/clear", "/compact", "/status", "/memory", "/init", "exit"]);
        assert_eq!(AppSettings::default().claude_commands, d);
    }

    #[test]
    fn the_default_list_is_not_written_and_a_custom_one_round_trips() {
        let s = AppSettings::default();
        let json = serde_json::to_value(&s).unwrap();
        assert!(json.get("ClaudeCommands").is_none(), "a WPF file stays as it was");
        let mut s2 = s.clone();
        s2.claude_commands = vec![SlashCommand::new("/review", "Review the diff")];
        let json = serde_json::to_string(&s2).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.claude_commands, s2.claude_commands);
        // An explicit null reads as empty, not as a failure.
        let back: AppSettings = serde_json::from_str(r#"{"ClaudeCommands": null}"#).unwrap();
        assert!(back.claude_commands.is_empty());
    }

    #[test]
    fn cleaning_drops_what_cannot_be_typed_safely() {
        let list = vec![
            SlashCommand::new("  /compact  ", "Compact\nconversation"),
            SlashCommand::new("/clear\r\nrm -rf ~", "two lines"),
            SlashCommand::new("/x\u{202E}y", "bidi"),
            SlashCommand::new("   ", "blank"),
            SlashCommand::new("/compact", "duplicate"),
            SlashCommand::new(&"y".repeat(201), "too long"),
        ];
        let clean = SlashCommand::clean_list(&list);
        assert_eq!(clean, vec![SlashCommand::new("/compact", "Compactconversation")]);
        let many: Vec<SlashCommand> = (0..100).map(|i| SlashCommand::new(&format!("/c{i}"), "")).collect();
        assert_eq!(SlashCommand::clean_list(&many).len(), SlashCommand::MAX);
    }
}
