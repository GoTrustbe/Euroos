//! Root block device (Run 7): one type that carries EuroFS — either in RAM (live mode,
//! no disk) or DIRECTLY on the virtio-blk disk (INSTALLED mode).
//! In disk mode EuroFS sits as the REAL root on a GPT partition: files
//! are read/written from disk instead of being rebuilt in RAM on every boot —
//! the difference between a live USB and an installed OS.

use alloc::vec;
use alloc::vec::Vec;

use spin::RwLock;

use eurofs::{BlockDevice, BlockError, BlockResult};

const BS: usize = 4096;
const SPB: u64 = (BS / 512) as u64; // 512-byte sectors per 4 KiB EuroFS block = 8

// ── Write-back block cache (makes disk mode fast) ─────────────────────
// Direct-mapped cache of 4 KiB blocks; reads avoid repeated disk reads,
// writes are batched and only written out on `flush()` (= EuroFS checkpoint).
//
// J1: the slot is protected by an **RwLock** instead of a single global Mutex, so
// a cache HIT (the common FS read path) takes only a **read-lock** → multiple
// cores read cached FS blocks simultaneously without serializing each other. Only
// a miss/write/flush takes the write-lock. Write-back semantics (dirty until flush)
// remain exactly preserved, so the EuroFS checkpoint/crash consistency does not change.
const CACHE_SLOTS: usize = 1024; // 1024 × 4 KiB = 4 MiB cache

struct Slot {
    sector: u64, // start sector of this 4 KiB block on the disk
    valid: bool,
    dirty: bool,
    data: [u8; BS],
}

static CACHE: RwLock<[Slot; CACHE_SLOTS]> =
    RwLock::new([const { Slot { sector: 0, valid: false, dirty: false, data: [0u8; BS] } }; CACHE_SLOTS]);

fn slot_index(sector: u64) -> usize {
    ((sector / SPB) as usize) % CACHE_SLOTS
}

/// Read a 4 KiB block that starts at `sector` (via the cache).
fn cache_read(sector: u64, out: &mut [u8]) -> bool {
    let i = slot_index(sector);
    // Fast path: read-lock, hit? (concurrent with other readers)
    {
        let c = CACHE.read();
        if c[i].valid && c[i].sector == sector {
            out[..BS].copy_from_slice(&c[i].data);
            return true;
        }
    }
    // Miss: write-lock, double-check (another core may have loaded it in the meantime),
    // write back the dirty occupant, then load from disk.
    let mut c = CACHE.write();
    if !(c[i].valid && c[i].sector == sector) {
        if c[i].valid && c[i].dirty {
            let s = c[i].sector;
            let data = c[i].data;
            if !crate::virtio_blk::write_io(s, &data) {
                return false;
            }
        }
        let mut tmp = [0u8; BS];
        if !crate::virtio_blk::read_io(sector, &mut tmp) {
            return false;
        }
        c[i].data = tmp;
        c[i].sector = sector;
        c[i].valid = true;
        c[i].dirty = false;
    }
    out[..BS].copy_from_slice(&c[i].data);
    true
}

/// Write a 4 KiB block to `sector` (write-back: stays in the cache, dirty).
fn cache_write(sector: u64, data: &[u8]) -> bool {
    let mut c = CACHE.write();
    let i = slot_index(sector);
    if c[i].valid && c[i].sector != sector && c[i].dirty {
        let s = c[i].sector;
        let old = c[i].data;
        if !crate::virtio_blk::write_io(s, &old) {
            return false;
        }
    }
    c[i].data[..BS].copy_from_slice(&data[..BS]);
    c[i].sector = sector;
    c[i].valid = true;
    c[i].dirty = true;
    true
}

/// Write all dirty blocks to disk (on EuroFS checkpoint / shutdown).
pub fn cache_flush() {
    let mut c = CACHE.write();
    for slot in c.iter_mut() {
        if slot.valid && slot.dirty {
            if crate::virtio_blk::write_io(slot.sector, &slot.data) {
                slot.dirty = false;
            }
        }
    }
}

#[derive(Clone)]
pub struct RootBlk {
    data: Vec<u8>,   // RAM backing (empty in disk mode)
    part_start: u64, // disk mode: first 512-byte sector of the EuroFS partition
    on_disk: bool,
    blocks: u64,
    dev: usize, // virtio-blk device index (0 = root via cache; >0 = extra disk, direct)
}

impl RootBlk {
    pub fn ram(blocks: u64) -> Self {
        Self { data: vec![0u8; (blocks * BS as u64) as usize], part_start: 0, on_disk: false, blocks, dev: 0 }
    }
    pub fn disk(part_start: u64, blocks: u64) -> Self {
        Self::disk_on(0, part_start, blocks)
    }
    /// Disk mode on a specific virtio-blk device (B3 multi-disk). Device 0
    /// goes through the block cache; further devices do uncached direct I/O.
    pub fn disk_on(dev: usize, part_start: u64, blocks: u64) -> Self {
        Self { data: Vec::new(), part_start, on_disk: true, blocks, dev }
    }
    pub fn is_disk(&self) -> bool {
        self.on_disk
    }
}

impl BlockDevice for RootBlk {
    fn block_size(&self) -> u32 {
        BS as u32
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        if !self.on_disk {
            let off = (start * BS as u64) as usize;
            let len = count as usize * BS;
            if off + len > self.data.len() {
                return Err(BlockError::OutOfBounds);
            }
            buf[..len].copy_from_slice(&self.data[off..off + len]);
            return Ok(());
        }
        for i in 0..count as u64 {
            let base = self.part_start + (start + i) * SPB;
            let o = (i * BS as u64) as usize;
            let ok = if self.dev == 0 {
                cache_read(base, &mut buf[o..o + BS])
            } else {
                crate::virtio_blk::read_io_dev(self.dev, base, &mut buf[o..o + BS])
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        if !self.on_disk {
            let off = (start * BS as u64) as usize;
            let len = count as usize * BS;
            if off + len > self.data.len() {
                return Err(BlockError::OutOfBounds);
            }
            self.data[off..off + len].copy_from_slice(&buf[..len]);
            return Ok(());
        }
        for i in 0..count as u64 {
            let base = self.part_start + (start + i) * SPB;
            let o = (i * BS as u64) as usize;
            let ok = if self.dev == 0 {
                cache_write(base, &buf[o..o + BS])
            } else {
                crate::virtio_blk::write_io_dev(self.dev, base, &buf[o..o + BS])
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        if self.on_disk {
            // Device 0 goes through the cache; further disks write directly.
            let ok = if self.dev == 0 {
                cache_flush(); // dirty cache blocks to disk (EuroFS checkpoint commit)
                // Then force the disk's OWN write-back cache to the persistent
                // medium (VIRTIO_BLK_T_FLUSH) — makes the A/B-superblock barrier a hard
                // I/O barrier. No-op if the device has no volatile cache.
                crate::virtio_blk::flush()
            } else {
                crate::virtio_blk::flush_dev(self.dev)
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }
}
