//! The office side of uploads from a remote window (design: "Uploads: file drops and image paste",
//! sections 2 to 8).
//!
//! The link reader validates each upload frame and hands the bytes to a worker thread per upload
//! ([`UploadHub`]). The reader never waits for the disk: a laptop may have at most
//! [`UPLOAD_CREDIT`] chunks unacknowledged, and the channel to the worker holds one more slot than
//! that (for `UploadEnd`), so a compliant sender never fills it. A worker writes `<stamp>-<name>.part`, hashes as it goes,
//! acknowledges every [`UPLOAD_ACK_EVERY`] chunks, and on `UploadEnd` checks the size and digest
//! and renames the file into place. Any failure deletes the partial file and refuses the upload;
//! the link stays up.
//!
//! Nothing here trusts a name, a size or a session id from the laptop. Names are sanitised
//! ([`sanitise_name`]); a session id becomes a folder name only when it is `[A-Za-z0-9_-]`
//! ([`is_folder_id`]); images must start with the magic bytes of a supported format ([`sniff_image`]).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use csm_peer_proto::ids::hex;
use csm_peer_proto::limits::{MAX_UPLOAD_NAME_BYTES, MAX_UPLOAD_SESSION_ID, UPLOAD_ACK_EVERY, UPLOAD_ACTIVE_TOTAL, UPLOAD_CREDIT};
use csm_peer_proto::msg::{Msg, UploadKind, UploadRefused};
use sha2::{Digest, Sha256};

use crate::audit::{Audit, clean};
use crate::fsperm;
use crate::link::Link;

/// With an upload open, no chunk for this long ends it.
pub const UPLOAD_STALL: Duration = Duration::from_secs(30);
/// Finished uploads older than this are removed when the office starts.
pub const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Partial files older than this are removed when the office starts.
pub const KEEP_PARTS_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// `<peer folder>/uploads`: the only place uploads are written.
pub fn root_in(peer_dir: &Path) -> PathBuf {
    peer_dir.join("uploads")
}

/// A session id usable as a folder name: `[A-Za-z0-9_-]`, 1 to 64 bytes.
pub fn is_folder_id(id: &str) -> bool {
    (1..=MAX_UPLOAD_SESSION_ID).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The name the audit log and the refusal use. The wire enum's own name, never free text.
pub fn reason_name(reason: UploadRefused) -> &'static str {
    match reason {
        UploadRefused::TooLarge => "too_large",
        UploadRefused::TooMany => "too_many",
        UploadRefused::Forbidden => "forbidden",
        UploadRefused::NoSession => "no_session",
        UploadRefused::BadName => "bad_name",
        UploadRefused::NotAnImage => "not_an_image",
        UploadRefused::Overrun => "overrun",
        UploadRefused::DiskFull => "disk_full",
        UploadRefused::Corrupt => "corrupt",
        UploadRefused::Stalled => "stalled",
        UploadRefused::Failed => "failed",
    }
}

/// Invisible format characters that would hide or reorder a name.
fn is_invisible(c: char) -> bool {
    matches!(c as u32, 0x00AD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF)
}

/// A Windows device name, with or without an extension (`CON.txt` is still `CON`).
fn is_reserved(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes().get(3).copied().is_some_and(|b| (b'1'..=b'9').contains(&b)))
}

/// Turns the laptop's name into a file name the office can use. The name is a label only, so
/// every rule here is about the result, never about what the laptop meant. An image is named
/// `clipboard` here; the worker adds the extension it sniffed.
pub fn sanitise_name(raw: &str, kind: UploadKind) -> String {
    if kind == UploadKind::Image {
        return "clipboard".to_owned();
    }
    // Keep the last component after either separator.
    let last = raw.rsplit(['\\', '/']).next().unwrap_or("");
    let replaced: String = last
        .chars()
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let mut name = replaced.trim().trim_end_matches(['.', ' ']).to_owned();
    if name.is_empty() || name == "." || name == ".." {
        return "file".to_owned();
    }
    let stem = name.split('.').next().unwrap_or("");
    if is_reserved(stem) {
        name.insert(0, '_');
    }
    cap_length(name)
}

