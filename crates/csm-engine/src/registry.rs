//! The list of tools, checked once at start-up.

use std::collections::HashMap;

use crate::tool::{ToolError, ToolSpec};

#[derive(Debug)]
pub struct Registry {
    specs: Vec<ToolSpec>,
    by_id: HashMap<&'static str, usize>,
}

impl Registry {
    /// Fails on a duplicate or malformed id, so a bad registration is caught by any test that
    /// starts an engine.
    pub fn new(specs: Vec<ToolSpec>) -> Result<Registry, ToolError> {
        let mut by_id = HashMap::new();
        for (i, s) in specs.iter().enumerate() {
            let ok = !s.id.is_empty()
                && s.id.len() <= 64
                && s.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && !s.id.starts_with('_')
                && !s.id.ends_with('_');
            if !ok {
                return Err(ToolError::Failed(format!("tool id `{}` is not snake_case", s.id)));
            }
            if by_id.insert(s.id, i).is_some() {
                return Err(ToolError::Failed(format!("tool id `{}` is registered twice", s.id)));
            }
        }
        Ok(Registry { specs, by_id })
    }

    pub fn get(&self, id: &str) -> Option<&ToolSpec> {
        self.by_id.get(id).and_then(|&i| self.specs.get(i))
    }

    /// In registration order.
    pub fn all(&self) -> &[ToolSpec] {
        &self.specs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Effects, ToolOutput, Toolset, empty_schema};

    fn spec(id: &'static str) -> ToolSpec {
        ToolSpec {
            id,
            title: "t",
            summary: "s",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: |_, _| Ok(ToolOutput::message("")),
        }
    }

    #[test]
    fn rejects_duplicates_and_bad_ids() {
        assert!(Registry::new(vec![spec("a_b"), spec("a_b")]).is_err());
        assert!(Registry::new(vec![spec("A")]).is_err());
        assert!(Registry::new(vec![spec("a-b")]).is_err());
        assert!(Registry::new(vec![spec("_a")]).is_err());
        let r = Registry::new(vec![spec("a_b"), spec("c")]).unwrap();
        assert_eq!(r.get("c").map(|s| s.id), Some("c"));
        assert!(r.get("zz").is_none());
    }

    #[test]
    fn schemas_are_objects() {
        let r = Registry::new(vec![spec("a")]).unwrap();
        for s in r.all() {
            assert_eq!((s.input_schema)()["type"], "object", "{}", s.id);
        }
    }
}
