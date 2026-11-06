//! virtio-blk-pci driver (Run 7 / doc §10) — a REAL block disk.
//!
//! Runs over the shared virtio PCI transport (`virtio_pci.rs`): modern
//! virtio 1.x (PCI id 1af4:1042, memory BAR, `disable-legacy=on`) or legacy
//! 0.9.5 (1af4:1001, I/O BAR 0). One request queue; a request is a chain of
//! three descriptors: [16-byte header | data | 1-byte status].
//!
//! Features accepted (only what is implemented): VERSION_1 (modern), FLUSH
//! (EuroFS checkpoints really reach the medium), DISCARD, RO (writes are
//! refused). Not accepted: EVENT_IDX, INDIRECT_DESC, packed rings, MQ,
//! SEG_MAX/BLK_SIZE (one descriptor per data buffer, 512-byte sectors).

use core::sync::atomic::{compiler_fence, Ordering};

use euromm::FrameAllocator;

use crate::virtio_pci::{feat, Queue, Transport, DESC_NEXT, DESC_WRITE};

pub const SECTOR: usize = 512;
const VIRTIO_BLK_T_IN: u32 = 0; // read (device → us)
const VIRTIO_BLK_T_OUT: u32 = 1; // write (us → device)
const VIRTIO_BLK_T_FLUSH: u32 = 4; // force device cache → persistent medium
const VIRTIO_BLK_T_DISCARD: u32 = 11; // TRIM: tell the device a range is unused

pub struct VirtioBlk {
    tp: Transport,
    vq: Queue,
    pub capacity_sectors: u64,
    hdr: u64,
    data: u64,
    status: u64,
    flush_ok: bool, // VIRTIO_BLK_F_FLUSH negotiated → the FLUSH command is valid
    discard_ok: bool, // VIRTIO_BLK_F_DISCARD negotiated → the DISCARD (TRIM) command is valid
    read_only: bool, // VIRTIO_BLK_F_RO: the device refuses writes, so do we
}

/// Up to 4 virtio-blk disks (root + extra mounts). Index 0 = the first/root.
pub const MAX_BLK: usize = 4;
static mut BLKS: [Option<VirtioBlk>; MAX_BLK] = [None, None, None, None];

/// Return a mutable reference to disk `dev` (None if absent).
unsafe fn dev_mut(dev: usize) -> Option<&'static mut VirtioBlk> {
    if dev >= MAX_BLK {
        return None;
    }
    (*core::ptr::addr_of_mut!(BLKS))[dev].as_mut()
}

/// Initialize ALL virtio-blk disks (up to `MAX_BLK`). Returns false if there is no
/// device at all. Disk 0 is the root; further disks are extra mounts (B3).
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let devs = crate::virtio_pci::find_all(2);
    if devs.is_empty() {
        crate::serial_println!("[blk] no virtio-blk device found");
        return false;
    }
    let mut count = 0;
    for d in devs.iter() {
        if count >= MAX_BLK {
            break;
        }
        if let Some(blk) = setup_device(d, falloc) {
            unsafe {
                (*core::ptr::addr_of_mut!(BLKS))[count] = Some(blk);
            }
            count += 1;
            crate::pci::claim(d.bus, d.dev, d.func, "virtio-blk"); // hwprobe (M1-3)
        }
    }
    crate::serial_println!("[blk] {count} virtio-blk disk(s) initialized");
    count > 0
}

