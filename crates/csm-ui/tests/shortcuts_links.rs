//! Keyboard shortcuts, the Recent menu and links in terminals, through the real UI on the demo
//! backend and a real engine.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::*;
use csm_ui::app::{Menu, Overlay};
use csm_ui::theme::ThemeKind;
use egui::{Event, Key, Modifiers, PointerButton, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn calls_since(h: &Harness<'static, csm_ui::CsmApp>, before: usize) -> Vec<(String, serde_json::Value)> {
    h.state().submitted()[before..].iter().map(|c| (c.id.clone(), c.params.clone())).collect()
}

// ── Every shortcut the UI shows is bound ───────────────────────────────────────────────────

/// Shortcuts that only mean something inside one panel (the palette, the new-session panel, a
/// dialog), where their own tests press them, and the mouse chords (Ctrl+click on a link, tested
/// below; Shift+click in the sidebar, tested in sidebar_ops.rs), and Ctrl+Alt+C, which copies a
/// selection in a Claude pane as shown (tested in claude_copy_ui.rs).
const IN_PANEL: [&str; 5] = ["Ctrl+Enter", "Shift+Tab", "Ctrl+click", "Shift+click", "Ctrl+Alt+C"];

/// Every chord shown in a string literal in `src/` (tooltips, menu hints, the palette, the help
/// list, toasts), normalised to `Ctrl+Shift+T`. A chord is one or more of Ctrl / Shift / Alt
/// joined by `+` or a space and a key, or a bare F-key.
fn displayed_chords() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut files = Vec::new();
    collect_rs(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
            for lit in literals(line) {
                chords_in(&lit, &mut out);
            }
        }
    }
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The contents of the `"…"` string literals on one line (good enough for this source).
fn literals(line: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur: Option<String> = None;
    let mut esc = false;
    for c in line.chars() {
        match (&mut cur, c) {
            (Some(s), _) if esc => {
                s.push(c);
                esc = false;
            }
            (Some(_), '\\') => esc = true,
            (Some(_), '"') => v.push(cur.take().unwrap()),
            (Some(s), _) => s.push(c),
            (None, '"') => cur = Some(String::new()),
            (None, _) => {}
        }
    }
    v
}

fn chords_in(lit: &str, out: &mut BTreeSet<String>) {
    let words: Vec<String> = lit.replace('+', " ").split_whitespace().map(|w| w.trim_start_matches('(').to_string()).collect();
    let is_mod = |w: &str| matches!(w, "Ctrl" | "Shift" | "Alt");
    let is_fkey = |w: &str| w.len() >= 2 && w.starts_with('F') && w[1..].chars().all(|c| c.is_ascii_digit());
    let key_of = |w: &str| -> String {
        // "T." at the end of a sentence is T; "." alone is the key.
        if w.chars().count() > 1 { w.trim_end_matches(['.', ',', ')', ':', ';']).to_string() } else { w.to_string() }
    };
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if is_mod(w) {
            let mut mods = Vec::new();
            while i < words.len() && is_mod(&words[i]) {
                mods.push(words[i].clone());
                i += 1;
            }
            if let Some(k) = words.get(i) {
                let k = key_of(k);
                if !k.is_empty() {
                    out.insert(format!("{}+{k}", mods.join("+")));
                }
            }
        } else if is_fkey(&key_of(w)) {
            out.insert(key_of(w));
        }
        i += 1;
    }
}

/// `Ctrl+Shift+T` → the modifiers and key(s) to press (`1…9`: the first and second tab).
fn parse(chord: &str) -> (Modifiers, Vec<Key>) {
    let parts: Vec<&str> = chord.split('+').collect();
    let (key, mods) = parts.split_last().unwrap();
    let mut m = Modifiers::NONE;
    for p in mods {
        m |= match *p {
            "Ctrl" => Modifiers::CTRL | Modifiers::COMMAND,
            "Shift" => Modifiers::SHIFT,
            "Alt" => Modifiers::ALT,
            other => panic!("modifier {other} in {chord}"),
        };
    }
    let keys = if *key == "1…9" {
        vec![Key::Num1, Key::Num2]
    } else {
        vec![Key::from_name(key).unwrap_or_else(|| panic!("`{chord}` is shown as a shortcut but `{key}` is not a key"))]
    };
    (m, keys)
}

/// Everything a shortcut can visibly do: call a tool, open an overlay, a menu or the settings,
/// start a rename.
fn fingerprint(h: &Harness<'static, csm_ui::CsmApp>) -> String {
    let a = h.state();
    format!("{} {:?} {:?} {} {}", a.submitted().len(), a.ui.overlay, a.ui.menu.is_some(), a.ui.settings.open, a.ui.inline.is_some())
}

