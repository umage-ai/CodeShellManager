//! Uploads from a remote window: the laptop sends its files (a drop) or the clipboard's image to the
//! office, in chunks, and the office writes them into the session's folder (`csm-peer`'s uploads).
//! This is the laptop's side of that protocol: the state of each drop and each file, the credit
//! window, the stall rule, and which frame goes out next.
//!
//! It runs on the link's worker thread, between the window's commands and the office's frames. Each
//! [`Uploads::pump`] sends at most one frame. Nothing here touches the UI, and no call here waits on
//! a disk: the size checks of a drop and the reading of each file run on their own threads (a slow
//! network share delays that file only, never a keystroke or a ping), and the worker takes what is
//! ready.
//!
//! A drop is all-or-nothing: the paste happens only when every file is in place, and a failed or
//! cancelled drop leaves nothing for the window to paste.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use csm_peer::uploads::UPLOAD_STALL;
use csm_peer_proto::codec::Frame;
use csm_peer_proto::limits::{MAX_UPLOAD_DROP, MAX_UPLOAD_FILES_PER_DROP, UPLOAD_CHUNK, UPLOAD_CREDIT};
use csm_peer_proto::msg::{Msg, UploadKind, UploadRefused};
use csm_ui::UploadEvent;
use sha2::{Digest, Sha256};

/// Uploads a device may have running at once. The rest wait in this queue.
pub const ACTIVE_PER_DEVICE: usize = 2;

/// The most unacknowledged bytes the laptop sends for one file (the office's credit, in bytes).
const CREDIT_BYTES: u64 = (UPLOAD_CREDIT * UPLOAD_CHUNK) as u64;

/// Where one file's bytes come from.
#[derive(Debug)]
pub enum Source {
    /// A file on this laptop, read as it is sent.
    File(PathBuf),
    /// Bytes already in memory: the clipboard's image, encoded as PNG.
    Memory { name: String, data: Vec<u8> },
}

/// One drop, as the window asked for it.
#[derive(Debug)]
pub struct Request {
    /// The window's id for the drop (it reports progress under this id).
    pub drop: u64,
    pub session: String,
    pub kind: UploadKind,
    pub files: Vec<Source>,
}

/// A file waiting for its turn on the link.
struct Pending {
    drop: u64,
    index: usize,
    kind: UploadKind,
    name: String,
    size: u64,
    source: Source,
}

/// What a reader thread hands over: the next chunk, or why it cannot.
type ChunkResult = Result<Vec<u8>, String>;

/// Chunks a reader thread may read ahead of the link.
const READ_AHEAD: usize = 2;

/// The bytes of a file being sent.
enum Reader {
    /// A file on this laptop that the office has not accepted yet: nothing is opened until it has.
    Unstarted(PathBuf),
    /// A reader thread feeding chunks of the file through a small bounded channel.
    File(Receiver<ChunkResult>),
    Memory { data: Vec<u8>, at: usize },
}

/// Starts a thread that opens a source and reads it in chunks of [`UPLOAD_CHUNK`] up to `size`
/// bytes, sending each through a channel of [`READ_AHEAD`] slots. It ends at the first error (sent
/// as the last item), when `size` bytes are read, or when the receiver is dropped.
fn spawn_reader<R: Read + Send + 'static>(open: impl FnOnce() -> std::io::Result<R> + Send + 'static, size: u64) -> Receiver<ChunkResult> {
    let (tx, rx) = mpsc::sync_channel::<ChunkResult>(READ_AHEAD);
    let spawned = std::thread::Builder::new().name("csm-upload-read".into()).spawn({
        let tx = tx.clone();
        move || {
            let mut source = match open() {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };
            let mut left = size;
            while left > 0 {
                let len = left.min(UPLOAD_CHUNK as u64) as usize;
                let mut buf = vec![0u8; len];
                if let Err(e) = source.read_exact(&mut buf) {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
                if tx.send(Ok(buf)).is_err() {
                    return;
                }
                left -= len as u64;
            }
        }
    });
    if let Err(e) = spawned {
        let _ = tx.send(Err(format!("could not start a reader: {e}")));
    }
    rx
}

/// A file on the link: begun, sent in chunks, ended.
struct Transfer {
    drop: u64,
    index: usize,
    /// The id on the wire (unique per link).
    wire: u64,
    name: String,
    size: u64,
    reader: Reader,
    hasher: Sha256,
    /// Bytes sent so far.
    offset: u64,
    /// Bytes the office has written, from its acks.
    received: u64,
    /// The office has accepted the begin: chunks may follow.
    accepted: bool,
    /// The end has been sent: nothing more follows but the office's answer.
    ended: bool,
    /// The last time this file moved (a begin, an accept, a chunk, an ack): the stall clock.
    last: Instant,
}

/// A drop the window is waiting for.
struct DropState {
    id: u64,
    session: String,
    /// The office's path for each file, by index, once it is in place.
    paths: Vec<Option<String>>,
    /// Bytes of the files already in place.
    done_bytes: u64,
    total: u64,
}

/// The files of a drop, with each name and size or why they cannot be sent.
type Measured = (Vec<Source>, Result<Vec<(String, u64)>, String>);

/// A drop of files whose sizes are being read on another thread.
struct Preparing {
    drop: u64,
    session: String,
    kind: UploadKind,
    rx: Receiver<Measured>,
}

/// Everything the window's uploads have in hand, for one link.
#[derive(Default)]
pub struct Uploads {
    preparing: Vec<Preparing>,
    drops: Vec<DropState>,
    queue: VecDeque<Pending>,
    active: Vec<Transfer>,
    next_wire: u64,
    /// Round robin over `active`: the next transfer that may send a chunk.
    turn: usize,
}

/// What one [`Uploads::pump`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Pumped {
    /// One frame went out.
    Sent,
    /// Nothing could be sent now (waiting for the office).
    Idle,
    /// The link is gone; the caller ends the connection.
    Lost,
}

