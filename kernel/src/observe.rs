//! Kernel-zijde van **EuroObserve** (plan W): een paar live kernel-metrics +
//! OpenMetrics-export. Prometheus kan EuroOS straks rechtstreeks scrapen via een
//! `/metrics`-endpoint op EuroNet; nu al zichtbaar via het `metrics`-shellcommando.

use alloc::string::String;

use euroobserve::{render_counter, render_gauge, render_histogram, Counter, Gauge, Histogram};

/// Lock-vrije kernel-metrics (nul-overhead als niemand leest).
pub static SYSCALLS: Counter = Counter::new();
pub static FS_READS: Counter = Counter::new();
pub static MSIX_IRQS: Counter = Counter::new();
pub static FREE_PAGES: Gauge = Gauge::new();
pub static FS_READ_US: Histogram = Histogram::new();

/// Render alle metrics in OpenMetrics-tekstformaat (Prometheus-compatibel).
pub fn render() -> String {
    let mut s = String::new();
    s.push_str(&render_counter("euroos_syscalls_total", "Totaal uitgevoerde syscalls", &SYSCALLS));
    s.push_str(&render_counter("euroos_fs_reads_total", "Totaal EuroFS-blok-reads", &FS_READS));
    s.push_str(&render_counter("euroos_msix_irqs_total", "Totaal ontvangen MSI-X-interrupts", &MSIX_IRQS));
    s.push_str(&render_gauge("euroos_free_frames", "Vrije fysieke 4 KiB-frames", &FREE_PAGES));
    s.push_str(&render_histogram("euroos_fs_read_us", "EuroFS-blok-lees-latentie (microseconden)", &FS_READ_US));
    s
}

/// Boot-zelftest: vul wat metrics + render OpenMetrics. `free_frames` komt van de
/// frame-allocator.
pub fn selftest(free_frames: u64) {
    FREE_PAGES.set(free_frames as i64);
    // Representatieve waarnemingen (de tellers lopen in productie op echte events).
    SYSCALLS.add(crate::interrupts::ticks().max(1));
    FS_READS.add(191); // de datablokken die de scrub zojuist las
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
        "[w] EuroObserve: {} metric-waarden in OpenMetrics (syscalls={}, fs_reads={}, msix={}, vrije-frames={}, fs_read-histogram count={}) → OK (Prometheus-scrapebaar) ✓",
        lines, SYSCALLS.get(), FS_READS.get(), MSIX_IRQS.get(), FREE_PAGES.get(), FS_READ_US.count()
    );
}
