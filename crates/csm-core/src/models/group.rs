//! `SessionGroup` (C# `Models/SessionGroup.cs`).

use serde::{Deserialize, Serialize};

use super::serde_util::null_default as nd;

fn default_name() -> String {
    "Default".to_owned()
}

fn default_true() -> bool {
    true
}

/// A named group of sessions in the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SessionGroup {
    #[serde(deserialize_with = "nd")]
    pub id: String,
    pub name: String,
    pub sort_order: i32,
    #[serde(default = "default_true")]
    pub is_expanded: bool,
}

impl Default for SessionGroup {
    /// C# also mints a GUID `Id`; this crate has no random source, so `id` is left to the
    /// caller.
    fn default() -> Self {
        Self { id: String::new(), name: default_name(), sort_order: 0, is_expanded: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_csharp_initialisers() {
        let g: SessionGroup = serde_json::from_str(r#"{"Id":"g"}"#).unwrap();
        assert_eq!((g.name.as_str(), g.sort_order, g.is_expanded), ("Default", 0, true));
    }
}
