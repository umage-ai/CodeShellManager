//! The output tap: a bounded ring of one session's PTY output, addressed by absolute byte
//! offsets, with subscribers that read it at their own pace and byte-rate counters.
//!
//! Feeding ([`OutputTap::feed`], called from the PTY reader thread) never waits. It takes the
//! tap's lock only with `try_lock`: when a subscriber or a stats call holds the lock (for at most
//! [`READ_CHUNK_BYTES`] of copying), the feed drops its bytes and counts them in
//! [`OutputStats::dropped_bytes`], as the index feed does. The ring keeps the newest
//! [`RING_BYTES`] and counts what it evicts. Each subscriber is only nudged through a one-slot
//! channel, and a nudge that finds the slot full is simply skipped (the subscriber already has
//! one pending). A subscriber reads from its own offset whenever it likes; one that falls behind
//! the ring's oldest byte, or behind bytes the feed dropped, is told to resync.
//!
//! Offsets are absolute: the first byte a session ever printed is offset 0. Every byte fed takes
//! an offset, dropped ones included, so an offset keeps meaning the same byte after the ring has
//! wrapped, and a subscriber can say "I have up to here".

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
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
    /// Every byte fed so far. Only the feeder advances it, so its value is the absolute offset
    /// of the next feed, and it is read without the lock.
    fed: AtomicU64,
    /// Bytes a feed could not take in because the tap was busy. They never reached the ring.
    dropped_bytes: AtomicU64,
    /// Dropped bytes the rate meter has not counted yet; the next feed that gets in adds them.
    unmetered: AtomicU64,
}

struct Inner {
    ring: Ring,
    /// One wake slot per live subscription; a subscription that is dropped is removed on the
    /// next feed that gets in.
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
    /// The offset the subscriber asked for is no longer in the ring (it was evicted, dropped by a
    /// busy feed, or lies past the end). The subscriber must take a fresh snapshot of the screen;
    /// reading then continues from `end`.
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
    /// Every byte fed since the tap started, dropped bytes included.
    pub total_bytes: u64,
    /// Bytes fed in the last [`RATE_WINDOW_SECS`] seconds, divided by the seconds the window
    /// covers: 60, or the seconds since the tap started (counting the first one) while it is
    /// younger than that. A young session therefore reads its true rate, not a twelfth of it.
    pub bytes_per_sec_last_minute: u64,
    /// The busiest single second so far.
    pub peak_bytes_per_sec: u64,
    /// The ring's capacity in bytes.
    pub ring_bytes: u64,
    /// The offset of the oldest byte still in the ring.
    pub oldest_offset: u64,
    /// The offset just past the newest byte the ring holds.
    pub end_offset: u64,
    /// Bytes the ring has pushed out of its front to stay within capacity.
    pub evicted_bytes: u64,
    /// Bytes fed while the tap was busy with a subscriber or a stats call. They never reached the
    /// ring, so subscribers behind them resync.
    pub dropped_bytes: u64,
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
            fed: AtomicU64::new(0),
            dropped_bytes: AtomicU64::new(0),
            unmetered: AtomicU64::new(0),
        }
    }

    /// The lock for subscribers and stats, which may wait for one another's short copies.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lock for the feed, which never waits: `None` when the lock is held right now.
    fn try_lock(&self) -> Option<MutexGuard<'_, Inner>> {
        match self.inner.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Appends PTY output. Never waits: if the tap is busy the bytes are dropped and counted.
    /// Returns the absolute offset just past these bytes (dropped ones take an offset too).
    pub fn feed(&self, data: &[u8]) -> u64 {
        self.feed_at(data, Instant::now())
    }

    /// The absolute offset just past every byte fed so far (dropped ones included). Lock-free.
    pub fn fed(&self) -> u64 {
        self.fed.load(Ordering::Acquire)
    }

    /// Whether a subscriber can start reading at `offset`: it lies inside the ring, or at its end.
    pub fn holds(&self, offset: u64) -> bool {
        let g = self.lock();
        offset >= g.ring.oldest && offset <= g.ring.end()
    }

    fn feed_at(&self, data: &[u8], now: Instant) -> u64 {
        if data.is_empty() {
            return self.fed();
        }
        let n = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let start = self.fed.fetch_add(n, Ordering::AcqRel);
        let end = start.saturating_add(n);
        let Some(mut g) = self.try_lock() else {
            self.dropped_bytes.fetch_add(n, Ordering::Relaxed);
            self.unmetered.fetch_add(n, Ordering::Relaxed);
            return end;
        };
        let folded = self.unmetered.swap(0, Ordering::Relaxed);
        match g.ring.push(start, data) {
            Ok(evicted) => g.evicted_bytes = g.evicted_bytes.saturating_add(evicted),
            // A feed that lost a race to a later one: its bytes are behind the ring already.
            Err(Stale) => {
                self.dropped_bytes.fetch_add(n, Ordering::Relaxed);
            }
        }
        g.rate.record(n.saturating_add(folded), now.saturating_duration_since(self.origin));
        g.subscribers.retain(|tx| !matches!(tx.try_send(()), Err(TrySendError::Disconnected(()))));
        end
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
            total_bytes: self.fed.load(Ordering::Acquire),
            bytes_per_sec_last_minute: g.rate.last_minute_average(since),
            peak_bytes_per_sec: g.rate.peak,
            ring_bytes: u64::try_from(g.ring.capacity).unwrap_or(u64::MAX),
            oldest_offset: g.ring.oldest,
            end_offset: g.ring.end(),
            evicted_bytes: g.evicted_bytes,
            dropped_bytes: self.dropped_bytes.load(Ordering::Relaxed),
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

    /// How many bytes were fed that this subscription has not read yet. Lock-free.
    pub fn behind(&self) -> u64 {
        self.tap.fed().saturating_sub(self.next)
    }

    /// Waits up to `timeout` for a nudge. Returns true when one is pending. A feed that found the
    /// tap busy does not nudge (it only counts its bytes), so a false return does not prove that
    /// nothing was fed: callers bound the wait and read regardless.
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

/// A push whose bytes lie behind the ring's end.
struct Stale;

impl Ring {
    fn end(&self) -> u64 {
        self.oldest.saturating_add(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX))
    }

    /// Appends `data`, which starts at absolute offset `start`. Returns how many bytes left the
    /// ring (evicted), counting a prefix of `data` that could never fit.
    ///
    /// A `start` past the end means bytes were dropped in between: the ring starts afresh at
    /// `start` and its old contents go, so a subscriber behind them resyncs. A `start` before the
    /// end is stale and changes nothing.
    fn push(&mut self, start: u64, data: &[u8]) -> Result<u64, Stale> {
        let end = self.end();
        if start < end {
            return Err(Stale);
        }
        let mut evicted = 0u64;
        if start > end {
            evicted = u64::try_from(self.bytes.len()).unwrap_or(u64::MAX);
            self.bytes.clear();
        }
        let keep = data.len().min(self.capacity);
        let skipped = data.len() - keep;
        let kept = data.get(skipped..).unwrap_or_default();
        let over = (self.bytes.len() + kept.len()).saturating_sub(self.capacity).min(self.bytes.len());
        self.bytes.drain(..over);
        self.bytes.extend(kept.iter().copied());
        evicted = evicted.saturating_add(u64::try_from(over.saturating_add(skipped)).unwrap_or(u64::MAX));
        let new_end = start.saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        self.oldest = new_end.saturating_sub(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX));
        Ok(evicted)
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
    peak: u64,
}

