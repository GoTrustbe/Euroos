//! The block layer over `megaraidd`: one `eurofs::BlockDevice` per logical
//! drive, requests as `euromegaraid::blkif` messages over EuroIPC.
//!
//! This is the thin coupling layer (docs/SPRINT-PLAN-MEGARAID.md): it knows the
//! slot geometry and the message format, nothing about MFI. Swapping it for a
//! shared block-over-IPC interface leaves `udrv_mr.rs`, the driver and the
//! MFI logic alone.
//!
//! A transfer is cut into chunks of at most one data slot (64 KiB); up to
//! `SLOTS` chunks are in flight at once. Write data is copied into the slot
//! before the request is sent, read data out of it after `Done`. When the
//! driver dies with requests in flight, the chunks that were not acknowledged
//! are sent again to the next instance (write data copied in again from the
//! caller's buffer): block writes are idempotent, and the new instance has
//! reset the controller and drained it (a cache flush) before it says `Ready`.
//! With replay switched off (one self-test does that) the caller gets an I/O
//! error instead, and the filesystem above has to cope on its own.
//!
//! Waiting needs the CPU to go to the driver: task context with interrupts on.
//! A caller with interrupts off (a syscall path) gets an I/O error rather than
//! a hang. Only task 0 supervises (restarts) from inside a wait.
//!
//! **Not the block interface of nvmed** (`udrv_blk.rs`, `eurodrv::proto`
//! `BlkInfo`/`BlkRead`/`BlkWrite`/`BlkDone`). nvmed carries the data inside
//! the messages (1 KiB per message, one request in flight, one disk);
//! megaraidd keeps it in data slots of its DMA window (64 KiB per request, up
//! to `SLOTS` in flight, several logical drives, an explicit `Flush`, chunks
//! replayed to the next instance after a crash). Both end up as a EuroFS
//! `BlockDevice`, which is where the kernel sees them alike. Moving one onto
//! the other's wire format would cost megaraidd its multi-disk, flush and
//! replay paths, or nvmed its proven DMA layout; the shared interface is
//! left for a later sprint (docs/SPRINT-PLAN-USERSPACE-STORAGE.md).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use eurofs::{BlockDevice, BlockError, BlockResult};
use euromegaraid::blkif::{Io, Msg};
use euromegaraid::layout::{SLOTS, SLOT_BYTES};
use spin::Mutex;

use crate::udrv_mr as mr;

const SECTOR: usize = 512;
const SLOT_SECTORS: u64 = SLOT_BYTES as u64 / SECTOR as u64;
/// A transfer that makes no progress for this long fails (ticks of 10 ms).
const NO_PROGRESS_TICKS: u64 = 3000;

static NEXT_TAG: AtomicU32 = AtomicU32::new(1);
static REPLAY: AtomicBool = AtomicBool::new(true);
static REPLAYED: AtomicU64 = AtomicU64::new(0);
static IO_ERRORS: AtomicU64 = AtomicU64::new(0);
static REQUESTS: AtomicU64 = AtomicU64::new(0);
/// One transfer at a time (the slots are shared).
static IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
    Flush,
}

#[derive(Clone, Copy)]
struct Chunk {
    off: usize,
    lba: u64,
    sectors: u32,
    tag: u32,
    epoch: u64,
    addr: u64,
    sent: bool,
    done: bool,
}

