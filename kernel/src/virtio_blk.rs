//! virtio-blk-pci legacy driver (Run 7 / doc §10) — a REAL block disk.
//!
//! Same legacy-virtio approach as `virtio_net` (PIO via BAR0, split-ring
//! virtqueue), but with a single request queue. A block request is a chain of
//! three descriptors: [16-byte header | 512-byte data | 1-byte status].
//! This lets EuroFS eventually live on a real disk → files survive
//! a restart.

use core::sync::atomic::{compiler_fence, Ordering};

use euromm::FrameAllocator;
use x86_64::instructions::port::Port;

const VIRTIO_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_DRIVER_FEATURES: u16 = 0x04;
const VIRTIO_QUEUE_PFN: u16 = 0x08;
const VIRTIO_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_STATUS: u16 = 0x12;
const VIRTIO_BLK_CAPACITY: u16 = 0x14; // u64 capacity in sectors

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

pub const SECTOR: usize = 512;
const VIRTIO_BLK_T_IN: u32 = 0; // read (device → us)
const VIRTIO_BLK_T_OUT: u32 = 1; // write (us → device)
const VIRTIO_BLK_T_FLUSH: u32 = 4; // force device cache → persistent medium
const VIRTIO_BLK_T_DISCARD: u32 = 11; // TRIM: tell the device a range is unused
const VIRTIO_BLK_F_FLUSH: u32 = 1 << 9; // device supports the FLUSH command
const VIRTIO_BLK_F_DISCARD: u32 = 1 << 13; // device supports the DISCARD command

#[repr(C)]
struct VqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

struct VirtQueue {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    last_used: u16,
}

impl VirtQueue {
    fn desc(&self, i: u16) -> *mut VqDesc {
        (self.desc + (i as u64) * 16) as *mut VqDesc
    }
    fn avail_idx_ptr(&self) -> *mut u16 {
        (self.avail + 2) as *mut u16
    }
    fn avail_ring(&self, i: u16) -> *mut u16 {
        (self.avail + 4 + (i as u64) * 2) as *mut u16
    }
    fn used_idx(&self) -> u16 {
        unsafe { ((self.used + 2) as *const u16).read_volatile() }
    }
}

