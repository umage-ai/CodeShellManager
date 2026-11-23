//! `csm attach`: one session of a paired office, in this console, the way tmux attaches (design
//! section 7, P1 step 8, C1). It is a harness for the peer link as much as a tool: the screen is the
//! office's, the keys are the office's, and Ctrl+] then `d` detaches without touching the session.
//!
//! It never pairs. Pairing starts only in the app's window, and this command only dials an office the
//! app paired with (read from the app's `peer-client/` folder). It writes nothing to that folder.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ArgMatches;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use csm_remote::attach::{self, AttachEnd, Console, ConsoleEvent};
use csm_remote::{PairedOffice, paired_offices};
use serde_json::Value;

use crate::run::{EXIT_APP, EXIT_OK, EXIT_USAGE};

/// The console's size when the terminal cannot say (it is not a terminal).
const FALLBACK_SIZE: (u16, u16) = (80, 24);

/// The client folder of the app whose state is at `config_dir` (or the default one): `peer-client/`
/// beside the state file. `None` when the app keeps no state (an isolated run).
fn client_folder(config_dir: Option<&str>) -> Option<PathBuf> {
    let state = csm_engine::persist::state_path(config_dir.map(Path::new))?;
    attach::client_folder(&state)
}

/// The office the command means: the one `--host` names (id or name), or the only one paired.
fn pick_office<'a>(offices: &'a [PairedOffice], host: Option<&str>) -> Result<&'a PairedOffice, String> {
    match host {
        Some(wanted) => {
            let matches: Vec<&PairedOffice> = offices.iter().filter(|o| o.host_id == wanted || o.name.eq_ignore_ascii_case(wanted)).collect();
            match matches.as_slice() {
                [one] => Ok(one),
                [] => Err(format!("no paired office is called `{wanted}`. Run `csm attach --offices` to see them.")),
                _ => Err(format!("several paired offices are called `{wanted}`; use the office's id.")),
            }
        }
        None => match offices {
            [] => Err("no office is paired with this laptop. Pair one in CodeShellManager: Connect to another CodeShellManager.".into()),
            [one] => Ok(one),
            _ => {
                let names: Vec<&str> = offices.iter().map(|o| o.name.as_str()).collect();
                Err(format!("several offices are paired ({}); choose one with --host.", names.join(", ")))
            }
        },
    }
}

/// The session the command means, from the office's `session_list`: its id, or its name when that is
/// unique. Returns the id and the name.
fn pick_session(sessions: &[Value], wanted: &str) -> Result<(String, String), String> {
    let by_id = sessions.iter().find(|s| s["id"].as_str() == Some(wanted));
    if let Some(s) = by_id {
        return Ok((wanted.to_owned(), name_of(s)));
    }
    let named: Vec<&Value> = sessions.iter().filter(|s| name_of(s).eq_ignore_ascii_case(wanted)).collect();
    match named.as_slice() {
        [one] => Ok((one["id"].as_str().unwrap_or_default().to_owned(), name_of(one))),
        [] => Err(format!("the office has no session `{wanted}`. Run `csm attach --list` to see them.")),
        _ => Err(format!("several sessions are called `{wanted}`; use the session's id.")),
    }
}