#[test]
fn every_shortcut_shown_in_the_ui_is_bound() {
    let chords = displayed_chords();
    // The scan finds what it should (so an empty scan can't pass).
    for want in ["Ctrl+K", "Ctrl+Shift+E", "Ctrl+Shift+N", "Ctrl+Shift+R", "Ctrl+.", "F5", "Shift+F5", "Ctrl+Shift+T", "F2", "Ctrl+,"] {
        assert!(chords.contains(want), "{want} not found among {chords:?}");
    }
    let mut unbound = Vec::new();
    for chord in chords.iter().filter(|c| !IN_PANEL.contains(&c.as_str())) {
        let (m, keys) = parse(chord);
        for key in keys {
            let mut h = h();
            let before = fingerprint(&h);
            h.key_press_modifiers(m, key);
            settle(&mut h);
            if fingerprint(&h) == before {
                unbound.push(format!("{chord} ({key:?})"));
            }
        }
    }
    assert!(unbound.is_empty(), "shown in the UI but nothing happens: {unbound:?}");
}

#[test]
fn shift_and_alt_chords_do_not_also_fire_the_plain_one() {
    // egui's consume_shortcut ignores extra Shift: Ctrl+Shift+T used to open the new-session
    // panel instead of reopening.
    let mut h = h();
    let before = h.state().submitted().len();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
    settle(&mut h);
    assert_ne!(h.state().ui.overlay, Overlay::QuickOpen);
    assert!(calls_since(&h, before).iter().any(|(id, _)| id == "session_reopen_closed"));
    // And Ctrl+Alt+T (another binding's) is not Ctrl+T.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, Key::T);
    settle(&mut h);
    assert_ne!(h.state().ui.overlay, Overlay::QuickOpen);
}

#[test]
fn punctuation_shortcuts_work_where_the_character_needs_shift() {
    // `/` is Shift+7 on Danish and German keyboards: egui reports Slash with Shift held.
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Slash);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Help);
    // `.` is Shift+; on AZERTY.
    h.key_press(Key::Escape);
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Period);
    h.step();
    assert!(matches!(&h.state().ui.menu, Some(Menu::Pane { .. })));
}

#[test]
fn ctrl_shift_n_shows_notes_ctrl_shift_r_restarts_ctrl_dot_opens_the_pane_menu() {
    let mut h = h();
    let before = h.state().submitted().len();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::N);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::R);
    settle(&mut h);
    let calls = calls_since(&h, before);
    assert!(calls.contains(&("inspector_show".into(), json!({"tab": "notes"}))), "{calls:?}");
    assert!(calls.contains(&("session_restart".into(), json!({"session_id": "web"}))), "{calls:?}");
    h.key_press_modifiers(Modifiers::COMMAND, Key::Period);
    h.step();
    assert!(matches!(&h.state().ui.menu, Some(Menu::Pane { session, .. }) if session == "web"));
}

// ── The Recent menu ────────────────────────────────────────────────────────────────────────

fn recent_open(h: &Harness<'static, csm_ui::CsmApp>) -> bool {
    matches!(h.state().ui.menu, Some(Menu::Recent { .. }))
}

#[test]
fn the_title_bar_button_opens_recent_sessions_newest_first() {
    let mut h = h();
    h.get_by_label("Recent sessions  Ctrl+Shift+E").click();
    h.run();
    assert!(recent_open(&h), "the button opens the Recent menu, not the palette");
    assert_eq!(h.state().ui.overlay, Overlay::None);
    // The active session (web) is left out; the rest by last use, asleep ones included.
    let rows: Vec<String> = h.query_all_by_label_contains("Recent: ").map(|n| n.accesskit_node().label().unwrap_or_default()).collect();
    let names: Vec<&str> = rows.iter().map(|r| r.trim_start_matches("Recent: ").split(" · ").next().unwrap()).collect();
    assert_eq!(
        names,
        [
            "Odulodo",
            "CodeShellManager",
            "Odulodo.Web (PS)",
            "search-sort",
            "CodeShellManager-rs",
            "OptimizelyNewsletter",
            "api-gateway",
            "build-01",
            "craft-anything",
            "staging-db"
        ]
    );
    assert!(rows.iter().any(|r| r.contains("OptimizelyNewsletter · Asleep")), "{rows:?}");

    // A click on an asleep one focuses it (which wakes it and switches the rail tab).
    let before = h.state().submitted().len();
    h.get_by_label_contains("Recent: OptimizelyNewsletter").click();
    settle(&mut h);
    assert!(calls_since(&h, before).contains(&("session_focus".into(), json!({"session_id": "news"}))));
    assert!(!recent_open(&h));
    assert_eq!(h.state().model.active.as_deref(), Some("news"));
    assert_eq!(h.state().model.rail, csm_ui::model::Scope::Group("umage".into()));
    // A second click on the button closes it again.
    h.get_by_label("Recent sessions  Ctrl+Shift+E").click();
    h.run();
    assert!(recent_open(&h));
    h.get_by_label("Recent sessions  Ctrl+Shift+E").click();
    h.run();
    assert!(!recent_open(&h));
}

