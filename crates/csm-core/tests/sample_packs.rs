//! The sample add-ons that ship in `samples/addons` parse with no problems, and their status rules
//! recognise the screens they were written for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_core::addons::{AddonKind, ProfileStatus, ResumeRule, match_status, parse};

fn pack(id: &str, text: &str) -> csm_core::addons::ParsedAddon {
    let p = parse(text, id).unwrap();
    assert!(p.problems.is_empty(), "{id}: {:?}", p.problems);
    assert_eq!(p.meta.kind, AddonKind::Pack);
    assert_eq!(p.meta.id, id);
    p
}

#[test]
fn codex_cli_parses_clean_and_its_rules_match() {
    let p = pack("codex-cli", include_str!("../../../samples/addons/codex-cli/addon.toml"));
    assert_eq!(p.profiles.len(), 2);
    assert_eq!(p.profiles[0].argv, vec!["codex"]);
    assert_eq!(p.profiles[1].argv, vec!["codex", "resume", "--last"]);
    assert!(p.profiles.iter().all(|x| x.resume == ResumeRule::ReRun && x.status.len() == 4));
    assert_eq!(p.slash.len(), 3);
    assert_eq!(p.commands.len(), 2);
    let rules = &p.profiles[0].status;
    let st = |screen: &str| match_status(rules, screen);
    assert_eq!(st("Would you like to run the following command?\n  $ rm -rf x\n› 1. Yes, proceed (y)\n  2. No (esc)"), Some(ProfileStatus::NeedsYouApproval));
    assert_eq!(st("• Working (12s • esc to interrupt)\n\n› Explain this\n  ? for shortcuts"), Some(ProfileStatus::Working));
    assert_eq!(st("› Write tests for\n  ? for shortcuts            72% context left"), Some(ProfileStatus::Idle));
    assert_eq!(st("some plain output"), None);
}

#[test]
fn gemini_cli_parses_clean_and_its_rules_match() {
    let p = pack("gemini-cli", include_str!("../../../samples/addons/gemini-cli/addon.toml"));
    assert_eq!(p.profiles.len(), 2);
    assert_eq!(p.profiles[0].argv, vec!["gemini"]);
    assert_eq!(p.profiles[1].argv, vec!["gemini", "--resume"]);
    assert!(p.profiles.iter().all(|x| x.resume == ResumeRule::ReRun && x.status.len() == 4));
    assert_eq!(p.slash.len(), 3);
    assert_eq!(p.commands.len(), 2);
    let rules = &p.profiles[0].status;
    let st = |screen: &str| match_status(rules, screen);
    assert_eq!(st("Allow execution of: 'ls'?\n● 1. Allow once\n  2. Allow for this session\n  3. No, suggest changes (esc)"), Some(ProfileStatus::NeedsYouApproval));
    assert_eq!(st("⠏ Thinking... (esc to cancel, 5s)\n> Type your message or @path/to/file"), Some(ProfileStatus::Working));
    assert_eq!(st(" > Type your message or @path/to/file"), Some(ProfileStatus::Idle));
}

#[test]
fn the_session_summary_manifest_is_an_honest_process_add_on() {
    let text = include_str!("../../../samples/addons/session-summary/addon.toml");
    let p = parse(text, "session-summary").unwrap();
    assert!(p.problems.is_empty(), "{:?}", p.problems);
    assert_eq!(p.meta.kind, AddonKind::Process);
    let proc = p.process.as_ref().unwrap();
    assert_eq!(proc.argv, vec!["csm-session-summary"]);
    assert!(proc.effects.is_empty(), "the plugin only reads session_list");
}
