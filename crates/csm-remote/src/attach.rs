//! `csm attach`: one office session on this console, the way tmux attaches (design section 7, P1
//! step 8, C1).
//!
//! The console shows the session's grid as the office serialized it, then the live bytes after it.
//! Keys go to the session as typed input; a paste goes as pasted text (the office brackets it). The
//! console's size is the session's size: the console asks the office for it. The detach key is
//! Ctrl+] then `d`: the link closes and the session keeps running. Nothing here pairs or makes keys:
//! it only dials a host the app paired with (see [`connect`]).
//!
//! The terminal itself is behind [`Console`], so the loop is tested without one. The CLI's console
//! (`apps/csm-cli`) implements it with raw mode.

use std::path::Path;
use std::time::Duration;

use csm_peer::client::{Connection, Incoming, TermFollower, TermUpdate};

use csm_peer_proto::codec::{Frame, Input};
use csm_peer_proto::msg::{ByeReason, Outcome, TermClosedReason};
use serde_json::{Value, json};

use crate::connect;
use crate::link::{refusal_of_bye, refusal_of_error};

/// The stream id this client uses: one terminal per attach.
pub const STREAM: u32 = 1;

/// The detach key, Ctrl+] (0x1d). Ctrl+] then `d` detaches.
pub const DETACH_KEY: u8 = 0x1d;

/// How long the loop waits for the office when the console had nothing.
const POLL: Duration = Duration::from_millis(5);

/// How long a tool call may take (a session list is quick; this is only a bound).
const CALL_WAIT: Duration = Duration::from_secs(20);

/// Clears the console and its scrollback before the office's grid is drawn.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

/// What the console tells the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// Bytes a key press produced.
    Key(Vec<u8>),
    /// Text the person pasted (untrusted: the office brackets it).
    Paste(String),
    /// The console changed size (columns, rows).
    Resized { cols: u16, rows: u16 },
}

/// The terminal the attach runs on.
pub trait Console {
    /// The console's size (columns, rows).
    fn size(&self) -> (u16, u16);
    /// Writes terminal output as it is.
    fn write(&mut self, bytes: &[u8]);
    /// The next event, or `None` when there is none within `wait` (zero: do not wait).
    fn next(&mut self, wait: Duration) -> Option<ConsoleEvent>;
}

/// Why the attach ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachEnd {
    /// The person detached. The session is untouched.
    Detached,
    /// The office ended the terminal stream (the session is gone, asleep or exited).
    SessionEnded(TermClosedReason),
    /// The connection ended; the text says why.
    LinkLost(String),
}

/// The detach key's state: Ctrl+] arms it, the next key decides. Ctrl+] `d` detaches, Ctrl+] Ctrl+]
/// sends one Ctrl+], and Ctrl+] followed by any other key sends that key (the Ctrl+] is dropped).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Keys {
    armed: bool,
}

/// What a run of typed bytes means: the bytes to send, and whether the person detached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub send: Vec<u8>,
    pub detach: bool,
}

impl Keys {
    /// Reads typed bytes. Bytes after a detach are not sent.
    pub fn feed(&mut self, bytes: &[u8]) -> Typed {
        let mut send = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if self.armed {
                self.armed = false;
                match b {
                    b'd' => return Typed { send, detach: true },
                    DETACH_KEY => send.push(DETACH_KEY),
                    other => send.push(other),
                }
            } else if b == DETACH_KEY {
                self.armed = true;
            } else {
                send.push(b);
            }
        }
        Typed { send, detach: false }
    }
}

/// What the person is told when the connection ends.
pub fn explain_closed(bye: Option<ByeReason>) -> String {
    match bye {
        Some(b) => refusal_of_bye(b).unwrap_or_else(|| match b {
            ByeReason::ServerOff => "The office turned remote access off.".into(),
            ByeReason::ServerQuit => "CodeShellManager on the office quit.".into(),
            ByeReason::Timeout => "The office stopped answering.".into(),
            _ => "The connection closed.".into(),
        }),
        None => "The connection closed.".into(),
    }
}

/// What the person is told when the session's terminal ends.
pub fn explain_session_end(reason: TermClosedReason) -> String {
    match reason {
        TermClosedReason::SessionGone => "The session was closed on the office.".into(),
        TermClosedReason::NotRunning => "The session is not running (asleep or exited).".into(),
        TermClosedReason::UnknownSession => "The office has no such session.".into(),
        TermClosedReason::Forbidden => "This laptop may not use the terminal of the office.".into(),
        TermClosedReason::Unsubscribed => "The terminal was closed.".into(),
    }
}

