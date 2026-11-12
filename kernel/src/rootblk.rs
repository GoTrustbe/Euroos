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

/// Which storage carries the root partition (Metal M2-3: NVMe/AHCI in addition
/// to virtio-blk). Virtio keeps its `dev` index (device 0 uses the write-back
/// cache above); NVMe/AHCI do uncached direct I/O like the extra virtio disks.
#[derive(Clone, Copy)]
pub(crate) enum Backend {
    Virtio(usize),
    Nvme,
    Ahci(usize),
}

#[derive(Clone)]
pub struct RootBlk {
    data: Vec<u8>,   // RAM backing (empty in disk mode)
    part_start: u64, // disk mode: first 512-byte sector of the EuroFS partition
    on_disk: bool,
    blocks: u64,
    backend: Backend,
}

impl RootBlk {
    pub fn ram(blocks: u64) -> Self {
        Self { data: vec![0u8; (blocks * BS as u64) as usize], part_start: 0, on_disk: false, blocks, backend: Backend::Virtio(0) }
    }
    pub fn disk(part_start: u64, blocks: u64) -> Self {
        Self::disk_on(0, part_start, blocks)
    }
    /// Disk mode on a specific virtio-blk device (B3 multi-disk). Device 0
    /// goes through the block cache; further devices do uncached direct I/O.
    pub fn disk_on(dev: usize, part_start: u64, blocks: u64) -> Self {
        Self { data: Vec::new(), part_start, on_disk: true, blocks, backend: Backend::Virtio(dev) }
    }
    /// Disk mode on the NVMe controller (Metal M2-3). Direct, uncached I/O.
    pub fn nvme(part_start: u64, blocks: u64) -> Self {
        Self { data: Vec::new(), part_start, on_disk: true, blocks, backend: Backend::Nvme }
    }
    /// Disk mode on an AHCI/SATA disk (Metal M2-3). Direct, uncached I/O.
    pub fn ahci(idx: usize, part_start: u64, blocks: u64) -> Self {
        Self { data: Vec::new(), part_start, on_disk: true, blocks, backend: Backend::Ahci(idx) }
    }
    pub fn is_disk(&self) -> bool {
        self.on_disk
    }
    /// Remember which physical disk carries the root filesystem. The A/B update
    /// path (GPT lookup, slot_config, slot partitions) addresses THAT disk, whatever
    /// its bus: virtio-blk, NVMe or AHCI. Called once, after the root is chosen.
    pub fn register_as_boot_disk(&self) {
        *BOOT.lock() = Some(if self.on_disk { Some(self.backend) } else { None });
    }

    /// Read one 4 KiB block starting at 512-byte sector `base` from the backend.
    fn backend_read(&self, base: u64, out: &mut [u8]) -> bool {
        match self.backend {
            Backend::Virtio(0) => cache_read(base, out),
            Backend::Virtio(dev) => crate::virtio_blk::read_io_dev(dev, base, out),
            Backend::Nvme => crate::nvme::read_sectors(base, out),
            Backend::Ahci(idx) => crate::ahci::read_sectors(idx, base, out),
        }
    }

