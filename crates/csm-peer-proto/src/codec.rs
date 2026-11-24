//! What one Noise transport message carries: a [`Frame`]. The first plaintext byte is the kind.
//!
//! | kind | frame | layout after the kind byte |
//! |---|---|---|
//! | `01` | control message | JSON of one [`Msg`] |
//! | `02` | `term_data` | `stream u32, instance u64, offset u64, bytes` (1 to 32 KiB) |
//! | `03` | `term_attach` | `stream u32, instance u64, cols u16, rows u16, offset u64, more u8, vt` |
//! | `04` | `term_input` | `stream u32, input kind u8, payload` |
//! | `05` | control fragment | `last u8 (0 or 1), bytes`: part of a JSON message too big for one message |
//! | `06` | `upload_chunk` | `id u64, offset u64, bytes` (1 to [`UPLOAD_CHUNK`] bytes), laptop to office only |
//!
//! Integers are big endian. A control message above one Noise message is sent as consecutive
//! fragments (kind `05`); nothing else may come between them, and the joined JSON is at most
//! [`MAX_CONTROL_BYTES`]. An attach is the server's serialized grid as VT bytes: it is split in
//! chunks with `more = 1` on all but the last, and [`AttachAssembler`] joins them.
//!
//! Decoding is strict and total: any byte string gives a frame or a [`CodecError`], never a
//! panic, and nothing is allocated beyond the limits in [`crate::limits`].

use std::collections::HashMap;

use crate::limits::{MAX_ATTACH_BYTES, MAX_CONTROL_BYTES, MAX_INPUT_BYTES, MAX_PLAINTEXT, MAX_TERM_CHUNK, MAX_TERM_DIM, UPLOAD_CHUNK};
use crate::msg::Msg;