/// At most [`MAX_UPLOAD_NAME_BYTES`] bytes, cut on a character boundary, keeping a short extension.
fn cap_length(name: String) -> String {
    if name.len() <= MAX_UPLOAD_NAME_BYTES {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 => (name.get(..i).unwrap_or(""), name.get(i..).unwrap_or("")),
        _ => (name.as_str(), ""),
    };
    let room = MAX_UPLOAD_NAME_BYTES.saturating_sub(ext.len());
    let mut out = String::new();
    for c in stem.chars() {
        if out.len() + c.len_utf8() > room {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        out.push_str("file");
    }
    out.push_str(ext);
    out
}

/// The extension of a supported image, from its first bytes: PNG, JPEG, GIF, WebP or BMP.
pub fn sniff_image(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("gif")
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("webp")
    } else if head.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

/// What the cleanup did, for the log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: usize,
    pub failed: usize,
}

/// Removes old uploads under `root` (one folder per session). Only regular files directly inside
/// a session folder are touched; symbolic links and anything else are skipped, never followed.
pub fn clean_old(root: &Path, now: SystemTime) -> Cleaned {
    let mut out = Cleaned::default();
    let Ok(folders) = std::fs::read_dir(root) else { return out };
    for folder in folders.flatten() {
        let path = folder.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&path) else { continue };
        for file in files.flatten() {
            let p = file.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else {
                out.failed += 1;
                continue;
            };
            if !m.is_file() || m.file_type().is_symlink() {
                continue;
            }
            let Ok(modified) = m.modified() else { continue };
            let age = now.duration_since(modified).unwrap_or_default();
            let is_part = p.extension().is_some_and(|e| e == "part");
            if age <= if is_part { KEEP_PARTS_FOR } else { KEEP_FOR } {
                continue;
            }
            match std::fs::remove_file(&p) {
                Ok(()) => out.removed += 1,
                Err(_) => out.failed += 1,
            }
        }
    }
    out
}

/// What the link reader hands the worker.
enum Job {
    Chunk { offset: u64, bytes: Vec<u8> },
    End { sha256: String },
}

/// One upload the reader has accepted.
pub struct Plan {
    pub id: u64,
    pub kind: UploadKind,
    pub size: u64,
    pub session: String,
    pub device: String,
    /// Already sanitised ([`sanitise_name`]).
    pub name: String,
    /// The session's folder under the uploads root.
    pub dir: PathBuf,
    /// Unix milliseconds when the upload began.
    pub stamp_ms: u128,
}

