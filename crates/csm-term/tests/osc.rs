//! Byte-fixture tests: OSC events and reply ordering, including every split across chunk
//! boundaries.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use csm_term::{GridSize, ProbeReply, PromptMark, ReplySink, TapEvent, TermConfig, TermEvent, Terminal};

#[derive(Default)]
struct Replies(Mutex<Vec<u8>>);

impl ReplySink for Replies {
    fn send(&self, bytes: &[u8]) {
        self.0.lock().unwrap().extend_from_slice(bytes);
    }
}

fn term(probe: ProbeReply) -> (Terminal, Arc<Replies>) {
    let replies = Arc::new(Replies::default());
    let config = TermConfig { probe_reply: probe, ..TermConfig::default() };
    (Terminal::new(GridSize::new(80, 24), config, replies.clone()), replies)
}

/// Feeds `input` in one go and at every two-chunk split, asserting identical events and
/// replies each time. Returns them.
fn run_all_splits(input: &[u8], probe: ProbeReply) -> (Vec<TermEvent>, Vec<u8>) {
    let (mut t, r) = term(probe);
    t.advance(input);
    let expected_events = t.take_events();
    let expected_replies = r.0.lock().unwrap().clone();
    for split in 1..input.len() {
        let (mut t, r) = term(probe);
        t.advance(&input[..split]);
        t.advance(&input[split..]);
        assert_eq!(t.take_events(), expected_events, "events differ at split {split}");
        assert_eq!(*r.0.lock().unwrap(), expected_replies, "replies differ at split {split}");
    }
    // Byte by byte, too.
    let (mut t, r) = term(probe);
    for b in input {
        t.advance(std::slice::from_ref(b));
    }
    assert_eq!(t.take_events(), expected_events, "events differ byte-by-byte");
    assert_eq!(*r.0.lock().unwrap(), expected_replies, "replies differ byte-by-byte");
    (expected_events, expected_replies)
}

fn taps(events: &[TermEvent]) -> Vec<TapEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            TermEvent::Tap(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

const DA1_REPLY: &[u8] = b"\x1b[?6c";
const PROBE_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[test]
fn osc_9001_with_bel_and_st() {
    let input = b"hello\x1b]9001;color=#89b4fa;git-branch=main ;git-dirty=1;junk;=x;title=a=b\x07world\x1b]9001;title=two\x1b\\";
    let (events, _) = run_all_splits(input, ProbeReply::Off);
    assert_eq!(
        taps(&events),
        vec![
            TapEvent::ShellIntegration(vec![
                ("color".into(), "#89b4fa".into()),
                ("git-branch".into(), "main".into()),
                ("git-dirty".into(), "1".into()),
                ("title".into(), "a=b".into()),
            ]),
            TapEvent::ShellIntegration(vec![("title".into(), "two".into())]),
        ]
    );
}

#[test]
fn osc_7501_report_and_probe_answered_before_da1() {
    // Claude's startup shape: probe, then DA1 as the "no more replies" sentinel.
    let input = b"\x1b]7501;state=working:progress=40:id=build\x1b\\\x1b]7501;?\x1b\\\x1b[c";
    let (events, replies) = run_all_splits(input, ProbeReply::Raw);
    assert_eq!(taps(&events), vec![TapEvent::ProgramStatus("state=working:progress=40:id=build".into()), TapEvent::ProgramStatusProbe,]);
    assert!(events.contains(&TermEvent::ProbeAnswered { answered: true }));
    assert_eq!(replies, [PROBE_REPLY, DA1_REPLY].concat());
}

#[test]
fn da1_before_probe_is_answered_first() {
    let input = b"\x1b[c\x1b]7501;?\x07";
    let (_, replies) = run_all_splits(input, ProbeReply::Raw);
    assert_eq!(replies, [DA1_REPLY, PROBE_REPLY].concat());
}

#[test]
fn probe_inside_synchronized_update_keeps_order() {
    // DA1 held in a DEC 2026 synchronized update must still be answered before the probe.
    let input = b"\x1b[?2026h\x1b[c\x1b]7501;?\x07\x1b[c";
    let (_, replies) = run_all_splits(input, ProbeReply::Raw);
    assert_eq!(replies, [DA1_REPLY, PROBE_REPLY, DA1_REPLY].concat());
}

#[test]
fn probe_not_answered_when_off() {
    let (events, replies) = run_all_splits(b"\x1b]7501;?\x07\x1b[c", ProbeReply::Off);
    assert!(events.contains(&TermEvent::ProbeAnswered { answered: false }));
    assert_eq!(replies, DA1_REPLY);
}

#[test]
fn probe_is_rate_limited() {
    let (mut t, r) = term(ProbeReply::Raw);
    t.advance(b"\x1b]7501;?\x07\x1b]7501;?\x07");
    assert_eq!(*r.0.lock().unwrap(), PROBE_REPLY);
}

#[test]
fn win32_input_mode_reply_needs_the_mode() {
    let (mut t, r) = term(ProbeReply::Win32InputMode);
    t.advance(b"\x1b]7501;?\x07");
    assert!(r.0.lock().unwrap().is_empty());
    let (mut t, r) = term(ProbeReply::Win32InputMode);
    t.advance(b"\x1b[?9001h\x1b]7501;?\x07");
    assert!(r.0.lock().unwrap().starts_with(b"\x1b[0;0;27;1;0;1_"));
}

#[test]
fn osc_133_and_7() {
    let input =
        b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;2\x07\x1b]133;D\x07\x1b]7;file://host/C:/repo%20x\x1b\\\x1b]133;Z\x07";
    let (events, _) = run_all_splits(input, ProbeReply::Off);
    assert_eq!(
        taps(&events),
        vec![
            TapEvent::Prompt(PromptMark::PromptStart),
            TapEvent::Prompt(PromptMark::CommandStart),
            TapEvent::Prompt(PromptMark::CommandExecuted),
            TapEvent::Prompt(PromptMark::CommandFinished { exit_code: Some(2) }),
            TapEvent::Prompt(PromptMark::CommandFinished { exit_code: None }),
            TapEvent::Cwd("file://host/C:/repo%20x".into()),
        ]
    );
}