const KIND_CONTROL: u8 = 0x01;
const KIND_TERM_DATA: u8 = 0x02;
const KIND_TERM_ATTACH: u8 = 0x03;
const KIND_TERM_INPUT: u8 = 0x04;
const KIND_FRAGMENT: u8 = 0x05;
const KIND_UPLOAD_CHUNK: u8 = 0x06;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Bigger than a limit (the plaintext, a joined control message, an attach).
    TooLarge,
    /// A `Hello` or `Welcome` of another protocol version.
    Version(u32),
    /// The kind byte is not one of the above.
    UnknownKind(u8),
    /// The bytes do not parse as that frame.
    Malformed(&'static str),
    /// The JSON is not a control message of this protocol.
    Json,
    /// It parsed, but a field breaks the rules.
    Invalid(&'static str),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("message too large"),
            Self::Version(v) => write!(f, "protocol version {v} is not supported"),
            Self::UnknownKind(k) => write!(f, "unknown frame kind {k:#04x}"),
            Self::Malformed(w) => write!(f, "malformed frame: {w}"),
            Self::Json => f.write_str("not a control message"),
            Self::Invalid(w) => write!(f, "invalid {w}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Typed input from a client's pane. The server maps it to the session's terminal handle
/// (`send_key`, `send_mouse`, `focus_changed`, `paste`); pasted text is bracketed there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Bytes a key press produced.
    Key(Vec<u8>),
    /// A mouse report.
    Mouse(Vec<u8>),
    Focus(bool),
    /// Text the person pasted. Untrusted: the server brackets it.
    Paste(String),
}

/// One decoded message.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Control(Msg),
    /// Raw PTY bytes at an absolute offset of the session's output.
    TermData { stream: u32, instance: u64, offset: u64, bytes: Vec<u8> },
    /// A chunk of the serialized grid. The next `term_data` of the stream continues at `offset`
    /// (the offset the grid describes), in the size the chunks give.
    TermAttach { stream: u32, instance: u64, cols: u16, rows: u16, offset: u64, more: bool, vt: Vec<u8> },
    TermInput { stream: u32, input: Input },
    /// Bytes of an upload, laptop to office. `offset` is where they go in the file.
    UploadChunk { id: u64, offset: u64, bytes: Vec<u8> },
}

impl Frame {
    /// The plaintexts to send, in order (one Noise message each). Only a control message can be
    /// more than one, when its JSON does not fit a single message.
    pub fn encode(&self) -> Result<Vec<Vec<u8>>, CodecError> {
        match self {
            Self::Control(msg) => {
                msg.validate()?;
                let json = serde_json::to_vec(msg).map_err(|_| CodecError::Json)?;
                if json.len() > MAX_CONTROL_BYTES {
                    return Err(CodecError::TooLarge);
                }
                if json.len() < MAX_PLAINTEXT {
                    let mut out = Vec::with_capacity(1 + json.len());
                    out.push(KIND_CONTROL);
                    out.extend_from_slice(&json);
                    return Ok(vec![out]);
                }
                let part = MAX_PLAINTEXT - 2;
                let n = json.chunks(part).count();
                Ok(json
                    .chunks(part)
                    .enumerate()
                    .map(|(i, c)| {
                        let mut out = Vec::with_capacity(2 + c.len());
                        out.push(KIND_FRAGMENT);
                        out.push(u8::from(i + 1 == n));
                        out.extend_from_slice(c);
                        out
                    })
                    .collect())
            }
            Self::TermData { stream, instance, offset, bytes } => {
                if bytes.is_empty() || bytes.len() > MAX_TERM_CHUNK {
                    return Err(CodecError::Invalid("term_data length"));
                }
                let mut out = Vec::with_capacity(21 + bytes.len());
                out.push(KIND_TERM_DATA);
                out.extend_from_slice(&stream.to_be_bytes());
                out.extend_from_slice(&instance.to_be_bytes());
                out.extend_from_slice(&offset.to_be_bytes());
                out.extend_from_slice(bytes);
                Ok(vec![out])
            }
            Self::TermAttach { stream, instance, cols, rows, offset, more, vt } => {
                if vt.len() > MAX_TERM_CHUNK {
                    return Err(CodecError::Invalid("term_attach chunk length"));
                }
                if !dim_ok(*cols, *rows) {
                    return Err(CodecError::Invalid("terminal size"));
                }
                let mut out = Vec::with_capacity(26 + vt.len());
                out.push(KIND_TERM_ATTACH);
                out.extend_from_slice(&stream.to_be_bytes());
                out.extend_from_slice(&instance.to_be_bytes());
                out.extend_from_slice(&cols.to_be_bytes());
                out.extend_from_slice(&rows.to_be_bytes());
                out.extend_from_slice(&offset.to_be_bytes());
                out.push(u8::from(*more));
                out.extend_from_slice(vt);
                Ok(vec![out])
            }
            Self::TermInput { stream, input } => {
                let (kind, payload): (u8, Vec<u8>) = match input {
                    Input::Key(b) => (0, b.clone()),
                    Input::Mouse(b) => (1, b.clone()),
                    Input::Focus(f) => (2, vec![u8::from(*f)]),
                    Input::Paste(t) => (3, t.as_bytes().to_vec()),
                };
                if payload.is_empty() || payload.len() > MAX_INPUT_BYTES {
                    return Err(CodecError::Invalid("term_input length"));
                }
                let mut out = Vec::with_capacity(6 + payload.len());
                out.push(KIND_TERM_INPUT);
                out.extend_from_slice(&stream.to_be_bytes());
                out.push(kind);
                out.extend_from_slice(&payload);
                Ok(vec![out])
            }
            Self::UploadChunk { id, offset, bytes } => {
                if bytes.is_empty() || bytes.len() > UPLOAD_CHUNK {
                    return Err(CodecError::Invalid("upload chunk length"));
                }
                let mut out = Vec::with_capacity(17 + bytes.len());
                out.push(KIND_UPLOAD_CHUNK);
                out.extend_from_slice(&id.to_be_bytes());
                out.extend_from_slice(&offset.to_be_bytes());
                out.extend_from_slice(bytes);
                Ok(vec![out])
            }
        }
    }

    /// `term_data` frames for `bytes` starting at `offset`, split to fit. The offsets advance by
    /// each chunk's length.
    pub fn term_data(stream: u32, instance: u64, offset: u64, bytes: &[u8]) -> Vec<Frame> {
        let mut at = offset;
        bytes
            .chunks(MAX_TERM_CHUNK)
            .map(|c| {
                let f = Frame::TermData { stream, instance, offset: at, bytes: c.to_vec() };
                at = at.saturating_add(c.len() as u64);
                f
            })
            .collect()
    }

    /// The `term_attach` chunks for a serialized grid.
    pub fn attach(stream: u32, instance: u64, cols: u16, rows: u16, offset: u64, vt: &[u8]) -> Vec<Frame> {
        if vt.is_empty() {
            return vec![Frame::TermAttach { stream, instance, cols, rows, offset, more: false, vt: Vec::new() }];
        }
        let n = vt.chunks(MAX_TERM_CHUNK).count();
        vt.chunks(MAX_TERM_CHUNK)
            .enumerate()
            .map(|(i, c)| Frame::TermAttach { stream, instance, cols, rows, offset, more: i + 1 < n, vt: c.to_vec() })
            .collect()
    }
}

fn dim_ok(cols: u16, rows: u16) -> bool {
    (1..=MAX_TERM_DIM).contains(&cols) && (1..=MAX_TERM_DIM).contains(&rows)
}

/// A cursor over a byte slice that never indexes out of range.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.0.len() < n {
            return Err(CodecError::Malformed("truncated"));
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        self.take(1)?.first().copied().ok_or(CodecError::Malformed("truncated"))
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        let b: [u8; 2] = self.take(2)?.try_into().map_err(|_| CodecError::Malformed("truncated"))?;
        Ok(u16::from_be_bytes(b))
    }
    fn u32(&mut self) -> Result<u32, CodecError> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| CodecError::Malformed("truncated"))?;
        Ok(u32::from_be_bytes(b))
    }
    fn u64(&mut self) -> Result<u64, CodecError> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| CodecError::Malformed("truncated"))?;
        Ok(u64::from_be_bytes(b))
    }
    fn rest(self) -> &'a [u8] {
        self.0
    }
}

