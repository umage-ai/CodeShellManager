//! File drops and image paste in a remote window (design: "Uploads: file drops and image paste",
//! sections 5 and 6), on the demo stand-in for a remote host with a fake upload link. The drop and
//! the paste ask the link to upload; the card follows the link's events; Cancel reaches the link;
//! a finished drop pastes the office's paths through `session_drop_paths`; a failed one says why
//! and pastes nothing. The real link is tested in csm-remote's `tests/uploads.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::*;
use csm_ui::Action;
use csm_ui::backend::UploadEvent;
use csm_ui::demo::FakeUploadLink;
use csm_ui::remote_window::{RemoteLink, RemoteStatus};
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use serde_json::{Value, json};

type H = Harness<'static, csm_ui::CsmApp>;

fn status() -> RemoteStatus {
    RemoteStatus { host_name: "OFFICE".into(), address: "10.8.0.2:47300".into(), link: RemoteLink::Connected }
}

fn link() -> Arc<Mutex<FakeUploadLink>> {
    Arc::new(Mutex::new(FakeUploadLink::default()))
}

/// A remote window whose uploads go to `link`.
fn remote(link: &Arc<Mutex<FakeUploadLink>>) -> H {
    let mut b = backend(ThemeKind::Mocha);
    b.set_remote(Some(status()));
    b.set_fake_upload_link(link.clone());
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

/// A remote window with no upload link at all (the window has no office to send to).
fn remote_without_link() -> H {
    let mut b = backend(ThemeKind::Mocha);
    b.set_remote(Some(status()));
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

fn push(link: &Arc<Mutex<FakeUploadLink>>, ev: UploadEvent) {
    link.lock().unwrap().events.push_back(ev);
}

fn asked(link: &Arc<Mutex<FakeUploadLink>>) -> Vec<(u64, String, Vec<String>)> {
    link.lock().unwrap().asked.clone()
}

fn calls(h: &H, id: &str) -> Vec<Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn run(h: &mut H, action: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, action);
    settle(h);
}

fn toasts(h: &H) -> Vec<String> {
    h.state().ui.toasts.iter().map(|t| t.text.clone()).collect()
}

fn spaced_path() -> &'static str {
    if cfg!(windows) { r"C:\Users\alice\My Documents\notes.txt" } else { "/home/alice/My Documents/notes.txt" }
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

/// Drops `paths` on the active pane (the demo's Odulodo.Web, the session "web").
fn drop_files(h: &mut H, paths: &[&str]) {
    let input = h.input_mut();
    for p in paths {
        input.dropped_files.push(Arc::new(Dropped(PathBuf::from(p))));
    }
    h.step();
    settle(h);
}

#[test]
fn a_file_dropped_on_a_remote_pane_goes_up_to_the_office_and_nothing_is_pasted_yet() {
    let l = link();
    let mut h = remote(&l);
    drop_files(&mut h, &[spaced_path()]);

    let a = asked(&l);
    assert_eq!(a.len(), 1, "one drop asked of the link");
    assert_eq!((a[0].1.as_str(), a[0].2.clone()), ("web", vec![spaced_path().to_owned()]));
    assert!(calls(&h, "session_drop_paths").is_empty(), "the laptop's path means nothing on the office: nothing is pasted yet");
    assert!(calls(&h, "session_send_text").is_empty());
    assert_eq!(h.state().ui.uploads.len(), 1, "the card shows the drop");
    assert!(toasts(&h).is_empty(), "no refusal toast: the drop goes ahead");
}

#[test]
fn the_card_follows_the_link_and_a_done_drop_pastes_the_office_paths() {
    let l = link();
    let mut h = remote(&l);
    drop_files(&mut h, &[spaced_path()]);
    let drop = asked(&l)[0].0;

    push(&l, UploadEvent::Progress { drop, sent: 10, total: 100, files_done: 0, files_total: 1, file: "notes.txt".into() });
    settle(&mut h);
    let row = h.state().ui.uploads.iter().find(|r| r.drop == drop).cloned().expect("the row is still there");
    assert_eq!((row.sent, row.total, row.file.as_str()), (10, 100, "notes.txt"));

    push(&l, UploadEvent::Done { drop, session: "web".into(), paths: vec!["/office/uploads/web/1-notes.txt".into()] });
    settle(&mut h);
    assert!(h.state().ui.uploads.is_empty(), "the card goes when the drop is done");
    let pasted = calls(&h, "session_drop_paths");
    assert_eq!(pasted.len(), 1, "one paste for the one drop");
    assert_eq!(pasted[0]["session_id"], "web");
    assert_eq!(pasted[0]["paths"], json!(["/office/uploads/web/1-notes.txt"]));
    assert!(calls(&h, "session_send_text").is_empty(), "the office tool pastes, not the laptop");
}

#[test]
fn a_failed_drop_says_why_and_pastes_nothing() {
    let l = link();
    let mut h = remote(&l);
    drop_files(&mut h, &[spaced_path()]);
    let drop = asked(&l)[0].0;

    push(&l, UploadEvent::Failed { drop, message: "a folder was dropped: drop files only".into() });
    settle(&mut h);
    assert!(h.state().ui.uploads.is_empty(), "the card goes");
    assert!(toasts(&h).iter().any(|t| t.contains("folder")), "the person is told why: {:?}", toasts(&h));
    assert!(calls(&h, "session_drop_paths").is_empty(), "nothing pasted");
}

#[test]
fn cancel_reaches_the_link_and_the_card_says_nothing_was_pasted() {
    let l = link();
    let mut h = remote(&l);
    drop_files(&mut h, &[spaced_path()]);
    let drop = asked(&l)[0].0;

    run(&mut h, Action::UploadCancel(drop));
    assert_eq!(l.lock().unwrap().cancelled, vec![drop], "the link is told");

    push(&l, UploadEvent::Cancelled { drop });
    settle(&mut h);
    assert!(h.state().ui.uploads.is_empty());
    assert!(toasts(&h).iter().any(|t| t.contains("Nothing was pasted")), "{:?}", toasts(&h));
    assert!(calls(&h, "session_drop_paths").is_empty());
}

#[test]
fn an_image_pasted_in_a_remote_window_is_uploaded_and_its_office_path_pasted() {
    let l = link();
    let mut h = remote(&l);
    run(&mut h, Action::PasteImage("web".into()));

    let a = asked(&l);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].2, vec!["clipboard.png".to_owned()], "the clipboard's image, uploaded as a file");
    assert!(calls(&h, "session_send_text").is_empty(), "no raw Ctrl+V reaches the office's clipboard");
    assert_eq!(h.state().ui.uploads.len(), 1);

    push(&l, UploadEvent::Done { drop: a[0].0, session: "web".into(), paths: vec!["/office/uploads/web/2-clipboard.png".into()] });
    settle(&mut h);
    let pasted = calls(&h, "session_drop_paths");
    assert_eq!(pasted.last().map(|p| p["paths"].clone()), Some(json!(["/office/uploads/web/2-clipboard.png"])), "the office path is pasted");
}

