//! Live **deadman watchdog** (3G-2 wiring). The host-tested [`eurowatchdog::Watchdog`]
//! is now driven by the running system: the desktop main loop **pets** it every
//! iteration, and the 100 Hz **scheduler tick** (which runs even while the main
//! loop is busy) **checks** it. If the main loop hangs and stops petting, the
//! deadline passes and the watchdog trips — logged once from the timer IRQ (a
//! real reset would follow on hardware / via the i6300esb device).

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use eurowatchdog::Watchdog;
use spin::Mutex;

static WD: Mutex<Option<Watchdog>> = Mutex::new(None);
static TRIP_LOGGED: AtomicBool = AtomicBool::new(false);
/// The task that armed the watchdog: the desktop loop (BUG-280).
static MAIN_TASK: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Block requests of that task that completed and counted as a sign of life.
static IO_PETS: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    crate::interrupts::ticks()
}

/// Arm the watchdog with a `timeout_ticks` grace (100 Hz → ticks are ~10 ms).
pub fn arm(timeout_ticks: u64) {
    *WD.lock() = Some(Watchdog::new(timeout_ticks, now()));
    TRIP_LOGGED.store(false, Ordering::Relaxed);
    MAIN_TASK.store(crate::sched::current_lockfree(), Ordering::Relaxed);
}

/// Switch the watchdog off again (the boot self-test arms it for a moment).
pub fn disarm() {
    *WD.lock() = None;
    TRIP_LOGGED.store(false, Ordering::Relaxed);
    MAIN_TASK.store(usize::MAX, Ordering::Relaxed);
}

/// A block request of the running task completed (BUG-280). When that task is the
/// one that pets (the desktop loop), the loop is not hung but waiting on a slow
/// disk, one request after the other: a shell command that does hundreds of file
/// operations on a starved host disk (`churn fs`: 36 s) never gets back to the
/// top of the loop to pet, yet every one of its requests completes. Each completed
/// request is a sign of life. A request that does not complete still leaves the
/// loop without a pet, and the deadman trips after its 15 s as before; so does
/// any other task's disk traffic that does not belong to the loop (not counted).
pub fn io_progress() {
    // (deliberately empty until the self-test below has been seen to fail)
}

/// Completed block requests of the main task that counted (self-test / diagnostics).
pub fn io_pets() -> u64 {
    IO_PETS.load(Ordering::Relaxed)
}

/// Boot self-test (BUG-280): the main task's completed block requests are signs of
/// life for the deadman. Reads sector 0 four times and wants four more pets.
pub fn io_progress_selftest() {
    if !crate::virtio_blk::present() {
        crate::serial_println!("[wd-io] no virtio-blk disk: not run");
        return;
    }
    arm(1500);
    let before = pets();
    let mut buf = [0u8; 512];
    let mut ok = true;
    for _ in 0..4 {
        ok &= crate::virtio_blk::read_sector(0, &mut buf);
    }
    let gained = pets().saturating_sub(before);
    disarm();
    crate::serial_println!(
        "[wd-io] 4 block reads of the task that pets: reads ok={ok}, pets gained {gained} (want 4) -> {}",
        if ok && gained >= 4 { "OK (a completed request is a sign of life) ✓" } else { "FAILED" }
    );
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
