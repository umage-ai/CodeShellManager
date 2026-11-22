//! The output tap: a bounded ring of one session's PTY output, addressed by absolute byte
//! offsets, with subscribers that read it at their own pace and byte-rate counters.
//!
//! Feeding ([`OutputTap::feed`], called from the PTY reader thread) never waits for a subscriber.
//! The ring keeps the newest [`RING_BYTES`] and counts what it evicts. Each subscriber is only
//! nudged through a one-slot channel, and a nudge that finds the slot full is simply skipped
//! (the subscriber already has one pending). A subscriber reads from its own offset whenever it
//! likes; one that falls behind the ring's oldest byte is told to resync, and the feed never
//! waits for it. The feed does take the tap's lock, which subscribers hold only while copying at
//! most [`READ_CHUNK_BYTES`].
//!
//! Offsets are absolute: the first byte a session ever printed is offset 0, and an offset keeps
//! meaning the same byte after the ring has wrapped, so a subscriber can say "I have up to here".

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Bytes of output one session keeps for subscribers (the ring size).
pub const RING_BYTES: usize = 1024 * 1024;
/// Most bytes one [`Subscription::read`] hands out, so the tap's lock is held only briefly.
pub const READ_CHUNK_BYTES: usize = 64 * 1024;
/// The window of the rolling byte-rate counter, in whole seconds.
pub const RATE_WINDOW_SECS: u64 = 60;

/// One session's output tap. Cheap to share: subscriptions hold an `Arc` to it.
pub struct OutputTap {
    inner: Mutex<Inner>,
    origin: Instant,
}

struct Inner {
    ring: Ring,
    /// One wake slot per live subscription; a subscription that is dropped is removed on the
    /// next feed.
    subscribers: Vec<SyncSender<()>>,
    rate: RateMeter,
    evicted_bytes: u64,
    resyncs: u64,
}

/// What a subscriber gets from [`Subscription::read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// The bytes from `from` on, up to the chunk limit. Empty when the subscriber is caught up.
    Bytes { from: u64, data: Vec<u8> },
    /// The offset the subscriber asked for is no longer in the ring (or is past its end). The
    /// subscriber must take a fresh snapshot of the screen; reading then continues from `end`.
    Resync { oldest: u64, end: u64 },
}

/// A reader of one [`OutputTap`], at its own offset.
pub struct Subscription {
    tap: Arc<OutputTap>,
    next: u64,
    wake: Receiver<()>,
}

/// The tap's counters, as of one moment. Rates are bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputStats {
    /// Every byte fed since the tap started.
    pub total_bytes: u64,
    /// Bytes fed in the last [`RATE_WINDOW_SECS`] seconds, divided by the window.
    pub bytes_per_sec_last_minute: u64,
    /// The busiest single second so far.
    pub peak_bytes_per_sec: u64,
    /// The ring's capacity in bytes.
    pub ring_bytes: u64,
    /// The offset of the oldest byte still in the ring.
    pub oldest_offset: u64,
    /// The offset just past the newest byte.
    pub end_offset: u64,
    /// Bytes pushed out of the ring before a subscriber read them (or before anyone did).
    pub evicted_bytes: u64,
    /// Times a subscription was told to resync.
    pub resyncs: u64,
}

impl OutputTap {
    /// A tap with the default ring size.
    pub fn new() -> Self {
        Self::with_capacity(RING_BYTES)
    }

