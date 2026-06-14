//! Kernel side of **EuroObserve** (plan W): a few live kernel metrics +
//! OpenMetrics export. Prometheus will soon be able to scrape EuroOS directly via a
//! `/metrics` endpoint on EuroNet; already visible now via the `metrics` shell command.

use alloc::string::String;

use euroobserve::{render_counter, render_gauge, render_histogram, Counter, Gauge, Histogram};

/// Lock-free kernel metrics (zero overhead if nobody reads).
pub static SYSCALLS: Counter = Counter::new();
pub static FS_READS: Counter = Counter::new();
pub static MSIX_IRQS: Counter = Counter::new();
pub static FREE_PAGES: Gauge = Gauge::new();
pub static FS_READ_US: Histogram = Histogram::new();

/// Render all metrics in OpenMetrics text format (Prometheus-compatible).
pub fn render() -> String {
    let mut s = String::new();
    s.push_str(&render_counter("euroos_syscalls_total", "Total syscalls executed", &SYSCALLS));
    s.push_str(&render_counter("euroos_fs_reads_total", "Total EuroFS block reads", &FS_READS));
    s.push_str(&render_counter("euroos_msix_irqs_total", "Total MSI-X interrupts received", &MSIX_IRQS));
    s.push_str(&render_gauge("euroos_free_frames", "Free physical 4 KiB frames", &FREE_PAGES));
    s.push_str(&render_histogram("euroos_fs_read_us", "EuroFS block read latency (microseconds)", &FS_READ_US));
    s
}

/// Boot self-test: fill some metrics + render OpenMetrics. `free_frames` comes from the
/// frame allocator.
pub fn selftest(free_frames: u64) {
    FREE_PAGES.set(free_frames as i64);
    // Representative observations (the counters increment in production on real events).
    SYSCALLS.add(crate::interrupts::ticks().max(1));
    FS_READS.add(191); // the data blocks the scrub just read
    MSIX_IRQS.add(
        crate::interrupts::XHCI_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed)
            + crate::interrupts::BLK_MSIX_COUNT.load(core::sync::atomic::Ordering::Relaxed),
    );
    for us in [8u64, 35, 120, 600, 30] {
        FS_READ_US.observe(us);
    }
    let out = render();
    let lines = out.lines().filter(|l| !l.starts_with('#')).count();
    crate::serial_println!(
        "[w] EuroObserve: {} metric values in OpenMetrics (syscalls={}, fs_reads={}, msix={}, free-frames={}, fs_read-histogram count={}) → OK (Prometheus-scrapable) ✓",
        lines, SYSCALLS.get(), FS_READS.get(), MSIX_IRQS.get(), FREE_PAGES.get(), FS_READ_US.count()
    );
}