pub struct VirtioBlk {
    io: u16,
    vq: VirtQueue,
    pub capacity_sectors: u64,
    hdr: u64,
    data: u64,
    status: u64,
    flush_ok: bool, // VIRTIO_BLK_F_FLUSH negotiated → the FLUSH command is valid
    discard_ok: bool, // VIRTIO_BLK_F_DISCARD negotiated → the DISCARD (TRIM) command is valid
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

fn setup_queue(io: u16, sel: u16, falloc: &mut FrameAllocator) -> Option<VirtQueue> {
    unsafe {
        Port::new(io + VIRTIO_QUEUE_SELECT).write(sel);
        let qsz: u16 = Port::new(io + VIRTIO_QUEUE_SIZE).read();
        // The driver uses a fixed 3-descriptor chain (hdr/data/status); a
        // device that advertises a smaller queue would cause OOB descriptors to be
        // written (audit C2). Reject qsz < 3.
        if qsz < 3 {
            return None;
        }
        // Legacy split-ring: ONE contiguous region. The device computes the used-ring
        // at `align(desc+avail, 4096)` from the QUEUE_PFN base — so keep the same
        // layout and reserve enough contiguous frames.
        let q = qsz as u64;
        let desc_sz = 16 * q;
        let avail_sz = 6 + 2 * q;
        let used_off = (desc_sz + avail_sz + 4095) & !4095;
        let used_sz = 6 + 8 * q;
        let total = used_off + used_sz;
        let frames = ((total + 4095) / 4096) as usize;
        let base = falloc.allocate().ok()?;
        for _ in 1..frames {
            falloc.allocate().ok()?; // reserve the contiguous follow-up frames
        }
        core::ptr::write_bytes(base as *mut u8, 0, frames * 4096);
        let desc = base;
        let avail = base + desc_sz;
        let used = base + used_off;
        Port::new(io + VIRTIO_QUEUE_PFN).write((base >> 12) as u32);
        Some(VirtQueue { size: qsz, desc, avail, used, last_used: 0 })
    }
}

/// Initialize ALL virtio-blk disks (up to `MAX_BLK`). Returns false if there is no
/// device at all. Disk 0 is the root; further disks are extra mounts (B3).
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let devs: alloc::vec::Vec<_> = crate::pci::enumerate()
        .into_iter()
        .filter(|d| d.vendor == 0x1AF4 && (d.device == 0x1001 || d.device == 0x1042))
        .collect();
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
    let io = (dev.bar(0) & 0xFFFC) as u16;
    dev.enable(0x5); // I/O + bus master
    unsafe {
        let mut status: Port<u8> = Port::new(io + VIRTIO_STATUS);
        status.write(0);
        status.write(STATUS_ACK);
        status.write(STATUS_ACK | STATUS_DRIVER);
        // Negotiate ONLY VIRTIO_BLK_F_FLUSH (if the device offers it): that gives
        // us a real FLUSH command so that EuroFS checkpoints actually land on the medium
        // instead of in the disk's write-back cache. No other features.
        let dev_features = Port::<u32>::new(io + VIRTIO_DEVICE_FEATURES).read();
        let flush_ok = dev_features & VIRTIO_BLK_F_FLUSH != 0;
        let discard_ok = dev_features & VIRTIO_BLK_F_DISCARD != 0;
        let mut accept = 0u32;
        if flush_ok {
            accept |= VIRTIO_BLK_F_FLUSH;
        }
        if discard_ok {
            accept |= VIRTIO_BLK_F_DISCARD;
        }
        Port::<u32>::new(io + VIRTIO_DRIVER_FEATURES).write(accept);

        let capacity_sectors: u64 = {
            let lo = Port::<u32>::new(io + VIRTIO_BLK_CAPACITY).read() as u64;
            let hi = Port::<u32>::new(io + VIRTIO_BLK_CAPACITY + 4).read() as u64;
            (hi << 32) | lo
        };

        let vq = match setup_queue(io, 0, falloc) {
            Some(q) => q,
            None => return None,
        };
        // Fixed buffer frames: header/status in frame 0, a full 4 KiB
        // data frame so we do up to 8 sectors (one EuroFS block) per request.
        let frame = falloc.allocate().expect("blk-buf");
        let data_frame = falloc.allocate().expect("blk-data");
        let hdr = frame; // 16 B
        let status_buf = frame + 16; // 1 B
        let data = data_frame; // 4096 B

        // J2: MSI-X on the storage controller. ADDITIVE — the used-ring poll remains the
        // completion confirmation; the IRQ proves interrupt-driven storage completion.
        // Capacity has ALREADY been read ABOVE (device-config @0x14, MSI-X still off); once
        // MSI-X is on, device-config shifts to 0x18 and 0x14/0x16 become the vector
        // registers — we read NO more device-config after that, so no regression.
        let msix_n = crate::msix::enable(dev, 0, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, crate::apic::lapic_id() as u8);
        if msix_n > 0 {
            Port::<u16>::new(io + 0x0E).write(0); // queue select 0
            Port::<u16>::new(io + 0x16).write(0); // queue_msix_vector = MSI-X entry 0
            let rb: u16 = Port::<u16>::new(io + 0x16).read(); // 0xFFFF = NO_VECTOR (failed)
            Port::<u16>::new(io + 0x14).write(0xFFFF); // config_msix_vector = NO_VECTOR
            crate::serial_println!(
                "[j2-blk] virtio-blk MSI-X on ({} entries) → vector {:#x}, queue_msix_vector readback={:#06x}",
                msix_n, crate::interrupts::VIRTIO_BLK_MSIX_VECTOR, rb
            );
        }

        status.write(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
        crate::serial_println!(
            "[blk] virtio-blk OK — {} sectors ({} MiB) @ BAR0 I/O={:#06x} · FLUSH {}",
            capacity_sectors,
            capacity_sectors * 512 / (1024 * 1024),
            io,
            if flush_ok { "on (real durability)" } else { "n/a" }
        );
        Some(VirtioBlk { io, vq, capacity_sectors, hdr, data, status: status_buf, flush_ok, discard_ok })
    }
}

const DATA_MAX: usize = 4096; // max bytes per request = 8 sectors = 1 EuroFS block

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
unsafe fn kick_and_wait(blk: &mut VirtioBlk) -> bool {
    let idx = blk.vq.avail_idx_ptr().read();
    blk.vq.avail_ring(idx % blk.vq.size).write(0);
    compiler_fence(Ordering::SeqCst);
    blk.vq.avail_idx_ptr().write(idx.wrapping_add(1));
    compiler_fence(Ordering::SeqCst);
    Port::<u16>::new(blk.io + VIRTIO_QUEUE_NOTIFY).write(0);

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
        if n > DATA_MAX || sector + (n as u64).div_ceil(512) > blk.capacity_sectors {
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
