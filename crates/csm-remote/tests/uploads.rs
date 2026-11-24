//! File drops from a remote window over the real peer link (design: "Uploads: file drops and image
//! paste", steps 1 and 2): the laptop sends its files to the office over the encrypted link in
//! chunks, the office writes them into the session's folder, and the office's own tool pastes the
//! paths into the session. The fake PTY is the only fake. Image paste reads the clipboard, which a
//! test machine may not have, so it is covered by the UI tests and the unit tests instead.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use csm_remote::RemoteBackend;
use csm_ui::UploadEvent;
use csm_ui::backend::{Backend as UiBackend, ToolPort};
use serde_json::json;

/// Polls the backend as the UI does, until an event satisfies `done`. Returns every event seen.
fn until(b: &mut RemoteBackend, what: &str, mut done: impl FnMut(&UploadEvent) -> bool) -> Vec<UploadEvent> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    loop {
        let _ = b.poll();
        for ev in UiBackend::upload_events(b) {
            let stop = done(&ev);
            seen.push(ev);
            if stop {
                return seen;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {seen:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Where the office keeps a session's uploads (`<data>/peer/uploads/<session>`).
fn session_folder(office: &Office, id: &str) -> PathBuf {
    office.dir.join("peer").join("uploads").join(id)
}

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map(|rd| rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

fn pasted(pty: &FakePty) -> String {
    String::from_utf8_lossy(&pty.written()).into_owned()
}

#[test]
fn dropped_files_reach_the_office_and_their_paths_are_pasted_into_the_session() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    // Bracketed paste is on in this session, as it is in Claude Code.
    pty.emit(b"\x1b[?2004h");
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("notes.txt");
    std::fs::write(&small, b"hello from the laptop").unwrap();
    // More than the credit window (8 chunks of 32 KiB), so acknowledgements are needed to finish.
    let big = dir.path().join("big.bin");
    let big_bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&big, &big_bytes).unwrap();

    let drop = b.upload_files(&id, vec![small.to_string_lossy().into_owned(), big.to_string_lossy().into_owned()]).unwrap();
    let seen = until(&mut b, "the drop", |e| matches!(e, UploadEvent::Done { .. } | UploadEvent::Failed { .. }));
    let Some(UploadEvent::Done { drop: done_drop, session, paths }) = seen.last().cloned() else { panic!("{seen:?}") };
    assert_eq!((done_drop, session.as_str()), (drop, id.as_str()));
    assert_eq!(paths.len(), 2, "one path per file, in drop order");
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"hello from the laptop");
    assert_eq!(std::fs::read(&paths[1]).unwrap(), big_bytes, "the bytes arrive whole");
    for p in &paths {
        assert!(Path::new(p).starts_with(session_folder(&office, &id)), "{p} is in the session's folder, not the project's");
    }
    assert!(seen.iter().any(|e| matches!(e, UploadEvent::Progress { sent, .. } if *sent > 0)), "progress was reported");

    // The paste is the office's tool: it quotes the paths for the session and pastes them.
    call(&mut b, "session_drop_paths", json!({"session_id": id, "paths": paths})).unwrap();
    let written = wait_for("the pasted paths", || {
        let w = pasted(&pty);
        (w.contains("notes.txt") && w.contains("big.bin")).then_some(w)
    });
    assert!(written.contains("\x1b[200~"), "a bracketed paste: {written:?}");
    assert!(!written.ends_with('\r'), "never followed by Enter: {written:?}");
}

#[test]
fn a_folder_in_a_drop_is_refused_on_the_laptop_and_nothing_reaches_the_office() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Photos");
    std::fs::create_dir(&folder).unwrap();
    let drop = b.upload_files(&id, vec![folder.to_string_lossy().into_owned()]).unwrap();
    let seen = until(&mut b, "the refusal", |e| matches!(e, UploadEvent::Failed { .. }));
    let Some(UploadEvent::Failed { drop: failed, message }) = seen.last().cloned() else { panic!("{seen:?}") };
    assert_eq!(failed, drop);
    assert!(message.contains("folder") && message.contains("Photos"), "{message}");
    assert!(files_in(&session_folder(&office, &id)).is_empty(), "nothing was written on the office");
    assert!(!pasted(&pty).contains("Photos"), "nothing was pasted");
}

#[test]
fn cancelling_a_drop_deletes_the_partial_file_on_the_office_and_pastes_nothing() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    let dir = tempfile::tempdir().unwrap();
    let huge = dir.path().join("huge.bin");
    std::fs::write(&huge, vec![0u8; 48 << 20]).unwrap();
    let drop = b.upload_files(&id, vec![huge.to_string_lossy().into_owned()]).unwrap();
    // Wait until the office has acknowledged some of it: the file is really in flight.
    until(&mut b, "the first acknowledged bytes", |e| matches!(e, UploadEvent::Progress { sent, .. } if *sent > 0));
    b.upload_cancel(drop);
    let seen = until(&mut b, "the cancel", |e| matches!(e, UploadEvent::Cancelled { .. } | UploadEvent::Done { .. } | UploadEvent::Failed { .. }));
    assert!(matches!(seen.last(), Some(UploadEvent::Cancelled { drop: d }) if *d == drop), "cancelled, not finished: {seen:?}");
    wait_for("the office to delete the partial file", || files_in(&session_folder(&office, &id)).is_empty().then_some(()));
    assert!(!pasted(&pty).contains("huge.bin"), "nothing was pasted");
}
