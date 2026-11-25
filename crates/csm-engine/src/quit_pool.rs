//! The bounded pool a quit closes Claude sessions with.
//!
//! Up to `slots` Claude sessions are asked to exit at once; as one finishes (cleanly, forced after
//! its own wait, or already gone) the next one starts. Starts are spaced by a short gap so exits
//! do not land in the same instant (the Claude CLI rewrites `~/.claude.json` unlocked on exit; the
//! quit also backs that file up, see `claude_config`). With one slot this is the old sequence:
//! one at a time in sidebar order, with the flat settle after each clean exit.
//!
//! Every wait is bounded (the per-session wait) and every wait ends early when the quit is forced.
//! This runs on the quit's own worker thread, never the UI's or the engine's.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use csm_pty::Pty;

/// How the pool runs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PoolOptions {
    /// Sessions closing at once (at least 1).
    pub slots: usize,
    /// How long each session gets to exit by itself before it is stopped.
    pub per_session: Duration,
    /// The flat settle: after a clean exit with one slot, and (divided across the slots) between
    /// starts with several.
    pub settle: Duration,
}

impl PoolOptions {
    /// The pause between two starts. One slot has none: it settles after each exit instead.
    pub fn start_gap(&self) -> Duration {
        match u32::try_from(self.slots) {
            Ok(n) if n > 1 => self.settle / n,
            _ => Duration::ZERO,
        }
    }

    /// The longest the whole pool can take for `claudes` sessions: the waves of per-session
    /// waits plus the settles or start gaps.
    pub fn budget(&self, claudes: usize) -> Duration {
        let slots = self.slots.max(1);
        let claudes = u32::try_from(claudes.max(1)).unwrap_or(u32::MAX);
        let waves = claudes.div_ceil(u32::try_from(slots).unwrap_or(1));
        let pauses = if slots == 1 { self.settle } else { self.start_gap() };
        self.per_session.saturating_mul(waves).saturating_add(pauses.saturating_mul(claudes))
    }
}

/// What a session's turn reports: `emit(id, phase, elapsed_ms)` with phase `closing` when its
/// exit begins, then `clean` or `forced` when it is done (exactly one of those per session).
pub(crate) type Emit<'a> = &'a (dyn Fn(&str, &str, u64) + Sync);

/// Spaces the starts: each start waits until `gap` after the previous start ACTUALLY happened (not
/// after the time it was scheduled for, which an oversleep would shrink). The lock is held while
/// waiting, so slots take their turns however many ask at once; a forced quit ends the wait.
struct StartGate {
    gap: Duration,
    last_start: Mutex<Option<Instant>>,
}

impl StartGate {
    fn new(gap: Duration) -> Self {
        Self { gap, last_start: Mutex::new(None) }
    }

    /// Waits for this start's turn (unless `stop`) and returns the instant the start happened.
    fn wait_turn(&self, stop: &dyn Fn() -> bool) -> Instant {
        let mut last = self.last_start.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(prev) = *last {
            sleep_unless((prev + self.gap).saturating_duration_since(Instant::now()), stop);
        }
        let now = Instant::now();
        *last = Some(now);
        now
    }
}

/// Closes every session in `targets` (in order) with at most `o.slots` in flight. Returns when all
/// are done. `forced` is polled: once true, nothing waits any more.
pub(crate) fn close_all(targets: Vec<(String, Arc<dyn Pty>)>, o: PoolOptions, forced: &(dyn Fn() -> bool + Sync), emit: Emit<'_>) {
    if targets.is_empty() {
        return;
    }
    let slots = o.slots.clamp(1, targets.len());
    let queue = Mutex::new(VecDeque::from(targets));
    let gate = StartGate::new(o.start_gap());
    let one_slot = o.slots <= 1;
    let work = || {
        loop {
            let Some((id, pty)) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front() else { return };
            let t0 = Instant::now();
            let ms = |t: Instant| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
            if pty.has_exited() || forced() {
                // Decided before the kill: after it every PTY has exited. One that was gone is
                // clean; a live one that a forced quit reached is forced.
                let phase = if pty.has_exited() { "clean" } else { "forced" };
                pty.kill();
                emit(&id, phase, ms(t0));
                continue;
            }
            gate.wait_turn(forced);
            let t0 = Instant::now();
            if forced() {
                let phase = if pty.has_exited() { "clean" } else { "forced" };
                pty.kill();
                emit(&id, phase, ms(t0));
                continue;
            }
            emit(&id, "closing", 0);
            pty.close();
            let clean = wait_exit_unless(pty.as_ref(), o.per_session, forced);
            pty.kill();
            let phase = if clean { "clean" } else { "forced" };
            log::info!("quit: {id} {phase} exit={}ms", ms(t0));
            emit(&id, phase, ms(t0));
            if clean && one_slot && !forced() {
                sleep_unless(o.settle, forced);
            }
        }
    };
    thread::scope(|s| {
        for n in 1..slots {
            let spawned = thread::Builder::new().name(format!("csm-quit-{n}")).spawn_scoped(s, work);
            if let Err(e) = spawned {
                // Fewer slots, still correct: the threads that did start drain the queue.
                log::error!("quit: no pool thread {n}: {e}");
                break;
            }
        }
        work();
    });
}

