//! AHCI/SATA class driver (Metal M2-2, docs/SPRINT-PLAN-METAL.md).
//!
//! One from-scratch driver for the AHCI 1.3 class standard covers essentially
//! every SATA controller on modern machines (Intel/AMD chipsets and q35's
//! built-in ICH9). Scope, honestly: port bring-up, IDENTIFY DEVICE, and
//! polled READ/WRITE DMA EXT with a single-entry PRDT over a contiguous
//! 64 KiB DMA window. No NCQ, no hotplug, no port multipliers yet.
//!
//! Safety rule for the boot self-test: the q35 built-in controller usually
//! carries the BOOT MEDIUM on port 0. We therefore only WRITE-test a disk
//! whose sector 0 has no MBR/GPT boot signature (a blank scratch disk, like
//! the metal matrix attaches); a partitioned disk gets a read-only test.
//!
//! Memory is identity-mapped (physical = virtual) for MMIO and DMA, like the
//! other drivers.

use euromm::FrameAllocator;

use crate::pci;

// HBA (ABAR) registers.
const HBA_CAP: u64 = 0x00;
const HBA_GHC: u64 = 0x04;
const HBA_PI: u64 = 0x0C;
// Per-port register block: ABAR + 0x100 + port * 0x80.
const PX_CLB: u64 = 0x00;
const PX_CLBU: u64 = 0x04;
const PX_FB: u64 = 0x08;
const PX_FBU: u64 = 0x0C;
const PX_IS: u64 = 0x10;
const PX_CMD: u64 = 0x18;
const PX_TFD: u64 = 0x20;
const PX_SIG: u64 = 0x24;
const PX_SSTS: u64 = 0x28;
const PX_SERR: u64 = 0x30;
const PX_CI: u64 = 0x38;

const CMD_ST: u32 = 1 << 0; // start processing the command list
const CMD_FRE: u32 = 1 << 4; // FIS receive enable
const CMD_FR: u32 = 1 << 14; // FIS receive running
const CMD_CR: u32 = 1 << 15; // command list running

const SIG_SATA_DISK: u32 = 0x0000_0101;

/// 64 KiB contiguous DMA window (16 frames): 128 sectors per command through
/// one PRDT entry.
const DATA_MAX: usize = 64 * 1024;

#[inline]
unsafe fn rd32(a: u64) -> u32 {
    (a as *const u32).read_volatile()
}
#[inline]
unsafe fn wr32(a: u64, v: u32) {
    (a as *mut u32).write_volatile(v);
}

/// One brought-up SATA disk behind an AHCI port.
#[derive(Clone, Copy)]
pub struct AhciDisk {
    port: u64,     // port register base (ABAR + 0x100 + n*0x80)
    clb: u64,      // command list (1 KiB)
    ctba: u64,     // command table (CFIS + PRDT)
    data: u64,     // DMA data window (DATA_MAX bytes, contiguous)
    pub sectors: u64, // capacity in 512-byte LBAs
    pub port_no: u8,
    pub model: [u8; 40],
    pub partitioned: bool, // sector 0 carries a boot signature (don't write-test)
}

const MAX_DISKS: usize = 4;
static mut DISKS: [Option<AhciDisk>; MAX_DISKS] = [None, None, None, None];

/// Initialize every AHCI controller; bring up each attached SATA disk.
/// Returns the number of disks found.
pub fn init(falloc: &mut FrameAllocator) -> usize {
    let mut found = 0usize;
    for dev in pci::enumerate()
        .into_iter()
        .filter(|d| d.class == 0x01 && d.subclass == 0x06 && d.prog_if == 0x01)
    {
        dev.enable(0x6); // memory space + bus master
        let abar = dev.bar_addr(5);
        if abar == 0 {
            continue;
        }
        pci::claim(dev.bus, dev.dev, dev.func, "ahci"); // hwprobe (M1-3)
        unsafe {
            // AHCI-enable (GHC.AE); leave interrupts off — the driver polls.
            wr32(abar + HBA_GHC, rd32(abar + HBA_GHC) | (1 << 31));
            let pi = rd32(abar + HBA_PI);
            let nports = ((rd32(abar + HBA_CAP) & 0x1F) + 1).min(32);
            for p in 0..nports {
                if found >= MAX_DISKS || pi & (1 << p) == 0 {
                    continue;
                }
                let port = abar + 0x100 + (p as u64) * 0x80;
                // Device detected + PHY up? (SSTS.DET == 3)
                if rd32(port + PX_SSTS) & 0xF != 3 {
                    continue;
                }
                if rd32(port + PX_SIG) != SIG_SATA_DISK {
                    continue; // ATAPI etc.: out of scope
                }
                if let Some(d) = setup_port(port, p as u8, falloc) {
                    (*core::ptr::addr_of_mut!(DISKS))[found] = Some(d);
                    found += 1;
                }
            }
        }
    }
    found
}