/// Dials the office `host_id` from the client folder `dir`, and returns its name and the connection.
/// Never pairs: an unpaired host is refused and nothing is written.
pub fn connect(dir: &Path, host_id: &str) -> Result<(String, Connection), String> {
    let paired = connect::paired(dir, host_id)?;
    let conn = csm_peer::client::connect_saved(Some(&paired.addr), paired.relay.as_ref(), &paired.host_public, &paired.identity, &paired.cfg).map_err(|e| {
        let why = refusal_of_error(&e).unwrap_or_else(|| e.to_string());
        format!("could not connect to {}: {why}", paired.window.name)
    })?;
    Ok((conn.welcome.host_name.clone(), conn))
}

/// The client folder of the app whose state file is at `state_file`: `peer-client/` beside it.
pub fn client_folder(state_file: &Path) -> Option<std::path::PathBuf> {
    state_file.parent().map(|d| d.join("peer-client"))
}

/// The office's session list (`session_list`), one JSON object per session.
pub fn list_sessions(conn: &Connection) -> Result<Vec<Value>, String> {
    match conn.call_wait("session_list", json!({}), CALL_WAIT) {
        Ok(Outcome::Ok { data, .. }) => data.as_array().cloned().ok_or_else(|| "the office sent a session list that is not a list".to_owned()),
        Ok(Outcome::Err { message, .. }) => Err(message),
        Err(_) => Err("the office did not answer the session list".into()),
    }
}

/// Attaches to `session` until the person detaches, the session's terminal ends or the link is lost.
/// The connection is left open: the caller closes it.
pub fn run(conn: &Connection, session: &str, console: &mut dyn Console) -> AttachEnd {
    if conn.subscribe(STREAM, session, None).is_err() {
        return AttachEnd::LinkLost("The connection closed before the terminal was opened.".into());
    }
    let mut follow = TermFollower::new(STREAM);
    let mut keys = Keys::default();
    let mut sizing = Sizing { want: console.size(), claimed: false };
    loop {
        while let Some(event) = console.next(Duration::ZERO) {
            match event {
                ConsoleEvent::Key(bytes) => {
                    let typed = keys.feed(&bytes);
                    if !typed.send.is_empty() && conn.input(STREAM, Input::Key(typed.send)).is_err() {
                        return AttachEnd::LinkLost("The connection closed while typing.".into());
                    }
                    if typed.detach {
                        let _ = conn.unsubscribe(STREAM);
                        return AttachEnd::Detached;
                    }
                }
                ConsoleEvent::Paste(text) => {
                    if conn.input(STREAM, Input::Paste(text)).is_err() {
                        return AttachEnd::LinkLost("The connection closed while pasting.".into());
                    }
                }
                ConsoleEvent::Resized { cols, rows } => {
                    // The console changed size by its own doing: it takes the session's size.
                    sizing.want = (cols, rows);
                    if conn.resize(STREAM, cols, rows, true).is_err() {
                        return AttachEnd::LinkLost("The connection closed while resizing.".into());
                    }
                }
            }
        }
        match conn.recv_timeout(POLL) {
            Some(Incoming::Frame(frame)) => match on_frame(conn, &mut follow, &frame, &mut sizing, console) {
                Step::Go => {}
                Step::End(end) => return end,
            },
            Some(Incoming::Closed(bye)) => return AttachEnd::LinkLost(explain_closed(bye)),
            None => {
                if conn.is_closed() {
                    return AttachEnd::LinkLost(explain_closed(None));
                }
            }
        }
    }
}

enum Step {
    Go,
    End(AttachEnd),
}

/// The console's size, and whether this attach has already claimed the session's size.
///
/// The office gives the driver role to whoever sends a resize with `driver` set, so the console asks
/// for its size only when it changed the size itself, or once when it first attaches. A frame that
/// reports a size changed by another device never makes the console ask for its size back: it draws
/// the grid at the session's size instead. So two consoles of different sizes cannot take the size
/// from each other in turn.
struct Sizing {
    want: (u16, u16),
    claimed: bool,
}

