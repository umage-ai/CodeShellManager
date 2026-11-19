//! `RunCommandItem` (C# `Models/RunCommandItem.cs`).

use serde::{Deserialize, Serialize};

use super::serde_util::{null_default as nd, numeric_enum};

numeric_enum! {
    /// How a run command's command line is launched. `Process` = `cmd /c`, the historical
    /// default; `PowerShell` = `pwsh -EncodedCommand`. SSH and WSL parents ignore it.
    pub enum RunMode { Process = 0, PowerShell = 1 }
    default = Process;
}

/// One configured "run" command on a session. Exactly one item of a non-empty list is the
/// default (the toolbar play button and F5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct RunCommandItem {
    #[serde(deserialize_with = "nd")]
    pub id: String,
    #[serde(deserialize_with = "nd")]
    pub label: String,
    #[serde(deserialize_with = "nd")]
    pub command_line: String,
    pub is_default: bool,
    /// Defaults to `Process` for state files that predate the field.
    pub mode: RunMode,
    /// Opened in the browser when the command exits 0. Only absolute http/https URLs may
    /// ever be launched; that gate lives with the launcher, not in the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_run_url: Option<String>,
}

impl RunCommandItem {
    /// Normalises the list so exactly one item has `is_default` (when non-empty). If several
    /// are marked the LAST wins (the editor's "click to promote" makes the latest action
    /// authoritative); if none is, the first is promoted.
    pub fn ensure_single_default(items: &mut [RunCommandItem]) {
        if items.is_empty() {
            return;
        }
        let keep = items.iter().rposition(|i| i.is_default).unwrap_or(0);
        for (i, item) in items.iter_mut().enumerate() {
            item.is_default = i == keep;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, cmd: &str, default: bool) -> RunCommandItem {
        RunCommandItem {
            label: label.into(),
            command_line: cmd.into(),
            is_default: default,
            ..Default::default()
        }
    }

    #[test]
    fn ensure_single_default_promotes_first_when_none_marked() {
        let mut list = vec![
            item("run", "dotnet run", false),
            item("test", "dotnet test", false),
        ];
        RunCommandItem::ensure_single_default(&mut list);
        assert!(list[0].is_default);
        assert!(!list[1].is_default);
    }

    #[test]
    fn ensure_single_default_keeps_last_true_when_multiple_marked() {
        let mut list = vec![
            item("a", "x", true),
            item("b", "y", true),
            item("c", "z", false),
        ];
        RunCommandItem::ensure_single_default(&mut list);
        // The LAST-marked default wins (matches the dialog's click-to-promote behaviour).
        assert!(!list[0].is_default);
        assert!(list[1].is_default);
        assert!(!list[2].is_default);
    }

    #[test]
    fn ensure_single_default_empty_list_is_a_no_op() {
        let mut list: Vec<RunCommandItem> = Vec::new();
        RunCommandItem::ensure_single_default(&mut list);
        assert!(list.is_empty());
    }

    #[test]
    fn defaults_mode_is_process_and_post_run_url_is_none() {
        // Back-compat: legacy state.json entries lacking Mode/PostRunUrl keep working.
        let item = RunCommandItem::default();
        assert_eq!(item.mode, RunMode::Process);
        assert_eq!(item.post_run_url, None);
        let parsed: RunCommandItem = serde_json::from_str(
            r#"{"Id":"r","Label":"Run","CommandLine":"npm run dev","IsDefault":true}"#,
        )
        .unwrap();
        assert_eq!(parsed.mode, RunMode::Process);
        assert_eq!(parsed.post_run_url, None);
    }

    #[test]
    fn mode_is_stored_as_a_number_and_url_omitted_when_none() {
        let json = serde_json::to_string(&RunCommandItem {
            mode: RunMode::PowerShell,
            ..Default::default()
        })
        .unwrap();
        assert!(json.contains(r#""Mode":1"#), "{json}");
        assert!(!json.contains("PostRunUrl"), "{json}");
    }
}