/// Set up a single virtio-blk device (own virtqueue + buffer frames). None on error.
fn setup_device(dev: &crate::pci::PciDevice, falloc: &mut FrameAllocator) -> Option<VirtioBlk> {
    let mut tp = Transport::probe(dev)?;
    tp.begin();
    let accepted = match tp.negotiate(feat::BLK_FLUSH | feat::BLK_DISCARD | feat::BLK_RO) {
        Some(a) => a,
        None => {
            crate::serial_println!(
                "[blk] {:02x}:{:02x}.{}: feature negotiation refused (offered {:#x})",
                dev.bus, dev.dev, dev.func, tp.device_features()
            );
            return None;
        }
    };
    let flush_ok = accepted & feat::BLK_FLUSH != 0;
    let discard_ok = accepted & feat::BLK_DISCARD != 0;
    let read_only = accepted & feat::BLK_RO != 0;
    // Capacity (config offset 0, u64 sectors), read before MSI-X moves the
    // legacy device config.
    let capacity_sectors = tp.cfg64(0);

    // The driver uses a fixed 3-descriptor chain (hdr/data/status); a device
    // with a smaller queue would get descriptors past its table (audit C2).
    let vq = tp.setup_queue(0, 256, &mut |pages| falloc.allocate_contiguous(pages).ok())?;
    if vq.size < 3 {
        tp.set_status(eurovirtio::status::FAILED);
        return None;
    }
    // Fixed buffer frames: header/status in frame 0, a CONTIGUOUS data area of
    // DATA_FRAMES frames: one descriptor covers the whole request, so a 64 KiB
    // read is still a single device round-trip.
    let frame = falloc.allocate().ok()?;
    let data = falloc.allocate_contiguous(DATA_FRAMES).ok()?;
    let hdr = frame; // 16 B
    let status_buf = frame + 16; // 1 B

    // J2: MSI-X on the storage controller. ADDITIVE — the used-ring poll remains
    // the completion confirmation; the IRQ proves interrupt-driven completion.
    let msix_n = tp.enable_msix(0, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, crate::apic::lapic_id() as u8);
    if msix_n > 0 {
        let rb = tp.set_queue_vector(0, 0);
        tp.set_config_vector(eurovirtio::NO_VECTOR);
        crate::serial_println!(
            "[j2-blk] virtio-blk MSI-X on ({} entries) → vector {:#x}, queue_msix_vector readback={:#06x}",
            msix_n, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, rb
        );
    } else {
        crate::serial_println!("[blk] virtio-blk: no MSI-X, completions are polled");
    }

    tp.driver_ok();
    crate::serial_println!(
        "[blk] virtio-blk OK — {} sectors ({} MiB) @ {:02x}:{:02x}.{} {} · features {:#x} · FLUSH {}{}",
        capacity_sectors,
        capacity_sectors * 512 / (1024 * 1024),
        dev.bus, dev.dev, dev.func,
        tp.describe(),
        accepted,
        if flush_ok { "on (real durability)" } else { "n/a" },
        if read_only { " · READ-ONLY" } else { "" }
    );
    Some(VirtioBlk { tp, vq, capacity_sectors, hdr, data, status: status_buf, flush_ok, discard_ok, read_only })
}

// Max bytes per request. Was 4096 (one EuroFS block): serving a 495 MB chrome binary
// page by page meant ~120 000 device round-trips, each a VM exit + busy-wait under
// TCG, and the ledger showed that wait dominating kernel time. 64 KiB per request is
// 16x fewer round-trips for sequential fills; the data area is 16 contiguous frames.
const DATA_MAX: usize = 65536;
const DATA_FRAMES: usize = DATA_MAX / 4096;

/// One block request of `nbytes` (multiple of 512, ≤ 4096) starting at `sector`.
/// On write the caller first copies into `blk.data`; on read the caller
/// reads from it afterwards.
unsafe fn submit(blk: &mut VirtioBlk, write: bool, sector: u64, nbytes: usize) -> bool {
    let dlen = ((nbytes + 511) / 512 * 512) as u32;
    (blk.hdr as *mut u32).write_volatile(if write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN });
    ((blk.hdr + 4) as *mut u32).write_volatile(0);
    ((blk.hdr + 8) as *mut u64).write_volatile(sector);
    (blk.status as *mut u8).write_volatile(0xFF);

    let d0 = blk.vq.desc(0);
    (*d0).addr = blk.hdr;
    (*d0).len = 16;
    (*d0).flags = DESC_NEXT;
    (*d0).next = 1;
    let d1 = blk.vq.desc(1);
    (*d1).addr = blk.data;
    (*d1).len = dlen;
    (*d1).flags = if write { DESC_NEXT } else { DESC_NEXT | DESC_WRITE };
    (*d1).next = 2;
    let d2 = blk.vq.desc(2);
    (*d2).addr = blk.status;
    (*d2).len = 1;
    (*d2).flags = DESC_WRITE;
    (*d2).next = 0;

    kick_and_wait(blk)
}

/// Place descriptor 0 in the avail-ring, notify the device and wait (busy) until the
/// request appears in the used-ring; returns true on status 0 (OK).
/// Device round-trips and the cycles spent busy-waiting on them — the number the
/// demand-paging sprint stands or falls on.
pub static KICK_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static KICK_CYCLES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

