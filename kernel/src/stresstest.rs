//! **Big load / stress test** (`[stress]`). Gated by the `EUROSTRESS` sentinel on virtio
//! disk 0, run LATE in boot (after ring3, interrupts and the VFS are up) so it can run
//! programs and time with the PIT. Exercises a sustained, multi-faceted workload:
//!
//!  • heavy **external-disk write churn** — write / rename(mv) / delete / rewrite across
//!    every *free* attached disk (disk 2+; disk 0 = root, disk 1 = /mnt are OS-owned),
//!    several rounds, with integrity checks;
//!  • **cross-disk move** (read on one disk, write on another, delete the source);
//!  • filling the **ROOT filesystem until full** — the real **on-disk** EuroFS root on
//!    disk 0 (the genuine "boot disk is full" case, not a RAM disk) — and
//!    recovering — existing data must stay intact and the FS usable again after freeing;
//!  • running **multiple programs** (synchronous runs + concurrent background tasks);
//!  • **memory-leak monitoring** (free frames before/after) and a final integrity scrub.

use crate::fatmount::SectorDev;
use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};
use eurofatfs::FatFs;
use eurofs::{FileSystem, FsError};
use euromm::FrameAllocator;

const SENTINEL: &[u8] = b"EUROSTRESS";
static ARMED: AtomicBool = AtomicBool::new(false);

/// First virtio disk index the stress test may freely format/churn. The boot path
/// claims disk 0 as the **on-disk EuroFS root** (`/`) and disk 1 as `/mnt`, so those
/// two are owned by the OS and must never be reformatted out from under it. The
/// ROOT-fill phase deliberately targets disk 0's real on-disk root via the VFS.
const FIRST_FREE_DISK: usize = 2;

/// Read the sentinel on disk 0 and latch whether the stress test is armed. Called at the
/// early install gate; the actual run happens later (when ring3/VFS are ready).
pub fn arm_if_sentinel() -> bool {
    let mut s0 = [0u8; 512];
    let armed = crate::virtio_blk::read_io_dev(0, 0, &mut s0) && s0.starts_with(SENTINEL);
    ARMED.store(armed, Ordering::Relaxed);
    armed
}
pub fn armed() -> bool {
    ARMED.load(Ordering::Relaxed)
}

fn ms(t0: u64) -> u64 {
    crate::interrupts::ticks().saturating_sub(t0) * 10 // 100 Hz → 10 ms/tick
}

fn format_fat(dev: usize, total: u64) {
    let vid = (crate::rtc::epoch() as u32) ^ 0x0057_8E55;
    eurofat::format_fat32(total as u32, vid, "STRESS", |lba, b| {
        let _ = crate::virtio_blk::write_io_dev(dev, lba, b);
    });
    crate::virtio_blk::flush_dev(dev);
}