/// Applies one frame of the terminal stream to the console.
fn on_frame(conn: &Connection, follow: &mut TermFollower, frame: &Frame, sizing: &mut Sizing, console: &mut dyn Console) -> Step {
    let update = match follow.on_frame(frame) {
        Ok(u) => u,
        Err(_) => return Step::End(AttachEnd::LinkLost("The office sent a frame this laptop cannot read.".into())),
    };
    match update {
        TermUpdate::Attached(a) => {
            console.write(CLEAR);
            console.write(&a.vt);
            // The first attach claims the console's size once, when the session has another one. Every
            // later grid is drawn as it is, so a resync never claims the size again.
            if !sizing.claimed {
                sizing.claimed = true;
                if (a.cols, a.rows) != sizing.want {
                    let (cols, rows) = sizing.want;
                    if conn.resize(STREAM, cols, rows, true).is_err() || conn.resync(STREAM).is_err() {
                        return Step::End(AttachEnd::LinkLost("The connection closed while resizing.".into()));
                    }
                }
            }
        }
        TermUpdate::Data(bytes) => console.write(&bytes),
        TermUpdate::Resized { cols, rows } => {
            // Our own resize comes back as the same size: nothing to do. Another device resized the
            // session: the grid changed under the console, so draw it again at the session's size.
            // This never claims the size back.
            if (cols, rows) != sizing.want && conn.resync(STREAM).is_err() {
                return Step::End(AttachEnd::LinkLost("The connection closed while resizing.".into()));
            }
        }
        TermUpdate::Gap => {
            if conn.resync(STREAM).is_err() {
                return Step::End(AttachEnd::LinkLost("The connection closed while catching up.".into()));
            }
        }
        TermUpdate::Closed(reason) => return Step::End(AttachEnd::SessionEnded(reason)),
        TermUpdate::Ignored => {}
    }
    Step::Go
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_bracket_then_d_detaches_and_nothing_after_it_is_sent() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(b"ls\r"), Typed { send: b"ls\r".to_vec(), detach: false });
        assert_eq!(keys.feed(&[DETACH_KEY]), Typed { send: vec![], detach: false }, "the prefix is held back");
        assert_eq!(keys.feed(b"d"), Typed { send: vec![], detach: true });
        assert_eq!(keys.feed(b"x"), Typed { send: b"x".to_vec(), detach: false }, "a later attach starts fresh");
    }

    #[test]
    fn ctrl_bracket_twice_sends_one_ctrl_bracket() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(&[DETACH_KEY, DETACH_KEY]), Typed { send: vec![DETACH_KEY], detach: false });
    }

    #[test]
    fn ctrl_bracket_then_another_key_sends_that_key_only() {
        let mut keys = Keys::default();
        assert_eq!(keys.feed(&[DETACH_KEY, b'q']), Typed { send: b"q".to_vec(), detach: false });
        assert_eq!(keys.feed(b"abc"), Typed { send: b"abc".to_vec(), detach: false });
    }

    #[test]
    fn a_prefix_split_over_two_reads_still_detaches() {
        let mut keys = Keys::default();
        assert!(!keys.feed(b"a").detach);
        assert!(!keys.feed(&[DETACH_KEY]).detach);
        assert!(keys.feed(b"d").detach);
    }

    #[test]
    fn bytes_after_the_detach_in_the_same_read_are_not_sent() {
        let mut keys = Keys::default();
        let typed = keys.feed(&[b'a', DETACH_KEY, b'd', b'z']);
        assert_eq!(typed, Typed { send: b"a".to_vec(), detach: true });
    }

    #[test]
    fn a_closed_link_is_explained_in_plain_words() {
        assert!(explain_closed(Some(ByeReason::Revoked)).contains("removed"));
        assert!(explain_closed(Some(ByeReason::ServerOff)).contains("turned remote access off"));
        assert!(explain_closed(Some(ByeReason::ServerQuit)).contains("quit"));
        assert_eq!(explain_closed(None), "The connection closed.");
    }

    #[test]
    fn a_session_end_is_explained() {
        assert!(explain_session_end(TermClosedReason::SessionGone).contains("closed"));
        assert!(explain_session_end(TermClosedReason::NotRunning).contains("not running"));
    }

    #[test]
    fn the_client_folder_is_beside_the_state_file() {
        let state = Path::new("/home/me/csm/state.json");
        assert_eq!(client_folder(state), Some(Path::new("/home/me/csm/peer-client").to_path_buf()));
    }

    #[test]
    fn an_unpaired_folder_is_refused_without_writing_anything() {
        let dir = std::env::temp_dir().join(format!("csm-attach-unpaired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let err = connect(&dir, "ABC234").expect_err("nothing is paired");
        assert!(err.contains("no device key"), "{err}");
        assert!(!dir.exists(), "attach never makes a folder or a key");
    }
}
