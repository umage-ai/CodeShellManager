//! Crate dependencies against the layer map (`docs/plan.md` section 3).
//!
//! A crate may depend on workspace crates of a strictly lower layer, and only the UI crate and the
//! apps may use egui. A crate missing from the map is an error: classify it here.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use crate::Findings;

/// `(crate, layer)`. L0 pure core; L1 PTY and terminal model; L2 services; L3 engine; L4 front
/// ends over the engine (MCP bridge, egui UI); L5 what only programs use (`csm-setup`); L6 programs.
pub const LAYERS: &[(&str, u8)] = &[
    ("csm-core", 0),
    ("csm-remote-proto", 0), // phone remote wire protocol: pure, no csm-core dependency
    ("csm-peer-proto", 0),   // peer protocol: owned wire DTOs of the read model and events; pure
    ("csm-pty", 1),
    ("csm-term", 1),
    ("csm-services", 2),
    ("csm-index", 2), // FTS5 output index + notes; csm-core only
    ("csm-engine", 3),
    ("csm-mcp", 4),
    ("csm-ui", 4),
    ("csm-peer", 4), // peer link transport: the server (the engine's PeerHost) and the client connection; Noise over TCP
    ("csm-setup", 5),
    ("csm-remote", 5), // the remote window's client: RemoteBackend over csm-peer and csm-ui (no egui of its own)
    ("codeshell", 6),
    ("csm-cli", 6),
    ("csm-session-summary", 6),
    ("csm-review", 6),
    ("csm-legacy-launcher", 6), // CodeShellManager.exe: the 0.x exe name, starts codeshell.exe
    ("term-spike", 6),
    ("csm-relay", 6), // relay/: self-hostable phone-remote relay; speaks only csm-remote-proto
];

/// Crates allowed to depend on egui.
const EGUI_USERS: &[&str] = &["csm-ui", "codeshell", "term-spike"];
const EGUI_FAMILY: &[&str] = &["egui", "eframe", "egui_extras", "egui-wgpu", "egui-winit", "egui_glow"];

/// Tooling crates outside the map.
const EXEMPT: &[&str] = &["xtask"];

pub fn check() -> Result<Findings, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(crate::root())
        .output()
        .map_err(|e| format!("could not run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bad cargo metadata: {e}"))?;
    let packages = meta["packages"].as_array().ok_or("cargo metadata has no packages")?;
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in packages {
        let name = p["name"].as_str().unwrap_or_default().to_owned();
        let list = p["dependencies"].as_array().into_iter().flatten();
        // Dev-dependencies are checked too: a test may not reach upward either.
        deps.insert(name, list.filter_map(|d| d["name"].as_str().map(str::to_owned)).collect());
    }
    Ok(evaluate(&deps))
}

/// The rule itself, on a dependency map (tested without cargo).
pub fn evaluate(deps: &BTreeMap<String, BTreeSet<String>>) -> Findings {
    let layer = |n: &str| LAYERS.iter().find(|(c, _)| *c == n).map(|(_, l)| *l);
    let mut problems = Vec::new();
    for (name, wanted) in deps {
        if EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let Some(mine) = layer(name) else {
            problems.push(format!("{name}: not in the layer map (xtask/src/layers.rs); say which layer it belongs to"));
            continue;
        };
        for dep in wanted {
            if let Some(theirs) = layer(dep)
                && theirs >= mine
            {
                problems.push(format!("{name} (L{mine}) depends on {dep} (L{theirs}): a crate may only depend on a lower layer"));
            }
            if EGUI_FAMILY.contains(&dep.as_str()) && !EGUI_USERS.contains(&name.as_str()) {
                problems.push(format!("{name} depends on {dep}: only csm-ui and the apps may use egui"));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs.iter().map(|(n, d)| ((*n).to_owned(), d.iter().map(|s| (*s).to_owned()).collect())).collect()
    }

    #[test]
    fn lower_layers_are_fine_and_upward_or_sideways_is_not() {
        assert!(evaluate(&map(&[("csm-engine", &["csm-core", "csm-pty", "serde"]), ("csm-core", &[])])).is_empty());
        let up = evaluate(&map(&[("csm-core", &["csm-engine"])]));
        assert!(up.iter().any(|p| p.contains("lower layer")), "{up:?}");
        let side = evaluate(&map(&[("csm-pty", &["csm-term"])]));
        assert_eq!(side.len(), 1, "{side:?}");
    }

    #[test]
    fn egui_stays_in_the_ui_and_apps() {
        assert!(evaluate(&map(&[("csm-ui", &["egui"]), ("codeshell", &["eframe"])])).is_empty());
        assert_eq!(evaluate(&map(&[("csm-engine", &["egui"])])).len(), 1);
    }

    #[test]
    fn an_unclassified_crate_is_reported_and_xtask_is_exempt() {
        assert_eq!(evaluate(&map(&[("csm-new", &[])])).len(), 1);
        assert!(evaluate(&map(&[("xtask", &["csm-engine"])])).is_empty());
    }
}
