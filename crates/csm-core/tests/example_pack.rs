//! The shipped example add-on parses with no problems (docs/addons/examples).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_core::addons::{AddonKind, ProfileStatus, ResumeRule, parse};

#[test]
fn the_generic_agent_example_parses_clean() {
    let text = include_str!("../../../docs/addons/examples/generic-agent/addon.toml");
    let p = parse(text, "generic-agent").unwrap();
    assert!(p.problems.is_empty(), "{:?}", p.problems);
    assert_eq!(p.meta.kind, AddonKind::Pack);
    assert_eq!(p.meta.version.to_string(), "0.1.0");
    let profile = &p.profiles[0];
    assert_eq!(profile.name, "agent");
    assert_eq!(profile.argv, vec!["my-agent"]);
    assert_eq!(profile.args, vec!["--plain"]);
    assert_eq!(profile.resume, ResumeRule::ReRun);
    assert_eq!(profile.status.len(), 4);
    assert_eq!(profile.status[0].status, ProfileStatus::NeedsYouApproval);
    assert_eq!(p.run_templates[0].argv, vec!["npm", "test"]);
    assert_eq!(p.run_templates[0].cwd.as_deref(), Some("{folder}"));
    assert_eq!(p.slash[0].command, "/review");
    assert_eq!(p.commands[0].tool, "session_list");
    assert_eq!(p.commands[0].params["include_asleep"], serde_json::Value::Bool(false));
}
