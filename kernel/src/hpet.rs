//! HPET — High Precision Event Timer (Sprint S8 / Missing §16.2). Een nauwkeurige,
//! vrij-lopende teller (typisch 100 MHz) als HAL-tijdbron náást de RTC (wandklok)
//! en de APIC-timer (scheduling). Bruikbaar voor hoge-resolutie-metingen — o.a. de
//! SPERF-profilering (boot-fase- en frame-timing) en precieze delays.
//!
//! MMIO-registers op de standaard-base 0xFED0_0000 (identity-mapped supervisor):
//!   0x00 CAP        — bits[63:32] = klokperiode in femtoseconden/tick
//!   0x10 GEN_CONFIG — bit0 = ENABLE_CNF (hoofdteller aan)
//!   0xF0 MAIN_CNT   — 64-bit vrij-lopende teller

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const HPET_BASE: u64 = 0xFED0_0000;

static PERIOD_FS: AtomicU64 = AtomicU64::new(0); // femtoseconden per tick
static PRESENT: AtomicBool = AtomicBool::new(false);

#[inline]
fn reg(off: u64) -> *mut u64 {
    (HPET_BASE + off) as *mut u64
}

/// Detecteer + activeer de HPET. Geeft true als er een geldige HPET aanwezig is.
pub fn init() -> bool {
    let cap = unsafe { reg(0x00).read_volatile() };
    let period_fs = (cap >> 32) & 0xFFFF_FFFF;
    // Geldige HPET-periode: 1 fs .. 100 ns (100_000_000 fs). Anders geen/kapotte HPET.
    if period_fs == 0 || period_fs > 100_000_000 {
        return false;
    }
    unsafe {
        // Hoofdteller inschakelen (ENABLE_CNF).
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

/// Ruwe hoofdteller (HPET-ticks sinds inschakeling).
pub fn counter() -> u64 {
    if !present() {
        return 0;
    }
    unsafe { reg(0xF0).read_volatile() }
}

/// Klokfrequentie in Hz (10^15 fs/s ÷ periode).
pub fn freq_hz() -> u64 {
    let p = PERIOD_FS.load(Ordering::Relaxed);
    if p == 0 {
        0
    } else {
        1_000_000_000_000_000 / p
    }
}

/// Verstreken nanoseconden sinds inschakeling (ticks × periode_fs ÷ 10^6).
pub fn ns() -> u64 {
    let p = PERIOD_FS.load(Ordering::Relaxed);
    counter().saturating_mul(p) / 1_000_000
}

/// Verstreken microseconden.
pub fn us() -> u64 {
    ns() / 1_000
}