/// Text the office supplied, made safe to print: control characters (escape sequences included) are
/// shown as `?`, so a name or id cannot change the laptop's terminal.
pub(crate) fn printable(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

fn name_of(session: &Value) -> String {
    let raw = session["name"].as_str().filter(|n| !n.is_empty()).or_else(|| session["id"].as_str()).unwrap_or("session");
    printable(raw)
}

/// One line per session: id, status, name.
fn session_lines(sessions: &[Value]) -> String {
    let mut out = String::new();
    for s in sessions {
        let id = printable(s["id"].as_str().unwrap_or("?"));
        let status = printable(s["status"].as_str().unwrap_or("?"));
        out.push_str(&format!("{id}  {status:<18} {}\n", name_of(s)));
    }
    out
}

fn offices_text(offices: &[PairedOffice]) -> String {
    let mut out = String::new();
    for o in offices {
        out.push_str(&format!("{}  {}  {}\n", printable(&o.host_id), printable(&o.name), printable(&o.address)));
    }
    out
}

pub(crate) fn run(m: &ArgMatches) -> i32 {
    let as_json = m.get_flag("json");
    let config_dir = m.get_one::<String>("config_dir").map(String::as_str);
    let Some(dir) = client_folder(config_dir) else {
        eprintln!("error: this CodeShellManager keeps no state folder, so there are no paired offices. Use --config-dir.");
        return EXIT_APP;
    };
    let offices = match paired_offices(&dir) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_APP;
        }
    };
    if m.get_flag("offices") {
        if as_json {
            let list: Vec<Value> = offices.iter().map(|o| serde_json::json!({"host_id": o.host_id, "name": o.name, "address": o.address})).collect();
            println!("{}", Value::Array(list));
        } else if offices.is_empty() {
            println!("No office is paired with this laptop.");
        } else {
            print!("{}", offices_text(&offices));
        }
        return EXIT_OK;
    }
    let office = match pick_office(&offices, m.get_one::<String>("host").map(String::as_str)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_APP;
        }
    };
    let (name, conn) = match attach::connect(&dir, &office.host_id) {
        Ok((name, conn)) => (printable(&name), conn),
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_APP;
        }
    };
    let sessions = match attach::list_sessions(&conn) {
        Ok(s) => s,
        Err(e) => {
            conn.close();
            eprintln!("error: {e}");
            return EXIT_APP;
        }
    };
    if m.get_flag("list") {
        if as_json {
            println!("{}", Value::Array(sessions));
        } else if sessions.is_empty() {
            println!("{name} has no sessions.");
        } else {
            print!("{}", session_lines(&sessions));
        }
        conn.close();
        return EXIT_OK;
    }
    let Some(wanted) = m.get_one::<String>("session") else {
        conn.close();
        eprintln!("error: name a session (see `csm attach --list`), or use --list or --offices");
        return EXIT_USAGE;
    };
    let (id, session) = match pick_session(&sessions, wanted) {
        Ok(s) => s,
        Err(e) => {
            conn.close();
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let guard = match ConsoleGuard::enter() {
        Ok(g) => g,
        Err(e) => {
            conn.close();
            eprintln!("error: attach needs a terminal: {e}");
            return EXIT_USAGE;
        }
    };
    eprintln!("Attached to {session} on {name}. Ctrl+] then d detaches; the session keeps running.");
    let end = attach::run(&conn, &id, &mut Tty);
    drop(guard);
    conn.close();
    match end {
        AttachEnd::Detached => {
            eprintln!("Detached from {session}. It keeps running on {name}.");
            EXIT_OK
        }
        AttachEnd::SessionEnded(reason) => {
            eprintln!("{}", attach::explain_session_end(reason));
            EXIT_OK
        }
        AttachEnd::LinkLost(why) => {
            eprintln!("error: {why}");
            EXIT_APP
        }
    }
}

/// The terminal in raw mode on the alternate screen, with bracketed paste. Leaving the scope restores
/// the terminal, whatever ended the attach.
struct ConsoleGuard;

impl ConsoleGuard {
    fn enter() -> io::Result<ConsoleGuard> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(e) = execute!(out, EnterAlternateScreen, EnableBracketedPaste) {
            let _ = terminal::disable_raw_mode();
            return Err(e);
        }
        Ok(ConsoleGuard)
    }
}

impl Drop for ConsoleGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// This terminal, as the attach sees it.
struct Tty;

impl Console for Tty {
    fn size(&self) -> (u16, u16) {
        terminal::size().unwrap_or(FALLBACK_SIZE)
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    fn next(&mut self, wait: Duration) -> Option<ConsoleEvent> {
        if !event::poll(wait).unwrap_or(false) {
            return None;
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => key_bytes(&key).map(ConsoleEvent::Key),
            Ok(Event::Paste(text)) => Some(ConsoleEvent::Paste(text)),
            Ok(Event::Resize(cols, rows)) => Some(ConsoleEvent::Resized { cols, rows }),
            _ => None,
        }
    }
}

/// The bytes a key press means to a terminal program (xterm-style; arrows are not sent in application
/// cursor mode). `None` for keys with no bytes of their own.
pub(crate) fn key_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let plain: Vec<u8> = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => vec![control_byte(c)?],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => function_key(n)?,
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        let mut with_escape = vec![0x1b];
        with_escape.extend(plain);
        Some(with_escape)
    } else {
        Some(plain)
    }
}

/// The control byte of Ctrl+`c`.
fn control_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' => 0,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => attach::DETACH_KEY,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    })
}