#[test]
fn a_clipboard_without_an_image_says_so_and_uploads_nothing() {
    let l = link();
    let mut h = remote(&l);
    run(&mut h, Action::PasteImage("web".into()));
    let drop = asked(&l)[0].0;
    push(&l, UploadEvent::Failed { drop, message: "there is no image on the clipboard to paste".into() });
    settle(&mut h);
    assert!(toasts(&h).iter().any(|t| t.contains("no image")), "{:?}", toasts(&h));
    assert!(calls(&h, "session_drop_paths").is_empty());
}

#[test]
fn a_remote_window_with_no_link_refuses_a_drop_with_a_toast() {
    let mut h = remote_without_link();
    drop_files(&mut h, &[spaced_path()]);
    assert!(h.state().ui.uploads.is_empty(), "no card for a drop that never started");
    assert!(toasts(&h).iter().any(|t| t.contains("office")), "{:?}", toasts(&h));
    assert!(calls(&h, "session_drop_paths").is_empty());
}

#[test]
fn two_drops_keep_two_cards_and_each_finishes_on_its_own() {
    let l = link();
    let mut h = remote(&l);
    drop_files(&mut h, &[spaced_path()]);
    drop_files(&mut h, &["/home/alice/a.png"]);
    let ids: Vec<u64> = asked(&l).iter().map(|a| a.0).collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(h.state().ui.uploads.len(), 2);

    push(&l, UploadEvent::Done { drop: ids[1], session: "web".into(), paths: vec!["/o/a.png".into()] });
    settle(&mut h);
    assert_eq!(h.state().ui.uploads.iter().map(|r| r.drop).collect::<Vec<_>>(), vec![ids[0]], "the other drop is still running");
    assert_eq!(calls(&h, "session_drop_paths").len(), 1);
}