/// Decodes the plaintexts of one connection direction, joining control fragments.
#[derive(Debug, Default)]
pub struct Decoder {
    partial: Option<Vec<u8>>,
    role: Role,
}

/// Which end a [`Decoder`] runs on, and so which direction's frames it accepts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Either direction. For tests and vectors only; a connection uses `server` or `client`.
    #[default]
    Any,
    /// Receives what a client sends: client control messages and `term_input`.
    Server,
    /// Receives what a server sends: server control messages, `term_data` and `term_attach`.
    Client,
}

impl Decoder {
    /// A decoder that accepts either direction (tests and vectors).
    pub fn new() -> Self {
        Self::default()
    }

    /// The decoder of a server's connection: refuses frames only a server may send.
    pub fn server() -> Self {
        Self { partial: None, role: Role::Server }
    }

    /// The decoder of a client's connection: refuses frames only a client may send.
    pub fn client() -> Self {
        Self { partial: None, role: Role::Client }
    }

    fn direction_ok(&self, frame: &Frame) -> bool {
        match (self.role, frame) {
            (Role::Any, _) => true,
            (Role::Server, Frame::Control(m)) => m.from_client(),
            (Role::Client, Frame::Control(m)) => m.from_server(),
            (Role::Server, Frame::TermInput { .. } | Frame::UploadChunk { .. }) => true,
            (Role::Client, Frame::TermData { .. } | Frame::TermAttach { .. }) => true,
            _ => false,
        }
    }

    /// Decodes one Noise plaintext. `Ok(None)` means a fragment was taken and more are due.
    /// After an error, close the connection.
    pub fn decode(&mut self, plaintext: &[u8]) -> Result<Option<Frame>, CodecError> {
        let r = self.decode_inner(plaintext).and_then(|f| match f {
            Some(frame) if !self.direction_ok(&frame) => Err(CodecError::Malformed("direction")),
            other => Ok(other),
        });
        if r.is_err() {
            self.partial = None;
        }
        r
    }

