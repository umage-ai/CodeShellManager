//! One secured TCP connection: length-prefixed frames, the Noise handshakes, and the transport.
//!
//! Layout: [`Io`] reads and writes handshake frames. After the handshake a connection is split
//! into a [`Reader`] (one thread blocks on it) and a shared [`Link`] (any thread may send, one at
//! a time). The Noise transport state is behind a mutex held only while encrypting or decrypting
//! one message, never across a blocking read or write, so a slow peer cannot stall the other
//! direction. Rekeying follows `csm-peer-proto`'s vectors: each direction ticks its own counter and
//! rekeys after every `REKEY_INTERVAL`-th message.
//!
//! Everything read here is untrusted: lengths are checked on the header, a bad frame or a failed
//! decryption ends the connection, and nothing indexes or unwraps on what the network said.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use csm_peer_proto::codec::{CodecError, Decoder, Frame};
use csm_peer_proto::frame::{FrameDecoder, FrameError, encode_frame};
use csm_peer_proto::limits::{MAX_NOISE_MESSAGE, MAX_PLAINTEXT, TAG_LEN};
use csm_peer_proto::msg::{ByeReason, Msg};
use csm_peer_proto::noise::{PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, Rekey, prologue};

use crate::identity::Identity;

/// How a connection failed. The text is for the log, not for the person.
#[derive(Debug)]
pub enum LinkError {
    /// The other side closed, or we did.
    Closed,
    /// A deadline passed.
    Timeout,
    Io(String),
    Frame(FrameError),
    Noise(String),
    Codec(CodecError),
    /// The handshake succeeded but the peer is not allowed (an unknown key).
    Refused,
    /// A well-formed message that breaks the protocol (the wrong one at this point).
    Protocol(&'static str),
    /// The person cancelled a pairing while the handshake was waiting.
    Cancelled,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the connection is closed"),
            Self::Timeout => f.write_str("timed out"),
            Self::Io(e) => write!(f, "network error: {e}"),
            Self::Frame(e) => write!(f, "bad frame: {e}"),
            Self::Noise(e) => write!(f, "secure channel error: {e}"),
            Self::Codec(e) => write!(f, "bad message: {e}"),
            Self::Refused => f.write_str("refused"),
            Self::Protocol(w) => write!(f, "protocol error: {w}"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<FrameError> for LinkError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}

impl From<CodecError> for LinkError {
    fn from(e: CodecError) -> Self {
        Self::Codec(e)
    }
}

impl From<snow::Error> for LinkError {
    fn from(e: snow::Error) -> Self {
        Self::Noise(e.to_string())
    }
}

fn io_err(e: &std::io::Error) -> LinkError {
    match e.kind() {
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe => LinkError::Closed,
        _ => LinkError::Io(e.to_string()),
    }
}

/// How long a write may block before the connection is given up.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Frame reading and writing on a socket, before and while it is being secured.
pub(crate) struct Io {
    stream: TcpStream,
    dec: FrameDecoder,
    /// When set and raised, a handshake read gives up at its next slice (a quarter of a second).
    cancel: Option<Arc<AtomicBool>>,
}

impl Io {
    pub(crate) fn new(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self { stream, dec: FrameDecoder::new(), cancel: None })
    }

    /// Lets a raised `cancel` end a wait for a frame with [`LinkError::Cancelled`].
    pub(crate) fn cancel_on(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub(crate) fn write_frame(&mut self, payload: &[u8]) -> Result<(), LinkError> {
        let framed = encode_frame(payload)?;
        self.stream.write_all(&framed).map_err(|e| io_err(&e))
    }

    /// One frame, waiting at most `wait`; `Ok(None)` when none arrived in time.
    pub(crate) fn read_frame(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        if let Some(f) = self.dec.next_frame()? {
            return Ok(Some(f));
        }
        self.stream.set_read_timeout(Some(wait.max(Duration::from_millis(1)))).map_err(|e| io_err(&e))?;
        let mut buf = [0u8; 16 * 1024];
        match self.stream.read(&mut buf) {
            Ok(0) => Err(LinkError::Closed),
            Ok(n) => {
                self.dec.push(buf.get(..n).unwrap_or_default());
                Ok(self.dec.next_frame()?)
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(io_err(&e)),
        }
    }

    /// One frame, or [`LinkError::Timeout`] at `deadline`.
    pub(crate) fn read_frame_by(&mut self, deadline: Instant) -> Result<Vec<u8>, LinkError> {
        loop {
            if self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Acquire)) {
                return Err(LinkError::Cancelled);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LinkError::Timeout);
            }
            if let Some(f) = self.read_frame(left.min(Duration::from_millis(250)))? {
                return Ok(f);
            }
        }
    }
}