unsafe fn kick_and_wait(blk: &mut VirtioBlk) -> bool {
    let t0 = core::arch::x86_64::_rdtsc();
    let r = kick_and_wait_inner(blk);
    KICK_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    KICK_CYCLES.fetch_add(core::arch::x86_64::_rdtsc().wrapping_sub(t0),
        core::sync::atomic::Ordering::Relaxed);
    r
}

unsafe fn kick_and_wait_inner(blk: &mut VirtioBlk) -> bool {
    let idx = blk.vq.avail_idx_ptr().read();
    blk.vq.avail_ring(idx % blk.vq.size).write(0);
    compiler_fence(Ordering::SeqCst);
    blk.vq.avail_idx_ptr().write(idx.wrapping_add(1));
    compiler_fence(Ordering::SeqCst);
    blk.tp.notify(&blk.vq);

    for _ in 0..40_000_000 {
        if blk.vq.used_idx() != blk.vq.last_used {
            blk.vq.last_used = blk.vq.used_idx();
            return (blk.status as *const u8).read_volatile() == 0;
        }
        core::hint::spin_loop();
    }
    false
}

/// Send a VIRTIO_BLK_T_FLUSH: the device persists its write-back cache to
/// the medium. A FLUSH request has NO data descriptor (only hdr + status).
unsafe fn submit_flush(blk: &mut VirtioBlk) -> bool {
    (blk.hdr as *mut u32).write_volatile(VIRTIO_BLK_T_FLUSH);
    ((blk.hdr + 4) as *mut u32).write_volatile(0);
    ((blk.hdr + 8) as *mut u64).write_volatile(0); // sector ignored on FLUSH
    (blk.status as *mut u8).write_volatile(0xFF);

    let d0 = blk.vq.desc(0);
    (*d0).addr = blk.hdr;
    (*d0).len = 16;
    (*d0).flags = DESC_NEXT;
    (*d0).next = 1;
    let d1 = blk.vq.desc(1);
    (*d1).addr = blk.status;
    (*d1).len = 1;
    (*d1).flags = DESC_WRITE;
    (*d1).next = 0;

    kick_and_wait(blk)
}

/// Force that all previously written blocks are on the PERSISTENT medium (not
/// just in the disk's write-back cache). Returns true on success; if the device
/// has no FLUSH feature (no volatile cache) this is a successful no-op.
pub fn flush() -> bool {
    flush_dev(0)
}

/// FLUSH on a specific disk.
pub fn flush_dev(dev: usize) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        if !blk.flush_ok {
            return true; // no negotiated FLUSH feature → nothing to do
        }
        submit_flush(blk)
    }
}

/// Send a VIRTIO_BLK_T_DISCARD (TRIM) for one range. The request carries a single
/// 16-byte `virtio_blk_discard_write_zeroes { sector:u64, num_sectors:u32, flags:u32 }`
/// segment in the data descriptor (device reads it).
unsafe fn submit_discard(blk: &mut VirtioBlk, sector: u64, num_sectors: u32) -> bool {
    (blk.hdr as *mut u32).write_volatile(VIRTIO_BLK_T_DISCARD);
    ((blk.hdr + 4) as *mut u32).write_volatile(0);
    ((blk.hdr + 8) as *mut u64).write_volatile(0); // header sector ignored for DISCARD
    // The 16-byte discard segment goes in the data buffer.
    (blk.data as *mut u64).write_volatile(sector);
    ((blk.data + 8) as *mut u32).write_volatile(num_sectors);
    ((blk.data + 12) as *mut u32).write_volatile(0); // flags (no UNMAP)
    (blk.status as *mut u8).write_volatile(0xFF);

    let d0 = blk.vq.desc(0);
    (*d0).addr = blk.hdr;
    (*d0).len = 16;
    (*d0).flags = DESC_NEXT;
    (*d0).next = 1;
    let d1 = blk.vq.desc(1);
    (*d1).addr = blk.data;
    (*d1).len = 16;
    (*d1).flags = DESC_NEXT; // device READS the segment
    (*d1).next = 2;
    let d2 = blk.vq.desc(2);
    (*d2).addr = blk.status;
    (*d2).len = 1;
    (*d2).flags = DESC_WRITE;
    (*d2).next = 0;

    kick_and_wait(blk)
}

