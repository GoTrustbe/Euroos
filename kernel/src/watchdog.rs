//! Live **deadman watchdog** (3G-2 wiring). The host-tested [`eurowatchdog::Watchdog`]
//! is now driven by the running system: the desktop main loop **pets** it every
//! iteration, and the 100 Hz **scheduler tick** (which runs even while the main
//! loop is busy) **checks** it. If the main loop hangs and stops petting, the
//! deadline passes and the watchdog trips — logged once from the timer IRQ (a
//! real reset would follow on hardware / via the i6300esb device).

use core::sync::atomic::{AtomicBool, Ordering};

use eurowatchdog::Watchdog;
use spin::Mutex;

static WD: Mutex<Option<Watchdog>> = Mutex::new(None);
static TRIP_LOGGED: AtomicBool = AtomicBool::new(false);

fn now() -> u64 {
    crate::interrupts::ticks()
}

/// Arm the watchdog with a `timeout_ticks` grace (100 Hz → ticks are ~10 ms).
pub fn arm(timeout_ticks: u64) {
    *WD.lock() = Some(Watchdog::new(timeout_ticks, now()));
    TRIP_LOGGED.store(false, Ordering::Relaxed);
}

/// The main loop is alive — extend the deadline. `try_lock` so it never blocks.
pub fn pet() {
    if let Some(mut g) = WD.try_lock() {
        if let Some(w) = g.as_mut() {
            w.pet(now());
        }
    }
}

/// Called from the 100 Hz scheduler tick (the independent checker): trips the
/// moment the deadline is missed. `try_lock` — a contended tick is simply
/// skipped (the next one checks). Logs the trip exactly once.
pub fn tick_check() {
    if let Some(mut g) = WD.try_lock() {
        if let Some(w) = g.as_mut() {
            if w.check(now()) && !TRIP_LOGGED.swap(true, Ordering::Relaxed) {
                // Deadman fired: the main loop stopped petting. Panic-safe raw write.
                crate::serial::write_raw(b"[3g2-wire] WATCHDOG TRIPPED - main loop hung; would reset (i6300esb) FAILED\n");
            }
        }
    }
}

/// Number of pets so far (liveness proof).
pub fn pets() -> u64 {
    WD.try_lock().and_then(|g| g.as_ref().map(|w| w.pets())).unwrap_or(0)
}

pub fn is_tripped() -> bool {
    WD.try_lock().and_then(|g| g.as_ref().map(|w| w.is_tripped())).unwrap_or(false)
}