/// Sends frames for the uploads. The link implements it; a test records what would be sent.
pub trait Wire {
    fn send(&mut self, msg: Msg) -> Result<(), ()>;
    fn send_frame(&mut self, frame: &Frame) -> Result<(), ()>;
}

/// What the person is told when the office refuses a file.
pub fn refusal_text(reason: UploadRefused) -> &'static str {
    match reason {
        UploadRefused::TooLarge => "the file is larger than the office accepts",
        UploadRefused::TooMany => "the office is already receiving several uploads; try again in a moment",
        UploadRefused::Forbidden => "the office refused uploads: remote access is off on the office, this device was removed, or it may not type into sessions",
        UploadRefused::NoSession => "the session is gone on the office",
        UploadRefused::BadName => "the office could not use the file's name",
        UploadRefused::NotAnImage => "the clipboard's image is not one the office accepts",
        UploadRefused::Overrun => "the upload got out of step with the office",
        UploadRefused::DiskFull => "the office's disk is full",
        UploadRefused::Corrupt => "the file did not arrive intact",
        UploadRefused::Stalled => "the office stopped taking the file",
        UploadRefused::Failed => "the office could not write the file",
        UploadRefused::NotSupported => "files cannot be uploaded into an SSH session yet",
        UploadRefused::FolderFull => "The office's uploads folder is full; files older than 7 days are removed automatically",
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1024 * 1024))
}

/// Lower-case hex, as the office expects the digest in `UploadEnd`.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// What the next step for one active file is.
enum Step {
    Chunk,
    End,
    Wait,
}