#[test]
fn color_query_answered_in_order_and_after_change() {
    let input = b"\x1b]11;?\x07\x1b[c\x1b]11;rgb:ff/00/00\x07\x1b]11;?\x07";
    let (_, replies) = run_all_splits(input, ProbeReply::Off);
    let text = String::from_utf8_lossy(&replies).into_owned();
    // Default background (Catppuccin #1e1e2e), DA1, then the changed background; each reply
    // uses the query's own terminator (BEL here).
    assert_eq!(text, "\x1b]11;rgb:1e1e/1e1e/2e2e\x07\x1b[?6c\x1b]11;rgb:ffff/0000/0000\x07");
}

#[test]
fn oversized_and_invalid_payloads_do_not_panic() {
    let mut big = b"\x1b]7501;state=working:msg=".to_vec();
    big.extend(std::iter::repeat_n(b'A', 10_000));
    big.extend_from_slice(b"\x07");
    let (mut t, _) = term(ProbeReply::Raw);
    t.advance(&big);
    assert!(taps(&t.take_events()).is_empty(), "oversized 7501 must be dropped");
    t.advance(b"\x1b]9001;title=\xff\xfe\x07\x1b]7;\xc3\x07\x1b]133;\x07\x1b]9001\x07\x1b]\x07");
    let _ = t.take_events();
}

/// Cheap deterministic fuzz: random bytes with plenty of ESC/OSC structure never panic.
#[test]
fn random_bytes_never_panic() {
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let alphabet: &[&[u8]] = &[
        b"\x1b",
        b"]",
        b"9001;",
        b"7501;",
        b"133;",
        b"7;",
        b"?",
        b"\x07",
        b"\\",
        b"[",
        b"c",
        b"a",
        b";",
        b"=",
        b"\xff",
        b"\xe6\x97\xa5",
        b"\x9c",
        b"\x18",
    ];
    for _ in 0..200 {
        let (mut t, _) = term(ProbeReply::Raw);
        let mut buf = Vec::new();
        for _ in 0..500 {
            buf.extend_from_slice(alphabet[(next() % alphabet.len() as u64) as usize]);
        }
        for chunk in buf.chunks(1 + (next() % 37) as usize) {
            t.advance(chunk);
        }
        let _ = t.take_events();
    }
}

#[test]
fn xtversion_answered_in_order() {
    let input = b"[>q[?2026$p[c";
    let (_, replies) = run_all_splits(input, ProbeReply::Off);
    let text = String::from_utf8_lossy(&replies).into_owned();
    assert!(text.starts_with("P>|CodeShellManager("), "{text:?}");
    let x = text.find("\\").unwrap();
    // DECRQM 2026 (synchronized output) is reported by alacritty, then DA1.
    assert_eq!(&text[x + 2..], "[?2026;2$y[?6c");
}

#[test]
fn a_default_cursor_and_palette_can_be_set_and_programs_still_override_the_cursor() {
    use csm_term::alacritty_terminal::vte::ansi::{CursorShape, Rgb};
    let (mut t, r) = term(ProbeReply::Off);
    assert_eq!(t.term().cursor_style().shape, CursorShape::Block);
    t.set_default_cursor(CursorShape::Beam, false);
    assert_eq!(t.term().cursor_style().shape, CursorShape::Beam);
    // DECSCUSR 4 (steady underline) wins; DECSCUSR 0 goes back to the session's default.
    t.advance(b"\x1b[4 q");
    assert_eq!(t.term().cursor_style().shape, CursorShape::Underline);
    t.advance(b"\x1b[0 q");
    assert_eq!(t.term().cursor_style().shape, CursorShape::Beam);
    // OSC 11 answers with the palette that was set.
    let mut p = csm_term::Palette::default();
    p.background = Rgb { r: 0x01, g: 0x24, b: 0x56 };
    let g = t.generation();
    t.set_palette(p.clone());
    assert!(t.generation() != g);
    assert_eq!(t.palette(), &p);
    t.advance(b"\x1b]11;?\x1b\\");
    let reply = String::from_utf8_lossy(&r.0.lock().unwrap()).into_owned();
    assert!(reply.contains("rgb:0101/2424/5656"), "{reply:?}");
}