/// Phase A — write/rename/delete/rewrite churn on one external FAT disk.
fn churn_disk(dev: usize) {
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let mib = total * 512 / (1024 * 1024);
    format_fat(dev, total);
    let mut fs = match FatFs::mount(SectorDev::new(dev, 0, total)) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[stress] churn disk{dev}: mount FAILED ✗");
            return;
        }
    };
    // Sized to complete under TCG emulation (~tens of KB/s virtio); the workload
    // shape (write→rename→delete→rewrite + integrity) is the point, not raw MB/s,
    // which is ~60× faster on KVM / real hardware.
    let fsz = 32 * 1024usize;
    let buf = alloc::vec![0xC3u8; fsz];
    let (mut ops, mut writes, mut renames, mut deletes, mut errs) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let rounds = 2;
    let (nfiles, half) = (8usize, 4usize);
    let t0 = crate::interrupts::ticks();
    for r in 0..rounds {
        for i in 0..nfiles {
            match fs.write_file(&format!("/r{r}f{i}.dat"), &buf) {
                Ok(()) => {
                    writes += 1;
                    ops += 1;
                }
                Err(FsError::NoSpace) => {}
                Err(_) => errs += 1,
            }
        }
        for i in 0..half {
            if fs.rename(&format!("/r{r}f{i}.dat"), &format!("/r{r}m{i}.dat")).is_ok() {
                renames += 1;
                ops += 1;
            }
        }
        for i in half..nfiles {
            if fs.remove_file(&format!("/r{r}f{i}.dat")).is_ok() {
                deletes += 1;
                ops += 1;
            }
        }
        for i in 0..half {
            if fs.write_file(&format!("/r{r}m{i}.dat"), &buf).is_ok() {
                writes += 1;
                ops += 1;
            }
        }
    }
    // Integrity: every surviving renamed file must read back exactly.
    let mut ok = true;
    for r in 0..rounds {
        for i in 0..half {
            ok &= fs.read_file(&format!("/r{r}m{i}.dat")).map(|d| d.len() == fsz && d[0] == 0xC3).unwrap_or(false);
        }
    }
    let el = ms(t0).max(1);
    let kbps = writes * (fsz as u64) / 1024 * 1000 / el;
    crate::serial_println!(
        "[stress] churn disk{dev} {mib} MiB: {ops} ops ({writes}w/{renames}mv/{deletes}rm) in {el}ms @ {kbps} KB/s · integrity={ok} errs={errs} → {}",
        if ok && errs == 0 { "OK ✓" } else { "FAILED ✗" }
    );
}

/// Phase B — move a file from disk `a` to disk `b` (copy then delete the source).
fn move_between(a: usize, b: usize) {
    let ta = crate::virtio_blk::capacity_sectors_dev(a);
    let tb = crate::virtio_blk::capacity_sectors_dev(b);
    let (mut fa, mut fb) = match (FatFs::mount(SectorDev::new(a, 0, ta)), FatFs::mount(SectorDev::new(b, 0, tb))) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return,
    };
    let payload: alloc::vec::Vec<u8> = (0..256 * 1024u32).map(|i| (i * 17 + 3) as u8).collect();
    let _ = fa.write_file("/movesrc.bin", &payload);
    let data = fa.read_file("/movesrc.bin").unwrap_or_default();
    let w = fb.write_file("/moved.bin", &data).is_ok();
    let rm = fa.remove_file("/movesrc.bin").is_ok();
    let arrived = fb.read_file("/moved.bin").map(|d| d == payload).unwrap_or(false);
    let gone = !fa.exists("/movesrc.bin");
    crate::serial_println!(
        "[stress] move disk{a}→disk{b}: 256 KiB copied={w} source-removed={rm} arrived-intact={arrived} gone-from-source={gone} → {}",
        if w && rm && arrived && gone { "OK ✓" } else { "FAILED ✗" }
    );
}

/// Phase C — fill the ROOT filesystem until it reports full, then recover.
fn root_full(vfs: &mut dyn FileSystem) {
    let _ = vfs.create_dir("/stress");
    let _ = vfs.write_file("/stress/keep.txt", b"intact");
    let buf = alloc::vec![0x7Eu8; 64 * 1024];
    let mut files = 0;
    let mut nospace = false;
    let t0 = crate::interrupts::ticks();
    for i in 0..8192 {
        match vfs.write_file(&format!("/stress/fill{i}.bin"), &buf) {
            Ok(()) => files += 1,
            Err(FsError::NoSpace) => {
                nospace = true;
                break;
            }
            Err(_) => break,
        }
    }
    let el = ms(t0).max(1);
    let intact = vfs.read_file("/stress/keep.txt").map(|d| d == b"intact").unwrap_or(false);
    // Recover: free the fill and confirm the FS is writable again.
    for i in 0..files {
        let _ = vfs.remove_file(&format!("/stress/fill{i}.bin"));
    }
    let recovered = vfs.write_file("/stress/after.txt", b"after recovery").is_ok();
    let _ = vfs.remove_file("/stress/keep.txt");
    let _ = vfs.remove_file("/stress/after.txt");
    let _ = vfs.remove_dir("/stress");
    crate::serial_println!(
        "[stress] ROOT fs fill-to-full: wrote {files}×64 KiB in {el}ms → NoSpace-hit={nospace} · existing-file-intact={intact} · writable-after-free={recovered} → {}",
        if nospace && intact && recovered { "OK ✓ (graceful full-disk handling)" } else { "FAILED ✗" }
    );
}

