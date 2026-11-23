//! The byte stream under a peer connection. Direct: a TCP socket. Through the relay: one relay
//! channel, whose bytes the relay copies as opaque binary frames. The Noise session and the frame
//! layer on top are the same either way, so the relay sees ciphertext only.
//!
//! A relay channel is a byte stream over message frames: a write is cut into chunks of at most
//! [`RELAY_CHUNK`] bytes, one relay frame each; a read returns whatever the next frame holds. The
//! sending side is a bounded queue, so a burst of terminal output waits for the pump instead of
//! being dropped (backpressure), and the bound is generous enough not to stall ordinary output.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Bytes per relay data frame on a write. The relay's frame cap is 64 KiB including its header.
pub const RELAY_CHUNK: usize = 16 * 1024;

/// Frames queued for the pump before a write waits.
pub const RELAY_QUEUE: usize = 256;

/// What a relay channel sends to its pump.
#[derive(Debug)]
pub enum Out {
    Data(Vec<u8>),
    /// Close the channel from this side.
    Close,
}

/// Where a relay channel's writes go (a client's own socket, or the office's shared one with its
/// channel number attached).
pub trait Sink: Send + Sync {
    fn send(&self, out: Out) -> io::Result<()>;
}

/// One end of a relay channel.
pub struct RelayStream {
    inner: Arc<Chan>,
}

struct Chan {
    incoming: Mutex<Receiver<Vec<u8>>>,
    pending: Mutex<VecDeque<u8>>,
    read_timeout: Mutex<Option<Duration>>,
    sink: Arc<dyn Sink>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RelayStream {
    /// A channel fed by `incoming` (the pump's data frames) and written through `sink`.
    pub fn new(incoming: Receiver<Vec<u8>>, sink: Arc<dyn Sink>) -> Self {
        Self { inner: Arc::new(Chan { incoming: Mutex::new(incoming), pending: Mutex::new(VecDeque::new()), read_timeout: Mutex::new(None), sink }) }
    }

    fn set_read_timeout(&self, d: Option<Duration>) {
        *lock(&self.inner.read_timeout) = d;
    }

    fn read_bytes(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let timeout = *lock(&self.inner.read_timeout);
        let mut pending = lock(&self.inner.pending);
        while pending.is_empty() {
            let rx = lock(&self.inner.incoming);
            let next = match timeout {
                None => rx.recv().ok(),
                Some(d) => match rx.recv_timeout(d) {
                    Ok(v) => Some(v),
                    Err(RecvTimeoutError::Timeout) => return Err(io::Error::new(io::ErrorKind::TimedOut, "relay channel: no data")),
                    Err(RecvTimeoutError::Disconnected) => None,
                },
            };
            drop(rx);
            match next {
                Some(v) => pending.extend(v),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(pending.len());
        for (slot, byte) in buf.iter_mut().zip(pending.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        let mut sent = 0;
        for chunk in buf.chunks(RELAY_CHUNK) {
            self.inner.sink.send(Out::Data(chunk.to_vec()))?;
            sent += chunk.len();
        }
        Ok(sent)
    }

    fn close(&self) {
        let _ = self.inner.sink.send(Out::Close);
    }
}

/// The socket under a peer connection: TCP, or a relay channel.
pub enum Stream {
    Tcp(TcpStream),
    Relay(RelayStream),
}

impl Stream {
    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_nodelay(on),
            Self::Relay(_) => Ok(()),
        }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_nonblocking(on),
            Self::Relay(_) => Ok(()),
        }
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(d),
            Self::Relay(r) => {
                r.set_read_timeout(d);
                Ok(())
            }
        }
    }

    /// Relay writes wait on the pump's queue, so a write timeout does not apply to them.
    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_write_timeout(d),
            Self::Relay(_) => Ok(()),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(s) => s.try_clone().map(Self::Tcp),
            Self::Relay(r) => Ok(Self::Relay(RelayStream { inner: r.inner.clone() })),
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.shutdown(how),
            Self::Relay(r) => {
                r.close();
                Ok(())
            }
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            Self::Relay(r) => r.read_bytes(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            Self::Relay(r) => r.write_bytes(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            Self::Relay(_) => Ok(()),
        }
    }
}

/// A sink that queues writes for a pump thread (a client's own channel).
pub struct QueueSink(pub SyncSender<Out>);

impl Sink for QueueSink {
    fn send(&self, out: Out) -> io::Result<()> {
        self.0.send(out).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the relay connection is gone"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Sender, sync_channel};

    struct Collect(Mutex<Vec<Out>>);
    impl Sink for Collect {
        fn send(&self, out: Out) -> io::Result<()> {
            lock(&self.0).push(out);
            Ok(())
        }
    }

    fn pair() -> (Sender<Vec<u8>>, RelayStream, Arc<Collect>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink = Arc::new(Collect(Mutex::new(Vec::new())));
        (tx, RelayStream::new(rx, sink.clone()), sink)
    }

    #[test]
    fn reads_join_frames_and_end_when_the_pump_is_gone() {
        let (tx, s, _) = pair();
        let mut st = Stream::Relay(s);
        tx.send(b"abc".to_vec()).unwrap();
        tx.send(Vec::new()).unwrap();
        tx.send(b"de".to_vec()).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(st.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf, b"ab");
        let mut got = Vec::new();
        while got.len() < 3 {
            let mut rest = [0u8; 8];
            let n = st.read(&mut rest).unwrap();
            assert!(n > 0, "the frames hold three more bytes");
            got.extend_from_slice(&rest[..n]);
        }
        assert_eq!(got, b"cde");
        drop(tx);
        let mut end = [0u8; 8];
        assert_eq!(st.read(&mut end).unwrap(), 0);
    }

    #[test]
    fn a_read_timeout_is_timed_out_not_eof() {
        let (_tx, s, _) = pair();
        let mut st = Stream::Relay(s);
        st.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(st.read(&mut buf).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn writes_are_cut_into_relay_sized_chunks() {
        let (_tx, s, sink) = pair();
        let mut st = Stream::Relay(s);
        let big = vec![7u8; RELAY_CHUNK * 2 + 5];
        assert_eq!(st.write(&big).unwrap(), big.len());
        let sent = lock(&sink.0);
        let lens: Vec<usize> = sent.iter().map(|o| if let Out::Data(d) = o { d.len() } else { 0 }).collect();
        assert_eq!(lens, vec![RELAY_CHUNK, RELAY_CHUNK, 5]);
    }

    #[test]
    fn a_full_queue_makes_the_writer_wait_instead_of_dropping() {
        let (tx, rx) = sync_channel::<Out>(2);
        let mut st = Stream::Relay(RelayStream::new(std::sync::mpsc::channel().1, Arc::new(QueueSink(tx))));
        st.write_all(b"one").unwrap();
        st.write_all(b"two").unwrap();
        let writer = std::thread::spawn(move || {
            st.write_all(b"three").unwrap();
            st
        });
        std::thread::sleep(Duration::from_millis(30));
        assert!(!writer.is_finished(), "the third write must wait for the pump");
        assert!(matches!(rx.recv().unwrap(), Out::Data(d) if d == b"one"));
        let _st = writer.join().unwrap();
        assert!(matches!(rx.recv().unwrap(), Out::Data(d) if d == b"two"));
        assert!(matches!(rx.recv().unwrap(), Out::Data(d) if d == b"three"));
    }
}