struct WriteHalf {
    stream: TcpStream,
    rekey: Rekey,
}

/// The sending side of a secured connection, shared between threads.
pub(crate) struct Link {
    noise: Mutex<snow::TransportState>,
    writer: Mutex<WriteHalf>,
    shut: TcpStream,
    closed: AtomicBool,
}

impl Link {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Sends already-encoded plaintexts, one Noise message each, back to back (the fragments of
    /// one control message must not be separated by another sender).
    pub(crate) fn send_plains(&self, plains: &[Vec<u8>]) -> Result<(), LinkError> {
        if self.is_closed() {
            return Err(LinkError::Closed);
        }
        let mut w = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        for p in plains {
            if let Err(e) = self.write_one(&mut w, p) {
                self.close();
                return Err(e);
            }
        }
        Ok(())
    }

    fn write_one(&self, w: &mut WriteHalf, plain: &[u8]) -> Result<(), LinkError> {
        if plain.len() > MAX_PLAINTEXT {
            return Err(LinkError::Codec(CodecError::TooLarge));
        }
        let mut ct = vec![0u8; plain.len() + TAG_LEN];
        let n = {
            let mut t = self.noise.lock().unwrap_or_else(PoisonError::into_inner);
            let n = t.write_message(plain, &mut ct)?;
            if w.rekey.tick() {
                t.rekey_outgoing();
            }
            n
        };
        let framed = encode_frame(ct.get(..n).unwrap_or_default())?;
        w.stream.write_all(&framed).map_err(|e| io_err(&e))
    }

    pub(crate) fn send_frame(&self, frame: &Frame) -> Result<(), LinkError> {
        self.send_plains(&frame.encode()?)
    }

    pub(crate) fn send_msg(&self, msg: Msg) -> Result<(), LinkError> {
        self.send_frame(&Frame::Control(msg))
    }

    /// A goodbye, if the writer is free (a writer stuck on a dead peer is cut by `close`).
    pub(crate) fn try_bye(&self, reason: ByeReason) {
        if self.is_closed() {
            return;
        }
        let Ok(plains) = Frame::Control(Msg::Bye { reason }).encode() else { return };
        if let Ok(mut w) = self.writer.try_lock() {
            for p in &plains {
                if self.write_one(&mut w, p).is_err() {
                    break;
                }
            }
        }
    }

    /// Ends the connection: both directions, at once, from any thread.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.shut.shutdown(Shutdown::Both);
    }
}

/// The receiving side: owned by the one thread that reads.
pub(crate) struct Reader {
    io: Io,
    link: Arc<Link>,
    codec: Decoder,
    rekey: Rekey,
}

impl Reader {
    /// One decrypted Noise message, or `None` after `wait` with nothing.
    pub(crate) fn recv_plain(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        let Some(ct) = self.io.read_frame(wait)? else { return Ok(None) };
        let mut out = vec![0u8; ct.len()];
        let n = {
            let mut t = self.link.noise.lock().unwrap_or_else(PoisonError::into_inner);
            let n = t.read_message(&ct, &mut out)?;
            if self.rekey.tick() {
                t.rekey_incoming();
            }
            n
        };
        out.truncate(n);
        Ok(Some(out))
    }

    /// One whole frame (fragments joined), or `None` after `wait` with nothing complete.
    pub(crate) fn recv(&mut self, wait: Duration) -> Result<Option<Frame>, LinkError> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(plain) = self.recv_plain(left)? else { return Ok(None) };
            if let Some(frame) = self.codec.decode(&plain)? {
                return Ok(Some(frame));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }
}

