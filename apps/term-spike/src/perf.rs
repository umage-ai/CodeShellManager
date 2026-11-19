//! Frame-time bookkeeping for the gate: CPU time of each frame's `ui()`, the interval between
//! frames (→ fps), mesh rebuilds and lock misses.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::view::FrameCounters;

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: Instant,
    pub cpu_ms: f32,
    pub counters: FrameCounters,
}

#[derive(Default)]
pub struct Perf {
    samples: VecDeque<Sample>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Summary {
    pub frames: usize,
    pub window_s: f32,
    pub fps: f32,
    pub cpu_avg: f32,
    pub cpu_p50: f32,
    pub cpu_p95: f32,
    pub cpu_p99: f32,
    pub cpu_max: f32,
    pub interval_p95: f32,
    pub rebuilds_per_frame: f32,
    pub lock_misses: u32,
}

const KEEP: Duration = Duration::from_secs(120);

impl Perf {
    pub fn record(&mut self, s: Sample) {
        self.samples.push_back(s);
        while self.samples.front().is_some_and(|f| s.at.duration_since(f.at) > KEEP) {
            self.samples.pop_front();
        }
    }

    pub fn summary(&self, now: Instant, window: Duration) -> Summary {
        let recent: Vec<&Sample> = self.samples.iter().filter(|s| now.duration_since(s.at) <= window).collect();
        if recent.is_empty() {
            return Summary { window_s: window.as_secs_f32(), ..Summary::default() };
        }
        let mut cpu: Vec<f32> = recent.iter().map(|s| s.cpu_ms).collect();
        cpu.sort_by(f32::total_cmp);
        let pct = |p: f32| cpu.get(((cpu.len() - 1) as f32 * p).round() as usize).copied().unwrap_or(0.0);
        let mut intervals: Vec<f32> = recent
            .windows(2)
            .map(|w| w[1].at.duration_since(w[0].at).as_secs_f32() * 1000.0)
            .collect();
        intervals.sort_by(f32::total_cmp);
        let interval_p95 = intervals.get(((intervals.len().max(1) - 1) as f32 * 0.95).round() as usize).copied().unwrap_or(0.0);
        let rebuilds: u32 = recent.iter().map(|s| s.counters.rebuilds).sum();
        Summary {
            frames: recent.len(),
            window_s: window.as_secs_f32(),
            fps: recent.len() as f32 / window.as_secs_f32(),
            cpu_avg: cpu.iter().sum::<f32>() / cpu.len() as f32,
            cpu_p50: pct(0.5),
            cpu_p95: pct(0.95),
            cpu_p99: pct(0.99),
            cpu_max: cpu.last().copied().unwrap_or(0.0),
            interval_p95,
            rebuilds_per_frame: rebuilds as f32 / recent.len() as f32,
            lock_misses: recent.iter().map(|s| s.counters.lock_misses).sum(),
        }
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} frames in {:.0}s ({:.1} fps, frame interval p95 {:.1} ms) | ui() cpu ms avg {:.2} p50 {:.2} p95 {:.2} p99 {:.2} max {:.2} | mesh rebuilds/frame {:.2} | lock misses {}",
            self.frames, self.window_s, self.fps, self.interval_p95, self.cpu_avg, self.cpu_p50, self.cpu_p95, self.cpu_p99, self.cpu_max, self.rebuilds_per_frame, self.lock_misses
        )
    }
}
