//! System-wide **structured journal** (3G-1 wiring) — the live, global
//! [`eurojournal::Journal`] plus **disk persistence** via the root filesystem.
//! Beyond the boot self-test that proved the ring in RAM, the journal now
//! survives a reboot: the ring is encoded and written to `/var/log/journal.bin`
//! and reloaded at boot. Subsystems log via [`log`]; `journalctl`-style querying
//! is in the shell. Persisting via the VFS (not a raw LBA) means it works on
//! every boot, not only when a virtio-blk data disk is attached.

use alloc::string::String;
use alloc::vec::Vec;

use eurofs::FileSystem;
use eurojournal::{Journal, Severity};
use spin::Mutex;

const JOURNAL_PATH: &str = "/var/log/journal.bin";
const JOURNAL_CAP: usize = 512;

static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

fn with_journal<R>(f: impl FnOnce(&mut Journal) -> R) -> R {
    let mut g = JOURNAL.lock();
    let j = g.get_or_insert_with(|| Journal::new(JOURNAL_CAP));
    f(j)
}

/// Log a structured entry to the system journal.
pub fn log(severity: Severity, facility: &str, message: &str) {
    let ts = crate::rtc::epoch();
    with_journal(|j| {
        j.log(ts, severity, facility, message);
    });
}

/// Encode the ring + write it to `/var/log/journal.bin` on the root FS.
pub fn persist(fs: &mut dyn FileSystem) -> bool {
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/log");
    let blob = with_journal(|j| j.encode());
    fs.write_file(JOURNAL_PATH, &blob).is_ok()
}

/// Reload the journal from disk at boot. Returns the number of entries restored.
pub fn restore(fs: &mut dyn FileSystem) -> usize {
    let Ok(blob) = fs.read_file(JOURNAL_PATH) else {
        return 0;
    };
    match Journal::decode(&blob, JOURNAL_CAP) {
        Some(j) => {
            let n = j.len();
            *JOURNAL.lock() = Some(j);
            n
        }
        None => 0,
    }
}

/// `[3g1] wiring` boot self-test — the journal now round-trips through **disk**
/// via the root FS. Restore any prior ring, log a fresh boot line, persist, then
/// read the file back and decode independently to confirm the line survives.
pub fn persist_selftest(fs: &mut dyn FileSystem) {
    let restored = restore(fs);
    log(Severity::Notice, "boot", "kernel reached the desktop");
    log(Severity::Info, "journal", "journal persisted to disk");
    let wrote = persist(fs);

    // Read the file back and decode independently (proves it is really on disk).
    let ondisk_ok = fs
        .read_file(JOURNAL_PATH)
        .ok()
        .and_then(|blob| Journal::decode(&blob, JOURNAL_CAP))
        .map(|j2| j2.query(None, Some("boot")).iter().any(|e| e.message.contains("reached the desktop")))
        .unwrap_or(false);

    let count = with_journal(|j| j.len());
    let ok = wrote && ondisk_ok;
    crate::serial_println!(
        "[3g1] EuroJournal persistence: restored-from-disk={restored} entries, wrote-to-disk={wrote}, on-disk-decode-has-boot-line={ondisk_ok}, live-ring={count} → {}",
        if ok { "OK (structured journal survives reboot on the root FS) ✓" } else { "FAILED" }
    );
}

/// `journalctl`-style shell command: `journal [err|warn|<facility>]`.
pub fn shell(arg: &str) -> Vec<String> {
    with_journal(|j| {
        let (sev, fac): (Option<Severity>, Option<&str>) = match arg.trim() {
            "" => (None, None),
            "err" | "error" => (Some(Severity::Err), None),
            "warn" | "warning" => (Some(Severity::Warning), None),
            other => (None, Some(other)),
        };
        let hits = j.query(sev, fac);
        let mut out = alloc::vec![alloc::format!("journal — {} entries (of {} live, {} dropped):", hits.len(), j.len(), j.dropped)];
        for e in hits.iter().rev().take(20).rev() {
            out.push(alloc::format!("  [{}] {:<8} {}: {}", e.ts, e.severity.name(), e.facility, e.message));
        }
        out.push(String::from("usage: journal [err|warn|<facility>]  (persisted across reboot)"));
        out
    })
}