    /// A tap whose ring keeps `capacity` bytes (at least 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                ring: Ring { bytes: VecDeque::new(), capacity: capacity.max(1), oldest: 0 },
                subscribers: Vec::new(),
                rate: RateMeter::new(),
                evicted_bytes: 0,
                resyncs: 0,
            }),
            origin: Instant::now(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends PTY output. Never waits for a subscriber.
    pub fn feed(&self, data: &[u8]) {
        self.feed_at(data, Instant::now());
    }

    fn feed_at(&self, data: &[u8], now: Instant) {
        if data.is_empty() {
            return;
        }
        let mut g = self.lock();
        let evicted = g.ring.push(data);
        g.evicted_bytes = g.evicted_bytes.saturating_add(evicted);
        let n = u64::try_from(data.len()).unwrap_or(u64::MAX);
        g.rate.record(n, now.saturating_duration_since(self.origin));
        g.subscribers.retain(|tx| !matches!(tx.try_send(()), Err(TrySendError::Disconnected(()))));
    }

    /// A subscription that starts at `from`, or at the current end when `None`. A `from` the
    /// ring no longer holds gives [`Read::Resync`] on the first read.
    pub fn subscribe(self: &Arc<Self>, from: Option<u64>) -> Subscription {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut g = self.lock();
        let next = from.unwrap_or_else(|| g.ring.end());
        g.subscribers.push(tx);
        drop(g);
        Subscription { tap: Arc::clone(self), next, wake: rx }
    }

    /// The counters now.
    pub fn stats(&self) -> OutputStats {
        self.stats_at(Instant::now())
    }

    fn stats_at(&self, now: Instant) -> OutputStats {
        let g = self.lock();
        let since = now.saturating_duration_since(self.origin);
        OutputStats {
            total_bytes: g.rate.total,
            bytes_per_sec_last_minute: g.rate.last_minute_average(since),
            peak_bytes_per_sec: g.rate.peak,
            ring_bytes: u64::try_from(g.ring.capacity).unwrap_or(u64::MAX),
            oldest_offset: g.ring.oldest,
            end_offset: g.ring.end(),
            evicted_bytes: g.evicted_bytes,
            resyncs: g.resyncs,
        }
    }
}

impl Default for OutputTap {
    fn default() -> Self {
        Self::new()
    }
}

impl Subscription {
    /// Reads up to `max` bytes (and at most [`READ_CHUNK_BYTES`]) from this subscription's offset.
    pub fn read(&mut self, max: usize) -> Read {
        let mut g = self.tap.lock();
        let max = max.min(READ_CHUNK_BYTES);
        if let Some(data) = g.ring.read(self.next, max) {
            let from = self.next;
            self.next = self.next.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            return Read::Bytes { from, data };
        }
        g.resyncs = g.resyncs.saturating_add(1);
        let (oldest, end) = (g.ring.oldest, g.ring.end());
        self.next = end;
        Read::Resync { oldest, end }
    }

    /// The offset the next [`read`](Self::read) starts at.
    pub fn offset(&self) -> u64 {
        self.next
    }

    /// Waits up to `timeout` for new output. Returns true when some was fed since the last wait
    /// (or one is pending); a false return only means nothing new came in time.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.wake.recv_timeout(timeout).is_ok()
    }
}

/// The ring: the newest `capacity` bytes, with the absolute offset of its first byte.
struct Ring {
    bytes: VecDeque<u8>,
    capacity: usize,
    /// Absolute offset of `bytes[0]`.
    oldest: u64,
}

impl Ring {
    fn end(&self) -> u64 {
        self.oldest.saturating_add(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX))
    }

    /// Appends `data`; returns how many bytes left the front (evicted), including any prefix of
    /// `data` that could never fit.
    fn push(&mut self, data: &[u8]) -> u64 {
        let keep = data.len().min(self.capacity);
        let skipped = data.len() - keep;
        let kept = data.get(skipped..).unwrap_or_default();
        let over = (self.bytes.len() + kept.len()).saturating_sub(self.capacity);
        self.bytes.drain(..over);
        self.bytes.extend(kept.iter().copied());
        let evicted = u64::try_from(over + skipped).unwrap_or(u64::MAX);
        self.oldest = self.oldest.saturating_add(evicted);
        evicted
    }

    /// Up to `max` bytes from absolute offset `from`; `None` when `from` is outside the ring.
    fn read(&self, from: u64, max: usize) -> Option<Vec<u8>> {
        if from > self.end() {
            return None;
        }
        let rel = usize::try_from(from.checked_sub(self.oldest)?).ok()?;
        Some(self.bytes.iter().skip(rel).take(max).copied().collect())
    }
}

/// Bytes per second, kept per whole second for the last [`RATE_WINDOW_SECS`] seconds, plus the
/// busiest second seen.
struct RateMeter {
    /// `(second since the tap's origin, bytes in that second)`, indexed by second modulo the window.
    slots: [(u64, u64); RATE_WINDOW_SECS as usize],
    total: u64,
    peak: u64,
}

impl RateMeter {
    fn new() -> Self {
        Self { slots: [(0, 0); RATE_WINDOW_SECS as usize], total: 0, peak: 0 }
    }

    fn record(&mut self, n: u64, since_origin: Duration) {
        let sec = since_origin.as_secs();
        let idx = usize::try_from(sec % RATE_WINDOW_SECS).unwrap_or(0);
        if let Some(slot) = self.slots.get_mut(idx) {
            if slot.0 != sec {
                *slot = (sec, 0);
            }
            slot.1 = slot.1.saturating_add(n);
            self.peak = self.peak.max(slot.1);
        }
        self.total = self.total.saturating_add(n);
    }