    /// Write one 4 KiB block starting at 512-byte sector `base` to the backend.
    fn backend_write(&self, base: u64, data: &[u8]) -> bool {
        match self.backend {
            Backend::Virtio(0) => cache_write(base, data),
            Backend::Virtio(dev) => crate::virtio_blk::write_io_dev(dev, base, data),
            Backend::Nvme => crate::nvme::write_sectors(base, data),
            Backend::Ahci(idx) => crate::ahci::write_sectors(idx, base, data),
        }
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
            if !self.backend_read(base, &mut buf[o..o + BS]) {
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
            if !self.backend_write(base, &buf[o..o + BS]) {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        if self.on_disk {
            let ok = match self.backend {
                Backend::Virtio(0) => {
                    cache_flush(); // dirty cache blocks to disk (EuroFS checkpoint commit)
                    // Then force the disk's OWN write-back cache to the persistent
                    // medium (VIRTIO_BLK_T_FLUSH) — makes the A/B-superblock barrier a hard
                    // I/O barrier. No-op if the device has no volatile cache.
                    crate::virtio_blk::flush()
                }
                Backend::Virtio(dev) => crate::virtio_blk::flush_dev(dev),
                // NVMe writes here are synchronous (we poll for completion) and
                // AHCI writes are polled DMA — both are durable on completion.
                Backend::Nvme | Backend::Ahci(_) => true,
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn discard(&mut self, start: u64, count: u32) -> BlockResult<()> {
        // TRIM only makes sense on a real disk; the RAM ramdisk ignores it.
        if self.on_disk {
            if let Backend::Virtio(dev) = self.backend {
                let sector = self.part_start + start * SPB;
                let nsec = count as u64 * SPB;
                // Advisory: discard_dev is a no-op if the device lacks VIRTIO_BLK_F_DISCARD.
                crate::virtio_blk::discard_dev(dev, sector, nsec as u32);
            }
        }
        Ok(())
    }
}

// ── The boot disk, bus-agnostic, for everything outside the root filesystem ──
// (GPT, the ESP `slot_config`, the A/B slot partitions, the raw slot block).
// Reads and writes go STRAIGHT to the device, never through the root cache: those
// sectors are outside the root partition, so there is nothing to keep coherent,
// and a read-back after a write must see the medium, not a cache line.
/// `None` = root not chosen yet; `Some(None)` = RAM root, there is NO boot disk
/// (a foreign data disk on virtio 0 must never receive slot_config writes);
/// `Some(Some(b))` = the disk the root lives on.
static BOOT: spin::Mutex<Option<Option<Backend>>> = spin::Mutex::new(None);
/// Did this boot mount its root from an installed disk (as opposed to the
/// live/RAM root of the install medium)?
pub fn booted_from_disk() -> bool {
    matches!(*BOOT.lock(), Some(Some(_)))
}

fn boot_backend() -> Option<Backend> {
    match *BOOT.lock() {
        Some(chosen) => chosen,
        // Before the root is chosen (the GPT lookup that chooses it runs first) the
        // candidate is virtio-blk 0 when there is one: read-only use in practice.
        None => if crate::virtio_blk::present() { Some(Backend::Virtio(0)) } else { None },
    }
}
/// Is there a disk-backed root at all (an installed system)?
pub fn boot_present() -> bool {
    boot_backend().is_some()
}
/// Read whole 512-byte sectors from the boot disk (`buf.len()` multiple of 512, ≤ 4 KiB).
pub fn boot_read(sector: u64, buf: &mut [u8]) -> bool {
    match boot_backend() {
        Some(Backend::Virtio(dev)) => crate::virtio_blk::read_io_dev(dev, sector, buf),
        Some(Backend::Nvme) => crate::nvme::read_sectors(sector, buf),
        Some(Backend::Ahci(idx)) => crate::ahci::read_sectors(idx, sector, buf),
        None => false,
    }
}
/// Write whole 512-byte sectors to the boot disk.
pub fn boot_write(sector: u64, buf: &[u8]) -> bool {
    match boot_backend() {
        Some(Backend::Virtio(dev)) => crate::virtio_blk::write_io_dev(dev, sector, buf),
        Some(Backend::Nvme) => crate::nvme::write_sectors(sector, buf),
        Some(Backend::Ahci(idx)) => crate::ahci::write_sectors(idx, sector, buf),
        None => false,
    }
}
/// Force the boot disk's volatile write cache to the medium (virtio FLUSH; the
/// NVMe and AHCI drivers complete writes synchronously).
pub fn boot_flush() -> bool {
    match boot_backend() {
        Some(Backend::Virtio(dev)) => crate::virtio_blk::flush_dev(dev),
        Some(_) => true,
        None => false,
    }
}

// ── The disks of this machine, whatever their bus (BUG-055) ─────────────────

/// One disk as the Storage page and `lsblk` show it.
pub struct DiskInfo {
    /// `vblk0`, `sata0`, `nvme0`, `raid0`, `usb0`.
    pub name: alloc::string::String,
    pub bytes: u64,
    /// The bus or driver, in words: "virtio-blk", "SATA (AHCI)", "NVMe", ...
    pub bus: &'static str,
    /// What the disk calls itself; empty when the bus has no such string.
    pub model: alloc::string::String,
    /// The root file system of this boot lives on this disk.
    pub system: bool,
}

/// Every disk a kernel driver has brought up: virtio-blk, SATA (AHCI), NVMe,
/// MegaRAID logical drives and a USB mass-storage device. Sizes come from the
/// drivers; nothing is read from the disks.
pub fn all_disks() -> Vec<DiskInfo> {
    use alloc::format;
    use alloc::string::String;
    let boot = *BOOT.lock();
    let is_boot = |b: Backend| match (boot, b) {
        (Some(Some(Backend::Virtio(a))), Backend::Virtio(c)) => a == c,
        (Some(Some(Backend::Nvme)), Backend::Nvme) => true,
        (Some(Some(Backend::Ahci(a))), Backend::Ahci(c)) => a == c,
        _ => false,
    };
    let mut out = Vec::new();
    for dev in 0..crate::virtio_blk::device_count() {
        if crate::virtio_blk::present_dev(dev) {
            out.push(DiskInfo {
                name: format!("vblk{dev}"),
                bytes: crate::virtio_blk::capacity_sectors_dev(dev) * 512,
                bus: "virtio-blk",
                model: String::new(),
                system: is_boot(Backend::Virtio(dev)),
            });
        }
    }
    for idx in 0..crate::ahci::DISK_SLOTS {
        let sectors = crate::ahci::disk_sectors(idx);
        if sectors > 0 {
            out.push(DiskInfo {
                name: format!("sata{idx}"),
                bytes: sectors * 512,
                bus: "SATA (AHCI)",
                model: crate::ahci::disk_model(idx),
                system: is_boot(Backend::Ahci(idx)),
            });
        }
    }
    if crate::nvme::present() {
        out.push(DiskInfo {
            name: String::from("nvme0"),
            bytes: crate::nvme::capacity_sectors() * 512,
            bus: "NVMe",
            model: crate::nvme::model(),
            system: is_boot(Backend::Nvme),
        });
    }
    for ld in 0..8 {
        if let Some(sectors) = crate::mrblk::disk_sectors(ld) {
            out.push(DiskInfo {
                name: format!("raid{ld}"),
                bytes: sectors * 512,
                bus: "MegaRAID logical drive",
                model: String::new(),
                system: false,
            });
        }
    }
    if crate::xhci::usb_disk_present() {
        out.push(DiskInfo {
            name: String::from("usb0"),
            bytes: crate::xhci::usb_block_count() * 512,
            bus: "USB",
            model: String::new(),
            system: false,
        });
    }
    out
}

/// "20.0 GiB", "256 MiB", "1.7 MiB": one decimal below 100 of a unit, rounded
/// to the nearest, so two sizes and their ratio read consistently.
pub fn human_bytes(bytes: u64) -> alloc::string::String {
    use alloc::format;
    const K: u64 = 1024;
    const M: u64 = K * K;
    const G: u64 = M * K;
    let whole = |unit: u64| (bytes as u128 + unit as u128 / 2) / unit as u128;
    let tenths = |unit: u64| (bytes as u128 * 10 + unit as u128 / 2) / unit as u128;
    if bytes >= 100 * G {
        format!("{} GiB", whole(G))
    } else if bytes >= G {
        let t = tenths(G);
        format!("{}.{} GiB", t / 10, t % 10)
    } else if bytes >= 100 * M {
        format!("{} MiB", whole(M))
    } else if bytes >= M {
        let t = tenths(M);
        format!("{}.{} MiB", t / 10, t % 10)
    } else if bytes >= K {
        format!("{} KiB", whole(K))
    } else {
        format!("{bytes} bytes")
    }
}

/// `[disks]` boot self-test: the size formatting of the Storage page (the
/// "1 MiB of 8 MiB used (21%)" of BUG-055) and the disk list of this boot.
pub fn disks_selftest() {
    let fmt = human_bytes(1_782_579) == "1.7 MiB"
        && human_bytes(8_388_608) == "8.0 MiB"
        && human_bytes(0) == "0 bytes"
        && human_bytes(1023) == "1023 bytes"
        && human_bytes(4096) == "4 KiB"
        && human_bytes(268_435_456) == "256 MiB"
        && human_bytes(21_474_836_480) == "20.0 GiB"
        && human_bytes(1_610_612_736) == "1.5 GiB"
        && human_bytes(500_107_862_016) == "466 GiB";
    let disks = all_disks();
    let named = disks.iter().all(|d| !d.name.is_empty() && d.bytes > 0 && !d.bus.is_empty());
    let one_system = disks.iter().filter(|d| d.system).count() <= 1;
    let consistent = booted_from_disk() == disks.iter().any(|d| d.system);
    let list: Vec<alloc::string::String> = disks.iter().map(|d| alloc::format!("{} {} {}", d.name, human_bytes(d.bytes), d.bus)).collect();
    crate::serial_println!(
        "[disks] sizes formatted={fmt}, {} disk(s) [{}], each with name, size and bus={named}, system disk marked consistently={} → {}",
        disks.len(),
        list.join("; "),
        one_system && consistent,
        if fmt && named && one_system && consistent { "OK ✓" } else { "FAILED ✗" }
    );
}