impl Uploads {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything is running or waiting (the tests check the queue drains).
    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.preparing.is_empty() && self.drops.is_empty() && self.queue.is_empty() && self.active.is_empty()
    }

    /// Checks a drop and queues its files. A drop that fails a check is reported and nothing is
    /// queued. The office checks again; these checks only spare it a refusal. Files on disk are
    /// measured on a thread (a network share can be slow), so their drop is admitted, or failed, by
    /// a later [`Uploads::pump`]; a drop of in-memory data is checked here.
    pub fn enqueue(&mut self, req: Request) -> Vec<UploadEvent> {
        let Request { drop, session, kind, files } = req;
        if !files.iter().any(|s| matches!(s, Source::File(_))) {
            return match Self::check(kind, &files) {
                Ok(sized) => self.admit(drop, session, kind, files, sized),
                Err(message) => vec![UploadEvent::Failed { drop, message }],
            };
        }
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("csm-upload-stat".into()).spawn(move || {
            let result = Self::check(kind, &files);
            let _ = tx.send((files, result));
        });
        match spawned {
            Ok(_) => {
                self.preparing.push(Preparing { drop, session, kind, rx });
                Vec::new()
            }
            Err(e) => vec![UploadEvent::Failed { drop, message: format!("could not start reading the files: {e}") }],
        }
    }

    /// Takes the drops whose sizes are known now.
    fn poll_preparing(&mut self, events: &mut Vec<UploadEvent>) {
        let mut i = 0;
        while i < self.preparing.len() {
            let got = match self.preparing.get(i).map(|p| p.rx.try_recv()) {
                Some(Ok(got)) => Some(Some(got)),
                Some(Err(TryRecvError::Empty)) | None => None,
                Some(Err(TryRecvError::Disconnected)) => Some(None),
            };
            let Some(got) = got else {
                i += 1;
                continue;
            };
            let p = self.preparing.remove(i);
            match got {
                Some((files, Ok(sized))) => events.extend(self.admit(p.drop, p.session, p.kind, files, sized)),
                Some((_, Err(message))) => events.push(UploadEvent::Failed { drop: p.drop, message }),
                None => events.push(UploadEvent::Failed { drop: p.drop, message: "could not read the files".into() }),
            }
        }
    }

    /// Registers a checked drop and queues its files.
    fn admit(&mut self, drop: u64, session: String, kind: UploadKind, files: Vec<Source>, sized: Vec<(String, u64)>) -> Vec<UploadEvent> {
        let total: u64 = sized.iter().map(|(_, s)| *s).sum();
        let files_total = sized.len();
        self.drops.push(DropState { id: drop, session, paths: vec![None; files_total], done_bytes: 0, total });
        for (index, ((name, size), source)) in sized.into_iter().zip(files).enumerate() {
            self.queue.push_back(Pending { drop, index, kind, name, size, source });
        }
        vec![UploadEvent::Progress { drop, sent: 0, total, files_done: 0, files_total, file: String::new() }]
    }

    /// The checks a drop passes before anything is sent: a count, the sizes, and no folders. Returns
    /// each file's name and size.
    fn check(kind: UploadKind, files: &[Source]) -> Result<Vec<(String, u64)>, String> {
        if files.is_empty() {
            return Err("there is nothing to upload".into());
        }
        if files.len() > MAX_UPLOAD_FILES_PER_DROP {
            return Err(format!("drop at most {MAX_UPLOAD_FILES_PER_DROP} files at a time"));
        }
        let limit = kind.limit();
        let mut out = Vec::with_capacity(files.len());
        let mut total = 0u64;
        for source in files {
            let (name, size) = match source {
                Source::Memory { name, data } => (name.clone(), data.len() as u64),
                Source::File(path) => {
                    let name = path.file_name().map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
                    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {name}: {e}"))?;
                    if meta.is_dir() {
                        return Err(format!("{name} is a folder. Drop files only: folders are not sent yet."));
                    }
                    if !meta.is_file() {
                        return Err(format!("{name} is not a regular file, so it cannot be sent."));
                    }
                    (name, meta.len())
                }
            };
            if size > limit {
                return Err(format!("{name} is {}; the limit is {}.", megabytes(size), megabytes(limit)));
            }
            total = total.saturating_add(size);
            if total > MAX_UPLOAD_DROP {
                return Err(format!("these files are more than {} together; drop fewer or smaller ones.", megabytes(MAX_UPLOAD_DROP)));
            }
            out.push((name, size));
        }
        Ok(out)
    }

    /// Cancels a drop: each of its files on the link is cancelled on the office (which deletes the
    /// partial file), the queued ones are dropped. Nothing is pasted.
    pub fn cancel(&mut self, drop: u64, wire: &mut dyn Wire) -> Vec<UploadEvent> {
        if self.preparing.iter().any(|p| p.drop == drop) {
            self.preparing.retain(|p| p.drop != drop);
            return vec![UploadEvent::Cancelled { drop }];
        }
        if !self.drops.iter().any(|d| d.id == drop) {
            return Vec::new();
        }
        self.abort(drop, None, wire, None)
    }

    /// The link is gone: every drop fails. The office deletes its partial files when the link
    /// closes, so nothing is sent here.
    pub fn lost(&mut self) -> Vec<UploadEvent> {
        let ids = self.preparing.iter().map(|p| p.drop).chain(self.drops.iter().map(|d| d.id));
        let events = ids.map(|drop| UploadEvent::Failed { drop, message: LOST.into() }).collect();
        self.preparing.clear();
        self.drops.clear();
        self.queue.clear();
        self.active.clear();
        self.turn = 0;
        events
    }

    /// Sends the next frame that may go: a begin for a queued file, a chunk of an accepted file, or
    /// the end of a file whose bytes are all sent. Checks the stall clock first. At most one frame a
    /// call; the events it causes go into `events`.
    pub fn pump(&mut self, wire: &mut dyn Wire, now: Instant, events: &mut Vec<UploadEvent>) -> Pumped {
        self.poll_preparing(events);
        if let Some(i) = self.stalled(now) {
            let (drop, name) = (self.active[i].drop, self.active[i].name.clone());
            events.extend(self.abort(drop, None, wire, Some(format!("the upload of {name} stalled: the office stopped taking it. Nothing was pasted."))));
            return Pumped::Idle;
        }
        // A begin first: starting a file costs one frame, and the drop's other files then take turns.
        if self.active.len() < ACTIVE_PER_DEVICE && !self.queue.is_empty() {
            return self.begin_next(wire, now);
        }
        let n = self.active.len();
        for step in 0..n {
            let i = (self.turn + step) % n;
            let next = match self.active.get(i) {
                Some(t) if t.accepted && !t.ended => {
                    let unacked = t.offset.saturating_sub(t.received);
                    let next_len = (t.size - t.offset).min(UPLOAD_CHUNK as u64);
                    if t.offset == t.size {
                        Step::End
                    } else if unacked + next_len <= CREDIT_BYTES {
                        Step::Chunk
                    } else {
                        Step::Wait
                    }
                }
                _ => Step::Wait,
            };
            match next {
                Step::Chunk => {
                    self.turn = (i + 1) % n;
                    // `None`: the reader has nothing ready yet; another file may.
                    if let Some(pumped) = self.send_chunk(i, wire, now, events) {
                        return pumped;
                    }
                }
                Step::End => {
                    self.turn = (i + 1) % n;
                    return self.send_end(i, wire, now);
                }
                Step::Wait => {}
            }
        }
        Pumped::Idle
    }

    /// The index of the first active file whose clock has run out, if any.
    fn stalled(&self, now: Instant) -> Option<usize> {
        self.active.iter().position(|t| now.saturating_duration_since(t.last) > UPLOAD_STALL)
    }

    /// Ends a drop as failed. Every file of it on the link is cancelled on the office, except
    /// `skip` (a file the office has already refused). With no `message`, the drop is cancelled.
    fn abort(&mut self, drop: u64, skip: Option<u64>, wire: &mut dyn Wire, message: Option<String>) -> Vec<UploadEvent> {
        let (gone, kept): (Vec<Transfer>, Vec<Transfer>) = std::mem::take(&mut self.active).into_iter().partition(|t| t.drop == drop);
        self.active = kept;
        for t in gone {
            if Some(t.wire) != skip {
                // A link that is already gone says so on its own; the window hears about it there.
                let _ = wire.send(Msg::UploadCancel { id: t.wire });
            }
        }
        self.queue.retain(|p| p.drop != drop);
        self.drops.retain(|d| d.id != drop);
        self.clamp_turn();
        match message {
            Some(message) => vec![UploadEvent::Failed { drop, message }],
            None => vec![UploadEvent::Cancelled { drop }],
        }
    }

    fn begin_next(&mut self, wire: &mut dyn Wire, now: Instant) -> Pumped {
        let Some(p) = self.queue.pop_front() else { return Pumped::Idle };
        let reader = match p.source {
            Source::Memory { data, .. } => Reader::Memory { data, at: 0 },
            Source::File(path) => Reader::Unstarted(path),
        };
        self.next_wire = self.next_wire.wrapping_add(1).max(1);
        let wire_id = self.next_wire;
        let session = self.drops.iter().find(|d| d.id == p.drop).map(|d| d.session.clone()).unwrap_or_default();
        let msg = Msg::UploadBegin { id: wire_id, session, name: p.name.clone(), size: p.size, kind: p.kind };
        self.active.push(Transfer {
            drop: p.drop,
            index: p.index,
            wire: wire_id,
            name: p.name,
            size: p.size,
            reader,
            hasher: Sha256::new(),
            offset: 0,
            received: 0,
            accepted: false,
            ended: false,
            last: now,
        });
        match wire.send(msg) {
            Ok(()) => Pumped::Sent,
            Err(()) => Pumped::Lost,
        }
    }

    /// Reads the next chunk of active file `i`, hashes it and sends it.
    fn send_chunk(&mut self, i: usize, wire: &mut dyn Wire, now: Instant, events: &mut Vec<UploadEvent>) -> Option<Pumped> {
        let t = self.active.get_mut(i)?;
        let len = (t.size - t.offset).min(UPLOAD_CHUNK as u64) as usize;
        let read: ChunkResult = match &mut t.reader {
            Reader::Unstarted(_) => return None,
            Reader::File(rx) => match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => {
                    // A slow disk or share is not the office stalling: the file's clock waits too.
                    t.last = now;
                    return None;
                }
                Err(TryRecvError::Disconnected) => Err("the file could not be read".to_owned()),
            },
            Reader::Memory { data, at } => match data.get(*at..*at + len) {
                Some(s) => {
                    *at += len;
                    Ok(s.to_vec())
                }
                None => Err("the file is shorter than it was".to_owned()),
            },
        };
        let buf = match read {
            Ok(buf) if buf.len() == len => buf,
            Ok(_) => {
                return Some(self.fail_read(i, "the file changed while it was read", wire, events));
            }
            Err(why) => return Some(self.fail_read(i, &why, wire, events)),
        };
        let t = self.active.get_mut(i)?;
        t.hasher.update(&buf);
        let frame = Frame::UploadChunk { id: t.wire, offset: t.offset, bytes: buf };
        t.offset += len as u64;
        t.last = now;
        Some(match wire.send_frame(&frame) {
            Ok(()) => Pumped::Sent,
            Err(()) => Pumped::Lost,
        })
    }

    /// A file could not be read: its drop fails and the office is told to drop the rest.
    fn fail_read(&mut self, i: usize, why: &str, wire: &mut dyn Wire, events: &mut Vec<UploadEvent>) -> Pumped {
        let Some(t) = self.active.get(i) else { return Pumped::Idle };
        let message = format!("could not read {} while it was sent ({why}). Nothing was pasted.", t.name);
        let drop = t.drop;
        events.extend(self.abort(drop, None, wire, Some(message)));
        Pumped::Idle
    }

    /// The office accepted a file: a file on disk starts being read now.
    fn start_reader(t: &mut Transfer) {
        if let Reader::Unstarted(path) = &t.reader {
            let path = path.clone();
            t.reader = Reader::File(spawn_reader(move || File::open(path), t.size));
        }
    }

    /// Sends the end of active file `i`: every byte is out, and the hash is of all of them.
    fn send_end(&mut self, i: usize, wire: &mut dyn Wire, now: Instant) -> Pumped {
        let Some(t) = self.active.get_mut(i) else { return Pumped::Idle };
        t.ended = true;
        t.last = now;
        let msg = Msg::UploadEnd { id: t.wire, sha256: hex(&t.hasher.clone().finalize()) };
        match wire.send(msg) {
            Ok(()) => Pumped::Sent,
            Err(()) => Pumped::Lost,
        }
    }

    /// A frame from the office about an upload, and the events it causes. Frames for an upload that
    /// is no longer running change nothing: the window already heard what became of it. A refusal
    /// cancels the drop's other files on the link.
    pub fn office(&mut self, msg: &Msg, now: Instant, wire: &mut dyn Wire) -> Vec<UploadEvent> {
        match msg {
            Msg::UploadAccepted { id } => {
                if let Some(t) = self.find_wire(*id) {
                    t.accepted = true;
                    t.last = now;
                    Self::start_reader(t);
                }
                Vec::new()
            }
            Msg::UploadAck { id, received } => {
                let Some(t) = self.find_wire(*id) else { return Vec::new() };
                // Credit only moves forward, and never past what was sent.
                if *received > t.received && *received <= t.offset {
                    t.received = *received;
                    t.last = now;
                }
                let drop = t.drop;
                self.progress(drop)
            }
            Msg::UploadDone { id, path } => {
                let Some(pos) = self.active.iter().position(|t| t.wire == *id) else { return Vec::new() };
                let t = self.active.remove(pos);
                self.clamp_turn();
                let Some(d) = self.drops.iter_mut().find(|d| d.id == t.drop) else { return Vec::new() };
                d.done_bytes += t.size;
                if let Some(slot) = d.paths.get_mut(t.index) {
                    *slot = Some(path.clone());
                }
                if d.paths.iter().all(Option::is_some) {
                    let Some(pos) = self.drops.iter().position(|d| d.id == t.drop) else { return Vec::new() };
                    let d = self.drops.remove(pos);
                    return vec![UploadEvent::Done { drop: d.id, session: d.session, paths: d.paths.into_iter().flatten().collect() }];
                }
                self.progress(t.drop)
            }
            Msg::UploadRefused { id, reason } => {
                let Some(t) = self.find_wire(*id) else { return Vec::new() };
                let (drop, name) = (t.drop, t.name.clone());
                let message = format!("the upload of {name} was refused: {}. Nothing was pasted.", refusal_text(*reason));
                self.abort(drop, Some(*id), wire, Some(message))
            }
            _ => Vec::new(),
        }
    }

    fn find_wire(&mut self, wire: u64) -> Option<&mut Transfer> {
        self.active.iter_mut().find(|t| t.wire == wire)
    }

    /// The drop's progress: the bytes in place plus what the office has acknowledged of the rest.
    fn progress(&self, drop: u64) -> Vec<UploadEvent> {
        let Some(d) = self.drops.iter().find(|d| d.id == drop) else { return Vec::new() };
        let on_link: u64 = self.active.iter().filter(|t| t.drop == drop).map(|t| t.received).sum();
        let file = self.active.iter().find(|t| t.drop == drop).map(|t| t.name.clone()).unwrap_or_default();
        let files_done = d.paths.iter().filter(|p| p.is_some()).count();
        vec![UploadEvent::Progress { drop, sent: d.done_bytes + on_link, total: d.total, files_done, files_total: d.paths.len(), file }]
    }

    fn clamp_turn(&mut self) {
        if self.turn >= self.active.len() {
            self.turn = 0;
        }
    }
}

