//! `OpenWithEntry` (C# `Models/OpenWithEntry.cs`): the persisted shape only. Template
//! tokenising, validation and launching are the open-with crate's job.

use serde::{Deserialize, Serialize};

use super::serde_util::null_default as nd;
use super::session::SessionKind;

/// One "Open with..." target: an editor or IDE a session's folder can be opened in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct OpenWithEntry {
    #[serde(deserialize_with = "nd")]
    pub id: String,
    #[serde(deserialize_with = "nd")]
    pub label: String,
    /// Absolute path to the program. Never a shell.
    #[serde(deserialize_with = "nd")]
    pub executable: String,
    /// Arguments for a local session, e.g. `{folder}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_args: Option<String>,
    /// Exactly one entry is the default.
    pub is_default: bool,
}

impl OpenWithEntry {
    /// The argument template for a session kind.
    pub fn args_for(&self, kind: SessionKind) -> Option<&str> {
        match kind {
            SessionKind::Local => self.local_args.as_deref(),
            SessionKind::Wsl => self.wsl_args.as_deref(),
            SessionKind::Ssh => self.ssh_args.as_deref(),
        }
    }

    /// A blank template hides the entry for that kind.
    pub fn applies_to(&self, kind: SessionKind) -> bool {
        self.args_for(kind).is_some_and(|a| !a.trim().is_empty())
    }

    /// Leaves exactly one default when the list is non-empty: the first flagged, else the
    /// first entry. (Unlike run commands, the FIRST flagged one wins here.)
    pub fn ensure_single_default(list: &mut [OpenWithEntry]) {
        let mut seen = false;
        for e in list.iter_mut() {
            if e.is_default && !seen {
                seen = true;
            } else {
                e.is_default = false;
            }
        }
        if !seen && let Some(first) = list.first_mut() {
            first.is_default = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(default: bool) -> OpenWithEntry {
        OpenWithEntry {
            is_default: default,
            ..Default::default()
        }
    }

    #[test]
    fn ensure_single_default_first_flagged_wins_else_first_entry() {
        let mut list = vec![e(false), e(true), e(true)];
        OpenWithEntry::ensure_single_default(&mut list);
        assert_eq!(
            list.iter().map(|x| x.is_default).collect::<Vec<_>>(),
            [false, true, false]
        );

        let mut none = vec![e(false), e(false)];
        OpenWithEntry::ensure_single_default(&mut none);
        assert_eq!(
            none.iter().map(|x| x.is_default).collect::<Vec<_>>(),
            [true, false]
        );

        let mut empty: Vec<OpenWithEntry> = Vec::new();
        OpenWithEntry::ensure_single_default(&mut empty);
    }

    #[test]
    fn applies_to_needs_a_non_blank_template() {
        let entry = OpenWithEntry {
            local_args: Some("{folder}".into()),
            wsl_args: Some("   ".into()),
            ssh_args: None,
            ..Default::default()
        };
        assert!(entry.applies_to(SessionKind::Local));
        assert!(!entry.applies_to(SessionKind::Wsl));
        assert!(!entry.applies_to(SessionKind::Ssh));
    }
}
