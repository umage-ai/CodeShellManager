//! Resizes off the UI thread. A PTY resize is a synchronous system call (ConPTY can take
//! milliseconds, or longer while the host is busy), and a pane reports its size every frame while
//! the user drags a divider. [`Coalescer`] hands the values to a worker thread that applies only
//! the latest one: intermediate sizes are dropped, since only the final size matters.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

struct State<T> {
    latest: Option<T>,
    closed: bool,
}

type Shared<T> = Arc<(Mutex<State<T>>, Condvar)>;

/// A worker thread applying the latest submitted value. Dropping it stops the thread (after the
/// call in flight, if any).
pub struct Coalescer<T: Send + 'static> {
    shared: Shared<T>,
}

impl<T: Send + 'static> Coalescer<T> {
    /// Starts the worker; `apply` runs on it with each value that was not superseded.
    pub fn spawn(name: &str, mut apply: impl FnMut(T) + Send + 'static) -> Self {
        let shared: Shared<T> = Arc::new((Mutex::new(State { latest: None, closed: false }), Condvar::new()));
        let worker = shared.clone();
        let spawned = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            loop {
                let next = {
                    let (m, cv) = &*worker;
                    let mut st = m.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        if let Some(v) = st.latest.take() {
                            break Some(v);
                        }
                        if st.closed {
                            break None;
                        }
                        st = cv.wait(st).unwrap_or_else(PoisonError::into_inner);
                    }
                };
                match next {
                    Some(v) => apply(v),
                    None => return,
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("could not start {name}: {e}");
        }
        Self { shared }
    }

    /// Replaces any value the worker has not started on yet. Never blocks on the worker.
    pub fn submit(&self, v: T) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap_or_else(PoisonError::into_inner).latest = Some(v);
        cv.notify_one();
    }
}

impl<T: Send + 'static> Drop for Coalescer<T> {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
        cv.notify_one();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn only_the_latest_value_is_applied_while_the_worker_is_busy() {
        let (seen_tx, seen_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let c = Coalescer::spawn("test-resize", move |v: u32| {
            seen_tx.send(v).unwrap();
            if v == 0 {
                // The first call blocks, like a slow ConPTY resize.
                let _ = gate_rx.lock().unwrap().recv();
            }
        });
        c.submit(0);
        assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
        for v in 1..=50 {
            c.submit(v);
        }
        gate_tx.send(()).unwrap();
        assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 50);
        assert!(seen_rx.recv_timeout(Duration::from_millis(100)).is_err(), "no intermediate size is applied");
    }

    #[test]
    fn submit_does_not_wait_for_the_worker() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let c = Coalescer::spawn("test-resize", move |_: u32| {
            let _ = gate_rx.lock().unwrap().recv();
        });
        let t = std::time::Instant::now();
        for v in 0..100 {
            c.submit(v);
        }
        assert!(t.elapsed() < Duration::from_secs(1));
        drop(gate_tx);
    }
}