impl RateMeter {
    fn new() -> Self {
        Self { slots: [(0, 0); RATE_WINDOW_SECS as usize], peak: 0 }
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
    }

    /// Bytes in the window ending at `since_origin`, divided by the seconds the window covers.
    fn last_minute_average(&self, since_origin: Duration) -> u64 {
        let sec = since_origin.as_secs();
        let sum = self.slots.iter().filter(|(s, _)| sec.saturating_sub(*s) < RATE_WINDOW_SECS).fold(0u64, |acc, (_, b)| acc.saturating_add(*b));
        let window = sec.saturating_add(1).min(RATE_WINDOW_SECS);
        sum / window
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
        assert_eq!(s.evicted_bytes, 8, "two bytes pushed out, six that never fit");
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
        assert_eq!(s.dropped_bytes, 0, "nothing held the lock, so nothing was dropped");
    }

    #[test]
    fn a_feed_does_not_wait_for_a_read_or_stats_held_on_another_thread() {
        let t = tap(1024);
        t.feed(b"abc");
        let holder_tap = Arc::clone(&t);
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        // Stands in for a subscriber's read (or a stats call) paused mid-copy under the lock.
        let holder = std::thread::spawn(move || {
            let g = holder_tap.lock();
            let _ = locked_tx.send(());
            let _ = release_rx.recv();
            drop(g);
        });
        locked_rx.recv_timeout(Duration::from_secs(5)).expect("the holder took the lock");

        let feeder = Arc::clone(&t);
        let (done_tx, done_rx) = mpsc::channel();
        let feeding = std::thread::spawn(move || {
            feeder.feed(b"defg");
            feeder.feed(b"hi");
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(Duration::from_secs(5)).expect("the feed returned while the lock was held elsewhere");
        feeding.join().expect("feeder thread");
        release_tx.send(()).expect("release the holder");
        holder.join().expect("holder thread");

        let s = t.stats();
        assert_eq!(s.dropped_bytes, 6, "both feeds were dropped and counted");
        assert_eq!(s.total_bytes, 9, "dropped bytes still take offsets");
        assert_eq!(s.end_offset, 3, "the ring holds only what it accepted");
    }

    #[test]
    fn a_subscriber_behind_a_dropped_feed_resyncs_and_the_stream_goes_on() {
        let t = tap(1024);
        let mut sub = t.subscribe(None);
        t.feed(b"abc");
        assert_eq!(bytes(sub.read(100)), (0, b"abc".to_vec()));

        let holder_tap = Arc::clone(&t);
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let g = holder_tap.lock();
            let _ = locked_tx.send(());
            let _ = release_rx.recv();
            drop(g);
        });
        locked_rx.recv_timeout(Duration::from_secs(5)).expect("lock held");
        t.feed(b"XXXX");
        release_tx.send(()).expect("release");
        holder.join().expect("holder");

        t.feed(b"yz");
        assert_eq!(sub.read(100), Read::Resync { oldest: 7, end: 9 }, "bytes 3..7 are gone for good");
        assert_eq!(sub.offset(), 9);
        t.feed(b"!");
        assert_eq!(bytes(sub.read(100)), (9, b"!".to_vec()));
        let s = t.stats();
        assert_eq!((s.dropped_bytes, s.evicted_bytes, s.resyncs), (4, 3, 1));
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
        assert_eq!(s.bytes_per_sec_last_minute, 450 / 2, "a 1.9 s old tap divides by the 2 seconds it has");

        t.feed_at(&[b'd'; 6000], at(30.0));
        let s = t.stats_at(at(30.5));
        assert_eq!(s.total_bytes, 6450);
        assert_eq!(s.peak_bytes_per_sec, 6000);
        assert_eq!(s.bytes_per_sec_last_minute, (450 + 6000) / 31, "the first seconds are still in the window");

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