    fn decode_inner(&mut self, plaintext: &[u8]) -> Result<Option<Frame>, CodecError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(CodecError::TooLarge);
        }
        let mut r = Reader(plaintext);
        let kind = r.u8().map_err(|_| CodecError::Malformed("empty message"))?;
        if self.partial.is_some() && kind != KIND_FRAGMENT {
            return Err(CodecError::Malformed("a frame inside a fragmented message"));
        }
        match kind {
            KIND_CONTROL => parse_control(r.rest()).map(|m| Some(Frame::Control(m))),
            KIND_FRAGMENT => {
                let last = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CodecError::Malformed("fragment flag")),
                };
                let body = r.rest();
                if body.is_empty() {
                    return Err(CodecError::Malformed("empty fragment"));
                }
                let buf = self.partial.get_or_insert_with(Vec::new);
                if buf.len() + body.len() > MAX_CONTROL_BYTES {
                    return Err(CodecError::TooLarge);
                }
                buf.extend_from_slice(body);
                if !last {
                    return Ok(None);
                }
                let whole = self.partial.take().unwrap_or_default();
                parse_control(&whole).map(|m| Some(Frame::Control(m)))
            }
            KIND_TERM_DATA => {
                let stream = r.u32()?;
                let instance = r.u64()?;
                let offset = r.u64()?;
                let bytes = r.rest();
                if bytes.is_empty() || bytes.len() > MAX_TERM_CHUNK {
                    return Err(CodecError::Invalid("term_data length"));
                }
                Ok(Some(Frame::TermData { stream, instance, offset, bytes: bytes.to_vec() }))
            }
            KIND_TERM_ATTACH => {
                let stream = r.u32()?;
                let instance = r.u64()?;
                let cols = r.u16()?;
                let rows = r.u16()?;
                let offset = r.u64()?;
                let more = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(CodecError::Malformed("attach flag")),
                };
                let vt = r.rest();
                if vt.len() > MAX_TERM_CHUNK {
                    return Err(CodecError::Invalid("term_attach chunk length"));
                }
                if !dim_ok(cols, rows) {
                    return Err(CodecError::Invalid("terminal size"));
                }
                Ok(Some(Frame::TermAttach { stream, instance, cols, rows, offset, more, vt: vt.to_vec() }))
            }
            KIND_TERM_INPUT => {
                let stream = r.u32()?;
                let input_kind = r.u8()?;
                let payload = r.rest();
                if payload.is_empty() || payload.len() > MAX_INPUT_BYTES {
                    return Err(CodecError::Invalid("term_input length"));
                }
                let input = match input_kind {
                    0 => Input::Key(payload.to_vec()),
                    1 => Input::Mouse(payload.to_vec()),
                    2 => match payload {
                        [0] => Input::Focus(false),
                        [1] => Input::Focus(true),
                        _ => return Err(CodecError::Malformed("focus flag")),
                    },
                    3 => Input::Paste(String::from_utf8(payload.to_vec()).map_err(|_| CodecError::Malformed("paste is not UTF-8"))?),
                    _ => return Err(CodecError::Malformed("input kind")),
                };
                Ok(Some(Frame::TermInput { stream, input }))
            }
            KIND_UPLOAD_CHUNK => {
                let id = r.u64()?;
                let offset = r.u64()?;
                let bytes = r.rest();
                if bytes.is_empty() || bytes.len() > UPLOAD_CHUNK {
                    return Err(CodecError::Invalid("upload chunk length"));
                }
                Ok(Some(Frame::UploadChunk { id, offset, bytes: bytes.to_vec() }))
            }
            other => Err(CodecError::UnknownKind(other)),
        }
    }
}

fn parse_control(json: &[u8]) -> Result<Msg, CodecError> {
    let msg: Msg = serde_json::from_slice(json).map_err(|_| CodecError::Json)?;
    msg.validate()?;
    Ok(msg)
}

/// A whole attach: the grid of one session's terminal as VT bytes, and where the stream goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attach {
    pub stream: u32,
    pub instance: u64,
    pub cols: u16,
    pub rows: u16,
    /// The session output offset the grid describes; `term_data` continues here.
    pub offset: u64,
    pub vt: Vec<u8>,
}

/// Joins `term_attach` chunks. One attach per stream may be open at a time; a new first chunk
/// for a stream starts over (the server restarted the attach).
#[derive(Debug, Default)]
pub struct AttachAssembler {
    open: HashMap<u32, Attach>,
}

