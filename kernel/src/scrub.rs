//! G5: achtergrond-data-scrubber. Een laag-prioritaire integriteits-pass over
//! EuroFS — superblok + structuur + de **data-path-XXH3-checksums** van elke inode
//! — die stille bit-rot detecteert (en, waar redundantie bestaat, herstelt). Draait
//! éénmaal bij boot en daarna periodiek/rate-limited vanuit de desktop-tick, en
//! rapporteert naar `/var/log/fsck.log` (op de echte EuroVar-partitie, G4) + serial.
//! In geest een `nice +19`-taak: niet-blokkerend, zelf-rapporterend, achtergrond.

use core::sync::atomic::{AtomicU64, Ordering};
use eurofs::{FileSystem, ScrubReport};

static LAST_SCRUB_TICK: AtomicU64 = AtomicU64::new(0);
static SCRUB_RUNS: AtomicU64 = AtomicU64::new(0);
/// ~60 s bij 100 Hz — infrequent genoeg om de desktop niet te haperen.
const INTERVAL_TICKS: u64 = 6000;

/// Voer één scrub-pass uit, hang het resultaat aan `/var/log/fsck.log` en log een
/// samenvatting naar serial. Geeft het rapport terug.
pub fn run(fs: &mut dyn FileSystem) -> ScrubReport {
    let r = fs.scrub();
    let run = SCRUB_RUNS.fetch_add(1, Ordering::Relaxed) + 1;
    let line = alloc::format!(
        "scrub #{run}: {} inodes, {} datablokken, data-geverifieerd {}, fouten {}, onherstelbaar {}, superblok {}, bitmap {}\n",
        r.objects,
        r.blocks_referenced,
        r.data_verified,
        r.errors,
        r.data_unrecoverable,
        if r.superblock_ok { "OK" } else { "FOUT" },
        if r.bitmap_ok { "OK" } else { "FOUT" },
    );
    // Append naar /var/log/fsck.log (read-modify-write; /var = echte EuroVar-partitie).
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/log");
    let mut buf = fs.read_file("/var/log/fsck.log").unwrap_or_default();
    buf.extend_from_slice(line.as_bytes());
    let _ = fs.write_file("/var/log/fsck.log", &buf);
    crate::serial_println!("[g5] {}", line.trim_end());
    r
}

/// Periodieke, rate-limited aanroep vanuit de desktop-tick. Doet niets tot het
/// interval (~60 s) sinds de vorige pass verstreken is — zo blijft de scrubber een
/// lichte achtergrondtaak i.p.v. een blokkerende volledige scan elke tick.
pub fn maybe_run(fs: &mut dyn FileSystem, now_ticks: u64) {
    let last = LAST_SCRUB_TICK.load(Ordering::Relaxed);
    if now_ticks.wrapping_sub(last) >= INTERVAL_TICKS {
        LAST_SCRUB_TICK.store(now_ticks, Ordering::Relaxed);
        run(fs);
    }
}

/// Hoeveel scrub-passes er sinds boot gedraaid hebben (voor het statuspaneel/diagnose).
pub fn runs() -> u64 {
    SCRUB_RUNS.load(Ordering::Relaxed)
}