/// What the window says when the link drops during an upload.
const LOST: &str = "the link to the office dropped during the upload. Nothing was pasted; drop the files again.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Records what the laptop would send, and whether the link is up.
    struct Fake {
        msgs: Vec<Msg>,
        chunks: Vec<(u64, u64, usize)>,
        up: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self { msgs: Vec::new(), chunks: Vec::new(), up: true }
        }
    }

    impl Wire for Fake {
        fn send(&mut self, msg: Msg) -> Result<(), ()> {
            if !self.up {
                return Err(());
            }
            self.msgs.push(msg);
            Ok(())
        }
        fn send_frame(&mut self, frame: &Frame) -> Result<(), ()> {
            if !self.up {
                return Err(());
            }
            match frame {
                Frame::UploadChunk { id, offset, bytes } => self.chunks.push((*id, *offset, bytes.len())),
                _ => return Err(()),
            }
            Ok(())
        }
    }

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    fn mem(name: &str, n: usize) -> Source {
        Source::Memory { name: name.into(), data: bytes(n) }
    }

    fn req(drop: u64, files: Vec<Source>) -> Request {
        Request { drop, session: "s1".into(), kind: UploadKind::File, files }
    }

    fn pump_all(u: &mut Uploads, w: &mut Fake, now: Instant, ev: &mut Vec<UploadEvent>) {
        while u.pump(w, now, ev) == Pumped::Sent {}
    }

    /// Pumps until `n` events have come out or ten seconds pass.
    fn pump_for_events(u: &mut Uploads, w: &mut Fake, n: usize) -> Vec<UploadEvent> {
        let mut ev = Vec::new();
        let start = Instant::now();
        while ev.len() < n && start.elapsed() < Duration::from_secs(10) {
            u.pump(w, Instant::now(), &mut ev);
            std::thread::sleep(Duration::from_millis(5));
        }
        ev
    }

    /// Pumps until at least `chunks` chunks were sent, an event came, or ten seconds passed:
    /// the reader threads are asynchronous, which is the point.
    fn pump_until_chunks(u: &mut Uploads, w: &mut Fake, ev: &mut Vec<UploadEvent>, chunks: usize) {
        let start = Instant::now();
        while w.chunks.len() < chunks && ev.is_empty() && start.elapsed() < Duration::from_secs(10) {
            u.pump(w, Instant::now(), ev);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn accept_all(u: &mut Uploads, w: &mut Fake, now: Instant) {
        let ids: Vec<u64> = u.active.iter().map(|t| t.wire).collect();
        for id in ids {
            u.office(&Msg::UploadAccepted { id }, now, w);
        }
    }

    fn begun(w: &Fake) -> Vec<u64> {
        w.msgs.iter().filter_map(|m| if let Msg::UploadBegin { id, .. } = m { Some(*id) } else { None }).collect()
    }

    #[test]
    fn a_drop_is_checked_before_anything_is_sent() {
        let mut u = Uploads::new();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("sub");
        std::fs::create_dir(&folder).unwrap();
        // Files on disk are measured on a thread: the answer comes through the pump.
        let mut w = Fake::new();
        assert!(u.enqueue(req(1, vec![Source::File(folder)])).is_empty());
        let ev = pump_for_events(&mut u, &mut w, 1);
        assert!(matches!(ev.as_slice(), [UploadEvent::Failed { drop: 1, message }] if message.contains("folder")), "{ev:?}");
        assert!(u.enqueue(req(2, vec![Source::File(dir.path().join("missing.txt"))])).is_empty());
        let ev = pump_for_events(&mut u, &mut w, 1);
        assert!(matches!(ev.as_slice(), [UploadEvent::Failed { drop: 2, .. }]), "{ev:?}");
        assert!(w.msgs.is_empty(), "nothing was sent for a refused drop");
        let too_many: Vec<Source> = (0..=MAX_UPLOAD_FILES_PER_DROP).map(|i| mem(&format!("f{i}"), 1)).collect();
        assert!(matches!(u.enqueue(req(3, too_many)).as_slice(), [UploadEvent::Failed { drop: 3, message }] if message.contains("20")));
        assert!(matches!(u.enqueue(req(4, Vec::new())).as_slice(), [UploadEvent::Failed { drop: 4, .. }]));
        let big = Request { drop: 5, session: "s1".into(), kind: UploadKind::Image, files: vec![mem("clipboard.png", (UploadKind::Image.limit() + 1) as usize)] };
        assert!(matches!(u.enqueue(big).as_slice(), [UploadEvent::Failed { drop: 5, .. }]));
        assert!(u.is_idle(), "refused drops queue nothing");
    }

    #[test]
    fn a_file_goes_begin_then_chunks_under_the_credit_then_its_end_with_the_hash() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        let size = UPLOAD_CHUNK * (UPLOAD_CREDIT + 3) + 10;
        let ev = u.enqueue(req(1, vec![mem("a.bin", size)]));
        assert!(matches!(ev.as_slice(), [UploadEvent::Progress { drop: 1, files_total: 1, .. }]));
        let mut ev = Vec::new();
        assert_eq!(u.pump(&mut w, now, &mut ev), Pumped::Sent);
        assert!(matches!(w.msgs.last(), Some(Msg::UploadBegin { size: s, .. }) if *s == size as u64));
        assert_eq!(u.pump(&mut w, now, &mut ev), Pumped::Idle, "no chunk before UploadAccepted");
        accept_all(&mut u, &mut w, now);
        pump_all(&mut u, &mut w, now, &mut ev);
        assert_eq!(w.chunks.len(), UPLOAD_CREDIT, "the ninth unacknowledged chunk is never sent");
        assert!(w.chunks.iter().all(|(_, _, n)| *n == UPLOAD_CHUNK));
        let id = begun(&w)[0];
        // The office acks 4 chunks: the window opens by 4 more.
        let ack = (UPLOAD_CHUNK * 4) as u64;
        let mut ev = u.office(&Msg::UploadAck { id, received: ack }, now, &mut w);
        assert!(matches!(ev.as_slice(), [UploadEvent::Progress { sent, .. }] if *sent == ack));
        pump_all(&mut u, &mut w, now, &mut ev);
        assert_eq!(w.chunks.len(), UPLOAD_CREDIT + 4);
        // The rest, then the end, with the hash of the whole file.
        u.office(&Msg::UploadAck { id, received: ack * 2 }, now, &mut w);
        pump_all(&mut u, &mut w, now, &mut ev);
        let end = w.msgs.iter().rev().find_map(|m| if let Msg::UploadEnd { sha256, .. } = m { Some(sha256.clone()) } else { None }).expect("UploadEnd sent");
        assert_eq!(end, hex(&Sha256::digest(bytes(size))));
        assert_eq!(w.chunks.iter().map(|(_, _, n)| *n).sum::<usize>(), size);
        let offsets: Vec<u64> = w.chunks.iter().map(|(_, o, _)| *o).collect();
        assert_eq!(offsets, (0..w.chunks.len()).map(|i| (i * UPLOAD_CHUNK) as u64).collect::<Vec<_>>(), "in order, no gaps");
        let ev = u.office(&Msg::UploadDone { id, path: "/office/a.bin".into() }, now, &mut w);
        assert_eq!(ev, vec![UploadEvent::Done { drop: 1, session: "s1".into(), paths: vec!["/office/a.bin".into()] }]);
        assert!(u.is_idle());
    }

    #[test]
    fn two_files_take_turns_and_the_paths_come_back_in_drop_order() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(9, vec![mem("first.txt", 100), mem("second.txt", 100)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        let ids = begun(&w);
        assert_eq!(ids.len(), 2, "two active at once");
        accept_all(&mut u, &mut w, now);
        pump_all(&mut u, &mut w, now, &mut ev);
        let sent_by_file = |id| w.chunks.iter().filter(|c| c.0 == id).count();
        assert!(sent_by_file(ids[0]) > 0 && sent_by_file(ids[1]) > 0, "both files move: {:?}", w.chunks);
        // The second file finishes first on the office's side: the paths still come back in drop order.
        u.office(&Msg::UploadDone { id: ids[1], path: "/o/second".into() }, now, &mut w);
        let done = u.office(&Msg::UploadDone { id: ids[0], path: "/o/first".into() }, now, &mut w);
        assert_eq!(done, vec![UploadEvent::Done { drop: 9, session: "s1".into(), paths: vec!["/o/first".into(), "/o/second".into()] }]);
    }

    #[test]
    fn a_third_file_waits_for_a_slot() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(1, vec![mem("a", 10), mem("b", 10), mem("c", 10)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        assert_eq!(begun(&w).len(), ACTIVE_PER_DEVICE);
        accept_all(&mut u, &mut w, now);
        pump_all(&mut u, &mut w, now, &mut ev);
        u.office(&Msg::UploadDone { id: begun(&w)[0], path: "/o/a".into() }, now, &mut w);
        pump_all(&mut u, &mut w, now, &mut ev);
        assert_eq!(begun(&w).len(), 3, "the freed slot takes the third");
    }

    #[test]
    fn a_refusal_fails_the_whole_drop_and_cancels_the_rest_on_the_link() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(4, vec![mem("a", 10), mem("b", 10), mem("c", 10)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        let ids = begun(&w);
        let ev = u.office(&Msg::UploadRefused { id: ids[0], reason: UploadRefused::Forbidden }, now, &mut w);
        assert!(matches!(ev.as_slice(), [UploadEvent::Failed { drop: 4, message }] if message.contains("refused") && message.contains("Nothing was pasted")), "{ev:?}");
        let cancelled: Vec<u64> = w.msgs.iter().filter_map(|m| if let Msg::UploadCancel { id } = m { Some(*id) } else { None }).collect();
        assert_eq!(cancelled, vec![ids[1]], "the refused file is not cancelled again; its sibling is");
        assert!(u.is_idle(), "the queued file of the drop is gone too");
    }

    #[test]
    fn a_cancel_tells_the_office_about_each_file_on_the_link() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(6, vec![mem("a", 10), mem("b", 10), mem("c", 10)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        assert_eq!(u.cancel(6, &mut w), vec![UploadEvent::Cancelled { drop: 6 }]);
        let cancels = w.msgs.iter().filter(|m| matches!(m, Msg::UploadCancel { .. })).count();
        assert_eq!(cancels, ACTIVE_PER_DEVICE, "the two begun files, not the queued third");
        assert!(u.is_idle());
        assert!(u.cancel(6, &mut w).is_empty(), "a second cancel says nothing");
    }

    #[test]
    fn a_stalled_file_is_cancelled_and_its_drop_fails() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let then = Instant::now();
        u.enqueue(req(2, vec![mem("a", 10)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, then, &mut ev);
        let later = then + UPLOAD_STALL + Duration::from_secs(1);
        let mut ev = Vec::new();
        assert_eq!(u.pump(&mut w, later, &mut ev), Pumped::Idle);
        assert!(matches!(ev.as_slice(), [UploadEvent::Failed { drop: 2, message }] if message.contains("stalled")), "{ev:?}");
        assert!(matches!(w.msgs.last(), Some(Msg::UploadCancel { .. })));
        assert!(u.is_idle());
    }

    #[test]
    fn a_link_that_drops_fails_every_drop_once() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(1, vec![mem("a", 10)]));
        u.enqueue(req(2, vec![mem("b", 10)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        let lost = u.lost();
        assert_eq!(lost.len(), 2);
        assert!(lost.iter().all(|e| matches!(e, UploadEvent::Failed { message, .. } if message.contains("Nothing was pasted"))));
        assert!(u.lost().is_empty());
    }

    #[test]
    fn a_dead_link_during_a_send_says_lost() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(1, vec![mem("a", 10)]));
        w.up = false;
        let mut ev = Vec::new();
        assert_eq!(u.pump(&mut w, now, &mut ev), Pumped::Lost);
    }

    #[test]
    fn a_file_that_shrinks_while_it_is_sent_fails_the_drop_instead_of_sending_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.bin");
        std::fs::write(&path, vec![7u8; 100]).unwrap();
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        assert!(u.enqueue(req(1, vec![Source::File(path.clone())])).is_empty());
        let mut ev = Vec::new();
        // The size is read on a thread; the begin goes out once it is known.
        let start = Instant::now();
        while begun(&w).is_empty() && start.elapsed() < Duration::from_secs(10) {
            u.pump(&mut w, now, &mut ev);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ev.iter().all(|e| !matches!(e, UploadEvent::Failed { .. })), "{ev:?}");
        // The file is opened only once the office accepts, so this change is seen.
        std::fs::write(&path, vec![7u8; 10]).unwrap();
        accept_all(&mut u, &mut w, now);
        let start = Instant::now();
        while !ev.iter().any(|e| matches!(e, UploadEvent::Failed { .. })) && start.elapsed() < Duration::from_secs(10) {
            u.pump(&mut w, now, &mut ev);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ev.iter().any(|e| matches!(e, UploadEvent::Failed { drop: 1, message } if message.contains("while it was sent"))), "{ev:?}");
        assert!(w.chunks.is_empty(), "no garbage was sent");
    }

    /// A reader that takes its time, like a network share.
    struct Slow(usize);

    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(150));
            let n = buf.len().min(self.0);
            buf[..n].fill(9);
            self.0 -= n;
            Ok(n)
        }
    }

    #[test]
    fn a_slow_source_never_holds_up_the_pump() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        u.enqueue(req(1, vec![mem("slow.bin", UPLOAD_CHUNK * 2)]));
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, now, &mut ev);
        accept_all(&mut u, &mut w, now);
        // Swap in a slow reader for the in-memory one.
        u.active[0].reader = Reader::File(spawn_reader(|| Ok(Slow(UPLOAD_CHUNK * 2)), (UPLOAD_CHUNK * 2) as u64));
        let t = Instant::now();
        for _ in 0..50 {
            u.pump(&mut w, now, &mut ev);
        }
        assert!(t.elapsed() < Duration::from_millis(100), "fifty pumps took {:?}: the worker waited on the reader", t.elapsed());
        assert!(w.chunks.is_empty(), "nothing was ready yet");
        pump_until_chunks(&mut u, &mut w, &mut ev, 2);
        assert_eq!(w.chunks.len(), 2, "the chunks arrive once the reader has them");
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_drop_cancelled_while_its_files_are_measured_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        let mut u = Uploads::new();
        let mut w = Fake::new();
        u.enqueue(req(3, vec![Source::File(path)]));
        assert_eq!(u.cancel(3, &mut w), vec![UploadEvent::Cancelled { drop: 3 }]);
        let mut ev = Vec::new();
        pump_all(&mut u, &mut w, Instant::now(), &mut ev);
        assert!(w.msgs.is_empty() && ev.is_empty() && u.is_idle());
    }

    #[test]
    fn frames_for_a_finished_upload_change_nothing() {
        let mut u = Uploads::new();
        let mut w = Fake::new();
        let now = Instant::now();
        assert!(u.office(&Msg::UploadAck { id: 99, received: 5 }, now, &mut w).is_empty());
        assert!(u.office(&Msg::UploadDone { id: 99, path: "/x".into() }, now, &mut w).is_empty());
        assert!(u.office(&Msg::UploadRefused { id: 99, reason: UploadRefused::Failed }, now, &mut w).is_empty());
        assert!(w.msgs.is_empty());
    }

    #[test]
    fn a_folder_is_named_without_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Photos");
        std::fs::create_dir(&folder).unwrap();
        let err = Uploads::check(UploadKind::File, &[Source::File(folder)]).unwrap_err();
        assert_eq!(err, "Photos is a folder. Drop files only: folders are not sent yet.");
    }
}
