//! CLI argument mapping: schema in, params out.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_cli::args::{self, Output};
use csm_engine::{Effects, Toolset};
use csm_mcp::ToolInfo;
use serde_json::{Value, json};

fn info(schema: Value) -> ToolInfo {
    ToolInfo {
        id: "session_new".into(),
        title: "New".into(),
        summary: "Open a session.".into(),
        toolset: Toolset::Core,
        effects: Effects::LAUNCHES_PROCESS,
        input_schema: schema,
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["folder"],
        "properties": {
            "folder": {"type": "string", "description": "Where."},
            "name": {"type": ["string", "null"]},
            "focus": {"type": "boolean"},
            "cols": {"type": "integer"},
            "ratio": {"type": "number"},
            "kind": {"$ref": "#/$defs/Kind"},
            "ssh": {"anyOf": [{"type": "object"}, {"type": "null"}]},
            "tags": {"type": "array", "items": {"type": "string"}},
            "mixed": {"anyOf": [{"type": "string"}, {"type": "integer"}]},
            "start": {"type": "string"}
        },
        "$defs": {"Kind": {"type": "string", "enum": ["local", "ssh", "wsl"]}}
    })
}

fn parse(argv: &[&str]) -> Result<(Value, Output), String> {
    let t = info(schema());
    let cmd = args::build(std::slice::from_ref(&t), None);
    let mut full = vec!["csm"];
    full.extend_from_slice(argv);
    let m = cmd.try_get_matches_from(full).map_err(|e| e.to_string())?;
    let (name, sub) = m.subcommand().ok_or("no subcommand")?;
    assert_eq!(name, "session-new");
    args::params_from_matches(&t, sub)
}

#[test]
fn flags_are_typed_from_the_schema() {
    let (p, out) =
        parse(&["session-new", "--folder", "C:/x y", "--focus", "--cols", "120", "--ratio", "0.5", "--kind", "wsl", "--name", "n"]).unwrap();
    assert_eq!(out, Output::Message);
    assert_eq!(p, json!({"folder": "C:/x y", "focus": true, "cols": 120, "ratio": 0.5, "kind": "wsl", "name": "n"}));
}

#[test]
fn booleans_take_an_optional_value() {
    assert_eq!(parse(&["session-new", "--folder", "f", "--focus", "false"]).unwrap().0["focus"], json!(false));
    assert_eq!(parse(&["session-new", "--focus=true", "--folder", "f"]).unwrap().0["focus"], json!(true));
    // A bare boolean directly before another flag does not eat it.
    let (p, _) = parse(&["session-new", "--focus", "--folder", "f"]).unwrap();
    assert_eq!(p, json!({"focus": true, "folder": "f"}));
}

#[test]
fn complex_values_are_json() {
    let (p, _) = parse(&["session-new", "--folder", "f", "--ssh", r#"{"host":"h"}"#, "--tags", r#"["a","b"]"#, "--mixed", "7"]).unwrap();
    assert_eq!(p["ssh"], json!({"host": "h"}));
    assert_eq!(p["tags"], json!(["a", "b"]));
    assert_eq!(p["mixed"], json!(7));
    let (p, _) = parse(&["session-new", "--folder", "f", "--mixed", "seven"]).unwrap();
    assert_eq!(p["mixed"], json!("seven"));
    assert!(parse(&["session-new", "--folder", "f", "--ssh", "{nope"]).is_err());
}

#[test]
fn bad_values_and_missing_required_are_usage_errors() {
    assert!(parse(&["session-new", "--folder", "f", "--cols", "many"]).unwrap_err().contains("whole number"));
    assert!(parse(&["session-new", "--folder", "f", "--focus", "maybe"]).unwrap_err().contains("true or false"));
    assert!(parse(&["session-new"]).unwrap_err().contains("--folder"));
    assert!(parse(&["session-new", "--folder", "f", "--bogus", "1"]).unwrap_err().contains("--bogus"));
}

#[test]
fn json_params_and_flags_combine_and_flags_win() {
    let (p, out) = parse(&["session-new", "--json", r#"{"folder":"a","cols":1}"#, "--cols", "2"]).unwrap();
    assert_eq!(out, Output::Message);
    assert_eq!(p, json!({"folder": "a", "cols": 2}));
    // Required may come from --json.
    assert!(parse(&["session-new", "--json", r#"{"folder":"a"}"#]).is_ok());
    assert!(parse(&["session-new", "--json", "[1]"]).is_err());
    assert!(parse(&["session-new", "--json", "folder"]).is_err());
}

#[test]
fn json_on_its_own_asks_for_json_output() {
    let (p, out) = parse(&["session-new", "--folder", "f", "--json"]).unwrap();
    assert_eq!(out, Output::Json);
    assert_eq!(p, json!({"folder": "f"}));
    // And before another flag.
    let (_, out) = parse(&["session-new", "--json", "--folder", "f"]).unwrap();
    assert_eq!(out, Output::Json);
}

#[test]
fn json_params_can_come_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("p.json");
    std::fs::write(&file, r#"{"folder": "from file"}"#).unwrap();
    let arg = format!("@{}", file.display());
    let (p, _) = parse(&["session-new", "--json", &arg]).unwrap();
    assert_eq!(p["folder"], "from file");
    assert!(parse(&["session-new", "--json", "@does-not-exist.json"]).is_err());
}

#[test]
fn a_property_named_like_a_global_flag_is_reachable_through_json() {
    // `start` would clash with the global --start, so it is not a flag...
    assert!(parse(&["session-new", "--folder", "f", "--start"]).is_ok_and(|(p, _)| p.get("start").is_none()));
    // ...but --json still carries it.
    let (p, _) = parse(&["session-new", "--json", r#"{"folder":"f","start":"x"}"#]).unwrap();
    assert_eq!(p["start"], "x");
}

#[test]
fn help_groups_tools_by_toolset_and_documents_flags() {
    let t = info(schema());
    let mut cmd = args::build(std::slice::from_ref(&t), None);
    let help = cmd.render_long_help().to_string();
    assert!(help.contains("Sessions (core)") && help.contains("session-new") && help.contains("Open a session."), "{help}");
    let mut sub = args::tool_command(&t);
    let sub_help = sub.render_long_help().to_string();
    assert!(sub_help.contains("--folder") && sub_help.contains("(required)") && sub_help.contains("local, ssh, wsl"), "{sub_help}");
}