/// DISCARD (TRIM) `count` 512-byte sectors starting at `sector` on disk `dev`. Advisory:
/// if the device did not negotiate VIRTIO_BLK_F_DISCARD this is a successful no-op.
pub fn discard_dev(dev: usize, sector: u64, count: u32) -> bool {
    if count == 0 {
        return true;
    }
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        if !blk.discard_ok {
            return true; // no DISCARD feature → nothing to do (honest no-op)
        }
        if sector.saturating_add(count as u64) > blk.capacity_sectors {
            return false; // out of range (mirror the read/write bounds checks)
        }
        submit_discard(blk, sector, count)
    }
}

/// Read `buf.len()` bytes (≤ 4096, multiple of 512) starting at `sector` of disk `dev`.
pub fn read_io_dev(dev: usize, sector: u64, buf: &mut [u8]) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        // No silent truncation or out-of-range LBA (audit C3): reject instead of
        // reporting a partial transfer as success.
        let n = buf.len();
        if n > DATA_MAX || sector + (n as u64).div_ceil(512) > blk.capacity_sectors {
            return false;
        }
        if !submit(blk, false, sector, n) {
            return false;
        }
        core::ptr::copy_nonoverlapping(blk.data as *const u8, buf.as_mut_ptr(), n);
        true
    }
}

/// Write `buf.len()` bytes (≤ 4096) starting at `sector` to disk `dev`.
pub fn write_io_dev(dev: usize, sector: u64, buf: &[u8]) -> bool {
    unsafe {
        let blk = match dev_mut(dev) {
            Some(b) => b,
            None => return false,
        };
        // No silent truncation or out-of-range LBA (audit C3).
        let n = buf.len();
        if n > DATA_MAX || sector + (n as u64).div_ceil(512) > blk.capacity_sectors || blk.read_only {
            return false;
        }
        core::ptr::copy_nonoverlapping(buf.as_ptr(), blk.data as *mut u8, n);
        // Zero-fill the rest of the last sector.
        let dlen = (n + 511) / 512 * 512;
        if dlen > n {
            core::ptr::write_bytes((blk.data + n as u64) as *mut u8, 0, dlen - n);
        }
        submit(blk, true, sector, n)
    }
}

/// Read/write on disk 0 (root) — backward-compat.
pub fn read_io(sector: u64, buf: &mut [u8]) -> bool {
    read_io_dev(0, sector, buf)
}
pub fn write_io(sector: u64, buf: &[u8]) -> bool {
    write_io_dev(0, sector, buf)
}

/// Read/write a single 512-byte sector (for the self-test).
pub fn read_sector(sector: u64, buf: &mut [u8]) -> bool {
    read_io(sector, buf)
}
pub fn write_sector(sector: u64, buf: &[u8]) -> bool {
    write_io(sector, buf)
}

pub fn present() -> bool {
    present_dev(0)
}

/// Is disk `dev` present?
pub fn present_dev(dev: usize) -> bool {
    unsafe { dev < MAX_BLK && (*core::ptr::addr_of!(BLKS))[dev].is_some() }
}

/// Number of initialized virtio-blk disks.
pub fn device_count() -> usize {
    unsafe { (*core::ptr::addr_of!(BLKS)).iter().filter(|b| b.is_some()).count() }
}

/// Capacity of disk 0 in 512-byte sectors (0 if no disk).
pub fn capacity_sectors() -> u64 {
    capacity_sectors_dev(0)
}

/// Capacity of disk `dev` in 512-byte sectors.
pub fn capacity_sectors_dev(dev: usize) -> u64 {
    unsafe {
        if dev >= MAX_BLK {
            return 0;
        }
        (*core::ptr::addr_of!(BLKS))[dev].as_ref().map(|b| b.capacity_sectors).unwrap_or(0)
    }
}

/// Self-test: write a pattern to a sector, read it back, verify.
pub fn self_test() {
    if !present() {
        return;
    }
    let sector = 2048u64; // well past any metadata
    let mut wbuf = [0u8; SECTOR];
    for (i, b) in wbuf.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    let w = write_sector(sector, &wbuf);
    let mut rbuf = [0u8; SECTOR];
    let r = read_sector(sector, &mut rbuf);
    let ok = w && r && rbuf == wbuf;
    crate::serial_println!(
        "[blk] self-test sector {}: write={} read={} data-match={} -> {}",
        sector, w, r, rbuf == wbuf, if ok { "OK (real disk works!)" } else { "FAILED" }
    );
}
