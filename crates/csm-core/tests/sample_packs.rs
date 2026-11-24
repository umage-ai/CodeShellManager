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
fn opencode_parses_clean_and_has_its_profiles() {
    let p = pack("opencode", include_str!("../../../samples/addons/opencode/addon.toml"));
    let argvs: Vec<_> = p.profiles.iter().map(|x| x.argv.clone()).collect();
    assert_eq!(argvs, vec![vec!["opencode"], vec!["opencode", "--continue"], vec!["opencode", "--auto"]]);
    assert!(p.profiles.iter().all(|x| x.resume == ResumeRule::ReRun && x.status.len() == 4));
    assert_eq!(p.slash.len(), 8);
    assert_eq!(p.commands.len(), 3);
    let rules = &p.profiles[0].status;
    let st = |screen: &str| match_status(rules, screen);
    assert_eq!(st("△ Permission required\n  Allow once   Allow always   Reject"), Some(ProfileStatus::NeedsYouApproval));
    assert_eq!(st("Ask anything... \"Fix a TODO\"\n  esc interrupt"), Some(ProfileStatus::Working));
    assert_eq!(st("  esc again to interrupt"), Some(ProfileStatus::Working));
    assert_eq!(st("Ask anything... \"Fix a TODO in the codebase\""), Some(ProfileStatus::Idle));
    assert_eq!(st("some plain output"), None);
}

#[test]
fn copilot_cli_parses_clean_and_has_its_profiles() {
    let p = pack("copilot-cli", include_str!("../../../samples/addons/copilot-cli/addon.toml"));
    let argvs: Vec<_> = p.profiles.iter().map(|x| x.argv.clone()).collect();
    assert_eq!(argvs, vec![vec!["copilot"], vec!["copilot", "--continue"], vec!["copilot", "--allow-all-tools"]]);
    assert!(p.profiles.iter().all(|x| x.resume == ResumeRule::ReRun && x.status.len() == 4));
    assert_eq!(p.slash.len(), 11);
    assert_eq!(p.commands.len(), 3);
    let rules = &p.profiles[0].status;
    let st = |screen: &str| match_status(rules, screen);
    assert_eq!(st("Do you want to run this command?\n 1. Yes\n 3. No, and tell Copilot what to do differently (Esc)"), Some(ProfileStatus::NeedsYouApproval));
    assert_eq!(st("Thinking (Esc to cancel)"), Some(ProfileStatus::Working));
    assert_eq!(st("Type @ to mention files, / for commands"), Some(ProfileStatus::Idle));
    assert_eq!(st("some plain output"), None);
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

#[test]
fn the_review_manifest_declares_what_it_does_and_the_calls_it_needs() {
    let text = include_str!("../../../samples/addons/review/addon.toml");
    let p = parse(text, "review").unwrap();
    assert!(p.problems.is_empty(), "{:?}", p.problems);
    assert_eq!(p.meta.kind, AddonKind::Process);
    let proc = p.process.as_ref().unwrap();
    assert_eq!(proc.argv, vec!["csm-review"]);
    assert_eq!(proc.effects, vec!["stores_data"], "it launches nothing itself: git runs through CSM's read-only tools");
    for tool in ["git_diff", "git_show_file", "git_ls_files"] {
        assert!(proc.calls.iter().any(|c| c == tool), "{tool} must be in the standing calls");
    }
    assert!(proc.calls.iter().all(|c| !c.starts_with("session_send") && !c.starts_with("addon_")), "{:?}", proc.calls);
}

#[test]
fn the_themes_pack_has_three_dark_themes_with_full_palettes_and_no_warnings() {
    let p = pack("themes", include_str!("../../../samples/addons/themes/addon.toml"));
    assert!(p.profiles.is_empty() && p.commands.is_empty() && p.slash.is_empty(), "colours only");
    let ids: Vec<&str> = p.themes.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["frappe", "macchiato", "high-contrast"]);
    for t in &p.themes {
        assert_eq!(t.base, csm_core::addons::ThemeBase::Dark);
        assert_eq!(t.colors.len(), 26, "{}: every palette slot", t.id);
    }
    // Spot checks against catppuccin/palette.
    assert_eq!(p.themes[0].colors["base"], "#303446");
    assert_eq!(p.themes[1].colors["base"], "#24273a");
    assert_eq!(p.themes[2].terminal["background"], "#000000");
}

#[test]
fn the_fun_themes_pack_has_two_dark_and_light_themes_with_full_palettes_and_no_warnings() {
    let p = pack("theme-fun", include_str!("../../../samples/addons/theme-fun/addon.toml"));
    assert!(p.profiles.is_empty() && p.commands.is_empty() && p.slash.is_empty(), "colours only");
    let ids: Vec<(&str, csm_core::addons::ThemeBase)> = p.themes.iter().map(|t| (t.id.as_str(), t.base)).collect();
    assert_eq!(ids, vec![("emacs", csm_core::addons::ThemeBase::Dark), ("win98", csm_core::addons::ThemeBase::Light), ("blossom", csm_core::addons::ThemeBase::Light)]);
    for t in &p.themes {
        assert_eq!(t.colors.len(), 26, "{}: every palette slot", t.id);
        assert_eq!(t.terminal.len(), 20, "{}: 16 ANSI colours plus foreground, background, cursor, selection", t.id);
    }
}
