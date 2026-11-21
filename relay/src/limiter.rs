//! Small in-memory rate limiters (all state is lost on restart, by design).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Token bucket: `rate` tokens per second, holding at most `cap`.
#[derive(Debug, Clone)]
pub struct Bucket {
    tokens: f64,
    cap: f64,
    rate: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(rate_per_sec: u32, burst_factor: u32, now: Instant) -> Self {
        let cap = f64::from(rate_per_sec) * f64::from(burst_factor.max(1));
        Self { tokens: cap, cap, rate: f64::from(rate_per_sec), last: now }
    }

    /// Takes `n` tokens if available.
    pub fn take(&mut self, n: u32, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
        let n = f64::from(n);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
}

/// The two per-channel, per-direction buckets: frames per second and bytes per second.
#[derive(Debug, Clone)]
pub struct ChannelLimiter {
    frames: Bucket,
    bytes: Bucket,
}

impl ChannelLimiter {
    pub fn new(frames_per_sec: u32, bytes_per_sec: u32, burst_factor: u32, now: Instant) -> Self {
        Self { frames: Bucket::new(frames_per_sec, burst_factor, now), bytes: Bucket::new(bytes_per_sec, burst_factor, now) }
    }

    pub fn allow(&mut self, len: usize, now: Instant) -> bool {
        let len = u32::try_from(len).unwrap_or(u32::MAX);
        // Charge both, so a refused frame costs a frame token but never "refunds" bytes.
        let f = self.frames.take(1, now);
        let b = self.bytes.take(len, now);
        f && b
    }
}

/// Sliding window: at most `max` hits in the last `span`.
#[derive(Debug, Default, Clone)]
pub struct Window {
    times: VecDeque<Instant>,
}

impl Window {
    /// True if one more hit would be within the limit (nothing is recorded).
    pub fn has_room(&mut self, now: Instant, span: Duration, max: usize) -> bool {
        self.prune(now, span);
        self.times.len() < max
    }

    pub fn record(&mut self, now: Instant) {
        self.times.push_back(now);
    }

    /// Records a hit and returns true if it is within the limit; a refused hit is not recorded.
    pub fn hit(&mut self, now: Instant, span: Duration, max: usize) -> bool {
        self.prune(now, span);
        if self.times.len() >= max {
            return false;
        }
        self.times.push_back(now);
        true
    }

    pub fn prune(&mut self, now: Instant, span: Duration) {
        while let Some(&t) = self.times.front() {
            if now.saturating_duration_since(t) > span {
                self.times.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_burst_then_refills() {
        let t0 = Instant::now();
        let mut b = Bucket::new(10, 2, t0);
        assert!((0..20).all(|_| b.take(1, t0)));
        assert!(!b.take(1, t0));
        assert!(b.take(1, t0 + Duration::from_millis(150)));
    }

    #[test]
    fn window_counts_and_expires() {
        let t0 = Instant::now();
        let mut w = Window::default();
        let span = Duration::from_secs(60);
        assert!(w.hit(t0, span, 2));
        assert!(w.hit(t0, span, 2));
        assert!(!w.hit(t0, span, 2));
        assert!(w.hit(t0 + Duration::from_secs(61), span, 2));
    }

    #[test]
    fn channel_limiter_limits_bytes() {
        let t0 = Instant::now();
        let mut c = ChannelLimiter::new(1000, 1000, 2, t0);
        assert!(c.allow(1500, t0));
        assert!(!c.allow(1500, t0));
    }
}
