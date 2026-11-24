//! The ops worker: runs [`Engine::handle`] (and so every CSM call a phone op makes) on its own
//! thread, so a slow or hostile CSM, or a slow phone, cannot stall the relay loop, its pings or the
//! revoke handling.
//!
//! The host hands over one [`Job`] per parsed request. The queue is bounded ([`QUEUE`]); a full
//! queue is refused by the host with `rate_limited`, never waited for. The worker answers each job
//! with a [`Done`], which the host reads with [`Worker::try_done`] on its own schedule and seals
//! into the channel's Noise transport. One worker thread means results come back in the order the
//! jobs went in, so the transport's nonces stay in order for each channel.
//!
//! The device list is not shared with the worker. Each job carries a copy, and [`Done`] carries the
//! copy back. The host merges only the fields the ops change (`last_seen`, `hide_names`, `notify`,
//! `push`), so a device revoked while its request was in flight is not brought back.

use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};

use csm_remote_proto::msg::Request;

use crate::config::Config;
use crate::ops::Engine;
use crate::store::Device;

/// Jobs that may wait for the worker. The host also caps requests in flight, so this is a second
/// bound, not the first.
pub const QUEUE: usize = 8;

/// The channel number a tracker read is answered on (never a real channel: the relay's are below
/// [`u32::MAX`] and the host never opens this one).
pub const TRACK_CH: u32 = u32::MAX;

/// One parsed request for the worker, or (with `track`) one read of CSM's session list for the
/// host-wide tracker (section 7.1). `req` is ignored for a tracker read.
pub struct Job {
    pub ch: u32,
    pub device_id: String,
    pub req: Request,
    pub now: u64,
    pub cfg: Config,
    pub devices: Vec<Device>,
    pub track: bool,
    /// The `remote_control` wait this job belongs to (its launch, or the `/remote-control` it sends):
    /// the host's token for the wait. Copied to the answer, so the host knows which wait it answers.
    pub wait: Option<u64>,
}

/// The answer to one job: the response (for a request) or the session list (for a tracker read),
/// and the device copy the worker used (the host merges it).
pub struct Done {
    pub ch: u32,
    pub device_id: String,
    pub response: Option<csm_remote_proto::msg::Response>,
    pub sessions: Option<Result<Vec<csm_remote_proto::msg::SessionInfo>, String>>,
    pub devices: Vec<Device>,
    pub devices_changed: bool,
    pub wait: Option<u64>,
}

/// The host's side of the worker: submit jobs, read answers. Dropping it stops the thread once the
/// queue is empty.
pub struct Worker {
    jobs: SyncSender<Job>,
    done: Receiver<Done>,
}

/// Why a job was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The queue is full: the host answers `rate_limited`.
    Full,
    /// The worker thread is gone: the host answers `unavailable`.
    Gone,
}

impl Worker {
    /// Starts the thread that owns `engine`. Fails only when the OS refuses a thread.
    pub fn start(engine: Engine) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel::<Job>(QUEUE);
        let (done_tx, done_rx) = channel::<Done>();
        std::thread::Builder::new()
            .name("csm-phone-ops".into())
            .spawn(move || run(engine, job_rx, done_tx))
            .map_err(|e| format!("the phone remote could not start its worker: {e}"))?;
        Ok(Self { jobs: job_tx, done: done_rx })
    }

    /// Queues a job without waiting.
    pub fn submit(&self, job: Job) -> Result<(), Refused> {
        match self.jobs.try_send(job) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(Refused::Full),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Err(Refused::Gone),
        }
    }

    /// The next answer, if one is ready. Never waits.
    pub fn try_done(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }

    /// The next answer, waiting up to `wait`. For tests and for shutdown; the relay loop uses
    /// [`Worker::try_done`].
    pub fn next_done(&self, wait: std::time::Duration) -> Option<Done> {
        self.done.recv_timeout(wait).ok()
    }
}

fn run(mut engine: Engine, jobs: Receiver<Job>, done: Sender<Done>) {
    while let Ok(job) = jobs.recv() {
        let Job { ch, device_id, req, now, cfg, mut devices, track, wait } = job;
        let finished = if track {
            let sessions = engine.track();
            Done { ch, device_id, response: None, sessions: Some(sessions), devices, devices_changed: false, wait }
        } else {
            let out = engine.handle(&cfg, &mut devices, &device_id, &req, now);
            Done { ch, device_id, response: Some(out.response), sessions: None, devices, devices_changed: out.devices_changed, wait }
        };
        if done.send(finished).is_err() {
            // The host is gone: nobody will read the answers.
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::csm::{Csm, CsmError};
    use csm_remote_proto::msg::{Level, Op};
    use serde_json::Value;
    use std::time::{Duration, Instant};

    /// A CSM that blocks for `delay` on every call, to show the host never waits for it.
    struct Slow(Duration);
    impl Csm for Slow {
        fn call(&self, _tool: &str, _params: Value) -> Result<Value, CsmError> {
            std::thread::sleep(self.0);
            Ok(serde_json::json!({"sessions": []}))
        }
    }

    fn device(id: &str, level: Level) -> Device {
        Device {
            device_id: id.into(),
            kind: "phone".into(),
            name: "Phone".into(),
            level,
            token_hash: "AAAA".into(),
            phone_pub: "AAAA".into(),
            paired_at: 1,
            last_seen: 1,
            hide_names: false,
            notify: true,
            push: None,
        }
    }

    fn ping(id: u64) -> Request {
        Request { id, op: Op::Ping }
    }

    #[test]
    fn a_job_is_answered_on_the_worker_and_the_answer_comes_back() {
        let w = Worker::start(Engine::new(Box::new(Slow(Duration::from_millis(0))))).unwrap();
        w.submit(Job { ch: 7, device_id: "d1".into(), req: ping(3), now: 50, cfg: Config::default(), devices: vec![device("d1", Level::Viewer)], track: false, wait: None }).unwrap();
        let started = Instant::now();
        let done = loop {
            if let Some(d) = w.try_done() {
                break d;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "the answer never came");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!((done.ch, done.device_id.as_str()), (7, "d1"));
        assert_eq!(done.devices.first().map(|d| d.last_seen), Some(50), "the device copy carries last_seen back");
    }

    #[test]
    fn a_full_queue_refuses_at_once_and_never_waits() {
        // A CSM that never answers in time keeps the worker busy with the first job.
        let w = Worker::start(Engine::new(Box::new(Slow(Duration::from_secs(3))))).unwrap();
        let mut refused = 0;
        let started = Instant::now();
        for i in 0..(QUEUE as u64 * 3) {
            // session.list calls CSM, so the worker is busy for the whole test; ping would not.
            let req = Request { id: i, op: Op::SessionList };
            let job = Job { ch: 1, device_id: "d1".into(), req, now: 1, cfg: Config::default(), devices: vec![device("d1", Level::Viewer)], track: false, wait: None };
            if w.submit(job) == Err(Refused::Full) {
                refused += 1;
            }
        }
        assert!(refused > 0, "a queue of {QUEUE} must refuse the overflow");
        assert!(started.elapsed() < Duration::from_secs(1), "refusal is immediate, not a wait");
    }
}