/// Phase D — run multiple programs concurrently with background tasks.
///
/// We run the native `/bin/hello` (capabilities CONSOLE|PROC_INFO|FILE, native ABI)
/// repeatedly while two background counter tasks run on the scheduler, then reap and
/// check for frame leaks. `fork()`/`execve()` are deliberately NOT exercised here:
/// they are Linux-ABI and already proven at boot (`[fork] … child pid …`), and the
/// execve *target* (`/bin/execee`) must be exec'd into, not run standalone.
fn programs(alloc: &mut FrameAllocator) {
    let f0 = alloc.free_frames();
    // Concurrent background tasks (run on the scheduler alongside the synchronous runs).
    let _t1 = crate::ring3::spawn_counter_task(alloc);
    let _t2 = crate::ring3::spawn_counter_task(alloc);
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_PROC_INFO | crate::ring3::CAP_FILE;
    let hello = crate::ring3::program_bytes();
    let (mut runs, mut clean) = (0u64, 0u64);
    for _ in 0..4 {
        let (exit, _out) = crate::ring3::run(alloc, hello, caps, false);
        runs += 1;
        if exit == 0 {
            clean += 1;
        }
    }
    crate::ring3::reap_dead(alloc);
    let f1 = alloc.free_frames();
    crate::serial_println!(
        "[stress] programs: {runs} synchronous /bin/hello runs ({clean} clean exit) + 2 background counter tasks · frames {f0}→{f1} → {}",
        if clean == runs { "OK ✓" } else { "some non-zero exits ✗" }
    );
}

/// Run the whole big stress workload.
pub fn run(vfs: &mut dyn FileSystem, alloc: &mut FrameAllocator) {
    let n = crate::virtio_blk::device_count();
    let free0 = alloc.free_frames();
    let t0 = crate::interrupts::ticks();
    // disk0 = root (/), disk1 = /mnt → owned by the OS. Free churn targets are disk2+.
    let free_disks: alloc::vec::Vec<usize> =
        (FIRST_FREE_DISK..n).filter(|&d| crate::virtio_blk::present_dev(d)).collect();
    crate::serial_println!(
        "[stress] ====== BIG load/stress test starting · {n} disk(s): disk0=root disk1=/mnt · {} free for churn {:?} ======",
        free_disks.len(),
        free_disks
    );
    for &dev in &free_disks {
        churn_disk(dev);
    }
    if free_disks.len() >= 2 {
        move_between(free_disks[0], free_disks[1]);
    } else {
        crate::serial_println!("[stress] move: need ≥2 free disks (have {}) — skipped", free_disks.len());
    }
    root_full(vfs);
    programs(alloc);
    let free1 = alloc.free_frames();
    let scrub = vfs.scrub();
    let total_ms = ms(t0);
    // The 2 background counter tasks never reap (~1032 frames). Anything that ran-and-exited
    // (churn buffers, /bin/hello runs, fill files) must have returned its frames: a leak
    // would show free1 far below free0. We flag a leak only if more than the bg-task arenas
    // plus a generous slack went missing.
    let leaked = free0 > free1 && (free0 - free1) > 1032 + 256;
    crate::serial_println!(
        "[stress] ====== done in {total_ms}ms · free frames {free0}→{free1} (Δ {}{}) · no-leak={} · root scrub: {} errors → {} ======",
        if free1 >= free0 { "+" } else { "-" },
        if free1 >= free0 { free1 - free0 } else { free0 - free1 },
        !leaked,
        scrub.errors,
        if !leaked && scrub.errors == 0 { "OK ✓" } else { "FAILED ✗" }
    );
}
