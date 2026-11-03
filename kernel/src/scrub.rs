//! G5: background data scrubber. A low-priority integrity pass over
//! EuroFS — superblock + structure + the **data-path XXH3 checksums** of every inode
//! — that detects silent bit-rot (and, where redundancy exists, repairs it). Runs
//! once at boot and then periodically/rate-limited from the desktop tick, and
//! reports to `/var/log/fsck.log` (on the real EuroVar partition, G4) + serial.
//! In spirit a `nice +19` task: non-blocking, self-reporting, background.

use core::sync::atomic::{AtomicU64, Ordering};
use eurofs::{FileSystem, ScrubReport};

static NEXT_SCRUB_TICK: AtomicU64 = AtomicU64::new(0);
static SCRUB_RUNS: AtomicU64 = AtomicU64::new(0);
/// Duration of the last pass, in ticks (10 ms).
static LAST_SCRUB_TICKS: AtomicU64 = AtomicU64::new(0);
/// ~60 s at 100 Hz — infrequent enough not to make the desktop stutter.
const INTERVAL_TICKS: u64 = 6000;
/// A pass runs synchronously in the desktop tick and reads every data block, so
/// its cost grows with the data stored: ~38 s with ~600 MiB on a virtio disk under
/// KVM (LOADTEST-R630 BUG-023), during which the desktop and the serial console
/// stand still. Until the scrub is incremental, the next pass waits at least
/// 100x as long as the last one took: the scrubber spends at most ~1 % of the
/// time, instead of most of it once a disk holds a few GiB.
const DUTY_FACTOR: u64 = 100;

/// Run one scrub pass, append the result to `/var/log/fsck.log`, and log a
/// summary to serial. Returns the report.
pub fn run(fs: &mut dyn FileSystem) -> ScrubReport {
    let t0 = crate::interrupts::ticks();
    let r = fs.scrub();
    let took = crate::interrupts::ticks().saturating_sub(t0);
    LAST_SCRUB_TICKS.store(took, Ordering::Relaxed);
    NEXT_SCRUB_TICK.store(t0 + took + INTERVAL_TICKS.max(took * DUTY_FACTOR), Ordering::Relaxed);
    let run = SCRUB_RUNS.fetch_add(1, Ordering::Relaxed) + 1;
    let line = alloc::format!(
        "scrub #{run}: {} inodes, {} data blocks, data verified {}, errors {}, unrecoverable {}, superblock {}, bitmap {}, took {} ms\n",
        r.objects,
        r.blocks_referenced,
        r.data_verified,
        r.errors,
        r.data_unrecoverable,
        if r.superblock_ok { "OK" } else { "FAIL" },
        if r.bitmap_ok { "OK" } else { "FAIL" },
        took * 10,
    );
    // Append to /var/log/fsck.log (read-modify-write; /var = real EuroVar partition).
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/log");
    let mut buf = fs.read_file("/var/log/fsck.log").unwrap_or_default();
    buf.extend_from_slice(line.as_bytes());
    let _ = fs.write_file("/var/log/fsck.log", &buf);
    crate::serial_println!("[g5] {}", line.trim_end());
    r
}

/// Periodic, rate-limited call from the desktop tick. Does nothing until the
/// interval (~60 s) since the previous pass has elapsed — this keeps the scrubber a
/// light background task instead of a blocking full scan every tick.
pub fn maybe_run(fs: &mut dyn FileSystem, now_ticks: u64) {
    let next = NEXT_SCRUB_TICK.load(Ordering::Relaxed);
    if next == 0 {
        // No pass yet: the first periodic one comes one interval from now.
        NEXT_SCRUB_TICK.store(now_ticks + INTERVAL_TICKS, Ordering::Relaxed);
    } else if now_ticks >= next {
        run(fs);
    }
}

/// How many scrub passes have run since boot (for the status panel/diagnostics).
pub fn runs() -> u64 {
    SCRUB_RUNS.load(Ordering::Relaxed)
}
