//! HPET — High Precision Event Timer (Sprint S8 / Missing §16.2). An accurate,
//! free-running counter (typically 100 MHz) as a HAL time source alongside the RTC
//! (wall clock) and the APIC timer (scheduling). Usable for high-resolution
//! measurements — among others the SPERF profiling (boot-phase and frame timing) and
//! precise delays.
//!
//! MMIO registers at the standard base 0xFED0_0000 (identity-mapped supervisor):
//!   0x00 CAP        — bits[63:32] = clock period in femtoseconds/tick
//!   0x10 GEN_CONFIG — bit0 = ENABLE_CNF (main counter on)
//!   0xF0 MAIN_CNT   — 64-bit free-running counter

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const HPET_BASE: u64 = 0xFED0_0000;

static PERIOD_FS: AtomicU64 = AtomicU64::new(0); // femtoseconds per tick
static PRESENT: AtomicBool = AtomicBool::new(false);

#[inline]
fn reg(off: u64) -> *mut u64 {
    (HPET_BASE + off) as *mut u64
}

/// Detect + activate the HPET. Returns true if a valid HPET is present.
pub fn init() -> bool {
    let cap = unsafe { reg(0x00).read_volatile() };
    let period_fs = (cap >> 32) & 0xFFFF_FFFF;
    // Valid HPET period: 1 fs .. 100 ns (100_000_000 fs). Otherwise no/broken HPET.
    if period_fs == 0 || period_fs > 100_000_000 {
        return false;
    }
    unsafe {
        // Enable the main counter (ENABLE_CNF).
        let cfg = reg(0x10);
        cfg.write_volatile(cfg.read_volatile() | 1);
    }
    PERIOD_FS.store(period_fs, Ordering::Relaxed);
    PRESENT.store(true, Ordering::Relaxed);
    true
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Raw main counter (HPET ticks since being enabled).
pub fn counter() -> u64 {
    if !present() {
        return 0;
    }
    unsafe { reg(0xF0).read_volatile() }
}

/// Clock frequency in Hz (10^15 fs/s ÷ period).
pub fn freq_hz() -> u64 {
    let p = PERIOD_FS.load(Ordering::Relaxed);
    if p == 0 {
        0
    } else {
        1_000_000_000_000_000 / p
    }
}

/// Elapsed nanoseconds since being enabled (ticks × period_fs ÷ 10^6).
pub fn ns() -> u64 {
    let p = PERIOD_FS.load(Ordering::Relaxed);
    counter().saturating_mul(p) / 1_000_000
}

/// Elapsed microseconds.
pub fn us() -> u64 {
    ns() / 1_000
}