/// Bring up one port: stop it, install command list + FIS + command table,
/// restart it, IDENTIFY the disk.
unsafe fn setup_port(port: u64, port_no: u8, falloc: &mut FrameAllocator) -> Option<AhciDisk> {
    // Stop the port cleanly (spec 10.1.2): ST=0 → wait !CR, FRE=0 → wait !FR.
    let mut cmd = rd32(port + PX_CMD);
    cmd &= !CMD_ST;
    wr32(port + PX_CMD, cmd);
    for _ in 0..1_000_000 {
        if rd32(port + PX_CMD) & CMD_CR == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    cmd = rd32(port + PX_CMD) & !CMD_FRE;
    wr32(port + PX_CMD, cmd);
    for _ in 0..1_000_000 {
        if rd32(port + PX_CMD) & CMD_FR == 0 {
            break;
        }
        core::hint::spin_loop();
    }

    // One frame carries the command list (1 KiB @ +0), the received-FIS area
    // (256 B @ +1024) and the command table (@ +2048: 64 B CFIS + PRDT @ +0x80).
    let meta = falloc.allocate().ok()?;
    core::ptr::write_bytes(meta as *mut u8, 0, 4096);
    let clb = meta;
    let fb = meta + 1024;
    let ctba = meta + 2048;
    let data = falloc.allocate_aligned(DATA_MAX / 4096, 1).ok()?;
    core::ptr::write_bytes(data as *mut u8, 0, DATA_MAX);

    wr32(port + PX_CLB, (clb & 0xFFFF_FFFF) as u32);
    wr32(port + PX_CLBU, (clb >> 32) as u32);
    wr32(port + PX_FB, (fb & 0xFFFF_FFFF) as u32);
    wr32(port + PX_FBU, (fb >> 32) as u32);
    wr32(port + PX_SERR, 0xFFFF_FFFF); // clear sticky errors
    wr32(port + PX_IS, 0xFFFF_FFFF);

    // Restart: FRE first, then ST.
    wr32(port + PX_CMD, rd32(port + PX_CMD) | CMD_FRE);
    wr32(port + PX_CMD, rd32(port + PX_CMD) | CMD_ST);

    let mut d = AhciDisk {
        port,
        clb,
        ctba,
        data,
        sectors: 0,
        port_no,
        model: [0; 40],
        partitioned: false,
    };

    // IDENTIFY DEVICE (0xEC): 512 B of device info into the data window.
    if !d.command(0xEC, 0, 0, 512, false) {
        return None;
    }
    let id = core::slice::from_raw_parts(d.data as *const u8, 512);
    // Model: words 27..46, each word byte-swapped ASCII.
    for i in 0..20 {
        d.model[i * 2] = id[27 * 2 + i * 2 + 1];
        d.model[i * 2 + 1] = id[27 * 2 + i * 2];
    }
    // LBA48 capacity: words 100..103.
    d.sectors = u64::from_le_bytes(id[200..208].try_into().ok()?);
    if d.sectors == 0 {
        return None;
    }
    // Boot-signature probe (decides write-test safety; also useful info).
    if d.command(0x25, 0, 1, 512, false) {
        let s0 = core::slice::from_raw_parts(d.data as *const u8, 512);
        d.partitioned = s0[510] == 0x55 && s0[511] == 0xAA;
    }
    Some(d)
}

impl AhciDisk {
    /// Issue one polled ATA command through slot 0 with a single-entry PRDT of
    /// `bytes` over the DMA window. Returns success.
    unsafe fn command(&self, ata: u8, lba: u64, count: u16, bytes: usize, write: bool) -> bool {
        // Command header 0 (32 B): CFIS length 5 dwords, W for writes, PRDTL=1.
        let hdr = self.clb;
        wr32(hdr, 5 | ((write as u32) << 6) | (1 << 16));
        wr32(hdr + 4, 0); // PRDBC (byte count transferred, device-updated)
        wr32(hdr + 8, (self.ctba & 0xFFFF_FFFF) as u32);
        wr32(hdr + 12, (self.ctba >> 32) as u32);

        // CFIS: H2D register FIS, LBA48.
        let f = self.ctba as *mut u8;
        core::ptr::write_bytes(f, 0, 64);
        f.write_volatile(0x27); // FIS type: host-to-device
        f.add(1).write_volatile(0x80); // C=1: command register update
        f.add(2).write_volatile(ata);
        f.add(4).write_volatile(lba as u8);
        f.add(5).write_volatile((lba >> 8) as u8);
        f.add(6).write_volatile((lba >> 16) as u8);
        f.add(7).write_volatile(0x40); // device: LBA mode
        f.add(8).write_volatile((lba >> 24) as u8);
        f.add(9).write_volatile((lba >> 32) as u8);
        f.add(10).write_volatile((lba >> 40) as u8);
        f.add(12).write_volatile(count as u8);
        f.add(13).write_volatile((count >> 8) as u8);

        // PRDT entry 0 @ ctba+0x80: the whole transfer in one entry (≤ 64 KiB).
        let prdt = self.ctba + 0x80;
        wr32(prdt, (self.data & 0xFFFF_FFFF) as u32);
        wr32(prdt + 4, (self.data >> 32) as u32);
        wr32(prdt + 12, (bytes as u32 - 1) & 0x3F_FFFF); // DBC is 0-based

        wr32(self.port + PX_IS, 0xFFFF_FFFF);
        wr32(self.port + PX_CI, 1); // issue slot 0
        for _ in 0..30_000_000u64 {
            if rd32(self.port + PX_CI) & 1 == 0 {
                // Task-file error bit set? (PxTFD.STS.ERR)
                return rd32(self.port + PX_TFD) & 0x01 == 0;
            }
            if rd32(self.port + PX_IS) & (1 << 30) != 0 {
                return false; // task-file error interrupt status
            }
            core::hint::spin_loop();
        }
        false // timeout
    }
}

fn with_disk<R>(idx: usize, f: impl FnOnce(&AhciDisk) -> R) -> Option<R> {
    unsafe { (*core::ptr::addr_of!(DISKS)).get(idx)?.as_ref().map(f) }
}

pub fn disk_count() -> usize {
    unsafe { (*core::ptr::addr_of!(DISKS)).iter().filter(|d| d.is_some()).count() }
}

pub fn disk_sectors(idx: usize) -> u64 {
    with_disk(idx, |d| d.sectors).unwrap_or(0)
}

pub fn disk_partitioned(idx: usize) -> bool {
    with_disk(idx, |d| d.partitioned).unwrap_or(true)
}

/// Read `buf.len()` bytes from 512-byte LBA `lba` on disk `idx` (chunked).
pub fn read_sectors(idx: usize, lba: u64, buf: &mut [u8]) -> bool {
    unsafe {
        let d = match (*core::ptr::addr_of!(DISKS)).get(idx).and_then(|d| d.as_ref()) {
            Some(d) => d,
            None => return false,
        };
        let mut done = 0usize;
        while done < buf.len() {
            let n = (buf.len() - done).min(DATA_MAX);
            let nsec = n.div_ceil(512).max(1);
            if !d.command(0x25, lba + (done / 512) as u64, nsec as u16, nsec * 512, false) {
                return false;
            }
            core::ptr::copy_nonoverlapping(d.data as *const u8, buf.as_mut_ptr().add(done), n);
            done += n;
        }
        true
    }
}

/// Write `buf.len()` bytes at 512-byte LBA `lba` on disk `idx` (chunked).
pub fn write_sectors(idx: usize, lba: u64, buf: &[u8]) -> bool {
    unsafe {
        let d = match (*core::ptr::addr_of!(DISKS)).get(idx).and_then(|d| d.as_ref()) {
            Some(d) => d,
            None => return false,
        };
        let mut done = 0usize;
        while done < buf.len() {
            let n = (buf.len() - done).min(DATA_MAX);
            core::ptr::copy_nonoverlapping(buf.as_ptr().add(done), d.data as *mut u8, n);
            let nsec = n.div_ceil(512).max(1);
            if n % 512 != 0 {
                core::ptr::write_bytes((d.data + n as u64) as *mut u8, 0, nsec * 512 - n);
            }
            if !d.command(0x35, lba + (done / 512) as u64, nsec as u16, nsec * 512, true) {
                return false;
            }
            done += n;
        }
        true
    }
}

/// EuroFS `BlockDevice` over one AHCI disk (4 KiB blocks), mirroring NvmeBlock:
/// SATA disks become mountable EuroFS carriers.
#[derive(Clone, Copy)]
pub struct AhciBlock {
    idx: usize,
    blocks: u64,
}

impl AhciBlock {
    pub fn new(idx: usize) -> Option<Self> {
        let sectors = disk_sectors(idx);
        if sectors == 0 {
            return None;
        }
        Some(AhciBlock { idx, blocks: sectors / 8 })
    }
}

impl eurofs::BlockDevice for AhciBlock {
    fn block_size(&self) -> u32 {
        4096
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> eurofs::BlockResult<()> {
        let n = count as usize * 4096;
        if !read_sectors(self.idx, start * 8, &mut buf[..n]) {
            return Err(eurofs::BlockError::IoError);
        }
        Ok(())
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> eurofs::BlockResult<()> {
        let n = count as usize * 4096;
        if !write_sectors(self.idx, start * 8, &buf[..n]) {
            return Err(eurofs::BlockError::IoError);
        }
        Ok(())
    }
    fn flush(&mut self) -> eurofs::BlockResult<()> {
        Ok(()) // polled writes: durable when the command completes
    }
}

/// Boot self-test. For every disk: IDENTIFY summary; content-read proof on
/// partitioned disks (the boot medium: never written); a full write/read/verify
/// (single sector + 64 KiB PRDT window) on blank disks only.
pub fn self_test() {
    for idx in 0..MAX_DISKS {
        let (sectors, partitioned, port_no, model) = match with_disk(idx, |d| {
            (d.sectors, d.partitioned, d.port_no, d.model)
        }) {
            Some(t) => t,
            None => continue,
        };
        let model_s = core::str::from_utf8(&model).unwrap_or("?").trim();
        crate::serial_println!(
            "[ahci] disk {idx} (port {port_no}): '{model_s}', {} sectors = {} MiB, {}",
            sectors,
            sectors * 512 / (1024 * 1024),
            if partitioned { "partitioned (boot signature)" } else { "blank" }
        );
        if partitioned {
            // Read-only proof on a disk that carries data: sector 0 must keep
            // its boot signature through our DMA path.
            let mut s0 = [0u8; 512];
            let ok = read_sectors(idx, 0, &mut s0) && s0[510] == 0x55 && s0[511] == 0xAA;
            crate::serial_println!(
                "[ahci] disk {idx} read-only self-test (boot sector via DMA): {}",
                if ok { "OK ✓" } else { "FAILED ✗" }
            );
            continue;
        }
        // Blank disk: full write/read/verify, single sector + 64 KiB window.
        let mut one = [0u8; 512];
        for (i, b) in one.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x5A;
        }
        let mut one_rd = [0u8; 512];
        let ok1 = write_sectors(idx, 1000, &one) && read_sectors(idx, 1000, &mut one_rd) && one == one_rd;
        let mut big = alloc::vec![0u8; DATA_MAX];
        for (i, b) in big.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17) ^ (i >> 9) as u8;
        }
        let mut big_rd = alloc::vec![0u8; DATA_MAX];
        let ok2 = write_sectors(idx, 2000, &big) && read_sectors(idx, 2000, &mut big_rd) && big == big_rd;
        crate::serial_println!(
            "[ahci] disk {idx} self-test write/read: sector {} · 64 KiB {}",
            if ok1 { "OK ✓" } else { "MISMATCH ✗" },
            if ok2 { "OK ✓" } else { "MISMATCH ✗" }
        );
    }
}