/// A handshake that finished: the secured halves, who is on the other end, and the handshake hash
/// (both ends derive the same, so the check code matches).
pub(crate) struct Established {
    pub(crate) link: Arc<Link>,
    pub(crate) reader: Reader,
    pub(crate) remote_static: [u8; 32],
    pub(crate) hash: Vec<u8>,
}

/// Which side of the connection decodes what: a server accepts client frames and the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Server,
    Client,
}

fn finish(io: Io, transport: snow::TransportState, remote_static: [u8; 32], hash: Vec<u8>, side: Side) -> Result<Established, LinkError> {
    let write = io.stream.try_clone().map_err(|e| io_err(&e))?;
    let shut = io.stream.try_clone().map_err(|e| io_err(&e))?;
    let link = Arc::new(Link {
        noise: Mutex::new(transport),
        writer: Mutex::new(WriteHalf { stream: write, rekey: Rekey::new() }),
        shut,
        closed: AtomicBool::new(false),
    });
    let codec = match side {
        Side::Server => Decoder::server(),
        Side::Client => Decoder::client(),
    };
    Ok(Established { link: link.clone(), reader: Reader { io, link, codec, rekey: Rekey::new() }, remote_static, hash })
}

fn builder<'a>(psk: Option<&'a [u8; 32]>, me: &'a Identity, prologue_bytes: &'a [u8]) -> Result<snow::Builder<'a>, LinkError> {
    let params = if psk.is_some() { PARAMS_PAIRING } else { PARAMS_SESSION };
    let params = params.parse().map_err(|e: snow::Error| LinkError::from(e))?;
    let mut b = snow::Builder::new(params).prologue(prologue_bytes)?.local_private_key(me.private())?;
    if let Some(psk) = psk {
        b = b.psk(PSK_POSITION, psk)?;
    }
    Ok(b)
}

/// The responder's half, after the selector frame was read. `accept_key` decides whether the
/// client's static key may continue; `false` ends the connection before the host says a word.
pub(crate) fn respond(
    mut io: Io,
    host: &Identity,
    psk: Option<&[u8; 32]>,
    accept_key: &dyn Fn(&[u8; 32]) -> bool,
    deadline: Instant,
) -> Result<Established, LinkError> {
    let host_id = host.host_id();
    let pro = prologue(&host_id);
    let mut hs = builder(psk, host, &pro)?.build_responder()?;
    let msg1 = io.read_frame_by(deadline)?;
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    hs.read_message(&msg1, &mut buf)?;
    let remote: [u8; 32] = hs.get_remote_static().and_then(|s| s.try_into().ok()).ok_or(LinkError::Noise("no client key".into()))?;
    if !accept_key(&remote) {
        return Err(LinkError::Refused);
    }
    let n = hs.write_message(&[], &mut buf)?;
    io.write_frame(buf.get(..n).unwrap_or_default())?;
    if !hs.is_handshake_finished() {
        return Err(LinkError::Protocol("the handshake did not finish"));
    }
    let hash = hs.get_handshake_hash().to_vec();
    finish(io, hs.into_transport_mode()?, remote, hash, Side::Server)
}

/// The initiator's half: sends the selector frame (`preamble`), then the handshake. The host's
/// key must be the one the offer or the saved host gave: a different key fails the handshake.
pub(crate) fn initiate(
    mut io: Io,
    me: &Identity,
    host_pub: &[u8; 32],
    host_id: &str,
    psk: Option<&[u8; 32]>,
    preamble: &[u8],
    deadline: Instant,
) -> Result<Established, LinkError> {
    io.write_frame(preamble)?;
    let pro = prologue(host_id);
    let mut hs = builder(psk, me, &pro)?.remote_public_key(host_pub)?.build_initiator()?;
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    let n = hs.write_message(&[], &mut buf)?;
    io.write_frame(buf.get(..n).unwrap_or_default())?;
    let msg2 = io.read_frame_by(deadline)?;
    hs.read_message(&msg2, &mut buf)?;
    if !hs.is_handshake_finished() {
        return Err(LinkError::Protocol("the handshake did not finish"));
    }
    let hash = hs.get_handshake_hash().to_vec();
    finish(io, hs.into_transport_mode()?, *host_pub, hash, Side::Client)
}
