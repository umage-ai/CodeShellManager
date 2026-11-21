//! The list of tools, checked once at start-up, plus the process add-on tools that come and go
//! (docs/addons.md). A registry is immutable: a change builds a new one (`with_dynamic`) and the
//! engine swaps it in, so a reader always sees one whole list.

use std::collections::HashMap;

use serde_json::Value;

use crate::tool::{ToolError, ToolSpec};

/// A tool a process add-on provides, as the registry holds it: its spec and its input schema
/// (a spec's `input_schema` is a plain `fn`, so a dynamic tool's schema is kept beside it).
#[derive(Debug, Clone)]
pub struct DynamicTool {
    pub spec: ToolSpec,
    pub schema: Value,
}

#[derive(Debug)]
pub struct Registry {
    specs: Vec<ToolSpec>,
    by_id: HashMap<&'static str, usize>,
    /// How many of `specs` are built in; the rest are dynamic (process add-on tools).
    base: usize,
    /// Schemas of the dynamic tools, by id.
    schemas: HashMap<&'static str, Value>,
}

/// Dynamic tool ids carry the add-on prefix: `addon.name`. Built-in ids never contain a dot.
pub fn is_add_on_tool_id(id: &str) -> bool {
    id.contains('.')
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
        let base = specs.len();
        Ok(Registry { specs, by_id, base, schemas: HashMap::new() })
    }

    /// A new registry: this one's built-in tools, then `dynamic` (process add-on tools). Each
    /// dynamic id must be `addon.name` and not clash with another tool; a clash refuses the
    /// whole set, so a bad add-on cannot shadow a built-in tool.
    pub fn with_dynamic(&self, dynamic: &[DynamicTool]) -> Result<Registry, ToolError> {
        let mut specs: Vec<ToolSpec> = self.specs.iter().take(self.base).copied().collect();
        let mut by_id = self.by_id.clone();
        by_id.retain(|_, i| *i < self.base);
        let mut schemas = HashMap::new();
        for d in dynamic {
            let id = d.spec.id;
            if !is_add_on_tool_id(id) {
                return Err(ToolError::Failed(format!("add-on tool id `{id}` has no `addon.` prefix")));
            }
            if by_id.contains_key(id) {
                return Err(ToolError::Failed(format!("tool id `{id}` is registered twice")));
            }
            by_id.insert(id, specs.len());
            specs.push(d.spec);
            schemas.insert(id, d.schema.clone());
        }
        Ok(Registry { specs, by_id, base: self.base, schemas })
    }

    pub fn get(&self, id: &str) -> Option<&ToolSpec> {
        self.by_id.get(id).and_then(|&i| self.specs.get(i))
    }

    /// The input schema of a tool in this registry (a dynamic tool's own, else its `fn`).
    pub fn schema(&self, spec: &ToolSpec) -> Value {
        self.schemas.get(spec.id).cloned().unwrap_or_else(|| (spec.input_schema)())
    }

    /// In registration order: built-in tools first, then the add-on tools.
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
    fn dynamic_tools_are_added_and_replaced_without_touching_the_base() {
        let base = Registry::new(vec![spec("a_b")]).unwrap();
        let dynamic = |id: &'static str| DynamicTool { spec: spec(id), schema: serde_json::json!({"type": "object", "x": 1}) };
        let r = base.with_dynamic(&[dynamic("demo.echo")]).unwrap();
        assert_eq!(r.all().len(), 2);
        assert!(r.get("demo.echo").is_some());
        assert_eq!(r.schema(r.get("demo.echo").unwrap())["x"], 1);
        // Rebuilding from the same base drops the old dynamic tools.
        let r2 = r.with_dynamic(&[]).unwrap();
        assert_eq!(r2.all().len(), 1);
        assert!(r2.get("demo.echo").is_none());
        // A dynamic tool cannot shadow a built-in one or lack the prefix.
        assert!(base.with_dynamic(&[dynamic("a_b")]).is_err());
        assert!(base.with_dynamic(&[dynamic("echo")]).is_err());
        assert!(base.with_dynamic(&[dynamic("demo.x"), dynamic("demo.x")]).is_err());
    }

    #[test]
    fn schemas_are_objects() {
        let r = Registry::new(vec![spec("a")]).unwrap();
        for s in r.all() {
            assert_eq!((s.input_schema)()["type"], "object", "{}", s.id);
        }
    }
}
