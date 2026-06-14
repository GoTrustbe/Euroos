//! EuroObserve — **in-kernel observability** (plan W).
//!
//! Operators want more than logs: CPU/memory metrics, counter series, latency
//! histograms — and they want to scrape them with standard tooling. EuroObserve
//! provides **lock-free metric primitives** (`Counter`/`Gauge`/`Histogram`, pure
//! atomics → zero overhead when nobody reads) and an **OpenMetrics** renderer, so
//! Prometheus can read EuroOS directly. Pure `no_std` logic → host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// A monotonic counter (only goes up).
pub struct Counter(AtomicU64);
impl Counter {
    pub const fn new() -> Self {
        Counter(AtomicU64::new(0))
    }
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}
impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// A gauge that can go up and down (e.g. free pages).
pub struct Gauge(AtomicI64);
impl Gauge {
    pub const fn new() -> Self {
        Gauge(AtomicI64::new(0))
    }
    pub fn set(&self, v: i64) {
        self.0.store(v, Ordering::Relaxed);
    }
    pub fn add(&self, d: i64) {
        self.0.fetch_add(d, Ordering::Relaxed);
    }
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}
impl Default for Gauge {
    fn default() -> Self {
        Self::new()
    }
}

/// A latency histogram with fixed `le` bounds (microseconds). `BUCKETS` bounds +
/// an implicit `+Inf` bucket; lock-free.
pub const HIST_BOUNDS: [u64; 6] = [10, 50, 100, 500, 1000, 5000];

pub struct Histogram {
    buckets: [AtomicU64; 7], // 6 bounds + +Inf
    sum: AtomicU64,
}
impl Histogram {
    pub const fn new() -> Self {
        Histogram {
            buckets: [const { AtomicU64::new(0) }; 7],
            sum: AtomicU64::new(0),
        }
    }
    /// Record an observation (µs).
    pub fn observe(&self, us: u64) {
        self.sum.fetch_add(us, Ordering::Relaxed);
        let idx = HIST_BOUNDS.iter().position(|&b| us <= b).unwrap_or(6);
        // OpenMetrics histogram buckets are CUMULATIVE (≤ bound): count all higher ones too.
        for b in idx..7 {
            self.buckets[b].fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn count(&self) -> u64 {
        self.buckets[6].load(Ordering::Relaxed)
    }
    pub fn sum(&self) -> u64 {
        self.sum.load(Ordering::Relaxed)
    }
}
impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

// ── OpenMetrics rendering (Prometheus text format) ──────────────────────────
/// Render a counter line: `# TYPE`/`# HELP` + value.
pub fn render_counter(name: &str, help: &str, c: &Counter) -> String {
    format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {}\n", c.get())
}

/// Render a gauge line.
pub fn render_gauge(name: &str, help: &str, g: &Gauge) -> String {
    format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {}\n", g.get())
}

/// Render a histogram (cumulative `_bucket{le=...}` + `_sum` + `_count`).
pub fn render_histogram(name: &str, help: &str, h: &Histogram) -> String {
    let mut s = format!("# HELP {name} {help}\n# TYPE {name} histogram\n");
    for (i, &b) in HIST_BOUNDS.iter().enumerate() {
        s.push_str(&format!("{name}_bucket{{le=\"{b}\"}} {}\n", h.buckets[i].load(Ordering::Relaxed)));
    }
    s.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {}\n", h.buckets[6].load(Ordering::Relaxed)));
    s.push_str(&format!("{name}_sum {}\n", h.sum()));
    s.push_str(&format!("{name}_count {}\n", h.count()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_and_gauge() {
        let c = Counter::new();
        c.inc();
        c.add(4);
        assert_eq!(c.get(), 5);
        let g = Gauge::new();
        g.set(100);
        g.add(-30);
        assert_eq!(g.get(), 70);
    }

    #[test]
    fn histogram_cumulative_buckets() {
        let h = Histogram::new();
        h.observe(5); // ≤10
        h.observe(75); // ≤100
        h.observe(20000); // +Inf
        assert_eq!(h.count(), 3);
        assert_eq!(h.sum(), 5 + 75 + 20000);
        // le=10 counts 1 (the 5), le=100 counts 2 (5 + 75), +Inf counts 3.
        assert_eq!(h.buckets[0].load(Ordering::Relaxed), 1);
        assert_eq!(h.buckets[2].load(Ordering::Relaxed), 2);
        assert_eq!(h.buckets[6].load(Ordering::Relaxed), 3);
    }

    #[test]
    fn openmetrics_format() {
        let c = Counter::new();
        c.add(42);
        let out = render_counter("euroos_syscalls_total", "Number of syscalls", &c);
        assert!(out.contains("# TYPE euroos_syscalls_total counter\n"));
        assert!(out.contains("euroos_syscalls_total 42\n"));
        let g = Gauge::new();
        g.set(2048);
        assert!(render_gauge("euroos_free_pages", "Free pages", &g).contains("euroos_free_pages 2048\n"));
    }

    #[test]
    fn histogram_render_has_buckets_sum_count() {
        let h = Histogram::new();
        h.observe(30);
        let out = render_histogram("euroos_fs_read_us", "FS read latency", &h);
        assert!(out.contains("# TYPE euroos_fs_read_us histogram\n"));
        assert!(out.contains("euroos_fs_read_us_bucket{le=\"50\"} 1\n"));
        assert!(out.contains("euroos_fs_read_us_bucket{le=\"+Inf\"} 1\n"));
        assert!(out.contains("euroos_fs_read_us_count 1\n"));
        assert!(out.contains("euroos_fs_read_us_sum 30\n"));
    }
}