    /// Bytes in the window ending at `since_origin`, divided by the window length.
    fn last_minute_average(&self, since_origin: Duration) -> u64 {
        let sec = since_origin.as_secs();
        let sum = self.slots.iter().filter(|(s, _)| sec.saturating_sub(*s) < RATE_WINDOW_SECS).fold(0u64, |acc, (_, b)| acc.saturating_add(*b));
        sum / RATE_WINDOW_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(cap: usize) -> Arc<OutputTap> {
        Arc::new(OutputTap::with_capacity(cap))
    }

    fn bytes(read: Read) -> (u64, Vec<u8>) {
        match read {
            Read::Bytes { from, data } => (from, data),
            Read::Resync { oldest, end } => panic!("expected bytes, got resync {oldest}..{end}"),
        }
    }

    #[test]
    fn offsets_stay_absolute_across_wrap_around() {
        let t = tap(8);
        t.feed(b"abcdef");
        t.feed(b"ghijkl");
        let s = t.stats();
        assert_eq!((s.oldest_offset, s.end_offset, s.total_bytes), (4, 12, 12));

        let mut sub = t.subscribe(Some(4));
        assert_eq!(bytes(sub.read(100)), (4, b"efghijkl".to_vec()));
        assert_eq!(sub.offset(), 12);
        assert_eq!(bytes(sub.read(100)), (12, Vec::new()), "caught up: empty bytes");

        t.feed(b"mnopqrstuv");
        let s = t.stats();
        assert_eq!((s.oldest_offset, s.end_offset), (14, 22));
        let mut late = t.subscribe(Some(14));
        assert_eq!(bytes(late.read(100)), (14, b"opqrstuv".to_vec()));
    }

    #[test]
    fn many_small_writes_keep_the_newest_window_and_its_offsets() {
        let t = tap(16);
        let mut all = Vec::new();
        for i in 0..1000u32 {
            let chunk = [(i % 251) as u8, (i % 7) as u8, b'x'];
            t.feed(&chunk);
            all.extend_from_slice(&chunk);
        }
        let total = all.len() as u64;
        let s = t.stats();
        assert_eq!(s.end_offset, total);
        assert_eq!(s.oldest_offset, total - 16);
        assert_eq!(s.evicted_bytes, total - 16);

        let mut sub = t.subscribe(Some(s.oldest_offset));
        let (from, data) = bytes(sub.read(1024));
        assert_eq!(from, total - 16);
        assert_eq!(data, all[all.len() - 16..].to_vec());
    }

    #[test]
    fn a_write_larger_than_the_ring_keeps_only_its_tail() {
        let t = tap(4);
        t.feed(b"ab");
        t.feed(b"0123456789");
        let s = t.stats();
        assert_eq!((s.oldest_offset, s.end_offset), (8, 12));
        let mut sub = t.subscribe(Some(8));
        assert_eq!(bytes(sub.read(100)), (8, b"6789".to_vec()));
    }

    #[test]
    fn eviction_past_a_subscriber_gives_a_resync() {
        let t = tap(8);
        let mut sub = t.subscribe(None);
        assert_eq!(sub.offset(), 0);
        t.feed(b"0123456789abcdef");
        assert_eq!(sub.read(100), Read::Resync { oldest: 8, end: 16 });
        assert_eq!(sub.offset(), 16, "after a resync the subscriber continues from the end");

        t.feed(b"!!");
        assert_eq!(bytes(sub.read(100)), (16, b"!!".to_vec()));
        assert_eq!(t.stats().resyncs, 1);
    }

    #[test]
    fn a_subscription_from_before_the_ring_resyncs_at_once() {
        let t = tap(8);
        t.feed(&[b'z'; 20]);
        let mut sub = t.subscribe(Some(3));
        assert_eq!(sub.read(10), Read::Resync { oldest: 12, end: 20 });
    }

    #[test]
    fn a_subscription_ahead_of_the_end_resyncs_rather_than_inventing_bytes() {
        let t = tap(8);
        t.feed(b"abc");
        let mut sub = t.subscribe(Some(99));
        assert_eq!(sub.read(10), Read::Resync { oldest: 0, end: 3 });
    }

    #[test]
    fn reads_are_capped_per_call() {
        let t = tap(RING_BYTES);
        t.feed(&vec![7u8; READ_CHUNK_BYTES + 100]);
        let mut sub = t.subscribe(Some(0));
        let (from, first) = bytes(sub.read(usize::MAX));
        assert_eq!((from, first.len()), (0, READ_CHUNK_BYTES));
        let (from, rest) = bytes(sub.read(usize::MAX));
        assert_eq!((from, rest.len()), (READ_CHUNK_BYTES as u64, 100));
    }

    #[test]
    fn a_wake_is_pending_after_feed_and_clears_on_read() {
        let t = tap(64);
        let sub = t.subscribe(None);
        assert!(!sub.wait(Duration::ZERO), "nothing fed yet");
        t.feed(b"x");
        assert!(sub.wait(Duration::from_millis(50)));
        t.feed(b"y");
        t.feed(b"z");
        assert!(sub.wait(Duration::ZERO), "several feeds coalesce into one pending wake");
        assert!(!sub.wait(Duration::ZERO));
    }

    #[test]
    fn a_slow_subscriber_does_not_block_the_feed() {
        // The subscriber never reads and never waits; the feeder must still finish quickly.
        let t = tap(1024 * 1024);
        let _stalled = t.subscribe(None);
        let feeder = Arc::clone(&t);
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let chunk = vec![b'.'; 4096];
            for _ in 0..5_000 {
                feeder.feed(&chunk);
            }
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_secs(20)).expect("the feed finished while a subscriber stalled");
        handle.join().expect("feeder thread");
        let s = t.stats();
        assert_eq!(s.total_bytes, 5_000 * 4096);
        assert_eq!(s.end_offset, 5_000 * 4096);
        assert_eq!(s.oldest_offset, s.end_offset - 1024 * 1024, "the ring kept only its window");
    }