fn function_key(n: u8) -> Option<Vec<u8>> {
    let bytes: &[u8] = match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    };
    Some(bytes.to_vec())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn office(id: &str, name: &str) -> PairedOffice {
        PairedOffice { host_id: id.into(), name: name.into(), address: "10.8.0.2:47300".into() }
    }

    #[test]
    fn plain_keys_send_their_bytes() {
        assert_eq!(key_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE)), Some(b"a".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::Char('é'), KeyModifiers::SHIFT)), Some("é".as_bytes().to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::Enter, KeyModifiers::NONE)), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::Backspace, KeyModifiers::NONE)), Some(vec![0x7f]));
        assert_eq!(key_bytes(&key(KeyCode::Esc, KeyModifiers::NONE)), Some(vec![0x1b]));
        assert_eq!(key_bytes(&key(KeyCode::Tab, KeyModifiers::NONE)), Some(b"\t".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::BackTab, KeyModifiers::SHIFT)), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn control_letters_send_the_control_byte() {
        assert_eq!(key_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![0x03]));
        assert_eq!(key_bytes(&key(KeyCode::Char('C'), KeyModifiers::CONTROL)), Some(vec![0x03]));
        assert_eq!(key_bytes(&key(KeyCode::Char('d'), KeyModifiers::CONTROL)), Some(vec![0x04]));
        assert_eq!(key_bytes(&key(KeyCode::Char(' '), KeyModifiers::CONTROL)), Some(vec![0]));
        assert_eq!(key_bytes(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)), Some(vec![attach::DETACH_KEY]));
        assert_eq!(key_bytes(&key(KeyCode::Char('1'), KeyModifiers::CONTROL)), None, "no control byte for a digit");
    }

    #[test]
    fn alt_puts_an_escape_in_front() {
        assert_eq!(key_bytes(&key(KeyCode::Char('x'), KeyModifiers::ALT)), Some(b"\x1bx".to_vec()));
    }

    #[test]
    fn cursor_and_editing_keys_use_the_csi_forms() {
        assert_eq!(key_bytes(&key(KeyCode::Up, KeyModifiers::NONE)), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::Left, KeyModifiers::NONE)), Some(b"\x1b[D".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::PageDown, KeyModifiers::NONE)), Some(b"\x1b[6~".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::Delete, KeyModifiers::NONE)), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::F(1), KeyModifiers::NONE)), Some(b"\x1bOP".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::F(12), KeyModifiers::NONE)), Some(b"\x1b[24~".to_vec()));
        assert_eq!(key_bytes(&key(KeyCode::F(13), KeyModifiers::NONE)), None);
        assert_eq!(key_bytes(&key(KeyCode::CapsLock, KeyModifiers::NONE)), None, "keys with no bytes send nothing");
    }

    #[test]
    fn an_office_is_picked_by_id_or_name_and_only_when_it_is_clear() {
        let one = vec![office("ABC234", "Office")];
        assert_eq!(pick_office(&one, None).unwrap().host_id, "ABC234");
        assert_eq!(pick_office(&one, Some("office")).unwrap().host_id, "ABC234");

        let two = vec![office("ABC234", "Office"), office("DEF567", "Lab")];
        assert!(pick_office(&two, None).unwrap_err().contains("choose one with --host"));
        assert_eq!(pick_office(&two, Some("DEF567")).unwrap().name, "Lab");
        assert!(pick_office(&two, Some("Nope")).unwrap_err().contains("no paired office"));

        let none: Vec<PairedOffice> = Vec::new();
        assert!(pick_office(&none, None).unwrap_err().contains("no office is paired"));

        let twins = vec![office("A", "Same"), office("B", "same")];
        assert!(pick_office(&twins, Some("same")).unwrap_err().contains("several"));
    }

    #[test]
    fn a_session_is_picked_by_id_or_a_unique_name() {
        let sessions = vec![json!({"id": "s1", "name": "Build", "status": "idle"}), json!({"id": "s2", "name": "Docs", "status": "working"})];
        assert_eq!(pick_session(&sessions, "s2").unwrap(), ("s2".to_owned(), "Docs".to_owned()));
        assert_eq!(pick_session(&sessions, "build").unwrap(), ("s1".to_owned(), "Build".to_owned()));
        assert!(pick_session(&sessions, "nope").unwrap_err().contains("--list"));

        let twins = vec![json!({"id": "a", "name": "Same"}), json!({"id": "b", "name": "Same"})];
        assert!(pick_session(&twins, "Same").unwrap_err().contains("id"));
    }

    #[test]
    fn the_session_list_prints_one_line_per_session() {
        let sessions = vec![json!({"id": "s1", "name": "Build", "status": "idle"})];
        let text = session_lines(&sessions);
        assert!(text.starts_with("s1"), "{text}");
        assert!(text.contains("idle") && text.contains("Build"), "{text}");
    }

    #[test]
    fn office_text_cannot_inject_control_characters_into_the_console() {
        let sessions = vec![json!({"id": "s\u{1b}[2J", "name": "Evil\u{1b}]0;title\u{7}\r\n", "status": "idle\u{9b}"})];
        let text = session_lines(&sessions);
        assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text:?}");
        assert_eq!(text.matches('\n').count(), 1, "one line per session: {text:?}");
        assert_eq!(printable("ok name"), "ok name");
    }

    #[test]
    fn no_state_folder_means_no_offices_and_nothing_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cfg");
        let config_str = config.to_string_lossy().into_owned();
        let argv: Vec<String> = ["csm", "attach", "--list", "--config-dir", &config_str].iter().map(|s| s.to_string()).collect();
        let code = crate::run(&argv, &crate::Env::default());
        assert_eq!(code, EXIT_APP, "no office is paired, so the command cannot reach one");
        assert!(!config.join("peer-client").exists(), "attach never makes the client folder");
    }
}
