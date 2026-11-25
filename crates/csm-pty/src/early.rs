//! Output read before anyone subscribes is kept and handed to the first subscriber, in order.
//!
//! Port of the WPF app's `EarlyOutputBuffer` (CLAUDE.md "Bundled ConPTY", "The first output
//! must not be dropped"). The bundled ConPTY's very first bytes are
//! `ESC[1t ESC[c ESC[?1004h ESC[?9001h`. If `ESC[c` is lost, the emulator never answers
//! OpenConsole's own DA1 query, and OpenConsole then consumes the NEXT DA1 answer — the one the
//! program (Claude's probe sentinel) is waiting for.
//!
//! Ordering: the backlog is flushed to the first subscriber while the lock is held, and
//! [`EarlyOutputBuffer::deliver`] takes the same lock, so no later chunk can overtake the
//! backlog. Only one reader thread ever calls `deliver`, so delivering under the lock costs a
//! concurrent `subscribe` at most one chunk's worth of waiting.

use std::sync::Mutex;

/// A consumer of PTY output. Called on the PTY's reader thread.
pub type OutputSink = Box<dyn FnMut(&[u8]) + Send>;

/// Cap on what is held for a subscriber that never comes (1 MiB, as in the WPF app).
pub const MAX_BUFFERED_BYTES: usize = 1 << 20;

#[derive(Default)]
struct Inner {
    sink: Option<OutputSink>,
    /// `Some` until the first subscriber takes it; `None` forever after.
    pending: Option<Vec<u8>>,
    /// Set when a chunk did not fit: everything after it is dropped too, so the backlog is a
    /// clean prefix of the stream, never a stream with a hole.
    overflowed: bool,
}

/// See the module docs.
pub struct EarlyOutputBuffer {
    inner: Mutex<Inner>,
}

impl Default for EarlyOutputBuffer {
    fn default() -> Self {
        Self { inner: Mutex::new(Inner { sink: None, pending: Some(Vec::new()), overflowed: false }) }
    }
}

impl EarlyOutputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panicking sink must not take the PTY down with it: recover the guard.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Installs the sink (replacing any previous one). The first sink ever installed receives
    /// the backlog before any later chunk.
    pub fn subscribe(&self, mut sink: OutputSink) {
        let mut inner = self.lock();
        if let Some(backlog) = inner.pending.take()
            && !backlog.is_empty()
        {
            sink(&backlog);
        }
        inner.sink = Some(sink);
    }

    /// Removes the sink. Output with no sink after the first subscription is dropped.
    pub fn unsubscribe(&self) {
        self.lock().sink = None;
    }

    /// Called by the reader thread with each chunk.
    pub fn deliver(&self, bytes: &[u8]) {
        let mut inner = self.lock();
        let Inner { sink, pending, overflowed } = &mut *inner;
        match sink {
            Some(sink) => sink(bytes),
            None => {
                if let Some(pending) = pending
                    && !*overflowed
                {
                    if pending.len() + bytes.len() <= MAX_BUFFERED_BYTES {
                        pending.extend_from_slice(bytes);
                    } else {
                        *overflowed = true;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn collector() -> (Arc<Mutex<Vec<u8>>>, OutputSink) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        (out, Box::new(move |b: &[u8]| o.lock().unwrap().extend_from_slice(b)))
    }

    #[test]
    fn backlog_is_flushed_to_first_subscriber_in_order() {
        let buf = EarlyOutputBuffer::new();
        buf.deliver(b"\x1b[1t");
        buf.deliver(b"\x1b[c");
        let (out, sink) = collector();
        buf.subscribe(sink);
        buf.deliver(b"\x1b[?1004h");
        assert_eq!(&*out.lock().unwrap(), b"\x1b[1t\x1b[c\x1b[?1004h");
    }

    #[test]
    fn only_first_subscriber_gets_backlog() {
        let buf = EarlyOutputBuffer::new();
        buf.deliver(b"early");
        let (first, sink) = collector();
        buf.subscribe(sink);
        let (second, sink) = collector();
        buf.subscribe(sink);
        buf.deliver(b"late");
        assert_eq!(&*first.lock().unwrap(), b"early");
        assert_eq!(&*second.lock().unwrap(), b"late");
    }

    #[test]
    fn backlog_is_capped() {
        let buf = EarlyOutputBuffer::new();
        let chunk = vec![b'x'; MAX_BUFFERED_BYTES / 2];
        buf.deliver(&chunk);
        buf.deliver(&chunk);
        buf.deliver(b"overflow");
        let (out, sink) = collector();
        buf.subscribe(sink);
        assert_eq!(out.lock().unwrap().len(), MAX_BUFFERED_BYTES);
    }

    #[test]
    fn overflow_latches_and_later_small_chunks_are_dropped_too() {
        let buf = EarlyOutputBuffer::new();
        let chunk = vec![b'x'; MAX_BUFFERED_BYTES - 10];
        buf.deliver(&chunk);
        buf.deliver(&[b'y'; 100]); // does not fit: latches
        buf.deliver(b"small"); // would fit, but must not leave a hole
        let (out, sink) = collector();
        buf.subscribe(sink);
        assert_eq!(*out.lock().unwrap(), chunk);
        // Live delivery after subscribing is unaffected.
        buf.deliver(b"live");
        assert!(out.lock().unwrap().ends_with(b"live"));
    }

    #[test]
    fn output_after_unsubscribe_is_dropped_not_buffered() {
        let buf = EarlyOutputBuffer::new();
        let (out, sink) = collector();
        buf.subscribe(sink);
        buf.unsubscribe();
        buf.deliver(b"dropped");
        let (out2, sink) = collector();
        buf.subscribe(sink);
        assert!(out.lock().unwrap().is_empty());
        assert!(out2.lock().unwrap().is_empty());
    }

    /// A subscriber attaching while the reader is mid-stream must see one contiguous, ordered
    /// sequence: everything before it (as backlog) then everything after, nothing lost.
    #[test]
    fn concurrent_subscribe_mid_stream_preserves_order() {
        for _ in 0..50 {
            let buf = Arc::new(EarlyOutputBuffer::new());
            let writer = {
                let buf = buf.clone();
                std::thread::spawn(move || {
                    for i in 0u32..2000 {
                        buf.deliver(&i.to_le_bytes());
                    }
                })
            };
            std::thread::yield_now();
            let (out, sink) = collector();
            buf.subscribe(sink);
            writer.join().unwrap();
            let bytes = out.lock().unwrap().clone();
            let got: Vec<u32> = bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
            let expected: Vec<u32> = (0..2000).collect();
            assert_eq!(got, expected);
        }
    }
}