    #[test]
    fn dropped_subscriptions_are_forgotten_by_the_feed() {
        let t = tap(64);
        let kept = t.subscribe(None);
        let dropped = t.subscribe(None);
        assert_eq!(t.lock().subscribers.len(), 2);
        drop(dropped);
        t.feed(b"a");
        assert_eq!(t.lock().subscribers.len(), 1, "the feed prunes the dropped one");
        drop(kept);
        t.feed(b"b");
        assert_eq!(t.lock().subscribers.len(), 0);
    }

    #[test]
    fn counters_track_totals_rate_and_peak() {
        let t = tap(1 << 20);
        let origin = t.origin;
        let at = |secs: f64| origin + Duration::from_secs_f64(secs);

        t.feed_at(&[b'a'; 100], at(0.1));
        t.feed_at(&[b'b'; 50], at(0.5));
        t.feed_at(&[b'c'; 300], at(1.2));
        let s = t.stats_at(at(1.9));
        assert_eq!(s.total_bytes, 450);
        assert_eq!(s.peak_bytes_per_sec, 300, "the busiest second");
        assert_eq!(s.bytes_per_sec_last_minute, 450 / 60);

        t.feed_at(&[b'd'; 6000], at(30.0));
        let s = t.stats_at(at(30.5));
        assert_eq!(s.total_bytes, 6450);
        assert_eq!(s.peak_bytes_per_sec, 6000);
        assert_eq!(s.bytes_per_sec_last_minute, (450 + 6000) / 60, "the first seconds are still in the window");

        let s = t.stats_at(at(61.0));
        assert_eq!(s.bytes_per_sec_last_minute, 6000 / 60, "second 0 has left the window; second 30 is still in");
        let s = t.stats_at(at(91.0));
        assert_eq!(s.bytes_per_sec_last_minute, 0, "nothing in the last minute");
        assert_eq!(s.total_bytes, 6450, "the total never drops");
        assert_eq!(s.peak_bytes_per_sec, 6000, "the peak never drops");
    }

    #[test]
    fn counters_report_the_ring_and_evictions() {
        let t = tap(10);
        t.feed(&[1u8; 25]);
        let s = t.stats();
        assert_eq!(s.ring_bytes, 10);
        assert_eq!(s.evicted_bytes, 15);
        assert_eq!((s.oldest_offset, s.end_offset), (15, 25));
        let s = OutputStats::default();
        assert_eq!(s.resyncs, 0);
    }

    #[test]
    fn an_empty_feed_changes_nothing() {
        let t = tap(8);
        t.feed(b"");
        assert_eq!(t.stats(), OutputStats { ring_bytes: 8, ..OutputStats::default() });
    }
}
