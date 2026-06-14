//! **Multi-disk load + functional test** (`[mdisk]`). Exercises every attached virtio
//! disk: format → fill (up to a cap, or until full) → verify → delete → reformat, plus
//! a cross-disk copy. Reports timing/throughput (RTC wall-clock seconds — this runs
//! before interrupts are enabled, so the PIT tick is frozen) so we can see how multiple
//! disks of different sizes actually behave before claiming we support them.
//!
//! Destructive — only runs when the harness writes the `EURODISKTEST` sentinel to the
//! first virtio disk's sector 0 (so it never touches the install/update harness disks).

use crate::fatmount::SectorDev;
use alloc::format;
use eurofatfs::FatFs;
use eurofs::FileSystem;

const SENTINEL: &[u8] = b"EURODISKTEST";
const FILE_SZ: usize = 256 * 1024; // 256 KiB per file
const FILL_CAP: u64 = 8 * 1024 * 1024; // cap the fill at 8 MiB/disk (bounds TCG time)
const MAX_FILES: usize = 4096;

// This test runs early in boot, BEFORE interrupts are enabled, so the PIT tick counter
// is frozen — we time with the RTC instead (wall-clock seconds; in TCG that is ~60× the
// native rate, so throughput numbers are guest wall-clock, not native).
fn now_s() -> u64 {
    crate::rtc::epoch()
}
fn dur(t0: u64) -> u64 {
    now_s().saturating_sub(t0)
}
/// Throughput in KiB/s over a wall-clock interval (0 s → reported as "<1s").
fn rate_kib_s(bytes: u64, secs: u64) -> u64 {
    if secs == 0 {
        0
    } else {
        bytes / 1024 / secs
    }
}

/// Is the multi-disk test armed (sentinel on virtio disk 0)?
pub fn armed() -> bool {
    let mut s0 = [0u8; 512];
    crate::virtio_blk::read_io_dev(0, 0, &mut s0) && s0.starts_with(SENTINEL)
}

fn format(dev: usize, total: u64, label: &str) {
    let vid = (crate::rtc::epoch() as u32) ^ 0x10AD_7E57;
    eurofat::format_fat32(total as u32, vid, label, |lba, bytes| {
        let _ = crate::virtio_blk::write_io_dev(dev, lba, bytes);
    });
    crate::virtio_blk::flush_dev(dev);
}

fn test_disk(dev: usize) {
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let mib = total * 512 / (1024 * 1024);

    // ── FORMAT ──
    let t0 = now_s();
    format(dev, total, "LOADTEST");
    let fmt_s = dur(t0);

    let mut fs = match FatFs::mount(SectorDev::new(dev, 0, total)) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[mdisk] disk{dev} {mib} MiB: mount after format FAILED ✗");
            return;
        }
    };

    // ── FILL (up to FILL_CAP, or until the disk reports full) ──
    let disk_bytes = total * 512;
    // Fill until the disk reports NoSpace (small disks → true "full" test) or the cap
    // (large disks, to bound TCG time).
    let cap = FILL_CAP.min(disk_bytes);
    let buf = alloc::vec![0xA5u8; FILE_SZ];
    let mut written = 0u64;
    let mut files = 0usize;
    let mut full = false;
    let t1 = now_s();
    while written < cap && files < MAX_FILES {
        let path = format!("/f{files}.dat");
        match fs.write_file(&path, &buf) {
            Ok(()) => {
                written += FILE_SZ as u64;
                files += 1;
            }
            Err(eurofs::FsError::NoSpace) => {
                full = true;
                break;
            }
            Err(_) => break,
        }
    }
    let fill_s = dur(t1);
    let thru = rate_kib_s(written, fill_s);

    // ── VERIFY (read back the first and last file) ──
    let v_first = fs.read_file("/f0.dat").map(|d| d.len() == FILE_SZ && d[0] == 0xA5).unwrap_or(false);
    let last = files.saturating_sub(1);
    let v_last = files > 0
        && fs.read_file(&format!("/f{last}.dat")).map(|d| d.len() == FILE_SZ).unwrap_or(false);

    // ── DELETE everything ──
    let t2 = now_s();
    for i in 0..files {
        let _ = fs.remove_file(&format!("/f{i}.dat"));
    }
    let del_s = dur(t2);
    let empty = fs.list_dir("/").map(|e| e.is_empty()).unwrap_or(false);

    // ── REFORMAT + remount + verify empty ──
    drop(fs);
    format(dev, total, "RELOAD");
    let re_ok = FatFs::mount(SectorDev::new(dev, 0, total))
        .map(|f| f.list_dir("/").map(|e| e.is_empty()).unwrap_or(false))
        .unwrap_or(false);

    crate::serial_println!(
        "[mdisk] disk{dev} {mib} MiB: format={fmt_s}s · fill={} MiB/{files} files ({}) in {fill_s}s @ {thru} KiB/s wall · verify(first={v_first},last={v_last}) · delete={del_s}s empty={empty} · reformat+empty={re_ok} → {}",
        written / (1024 * 1024),
        if full { "DISK FULL/NoSpace" } else { "capped" },
        if v_first && v_last && empty && re_ok { "OK ✓" } else { "FAILED ✗" }
    );
}

/// Copy a 1 MiB file from disk `a` to disk `b` (read on one, write on the other) and
/// verify byte-equality — the "copy/paste between disks" case.
fn copy_between(a: usize, b: usize) {
    let ta = crate::virtio_blk::capacity_sectors_dev(a);
    let tb = crate::virtio_blk::capacity_sectors_dev(b);
    // Both were reformatted empty by test_disk; mount them fresh.
    let mut fa = match FatFs::mount(SectorDev::new(a, 0, ta)) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut fb = match FatFs::mount(SectorDev::new(b, 0, tb)) {
        Ok(f) => f,
        Err(_) => return,
    };
    let payload: alloc::vec::Vec<u8> = (0..1024 * 1024u32).map(|i| (i * 31 + 7) as u8).collect();
    let t0 = now_s();
    let w = fa.write_file("/src.bin", &payload).is_ok();
    let read_back = fa.read_file("/src.bin").unwrap_or_default();
    let w2 = fb.write_file("/copy.bin", &read_back).is_ok();
    let dst = fb.read_file("/copy.bin").unwrap_or_default();
    let s_el = dur(t0);
    let ok = w && w2 && dst == payload;
    crate::serial_println!(
        "[mdisk] copy disk{a}→disk{b}: 1 MiB written+read+copied+verified in {s_el}s wall ({} KiB/s) → {}",
        rate_kib_s(1024 * 1024, s_el),
        if ok { "OK ✓" } else { "FAILED ✗" }
    );
}

/// Run the whole multi-disk load + functional sweep over every attached virtio disk.
pub fn run() {
    let n = crate::virtio_blk::device_count();
    crate::serial_println!("[mdisk] === multi-disk load+functional test: {n} virtio disk(s) ===");
    let mut tested = 0;
    for dev in 0..n {
        if crate::virtio_blk::present_dev(dev) {
            test_disk(dev);
            tested += 1;
        }
    }
    if tested >= 2 {
        copy_between(0, 1);
    }
    crate::serial_println!("[mdisk] === done ({tested} disk(s) tested) ===");
}