const MAX_OPEN_ATTACHES: usize = 64;
/// All open attaches together: one big grid or a few small ones, not 64 big ones.
pub const MAX_ATTACH_TOTAL_BYTES: usize = MAX_ATTACH_BYTES;

impl AttachAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a `TermAttach` frame (any other frame is an error). Returns the attach when its last
    /// chunk arrives. A chunk whose header differs from the open attach's is refused.
    pub fn push(&mut self, frame: &Frame) -> Result<Option<Attach>, CodecError> {
        let Frame::TermAttach { stream, instance, cols, rows, offset, more, vt } = frame else {
            return Err(CodecError::Malformed("not a term_attach"));
        };
        let same = |a: &Attach| a.instance == *instance && a.cols == *cols && a.rows == *rows && a.offset == *offset;
        let mut cur = match self.open.remove(stream) {
            Some(a) if same(&a) => a,
            Some(_) => return Err(CodecError::Malformed("term_attach header changed mid-attach")),
            None => {
                if self.open.len() >= MAX_OPEN_ATTACHES {
                    return Err(CodecError::TooLarge);
                }
                Attach { stream: *stream, instance: *instance, cols: *cols, rows: *rows, offset: *offset, vt: Vec::new() }
            }
        };
        // `cur` was removed from `open`, so `buffered` counts the other attaches only.
        let buffered: usize = self.open.values().map(|a| a.vt.len()).sum();
        if cur.vt.len() + vt.len() > MAX_ATTACH_BYTES || buffered + cur.vt.len() + vt.len() > MAX_ATTACH_TOTAL_BYTES {
            return Err(CodecError::TooLarge);
        }
        cur.vt.extend_from_slice(vt);
        if *more {
            self.open.insert(*stream, cur);
            Ok(None)
        } else {
            Ok(Some(cur))
        }
    }

    /// Forgets an open attach (the stream was unsubscribed).
    pub fn cancel(&mut self, stream: u32) {
        self.open.remove(&stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{ByeReason, Outcome};

    fn round(frame: &Frame) -> Frame {
        let parts = frame.encode().unwrap();
        let mut d = Decoder::new();
        let mut out = None;
        for p in &parts {
            assert!(p.len() <= MAX_PLAINTEXT);
            out = d.decode(p).unwrap();
        }
        out.unwrap()
    }

    #[test]
    fn every_frame_kind_round_trips() {
        let frames = [
            Frame::Control(Msg::Ping { n: 7 }),
            Frame::Control(Msg::Bye { reason: ByeReason::ServerOff }),
            Frame::TermData { stream: 3, instance: 9, offset: u64::MAX - 5, bytes: b"hi\x1b[0m".to_vec() },
            Frame::TermAttach { stream: 3, instance: 9, cols: 120, rows: 30, offset: 42, more: true, vt: b"\x1b[2J".to_vec() },
            Frame::TermAttach { stream: 3, instance: 9, cols: 1, rows: 1, offset: 0, more: false, vt: vec![] },
            Frame::TermInput { stream: 1, input: Input::Key(b"\r".to_vec()) },
            Frame::TermInput { stream: 1, input: Input::Mouse(b"\x1b[<0;1;1M".to_vec()) },
            Frame::TermInput { stream: 1, input: Input::Focus(true) },
            Frame::TermInput { stream: 1, input: Input::Focus(false) },
            Frame::TermInput { stream: 1, input: Input::Paste("line one\nline two é".into()) },
            Frame::UploadChunk { id: 4, offset: 1 << 40, bytes: b"chunk\x00bytes".to_vec() },
        ];
        for f in &frames {
            assert_eq!(&round(f), f);
        }
    }

    #[test]
    fn a_big_control_message_is_fragmented_and_joined() {
        let big = "x".repeat(MAX_PLAINTEXT * 2 + 100);
        let f = Frame::Control(Msg::Reply { id: 1, outcome: Outcome::Ok { message: "m".into(), data: serde_json::json!({ "blob": big }) } });
        let parts = f.encode().unwrap();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.first() == Some(&KIND_FRAGMENT) && p.len() <= MAX_PLAINTEXT));
        let mut d = Decoder::new();
        assert_eq!(d.decode(&parts[0]).unwrap(), None);
        assert_eq!(d.decode(&parts[1]).unwrap(), None);
        assert_eq!(d.decode(&parts[2]).unwrap(), Some(f));
        // Nothing may come between fragments.
        let mut d = Decoder::new();
        assert_eq!(d.decode(&parts[0]).unwrap(), None);
        let ping = Frame::Control(Msg::Ping { n: 1 }).encode().unwrap();
        assert_eq!(d.decode(&ping[0]), Err(CodecError::Malformed("a frame inside a fragmented message")));
    }

    #[test]
    fn a_joined_control_message_over_the_cap_is_refused() {
        let mut d = Decoder::new();
        let part = vec![KIND_FRAGMENT, 0].into_iter().chain(std::iter::repeat_n(b'a', MAX_PLAINTEXT - 2)).collect::<Vec<u8>>();
        let mut got = Ok(None);
        for _ in 0..(MAX_CONTROL_BYTES / (MAX_PLAINTEXT - 2) + 2) {
            got = d.decode(&part);
            if got.is_err() {
                break;
            }
        }
        assert_eq!(got, Err(CodecError::TooLarge));
        let huge = Frame::Control(Msg::Reply { id: 1, outcome: Outcome::Ok { message: String::new(), data: serde_json::json!("y".repeat(MAX_CONTROL_BYTES + 1)) } });
        assert_eq!(huge.encode(), Err(CodecError::TooLarge));
    }

    #[test]
    fn term_data_and_attach_split_to_fit() {
        let bytes = vec![7u8; MAX_TERM_CHUNK * 2 + 5];
        let frames = Frame::term_data(2, 1, 100, &bytes);
        assert_eq!(frames.len(), 3);
        let offsets: Vec<u64> = frames.iter().filter_map(|f| if let Frame::TermData { offset, .. } = f { Some(*offset) } else { None }).collect();
        assert_eq!(offsets, [100, 100 + MAX_TERM_CHUNK as u64, 100 + 2 * MAX_TERM_CHUNK as u64]);
        let vt: Vec<u8> = (0..(MAX_TERM_CHUNK * 3 + 1)).map(|i| (i % 251) as u8).collect();
        let chunks = Frame::attach(5, 2, 80, 24, 999, &vt);
        assert_eq!(chunks.len(), 4);
        let mut asm = AttachAssembler::new();
        let mut done = None;
        for c in &chunks {
            let enc = round(c);
            done = asm.push(&enc).unwrap();
        }
        let a = done.unwrap();
        assert_eq!((a.stream, a.instance, a.cols, a.rows, a.offset), (5, 2, 80, 24, 999));
        assert_eq!(a.vt, vt);
        // An empty grid is one chunk.
        assert_eq!(Frame::attach(1, 1, 80, 24, 0, &[]).len(), 1);
    }

    #[test]
    fn the_attach_assembler_refuses_a_changed_header_and_an_oversize_join() {
        let mut asm = AttachAssembler::new();
        let first = Frame::TermAttach { stream: 1, instance: 1, cols: 80, rows: 24, offset: 0, more: true, vt: vec![1] };
        assert_eq!(asm.push(&first).unwrap(), None);
        let other = Frame::TermAttach { stream: 1, instance: 2, cols: 80, rows: 24, offset: 0, more: false, vt: vec![2] };
        assert!(asm.push(&other).is_err());
        assert!(asm.push(&Frame::Control(Msg::Ping { n: 1 })).is_err());
        // Streams are independent.
        let a = Frame::TermAttach { stream: 7, instance: 1, cols: 80, rows: 24, offset: 0, more: false, vt: vec![9] };
        assert_eq!(asm.push(&a).unwrap().map(|x| x.vt), Some(vec![9]));
    }

    #[test]
    fn open_attaches_share_one_byte_budget() {
        let mut asm = AttachAssembler::new();
        let chunk = vec![0u8; MAX_TERM_CHUNK];
        let push = |asm: &mut AttachAssembler, stream: u32| {
            asm.push(&Frame::TermAttach { stream, instance: 1, cols: 80, rows: 24, offset: 0, more: true, vt: chunk.clone() })
        };
        let big = MAX_ATTACH_TOTAL_BYTES * 2 / 3 / MAX_TERM_CHUNK;
        for _ in 0..big {
            assert_eq!(push(&mut asm, 1).unwrap(), None);
        }
        let mut refused = false;
        for _ in 0..big {
            if push(&mut asm, 2) == Err(CodecError::TooLarge) {
                refused = true;
                break;
            }
        }
        assert!(refused, "a second attach cannot add another budget");
    }

    #[test]
    fn direction_is_enforced_by_role() {
        use crate::delta::Delta;
        let enc = |f: &Frame| f.encode().unwrap().remove(0);
        let to_server = [
            Frame::Control(Msg::TermUnsubscribe { stream: 1 }),
            Frame::Control(Msg::TermResync { stream: 1 }),
            Frame::TermInput { stream: 1, input: Input::Key(vec![b'a']) },
            Frame::UploadChunk { id: 1, offset: 0, bytes: vec![b'a'] },
        ];
        let to_client = [
            Frame::Control(Msg::Delta { delta: Delta { from: 1, to: 2, ops: vec![] } }),
            Frame::TermData { stream: 1, instance: 1, offset: 0, bytes: vec![b'x'] },
            Frame::TermAttach { stream: 1, instance: 1, cols: 80, rows: 24, offset: 0, more: false, vt: vec![] },
        ];
        let both = [Frame::Control(Msg::Ping { n: 1 }), Frame::Control(Msg::Pong { n: 1 })];
        for f in &to_server {
            assert!(Decoder::server().decode(&enc(f)).unwrap().is_some(), "{f:?}");
            assert_eq!(Decoder::client().decode(&enc(f)), Err(CodecError::Malformed("direction")), "{f:?}");
            assert!(Decoder::new().decode(&enc(f)).unwrap().is_some());
        }
        for f in &to_client {
            assert!(Decoder::client().decode(&enc(f)).unwrap().is_some(), "{f:?}");
            assert_eq!(Decoder::server().decode(&enc(f)), Err(CodecError::Malformed("direction")), "{f:?}");
            assert!(Decoder::new().decode(&enc(f)).unwrap().is_some());
        }
        for f in &both {
            assert!(Decoder::client().decode(&enc(f)).unwrap().is_some());
            assert!(Decoder::server().decode(&enc(f)).unwrap().is_some());
        }
    }

    #[test]
    fn refusals_are_specific() {
        let mut d = Decoder::new();
        assert_eq!(d.decode(&[]), Err(CodecError::Malformed("empty message")));
        assert_eq!(d.decode(&[0x7f]), Err(CodecError::UnknownKind(0x7f)));
        assert_eq!(d.decode(&[0x00]), Err(CodecError::UnknownKind(0)));
        assert_eq!(d.decode(&vec![0x01; MAX_PLAINTEXT + 1]), Err(CodecError::TooLarge));
        assert_eq!(d.decode(&[KIND_CONTROL]), Err(CodecError::Json));
        assert_eq!(d.decode(b"\x01{\"t\":\"nope\"}"), Err(CodecError::Json));
        assert_eq!(d.decode(b"\x01{\"t\":\"hello\",\"protocol\":1,\"client\":\"c\",\"app_version\":\"1\",\"term_version\":\"2.0.0\"}"), Err(CodecError::Version(1)));
        assert_eq!(d.decode(&[KIND_TERM_DATA, 0, 0, 0, 1]), Err(CodecError::Malformed("truncated")));
        let mut empty_data = vec![KIND_TERM_DATA];
        empty_data.extend_from_slice(&[0; 20]);
        assert_eq!(d.decode(&empty_data), Err(CodecError::Invalid("term_data length")));
        let mut bad_input = vec![KIND_TERM_INPUT, 0, 0, 0, 1, 9, 1];
        assert_eq!(d.decode(&bad_input), Err(CodecError::Malformed("input kind")));
        bad_input[5] = 3;
        bad_input[6] = 0xff;
        assert_eq!(d.decode(&bad_input), Err(CodecError::Malformed("paste is not UTF-8")));
        let zero_size = Frame::TermAttach { stream: 1, instance: 1, cols: 0, rows: 5, offset: 0, more: false, vt: vec![] };
        assert!(zero_size.encode().is_err());
    }
}