/// Counts an active upload against the total across all links; released when its worker ends.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(total: &Arc<AtomicUsize>) -> Result<Slot, UploadRefused> {
        if total.fetch_add(1, Ordering::AcqRel) >= UPLOAD_ACTIVE_TOTAL {
            total.fetch_sub(1, Ordering::AcqRel);
            return Err(UploadRefused::TooMany);
        }
        Ok(Slot(total.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Marks an upload finished when its worker returns, however it returns.
struct Finished(Arc<AtomicBool>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Why the reader ended an upload, if it has to tell the laptop. Set before the reader drops the
/// sender; the worker reads it when the channel closes. `None` at close is a silent end.
type StopReason = Arc<Mutex<Option<UploadRefused>>>;

fn take_reason(stop: &StopReason) -> Option<UploadRefused> {
    stop.lock().unwrap_or_else(PoisonError::into_inner).take()
}

struct Active {
    tx: SyncSender<Job>,
    stop: StopReason,
    finished: Arc<AtomicBool>,
    last: Instant,
}

/// The uploads open on one link. Dropping it ends them all silently (the partial files go): the
/// link closed, the device was revoked, or the server was turned off.
pub(crate) struct UploadHub {
    active: HashMap<u64, Active>,
    total: Arc<AtomicUsize>,
    audit: Arc<Audit>,
}

impl UploadHub {
    /// `total` is shared by every link of the server.
    pub(crate) fn new(total: Arc<AtomicUsize>, audit: Arc<Audit>) -> Self {
        Self { active: HashMap::new(), total, audit }
    }

    fn prune(&mut self) {
        self.active.retain(|_, a| !a.finished.load(Ordering::Acquire));
    }

    /// Starts an upload: creates the partial file and its worker, then sends `UploadAccepted`.
    /// `Err` is the refusal to send; nothing is left behind.
    ///
    /// The folder and the `.part` file are created here, on the reader thread: one short, bounded
    /// disk call per upload, accepted rather than moved to the worker. A slow create (antivirus,
    /// on Windows) delays only the frames behind this `UploadBegin` on its link. Chunk writes,
    /// hashing, fsync and the rename all run on the worker.
    pub(crate) fn begin(&mut self, plan: Plan, link: &Arc<Link>) -> Result<(), UploadRefused> {
        self.prune();
        if self.active.contains_key(&plan.id) {
            return Err(UploadRefused::Failed);
        }
        let slot = Slot::take(&self.total)?;
        fsperm::ensure_private_dir(&plan.dir).map_err(|_| UploadRefused::Failed)?;
        let part = plan.dir.join(format!("{}-{}.part", plan.stamp_ms, plan.name));
        let file = OpenOptions::new().write(true).create_new(true).open(&part).map_err(|_| UploadRefused::Failed)?;
        let (tx, rx) = mpsc::sync_channel::<Job>(UPLOAD_CREDIT + 1);
        let finished = Arc::new(AtomicBool::new(false));
        let stop: StopReason = Arc::new(Mutex::new(None));
        let worker = Worker {
            id: plan.id,
            kind: plan.kind,
            size: plan.size,
            session: plan.session,
            device: plan.device,
            name: plan.name,
            dir: plan.dir,
            stamp_ms: plan.stamp_ms,
            part: part.clone(),
            file: Some(file),
            received: 0,
            hasher: Sha256::new(),
            since_ack: 0,
            ext: None,
            link: link.clone(),
            audit: self.audit.clone(),
            stop: stop.clone(),
        };
        let done = Finished(finished.clone());
        let spawned = std::thread::Builder::new().name("csm-peer-upload".into()).spawn(move || {
            let _slot = slot;
            let _done = done;
            worker.run(&rx);
        });
        if spawned.is_err() {
            // The closure (and with it the worker's file) was dropped by the failed spawn.
            let _ = std::fs::remove_file(&part);
            return Err(UploadRefused::Failed);
        }
        self.active.insert(plan.id, Active { tx, stop, finished, last: Instant::now() });
        let _ = link.send_msg(Msg::UploadAccepted { id: plan.id });
        Ok(())
    }

    /// One chunk from the laptop. `Err` is a refusal for the reader to send, for an upload that is
    /// not open. A chunk beyond the credit ends its upload (the worker refuses it `Overrun`).
    pub(crate) fn chunk(&mut self, id: u64, offset: u64, bytes: Vec<u8>) -> Result<(), UploadRefused> {
        self.prune();
        let Some(a) = self.active.get_mut(&id) else { return Err(UploadRefused::Overrun) };
        match a.tx.try_send(Job::Chunk { offset, bytes }) {
            Ok(()) => {
                a.last = Instant::now();
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.end_with(id, Some(UploadRefused::Overrun));
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(UploadRefused::Overrun),
        }
    }

    /// `UploadEnd`: the worker checks the size and digest, then finishes or refuses.
    pub(crate) fn end(&mut self, id: u64, sha256: String) -> Result<(), UploadRefused> {
        self.prune();
        let Some(a) = self.active.get_mut(&id) else { return Err(UploadRefused::Overrun) };
        a.last = Instant::now();
        match a.tx.try_send(Job::End { sha256 }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.end_with(id, Some(UploadRefused::Overrun));
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(UploadRefused::Overrun),
        }
    }

    /// `UploadCancel` from the laptop: the partial file goes, and nothing is sent back.
    pub(crate) fn cancel(&mut self, id: u64) {
        self.end_with(id, None);
    }

    /// Ends the uploads with no chunk for [`UPLOAD_STALL`]. The worker refuses them `Stalled`.
    pub(crate) fn expire_stalled(&mut self, now: Instant) {
        self.prune();
        let stalled: Vec<u64> = self.active.iter().filter(|(_, a)| now.duration_since(a.last) > UPLOAD_STALL).map(|(id, _)| *id).collect();
        for id in stalled {
            self.end_with(id, Some(UploadRefused::Stalled));
        }
    }

    /// Ends an upload: the reason (if any) is stored, then the sender is dropped, which is what the
    /// worker waits for. The worker is never waited for here.
    fn end_with(&mut self, id: u64, reason: Option<UploadRefused>) {
        if let (Some(a), Some(r)) = (self.active.remove(&id), reason) {
            *a.stop.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        }
    }
}

/// Everything a worker thread owns while one upload is open.
struct Worker {
    id: u64,
    kind: UploadKind,
    size: u64,
    session: String,
    device: String,
    name: String,
    dir: PathBuf,
    stamp_ms: u128,
    part: PathBuf,
    file: Option<File>,
    received: u64,
    hasher: Sha256,
    since_ack: usize,
    ext: Option<&'static str>,
    link: Arc<Link>,
    audit: Arc<Audit>,
    stop: StopReason,
}

impl Worker {
    fn run(mut self, rx: &Receiver<Job>) {
        loop {
            // The reader dropped its sender: the upload was ended. Any reason it left is for the
            // laptop; no reason is a silent end (cancel, the link closed, the server went off).
            let Ok(job) = rx.recv() else {
                return match take_reason(&self.stop) {
                    Some(reason) => self.refuse(reason),
                    None => self.discard(),
                };
            };
            match job {
                Job::Chunk { offset, bytes } => {
                    if let Err(reason) = self.write(offset, &bytes) {
                        return self.refuse(reason);
                    }
                    if self.since_ack >= UPLOAD_ACK_EVERY {
                        self.since_ack = 0;
                        let _ = self.link.send_msg(Msg::UploadAck { id: self.id, received: self.received });
                    }
                }
                Job::End { sha256 } => {
                    return match self.finish(&sha256) {
                        Ok(path) => self.done(path),
                        Err(reason) => self.refuse(reason),
                    };
                }
            }
        }
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<(), UploadRefused> {
        if offset != self.received {
            return Err(UploadRefused::Overrun);
        }
        let len = u64::try_from(bytes.len()).map_err(|_| UploadRefused::TooLarge)?;
        if self.received.saturating_add(len) > self.size {
            return Err(UploadRefused::TooLarge);
        }
        if self.kind == UploadKind::Image && self.received == 0 {
            self.ext = Some(sniff_image(bytes).ok_or(UploadRefused::NotAnImage)?);
        }
        let file = self.file.as_mut().ok_or(UploadRefused::Failed)?;
        file.write_all(bytes).map_err(|e| {
            if e.kind() == std::io::ErrorKind::StorageFull || matches!(e.raw_os_error(), Some(28 | 112)) {
                UploadRefused::DiskFull
            } else {
                UploadRefused::Failed
            }
        })?;
        self.hasher.update(bytes);
        self.received += len;
        self.since_ack += 1;
        Ok(())
    }

    /// Checks the size and digest, makes the file durable, and renames it into place.
    fn finish(&mut self, sha256: &str) -> Result<PathBuf, UploadRefused> {
        if self.received != self.size {
            return Err(UploadRefused::Corrupt);
        }
        let digest = hex(&std::mem::take(&mut self.hasher).finalize());
        if !digest.eq_ignore_ascii_case(sha256) {
            return Err(UploadRefused::Corrupt);
        }
        let base = match self.kind {
            UploadKind::File => self.name.clone(),
            UploadKind::Image => format!("clipboard.{}", self.ext.ok_or(UploadRefused::NotAnImage)?),
        };
        let file = self.file.take().ok_or(UploadRefused::Failed)?;
        file.sync_all().map_err(|_| UploadRefused::Failed)?;
        drop(file);
        let target = self.free_name(&base)?;
        std::fs::rename(&self.part, &target).map_err(|_| UploadRefused::Failed)?;
        Ok(target)
    }

    /// `<stamp>-<base>`, or with ` (n)` before the extension when that name is taken.
    fn free_name(&self, base: &str) -> Result<PathBuf, UploadRefused> {
        let first = self.dir.join(format!("{}-{}", self.stamp_ms, base));
        if !first.exists() {
            return Ok(first);
        }
        let (stem, ext) = match base.rfind('.') {
            Some(i) if i > 0 => (base.get(..i).unwrap_or(base), base.get(i..).unwrap_or("")),
            _ => (base, ""),
        };
        for n in 2..=999u32 {
            let candidate = self.dir.join(format!("{}-{stem} ({n}){ext}", self.stamp_ms));
            if !candidate.exists() {
                return Ok(candidate);
            }
        }
        Err(UploadRefused::Failed)
    }

    fn done(&self, path: PathBuf) {
        let _ = self.link.send_msg(Msg::UploadAck { id: self.id, received: self.received });
        let shown = path.display().to_string();
        let _ = self.link.send_msg(Msg::UploadDone { id: self.id, path: shown.clone() });
        self.audit.record(
            "upload_done",
            serde_json::json!({"session": clean(&self.session), "device": clean(&self.device), "size": self.received, "path": clean(&shown)}),
        );
    }

    fn refuse(mut self, reason: UploadRefused) {
        self.discard();
        let _ = self.link.send_msg(Msg::UploadRefused { id: self.id, reason });
        self.audit.record(
            "upload_refused",
            serde_json::json!({"session": clean(&self.session), "device": clean(&self.device), "reason": reason_name(reason)}),
        );
    }

    fn discard(&mut self) {
        self.file = None;
        if let Err(e) = std::fs::remove_file(&self.part)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("remote access: could not delete a partial upload: {e}");
        }
    }
}

/// Cleanup on start, for the log: how many old files went and how many could not.
pub fn cleanup_summary(c: Cleaned) -> String {
    format!("removed {} old upload file(s), {} could not be removed", c.removed, c.failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_never_paths_or_reserved() {
        let f = UploadKind::File;
        assert_eq!(sanitise_name("report.pdf", f), "report.pdf");
        assert_eq!(sanitise_name("..\\..\\Windows\\evil.exe", f), "evil.exe");
        assert_eq!(sanitise_name("/etc/passwd", f), "passwd");
        assert_eq!(sanitise_name("..", f), "file");
        assert_eq!(sanitise_name(".", f), "file");
        assert_eq!(sanitise_name("   ", f), "file");
        assert_eq!(sanitise_name("a:b*c?.txt", f), "a_b_c_.txt");
        assert_eq!(sanitise_name("trailing dots...", f), "trailing dots");
        assert_eq!(sanitise_name("CON", f), "_CON");
        assert_eq!(sanitise_name("nul.txt", f), "_nul.txt");
        assert_eq!(sanitise_name("com1", f), "_com1");
        assert_eq!(sanitise_name("LPT9.log", f), "_LPT9.log");
        assert_eq!(sanitise_name("COM10", f), "COM10", "COM10 is not reserved");
        assert_eq!(sanitise_name("bi\u{202E}div.exe", f), "bidiv.exe", "bidi overrides are dropped");
        assert_eq!(sanitise_name("a\u{0007}b\nc.txt", f), "abc.txt");
        assert_eq!(sanitise_name("shot.png", UploadKind::Image), "clipboard", "an image is named by its sniffed type");
        let long = format!("{}.pdf", "x".repeat(300));
        let capped = sanitise_name(&long, f);
        assert!(capped.len() <= MAX_UPLOAD_NAME_BYTES && capped.ends_with(".pdf"), "{capped}");
        let accents = format!("{}.txt", "é".repeat(200));
        let capped = sanitise_name(&accents, f);
        assert!(capped.len() <= MAX_UPLOAD_NAME_BYTES && capped.ends_with(".txt"), "{capped}");
    }

    #[test]
    fn session_ids_must_be_plain_folder_names() {
        assert!(is_folder_id("s1"));
        assert!(is_folder_id("a-b_C9"));
        assert!(!is_folder_id(""));
        assert!(!is_folder_id("../x"));
        assert!(!is_folder_id("a/b"));
        assert!(!is_folder_id("a\\b"));
        assert!(!is_folder_id("a.b"));
        assert!(!is_folder_id("a b"));
        assert!(!is_folder_id(&"a".repeat(65)));
        assert!(is_folder_id(&"a".repeat(64)));
    }

    #[test]
    fn images_are_sniffed_by_magic_bytes() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("gif"));
        assert_eq!(sniff_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_image(b"BM\x00\x00"), Some("bmp"));
        assert_eq!(sniff_image(b"%PDF-1.7"), None);
        assert_eq!(sniff_image(b"RIFF\x00\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn old_uploads_go_and_nothing_else_does() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("uploads");
        let session = root.join("s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("old.bin"), b"x").unwrap();
        std::fs::write(session.join("fresh.bin"), b"x").unwrap();
        std::fs::write(session.join("half.part"), b"x").unwrap();
        let outside = tmp.path().join("keep.bin");
        std::fs::write(&outside, b"x").unwrap();
        let eight_days = SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60);
        // Everything is eight days old: the finished file and the partial one are both removed.
        let c = clean_old(&root, eight_days);
        assert_eq!(c, Cleaned { removed: 3, failed: 0 });
        assert!(outside.exists(), "nothing outside the uploads folder is touched");
        std::fs::write(session.join("fresh.bin"), b"x").unwrap();
        let c = clean_old(&root, SystemTime::now());
        assert_eq!(c, Cleaned { removed: 0, failed: 0 }, "recent files stay");
        assert!(session.join("fresh.bin").exists());
        // A missing root is not an error.
        assert_eq!(clean_old(&tmp.path().join("nope"), SystemTime::now()), Cleaned::default());
    }

    #[test]
    fn a_partial_file_goes_after_a_day_and_a_finished_one_stays_for_a_week() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("uploads");
        let session = root.join("s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("a.part"), b"x").unwrap();
        std::fs::write(session.join("b.bin"), b"x").unwrap();
        let two_days = SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
        assert_eq!(clean_old(&root, two_days), Cleaned { removed: 1, failed: 0 });
        assert!(!session.join("a.part").exists());
        assert!(session.join("b.bin").exists());
    }
}