enum Buf<'a> {
    R(&'a mut [u8]),
    W(&'a [u8]),
    None,
}

fn now() -> u64 {
    crate::interrupts::ticks()
}

/// Run one batch (at most SLOTS chunks) to completion.
fn run_batch(disk: usize, kind: Kind, chunks: &mut [Chunk], buf: &mut Buf) -> BlockResult<()> {
    let mut last_progress = now();
    loop {
        if !x86_64::instructions::interrupts::are_enabled() {
            return Err(BlockError::IoError); // the driver could never run
        }
        mr::pump();
        for c in chunks.iter_mut().filter(|c| c.sent && !c.done) {
            if let Some((status, scsi)) = mr::take_done(c.tag) {
                if status != 0 {
                    IO_ERRORS.fetch_add(1, Ordering::Relaxed);
                    crate::serial_println!(
                        "[mrblk] LD{disk} {} lba {} x{}: '{}' (status {status:#x}, scsi {scsi:#x})",
                        match kind { Kind::Read => "read", Kind::Write => "write", Kind::Flush => "flush" },
                        c.lba, c.sectors, euromegaraid::frame::status_name(status)
                    );
                    return Err(BlockError::IoError);
                }
                if let (Kind::Read, Buf::R(b)) = (kind, &mut *buf) {
                    let n = c.sectors as usize * SECTOR;
                    // SAFETY: c.addr is a data slot of a driver window the kernel
                    // allocated (identity-mapped; kept in quarantine if the driver
                    // died meanwhile), n <= SLOT_BYTES.
                    unsafe { core::ptr::copy_nonoverlapping(c.addr as *const u8, b[c.off..c.off + n].as_mut_ptr(), n) };
                }
                c.done = true;
                last_progress = now();
            }
        }
        if chunks.iter().all(|c| c.done) {
            return Ok(());
        }
        let ep = mr::epoch();
        let lost = chunks.iter().any(|c| c.sent && !c.done && (c.epoch != ep || !mr::ready()));
        if lost && !REPLAY.load(Ordering::Relaxed) {
            IO_ERRORS.fetch_add(1, Ordering::Relaxed);
            return Err(BlockError::IoError);
        }
        if mr::ready() {
            if let Some(lay) = mr::layout_now() {
                for (slot, c) in chunks.iter_mut().enumerate() {
                    if c.done || (c.sent && c.epoch == ep) {
                        continue;
                    }
                    if c.sent {
                        REPLAYED.fetch_add(1, Ordering::Relaxed); // its driver died before Done
                    }
                    let addr = lay.slot(slot).ok_or(BlockError::IoError)?;
                    if let (Kind::Write, Buf::W(b)) = (kind, &*buf) {
                        let n = c.sectors as usize * SECTOR;
                        // SAFETY: a data slot of the current driver window (identity-mapped), n <= SLOT_BYTES.
                        unsafe { core::ptr::copy_nonoverlapping(b[c.off..c.off + n].as_ptr(), addr as *mut u8, n) };
                    }
                    c.tag = NEXT_TAG.fetch_add(1, Ordering::Relaxed);
                    c.epoch = ep;
                    c.addr = addr;
                    let io = Io { tag: c.tag, disk: disk as u8, slot: slot as u8, lba: c.lba, sectors: c.sectors };
                    let m = match kind {
                        Kind::Read => Msg::Read(io),
                        Kind::Write => Msg::Write(io),
                        Kind::Flush => Msg::Flush { tag: c.tag },
                    };
                    c.sent = mr::send_request(&m);
                    if c.sent {
                        REQUESTS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        if now().saturating_sub(last_progress) > NO_PROGRESS_TICKS {
            IO_ERRORS.fetch_add(1, Ordering::Relaxed);
            crate::serial_println!("[mrblk] LD{disk}: no progress for {} s: I/O error", NO_PROGRESS_TICKS / 100);
            return Err(BlockError::IoError);
        }
        mr::supervise_from_wait();
        crate::sched::yield_now();
        if !mr::has_done() {
            x86_64::instructions::hlt();
        }
    }
}

fn transfer(disk: usize, kind: Kind, lba: u64, sectors: u64, mut buf: Buf) -> BlockResult<()> {
    let d = mr::disk(disk).ok_or(BlockError::IoError)?;
    if kind != Kind::Flush && lba.checked_add(sectors).is_none_or(|e| e > d.sectors) {
        return Err(BlockError::OutOfBounds);
    }
    let _g = IO_LOCK.lock();
    let mut chunks = Vec::new();
    if kind == Kind::Flush {
        chunks.push(Chunk { off: 0, lba: 0, sectors: 0, tag: 0, epoch: 0, addr: 0, sent: false, done: false });
    }
    let mut s = 0u64;
    while s < sectors {
        let n = (sectors - s).min(SLOT_SECTORS);
        chunks.push(Chunk { off: s as usize * SECTOR, lba: lba + s, sectors: n as u32, tag: 0, epoch: 0, addr: 0, sent: false, done: false });
        s += n;
    }
    for batch in chunks.chunks_mut(SLOTS) {
        run_batch(disk, kind, batch, &mut buf)?;
    }
    Ok(())
}

/// Read `buf.len() / 512` sectors at `lba` of logical drive `disk`.
pub fn read(disk: usize, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
    if buf.len() % SECTOR != 0 {
        return Err(BlockError::NotAligned);
    }
    transfer(disk, Kind::Read, lba, (buf.len() / SECTOR) as u64, Buf::R(buf))
}

/// Write `buf.len() / 512` sectors at `lba` of logical drive `disk`.
pub fn write(disk: usize, lba: u64, buf: &[u8]) -> BlockResult<()> {
    if buf.len() % SECTOR != 0 {
        return Err(BlockError::NotAligned);
    }
    transfer(disk, Kind::Write, lba, (buf.len() / SECTOR) as u64, Buf::W(buf))
}

/// Controller and disk cache flush (all logical drives of the controller).
pub fn flush(disk: usize) -> BlockResult<()> {
    transfer(disk, Kind::Flush, 0, 0, Buf::None)
}

/// EuroFS `BlockDevice` over one logical drive (4 KiB blocks = 8 sectors).
#[derive(Clone, Copy)]
pub struct MrBlock {
    disk: usize,
    blocks: u64,
}

impl MrBlock {
    pub fn new(disk: usize) -> Option<MrBlock> {
        let d = mr::disk(disk)?;
        Some(MrBlock { disk, blocks: d.sectors / 8 })
    }
}

impl BlockDevice for MrBlock {
    fn block_size(&self) -> u32 {
        4096
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        if buf.len() != count as usize * 4096 {
            return Err(BlockError::NotAligned);
        }
        if start.checked_add(count as u64).is_none_or(|e| e > self.blocks) {
            return Err(BlockError::OutOfBounds);
        }
        read(self.disk, start * 8, buf)
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        if buf.len() != count as usize * 4096 {
            return Err(BlockError::NotAligned);
        }
        if start.checked_add(count as u64).is_none_or(|e| e > self.blocks) {
            return Err(BlockError::OutOfBounds);
        }
        write(self.disk, start * 8, buf)
    }
    fn flush(&mut self) -> BlockResult<()> {
        flush(self.disk)
    }
}

pub fn stats_line() -> String {
    format!(
        "{} request(s), {} replayed after a driver death, {} I/O error(s)",
        REQUESTS.load(Ordering::Relaxed),
        REPLAYED.load(Ordering::Relaxed),
        IO_ERRORS.load(Ordering::Relaxed)
    )
}

// ── Self-tests (destructive only on blank or our own test disks) ────────────

/// Sector 0 of LD0 once it is ours: the self-test writes nowhere else on a
/// disk that does not carry this mark (or is not entirely blank where it writes).
const MARK: &[u8] = b"EUROOS-MEGARAID-SELFTEST-LD-V1\0";
/// The raw pattern region on LD0 (sectors), also checked from the host side.
pub const PATTERN_LBA: u64 = 4096;
pub const PATTERN_BYTES: usize = 1 << 20;

/// Byte `i` of the raw test pattern (`scripts/megaraid-proof.sh` recomputes it on the host).
pub fn pattern_byte(i: usize) -> u8 {
    (((i as u32).wrapping_mul(2_654_435_761) >> 13) as u8) ^ ((i >> 16) as u8)
}

fn gen_pattern(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Is LD0 ours to write? (marked, or blank where the test writes)
fn claim_ld0() -> Result<bool, &'static str> {
    let mut s0 = [0u8; SECTOR];
    read(0, 0, &mut s0).map_err(|_| "read of sector 0 failed")?;
    if s0.starts_with(MARK) {
        return Ok(false);
    }
    let mut region = vec![0u8; PATTERN_BYTES];
    read(0, PATTERN_LBA, &mut region).map_err(|_| "read of the test region failed")?;
    if s0.iter().any(|&b| b != 0) || region.iter().any(|&b| b != 0) {
        return Err("holds data and no self-test mark: left untouched");
    }
    let mut m = [0u8; SECTOR];
    m[..MARK.len()].copy_from_slice(MARK);
    write(0, 0, &m).map_err(|_| "write of the mark failed")?;
    Ok(true)
}

/// [mr-io]: raw write/read with data verification on LD0.
#[cfg(feature = "selftest")]
pub fn selftest_io(_falloc: &mut euromm::FrameAllocator) {
    let Some(d) = mr::disk(0) else {
        crate::serial_println!("[mr-io] no logical drive 0 → FAILED");
        return;
    };
    let fresh = match claim_ld0() {
        Ok(f) => f,
        Err(e) => {
            crate::serial_println!("[mr-io] LD0 {e} ⬜");
            return;
        }
    };
    // One sector, then 1 MiB = 16 chunks through all 4 slots at once.
    let mut one = [0u8; SECTOR];
    for (i, b) in one.iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    let mut one_rd = [0u8; SECTOR];
    let ok1 = write(0, 2048, &one).is_ok() && read(0, 2048, &mut one_rd).is_ok() && one == one_rd;
    let big: Vec<u8> = (0..PATTERN_BYTES).map(pattern_byte).collect();
    let t0 = now();
    let wok = write(0, PATTERN_LBA, &big).is_ok();
    let t1 = now();
    let mut back = vec![0u8; PATTERN_BYTES];
    let rok = read(0, PATTERN_LBA, &mut back).is_ok();
    let t2 = now();
    let ok2 = wok && rok && back == big;
    let fok = flush(0).is_ok();
    // A request past the end of the drive is refused (driver-side validation).
    let mut x = [0u8; SECTOR];
    let past = transfer(0, Kind::Read, d.sectors, 1, Buf::R(&mut x)).is_err();
    let ok = ok1 && ok2 && fok && past;
    crate::serial_println!(
        "[mr-io] LD0 ({}): 1 sector write/read {} · 1 MiB at LBA {PATTERN_LBA} written in {} ms, read back in {} ms, {} · cache flush {} · read past the end refused={past} → {}",
        if fresh { "blank, marked for the self-test" } else { "self-test mark from an earlier boot" },
        if ok1 { "equal" } else { "MISMATCH" },
        (t1 - t0) * 10,
        (t2 - t1) * 10,
        if ok2 { "byte-for-byte equal" } else { "MISMATCH" },
        if fok { "ok" } else { "FAILED" },
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// After the DMA probes: does the driver still serve a read? (LD0 sector 0.)
pub fn probe_read(disk: usize) -> bool {
    let mut s = [0u8; SECTOR];
    read(disk, 0, &mut s).is_ok()
}

fn mount_or_format(now_s: u64) -> Result<(eurofs::EuroFs<MrBlock>, bool), String> {
    use eurofs::EuroFs;
    let dev = MrBlock::new(1).ok_or_else(|| String::from("no logical drive 1"))?;
    if let Ok(fs) = EuroFs::mount(dev, now_s) {
        return Ok((fs, true));
    }
    let mut head = vec![0u8; 64 * 1024];
    read(1, 0, &mut head).map_err(|_| String::from("read of LD1 failed"))?;
    if head.iter().any(|&b| b != 0) {
        return Err(String::from("LD1 holds data that is not EuroFS: left untouched"));
    }
    EuroFs::format(dev, [0x3D; 16], now_s).map(|fs| (fs, false)).map_err(|e| format!("format failed: {e:?}"))
}

/// [mr-fs]: EuroFS on LD1 through the driver; the files of the previous boot
/// are read back first (persistence across a real reboot of the machine).
#[cfg(feature = "selftest")]
pub fn selftest_fs(_falloc: &mut euromm::FrameAllocator) {
    use eurofs::{EuroFs, FileSystem};
    let t = crate::rtc::epoch();
    let (mut fs, existing) = match mount_or_format(t) {
        Ok(x) => x,
        Err(e) => {
            crate::serial_println!("[mr-fs] {e} ⬜");
            return;
        }
    };
    let mut prev = 0u64;
    if existing {
        let g = fs.read_file("/mr/generation").ok().and_then(|b| core::str::from_utf8(&b).ok().and_then(|s| s.trim().parse::<u64>().ok()));
        match g {
            Some(g) => {
                let want = gen_pattern(g, 192 * 1024);
                let got = fs.read_file(&format!("/mr/gen-{g}.bin"));
                let hello = fs.read_file("/mr/hello.txt").ok();
                let same = got.as_ref().map(|d| *d == want).unwrap_or(false);
                crate::serial_println!(
                    "[mr-fs] EuroFS on LD1 mounted from disk: generation {g} written by the previous boot; /mr/gen-{g}.bin {} bytes, content {}; /mr/hello.txt {:?} → {}",
                    got.as_ref().map(|d| d.len()).unwrap_or(0),
                    if same { "matches" } else { "DIFFERS" },
                    hello.as_deref().map(|h| String::from_utf8_lossy(h).trim().to_string()).unwrap_or_default(),
                    if same { "OK (persisted across the reboot) ✓" } else { "FAILED" }
                );
                prev = g;
            }
            None => crate::serial_println!("[mr-fs] EuroFS on LD1 mounted, but without a generation file (not written by this self-test)"),
        }
    } else {
        crate::serial_println!("[mr-fs] LD1 was blank: EuroFS formatted on it through megaraidd (first boot)");
    }
    let g = prev + 1;
    let data = gen_pattern(g, 192 * 1024);
    let hello = format!("written by EuroOS through megaraidd, generation {g}, rtc {t}\n");
    let _ = fs.create_dir("/mr");
    let w = fs.write_file(&format!("/mr/gen-{g}.bin"), &data).is_ok()
        && fs.write_file("/mr/hello.txt", hello.as_bytes()).is_ok()
        && fs.write_file("/mr/generation", format!("{g}\n").as_bytes()).is_ok();
    if g >= 2 {
        let _ = fs.remove_file(&format!("/mr/gen-{}.bin", g - 1));
    }
    drop(fs);
    // A fresh mount reads everything from the disk through the driver again.
    let back = MrBlock::new(1).and_then(|d| EuroFs::mount(d, t).ok());
    let (same, scrub) = match back.as_ref() {
        Some(f) => (f.read_file(&format!("/mr/gen-{g}.bin")).map(|d| d == data).unwrap_or(false), Some(f.scrub())),
        None => (false, None),
    };
    let scrub_ok = scrub.as_ref().map(|s| s.errors == 0 && s.superblock_ok).unwrap_or(false);
    crate::serial_println!(
        "[mr-fs] wrote generation {g} (/mr/gen-{g}.bin 192 KiB, /mr/hello.txt, /mr/generation): written={w} · remounted from disk, read back equal={same} · scrub: {} → {}",
        scrub.map(|s| format!("{} objects, {} errors, superblock ok={}", s.objects, s.errors, s.superblock_ok)).unwrap_or_else(|| String::from("no mount")),
        if w && same && scrub_ok { "OK ✓" } else { "FAILED" }
    );
}

/// [mr-crash]: the driver dies in the middle of writing a set of files (a real
/// ring-3 fault, with its write posted to the controller and not completed).
/// With replay: the writes complete on the restarted driver, every file reads
/// back after a remount, the scrub is clean. Without replay: the write in
/// flight fails, and the filesystem is still consistent after a remount.
#[cfg(feature = "selftest")]
pub fn selftest_crash(falloc: &mut euromm::FrameAllocator) {
    use eurofs::{EuroFs, FileSystem};
    let t = crate::rtc::epoch();
    let Some(dev) = MrBlock::new(1) else { return };
    let Ok(mut fs) = EuroFs::mount(dev, t) else {
        crate::serial_println!("[mr-crash] no EuroFS on LD1 (see [mr-fs]): skipped ⬜");
        return;
    };
    let _ = fs.create_dir("/mr");
    // (a) replay on.
    let deaths0 = mr::death_count();
    let pid0 = mr::driver_pid();
    let replayed0 = REPLAYED.load(Ordering::Relaxed);
    mr::arm_crash_during_write(3);
    let files: Vec<(String, Vec<u8>)> = (0..6).map(|i| (format!("/mr/crash-{i}.bin"), gen_pattern(1000 + i, 96 * 1024))).collect();
    let mut written = 0;
    for (p, d) in &files {
        if fs.write_file(p, d).is_ok() {
            written += 1;
        }
    }
    drop(fs);
    let died = mr::death_count() - deaths0;
    let pid1 = mr::driver_pid();
    let replayed = REPLAYED.load(Ordering::Relaxed) - replayed0;
    let reason = mr::last_death();
    let remount = MrBlock::new(1).and_then(|d| EuroFs::mount(d, t).ok());
    let (verified, scrub) = match remount.as_ref() {
        Some(f) => (files.iter().filter(|(p, d)| f.read_file(p).map(|x| x == *d).unwrap_or(false)).count(), Some(f.scrub())),
        None => (0, None),
    };
    let scrub_ok = scrub.as_ref().map(|s| s.errors == 0 && s.superblock_ok).unwrap_or(false);
    let ok_a = died == 1 && pid1 != pid0 && written == files.len() && verified == files.len() && scrub_ok && replayed >= 1 && mr::sends_refused_while_dead();
    crate::serial_println!(
        "[mr-crash] megaraidd pid {pid0} killed by a real fault ({reason}) with write #3 posted to the controller and unacknowledged; restarted as pid {pid1}; {replayed} request(s) replayed; {written}/{} files written, after a remount {verified}/{} read back equal; scrub: {} → {}",
        files.len(), files.len(),
        scrub.map(|s| format!("{} objects, {} errors, superblock ok={}", s.objects, s.errors, s.superblock_ok)).unwrap_or_else(|| String::from("no mount")),
        if ok_a { "OK (driver crash mid-write = process exit, filesystem consistent, no data lost) ✓" } else { "FAILED" }
    );
    drop(remount);
    // (b) replay off: the in-flight write is lost; EuroFS's copy-on-write must hold.
    let Some(mut fs) = MrBlock::new(1).and_then(|d| EuroFs::mount(d, t).ok()) else { return };
    REPLAY.store(false, Ordering::Relaxed);
    let deaths1 = mr::death_count();
    mr::arm_crash_during_write(2);
    let lost = gen_pattern(77, 128 * 1024);
    let res = fs.write_file("/mr/lost.bin", &lost);
    drop(fs);
    REPLAY.store(true, Ordering::Relaxed);
    let back = mr::wait_ready_from_task0(falloc, 1500);
    let remount = MrBlock::new(1).and_then(|d| EuroFs::mount(d, t).ok());
    let (old_ok, lost_state, scrub) = match remount.as_ref() {
        Some(f) => {
            let old = files.iter().filter(|(p, d)| f.read_file(p).map(|x| x == *d).unwrap_or(false)).count();
            let st = match f.read_file("/mr/lost.bin") {
                Err(_) => "absent (the old checkpoint)",
                Ok(x) if x == lost => "complete",
                Ok(_) => "TORN",
            };
            (old, st, Some(f.scrub()))
        }
        None => (0, "no mount", None),
    };
    let scrub_ok = scrub.as_ref().map(|s| s.errors == 0 && s.superblock_ok).unwrap_or(false);
    let ok_b = res.is_err() && mr::death_count() == deaths1 + 1 && back && old_ok == files.len() && lost_state != "TORN" && scrub_ok;
    crate::serial_println!(
        "[mr-crash] without replay: driver killed during write #2 of /mr/lost.bin -> write_file returned {:?}; driver back={back}; remount: earlier files {old_ok}/{} equal, /mr/lost.bin {lost_state}; scrub: {} → {}",
        res.err(), files.len(),
        scrub.map(|s| format!("{} objects, {} errors, superblock ok={}", s.objects, s.errors, s.superblock_ok)).unwrap_or_else(|| String::from("no mount")),
        if ok_b { "OK (copy-on-write keeps the filesystem consistent when a write is lost) ✓" } else { "FAILED" }
    );
    crate::serial_println!("[mrblk] {}", stats_line());
}