#[test]
fn ctrl_shift_e_enter_flips_between_two_sessions_and_digits_pick() {
    let mut h = h();
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    for want in ["odulodo", "web", "odulodo"] {
        h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
        h.step();
        assert!(recent_open(&h));
        h.key_press(Key::Enter);
        settle(&mut h);
        assert!(!recent_open(&h));
        assert_eq!(h.state().model.active.as_deref(), Some(want));
    }
    // 3 picks the third row; arrows move the selection Enter takes.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.step();
    let third = csm_ui::screens::recent::items(&h.state().model).get(2).map(|i| i.action());
    h.key_press(Key::Num3);
    settle(&mut h);
    let last = h.state().submitted().last().cloned().unwrap();
    assert_eq!(Some(csm_ui::Action::Tool("session_focus", last.params.clone())), third);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.step();
    h.key_press(Key::ArrowDown);
    h.key_press(Key::ArrowDown);
    h.key_press(Key::ArrowUp);
    let second = csm_ui::screens::recent::items(&h.state().model).get(1).map(|i| i.action());
    h.key_press(Key::Enter);
    settle(&mut h);
    let last = h.state().submitted().last().cloned().unwrap();
    assert_eq!(Some(csm_ui::Action::Tool("session_focus", last.params.clone())), second);
    // Escape closes it without picking.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.step();
    let before = h.state().submitted().len();
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!recent_open(&h));
    assert!(calls_since(&h, before).iter().all(|(id, _)| id != "session_focus"));
}

#[test]
fn recently_closed_sessions_are_listed_and_reopen_by_position() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND, Key::W); // closes web
    settle(&mut h);
    // web is working: Ctrl+W asks first, like every close from the UI.
    h.get_by_label_contains("Close anyway").click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    h.run();
    assert!(h.query_by_label("Reopen: Odulodo.Web").is_some(), "the closed session is listed");
    let before = h.state().submitted().len();
    h.get_by_label("Reopen: Odulodo.Web").click();
    settle(&mut h);
    assert!(calls_since(&h, before).contains(&("session_reopen_closed".into(), json!({"index": 0}))));
    assert!(h.state().model.session("web").is_some());
    assert!(h.state().model.recently_closed.is_empty());
}

// ── Links in terminals ─────────────────────────────────────────────────────────────────────

/// Moves the pointer over the PowerShell pane until the view reports the dev server's link.
fn hover_link(h: &mut Harness<'static, csm_ui::CsmApp>) -> egui::Pos2 {
    // The label is the pane's header; its terminal is below it, down to the window's bottom.
    let head = h.get_by_label("Pane: Odulodo.Web (PS)").rect();
    let mut y = 796.0;
    while y > head.max.y {
        for x in [head.min.x + 150.0, head.min.x + 200.0] {
            let p = pos2(x, y);
            h.event(Event::PointerMoved(p));
            h.step();
            let host = h.state().host.clone();
            let found = csm_ui::terminal::lock(&host).views.get("main:webps").and_then(|v| v.hovered_link().map(str::to_owned));
            if found.as_deref() == Some("http://localhost:5173/") {
                return p;
            }
        }
        y -= 5.0;
    }
    panic!("the link was not found under the pointer");
}

#[test]
fn ctrl_click_on_a_link_opens_it_and_a_plain_click_does_not() {
    let mut h = h();
    let at = hover_link(&mut h);
    let before = h.state().submitted().len();
    // A plain click selects, as before.
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert!(calls_since(&h, before).iter().all(|(id, _)| id != "open_url"));
    // Ctrl+click opens it through the engine's open_url.
    h.event(Event::PointerMoved(at));
    h.step();
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::COMMAND });
    h.step();
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::COMMAND });
    settle(&mut h);
    let opens: Vec<_> = calls_since(&h, before).into_iter().filter(|(id, _)| id == "open_url").collect();
    // The pane passes its session, so an SSH session's loopback link can go through a forward.
    assert_eq!(opens, vec![("open_url".to_string(), json!({"url": "http://localhost:5173/", "session_id": "webps"}))]);
}