fn wait_exit_unless(pty: &dyn Pty, wait: Duration, stop: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    while !pty.has_exited() {
        if Instant::now() >= deadline || stop() {
            return pty.has_exited();
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

fn sleep_unless(d: Duration, stop: &dyn Fn() -> bool) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline && !stop() {
        thread::sleep(Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use csm_pty::{Backend, ExitCallback, OutputSink, PtySize};

    /// Exits `after` once closed (never when `None`); a kill exits at once.
    struct Fake {
        after: Option<Duration>,
        closed_at: Mutex<Option<Instant>>,
        killed: AtomicBool,
        gone: bool,
    }

    impl Fake {
        fn new(after: Option<Duration>) -> Arc<Fake> {
            Arc::new(Fake { after, closed_at: Mutex::new(None), killed: AtomicBool::new(false), gone: false })
        }
    }

    impl Pty for Fake {
        fn subscribe(&self, _: OutputSink) {}
        fn on_exit(&self, _: ExitCallback) {}
        fn write(&self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn resize(&self, _: PtySize) -> std::io::Result<()> {
            Ok(())
        }
        fn has_exited(&self) -> bool {
            if self.gone || self.killed.load(Ordering::SeqCst) {
                return true;
            }
            let at = *self.closed_at.lock().unwrap();
            matches!((at, self.after), (Some(c), Some(a)) if c.elapsed() >= a)
        }
        fn exit_code(&self) -> Option<i32> {
            None
        }
        fn kill(&self) {
            self.killed.store(true, Ordering::SeqCst);
        }
        fn close(&self) {
            *self.closed_at.lock().unwrap() = Some(Instant::now());
        }
        fn backend(&self) -> Backend {
            Backend::BundledConPty
        }
        fn pid(&self) -> Option<u32> {
            None
        }
    }

    #[derive(Default)]
    struct Log {
        events: Mutex<Vec<(String, String, Instant)>>,
        now: AtomicUsize,
        max: AtomicUsize,
    }

    impl Log {
        fn emit(&self, id: &str, phase: &str) {
            if phase == "closing" {
                let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                self.max.fetch_max(n, Ordering::SeqCst);
            } else if self.events.lock().unwrap().iter().any(|(i, p, _)| i == id && p == "closing") {
                self.now.fetch_sub(1, Ordering::SeqCst);
            }
            self.events.lock().unwrap().push((id.into(), phase.into(), Instant::now()));
        }
        fn phases(&self, which: &str) -> Vec<String> {
            self.events.lock().unwrap().iter().filter(|e| e.1 == which).map(|e| e.0.clone()).collect()
        }
    }

    fn run(ptys: Vec<Arc<Fake>>, o: PoolOptions, forced: &(dyn Fn() -> bool + Sync)) -> Log {
        let log = Log::default();
        let targets: Vec<(String, Arc<dyn Pty>)> = ptys.into_iter().enumerate().map(|(i, p)| (format!("s{i}"), p as Arc<dyn Pty>)).collect();
        close_all(targets, o, forced, &|id, phase, _| log.emit(id, phase));
        log
    }

    fn opts(slots: usize, per_session_ms: u64, settle_ms: u64) -> PoolOptions {
        PoolOptions { slots, per_session: Duration::from_millis(per_session_ms), settle: Duration::from_millis(settle_ms) }
    }

    #[test]
    fn never_more_than_the_slots_are_in_flight_and_all_finish() {
        let ptys: Vec<_> = (0..12).map(|_| Fake::new(Some(Duration::from_millis(60)))).collect();
        let log = run(ptys.clone(), opts(3, 5000, 0), &|| false);
        assert_eq!(log.max.load(Ordering::SeqCst), 3, "the pool fills its slots and never exceeds them");
        assert_eq!(log.phases("clean").len(), 12);
        assert!(log.phases("forced").is_empty());
        assert_eq!(log.now.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_slot_is_the_old_order_one_after_another() {
        let ptys: Vec<_> = (0..4).map(|_| Fake::new(Some(Duration::from_millis(30)))).collect();
        let log = run(ptys, opts(1, 5000, 10), &|| false);
        let seq: Vec<(String, String)> = log.events.lock().unwrap().iter().map(|e| (e.0.clone(), e.1.clone())).collect();
        let mut want = Vec::new();
        for i in 0..4 {
            want.push((format!("s{i}"), "closing".to_owned()));
            want.push((format!("s{i}"), "clean".to_owned()));
        }
        assert_eq!(seq, want);
        assert_eq!(log.max.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_wedged_session_is_forced_after_its_own_wait_and_the_next_one_starts() {
        let mut ptys = vec![Fake::new(None)];
        ptys.extend((0..3).map(|_| Fake::new(Some(Duration::from_millis(20)))));
        let t0 = Instant::now();
        let log = run(ptys, opts(2, 300, 0), &|| false);
        assert_eq!(log.phases("forced"), vec!["s0"]);
        assert_eq!(log.phases("clean").len(), 3);
        // The healthy ones did not wait behind the wedged one.
        let first_clean = log.events.lock().unwrap().iter().find(|e| e.1 == "clean").map(|e| e.2.duration_since(t0)).unwrap();
        assert!(first_clean < Duration::from_millis(250), "{first_clean:?}");
    }

    #[test]
    fn force_close_now_stops_every_wait_mid_pool() {
        let ptys: Vec<_> = (0..8).map(|_| Fake::new(None)).collect();
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            f2.store(true, Ordering::SeqCst);
        });
        let t0 = Instant::now();
        let log = run(ptys.clone(), opts(3, 60_000, 0), &|| flag.load(Ordering::SeqCst));
        stopper.join().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        assert_eq!(log.phases("forced").len(), 8, "in flight ones and queued ones are all stopped, none reported clean");
        assert!(ptys.iter().all(|p| p.killed.load(Ordering::SeqCst)));
        assert!(log.max.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn a_session_that_is_already_gone_is_clean_without_a_wait_or_a_slot() {
        let mut gone = Fake::new(None);
        Arc::get_mut(&mut gone).unwrap().gone = true;
        let log = run(vec![gone, Fake::new(Some(Duration::from_millis(20)))], opts(1, 1000, 0), &|| false);
        assert_eq!(log.phases("clean"), vec!["s0", "s1"]);
        assert_eq!(log.events.lock().unwrap().iter().filter(|e| e.1 == "closing").count(), 1);
    }

    #[test]
    fn starts_are_spaced_by_the_settle_divided_across_the_slots() {
        let ptys: Vec<_> = (0..4).map(|_| Fake::new(Some(Duration::from_millis(10)))).collect();
        let o = opts(4, 1000, 800);
        assert_eq!(o.start_gap(), Duration::from_millis(200));
        let log = run(ptys, o, &|| false);
        assert_eq!(log.phases("closing").len(), 4);
        // The spacing itself is checked on the gate, on the instants it reports: no scheduler
        // jitter between "the start happened" and "an event was stamped" can make it flaky.
        let gate = StartGate::new(o.start_gap());
        let starts: Vec<Instant> = thread::scope(|s| {
            let handles: Vec<_> = (0..4).map(|_| s.spawn(|| gate.wait_turn(&|| false))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut sorted = starts;
        sorted.sort();
        for w in sorted.windows(2) {
            assert!(w[1].duration_since(w[0]) >= o.start_gap(), "{:?}", w[1].duration_since(w[0]));
        }
    }

    #[test]
    fn a_late_start_does_not_shorten_the_next_gap() {
        // The second start waits a full gap after the first one really happened, even when the first
        // was late: nothing is scheduled ahead of time.
        let gate = StartGate::new(Duration::from_millis(50));
        let a = gate.wait_turn(&|| false);
        thread::sleep(Duration::from_millis(80)); // later than the gap: the next start is immediate
        let b = gate.wait_turn(&|| false);
        let c = gate.wait_turn(&|| false);
        assert!(b.duration_since(a) >= Duration::from_millis(80));
        assert!(c.duration_since(b) >= Duration::from_millis(50), "{:?}", c.duration_since(b));
    }

    #[test]
    fn the_budget_counts_waves_and_pauses() {
        // 70 sessions, 15 s each at worst: one slot is the old 70 x (15 + 2); four slots are 18 waves.
        assert_eq!(opts(1, 15_000, 2000).budget(70), Duration::from_millis(70 * 17_000));
        assert_eq!(opts(4, 15_000, 2000).budget(70), Duration::from_millis(18 * 15_000 + 70 * 500));
        assert_eq!(opts(4, 15_000, 2000).budget(0), opts(4, 15_000, 2000).budget(1));
    }
}
