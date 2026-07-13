//! Minimal NVMe driver (NVM Express 1.4) — B2. Enough to find an NVMe disk,
//! initialize the controller (admin + I/O queues), identify it,
//! and read/write blocks via PRP. Polling instead of interrupts (simple +
//! reliable for early boot). Memory is identity-mapped, so physical = virtual
//! for both the MMIO registers and the DMA queues/buffers.

use euromm::FrameAllocator;

// Controller registers (offsets from BAR0).
const REG_CAP: u64 = 0x00; // 64-bit capabilities
const REG_CC: u64 = 0x14; // controller configuration
const REG_CSTS: u64 = 0x1C; // controller status
const REG_AQA: u64 = 0x24; // admin queue attributes
const REG_ASQ: u64 = 0x28; // admin submission queue base (64-bit)
const REG_ACQ: u64 = 0x30; // admin completion queue base (64-bit)

const QDEPTH: usize = 8; // queue depth (entries) — small but sufficient
const SQE_SIZE: usize = 64; // submission-queue-entry
const CQE_SIZE: usize = 16; // completion-queue-entry

struct Queue {
    sq: u64,        // physical address submission queue
    cq: u64,        // physical address completion queue
    sq_tail: u32,   // next free SQ slot
    cq_head: u32,   // next CQ slot to read
    cq_phase: u16,  // expected phase tag (start 1)
    sq_db: u64,     // SQ tail doorbell address
    cq_db: u64,     // CQ head doorbell address
}

pub struct Nvme {
    mmio: u64,
    capacity: u64, // namespace size in LBAs
    lba_bytes: u32,
    admin: Queue,
    io: Queue,
    data: u64, // 4 KiB DMA data buffer
    next_cid: u16,
    model: [u8; 40],
}

static mut NVME: Option<Nvme> = None;

#[inline]
unsafe fn rd32(addr: u64) -> u32 {
    (addr as *const u32).read_volatile()
}
#[inline]
unsafe fn wr32(addr: u64, v: u32) {
    (addr as *mut u32).write_volatile(v);
}
#[inline]
unsafe fn rd64(addr: u64) -> u64 {
    (addr as *const u64).read_volatile()
}
#[inline]
unsafe fn wr64(addr: u64, v: u64) {
    (addr as *mut u64).write_volatile(v);
}

impl Queue {
    fn new(sq: u64, cq: u64, sq_db: u64, cq_db: u64) -> Self {
        Queue { sq, cq, sq_tail: 0, cq_head: 0, cq_phase: 1, sq_db, cq_db }
    }

    /// Place a 64-byte command (16 dwords) in the SQ and ring the doorbell.
    unsafe fn submit(&mut self, cmd: &[u32; 16]) {
        let slot = self.sq + (self.sq_tail as u64) * SQE_SIZE as u64;
        for (i, &w) in cmd.iter().enumerate() {
            wr32(slot + (i * 4) as u64, w);
        }
        self.sq_tail = (self.sq_tail + 1) % QDEPTH as u32;
        wr32(self.sq_db, self.sq_tail);
    }

    /// Wait (polling) for the next completion; return the status field
    /// (0 = success), or None on time-out.
    unsafe fn wait(&mut self) -> Option<u16> {
        let entry = self.cq + (self.cq_head as u64) * CQE_SIZE as u64;
        for _ in 0..8_000_000u64 {
            let dw3 = rd32(entry + 12);
            let phase = ((dw3 >> 16) & 1) as u16;
            if phase == self.cq_phase {
                let status = (dw3 >> 17) as u16; // status field (SC+SCT)
                self.cq_head = (self.cq_head + 1) % QDEPTH as u32;
                if self.cq_head == 0 {
                    self.cq_phase ^= 1; // phase flips at wrap
                }
                wr32(self.cq_db, self.cq_head);
                return Some(status);
            }
            core::hint::spin_loop();
        }
        None
    }
}

/// Initialize the first NVMe controller. Returns false if there is no NVMe device.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let dev = match crate::pci::find(|d| d.class == 0x01 && d.subclass == 0x08 && d.prog_if == 0x02) {
        Some(d) => d,
        None => return false,
    };
    crate::pci::claim(dev.bus, dev.dev, dev.func, "nvme"); // hwprobe (M1-3)
    dev.enable(0x6); // bus-master + memory space
    let bar0 = dev.bar(0) as u64 & 0xFFFF_FFF0;
    let bar1 = dev.bar(1) as u64;
    let mmio = (bar1 << 32) | bar0;
    if mmio == 0 {
        return false;
    }

    unsafe {
        let cap = rd64(mmio + REG_CAP);
        let dstrd = ((cap >> 32) & 0xF) as u64; // doorbell stride
        let db_base = mmio + 0x1000;
        let db_stride = 4u64 << dstrd;

        // Reset: CC.EN=0, wait until CSTS.RDY=0.
        wr32(mmio + REG_CC, 0);
        for _ in 0..5_000_000u64 {
            if rd32(mmio + REG_CSTS) & 1 == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // Allocate admin queues (each one 4 KiB frame, zeroed).
        let asq = match falloc.allocate() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let acq = match falloc.allocate() {
            Ok(a) => a,
            Err(_) => return false,
        };
        core::ptr::write_bytes(asq as *mut u8, 0, 4096);
        core::ptr::write_bytes(acq as *mut u8, 0, 4096);

        // AQA: admin SQ/CQ size (0-based). ASQ/ACQ: base addresses.
        wr32(mmio + REG_AQA, (((QDEPTH - 1) as u32) << 16) | (QDEPTH - 1) as u32);
        wr64(mmio + REG_ASQ, asq);
        wr64(mmio + REG_ACQ, acq);

        // CC: IOSQES=6 (64B), IOCQES=4 (16B), MPS=0 (4KB), CSS=0 (NVM), EN=1.
        let cc = (6u32 << 16) | (4u32 << 20) | (0u32 << 7) | (0u32 << 4) | 1;
        wr32(mmio + REG_CC, cc);
        for _ in 0..5_000_000u64 {
            if rd32(mmio + REG_CSTS) & 1 == 1 {
                break;
            }
            core::hint::spin_loop();
        }
        if rd32(mmio + REG_CSTS) & 1 != 1 {
            return false; // controller not ready
        }

        let admin = Queue::new(asq, acq, db_base, db_base + db_stride);
        let data = match falloc.allocate() {
            Ok(a) => a,
            Err(_) => return false,
        };
        core::ptr::write_bytes(data as *mut u8, 0, 4096);

        let mut nv = Nvme {
            mmio,
            capacity: 0,
            lba_bytes: 512,
            admin,
            io: Queue::new(0, 0, 0, 0), // gets filled in shortly
            data,
            next_cid: 1,
            model: [0; 40],
        };

        // Identify Controller (CNS=1) → model string (bytes 24..64).
        if !nv.identify(1, 0) {
            crate::serial_println!("[nvme] Identify Controller FAILED");
            return false;
        }
        nv.model.copy_from_slice(&core::slice::from_raw_parts(data as *const u8, 4096)[24..64]);

        // Identify Namespace 1 (CNS=0) → capacity + LBA size.
        if !nv.identify(0, 1) {
            return false;
        }
        let nsdata = core::slice::from_raw_parts(data as *const u8, 4096);
        nv.capacity = u64::from_le_bytes(nsdata[0..8].try_into().unwrap()); // NSZE (LBAs)
        let flbas = nsdata[26] & 0xF; // current LBA format index
        let lbaf_off = 128 + (flbas as usize) * 4;
        let lbads = nsdata[lbaf_off + 2]; // log2(LBA size)
        if (9..=12).contains(&lbads) {
            nv.lba_bytes = 1u32 << lbads;
        }

        // Create I/O completion + submission queue (qid 1).
        let iocq = match falloc.allocate() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let iosq = match falloc.allocate() {
            Ok(a) => a,
            Err(_) => return false,
        };
        core::ptr::write_bytes(iocq as *mut u8, 0, 4096);
        core::ptr::write_bytes(iosq as *mut u8, 0, 4096);
        nv.io = Queue::new(
            iosq,
            iocq,
            db_base + 2 * db_stride, // SQ1 tail doorbell
            db_base + 3 * db_stride, // CQ1 head doorbell
        );
        if !nv.create_io_cq(iocq) || !nv.create_io_sq(iosq) {
            return false;
        }

        let model = core::str::from_utf8(&nv.model).unwrap_or("?").trim();
        crate::serial_println!(
            "[nvme] controller OK — model '{}', {} LBAs × {} B = {} MiB",
            model,
            nv.capacity,
            nv.lba_bytes,
            nv.capacity * nv.lba_bytes as u64 / (1024 * 1024)
        );
        NVME = Some(nv);
    }
    true
}

impl Nvme {
    fn cid(&mut self) -> u16 {
        let c = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1).max(1);
        c
    }

    /// Identify (admin opcode 0x06): write 4 KiB to `self.data`. cns=1
    /// (controller) or 0 (namespace `nsid`).
    unsafe fn identify(&mut self, cns: u32, nsid: u32) -> bool {
        let mut cmd = [0u32; 16];
        cmd[0] = 0x06 | ((self.cid() as u32) << 16);
        cmd[1] = nsid;
        cmd[6] = (self.data & 0xFFFF_FFFF) as u32; // PRP1 low
        cmd[7] = (self.data >> 32) as u32; // PRP1 high
        cmd[10] = cns;
        self.admin.submit(&cmd);
        matches!(self.admin.wait(), Some(0))
    }

    unsafe fn create_io_cq(&mut self, cq: u64) -> bool {
        let mut cmd = [0u32; 16];
        cmd[0] = 0x05 | ((self.cid() as u32) << 16); // Create I/O CQ
        cmd[6] = (cq & 0xFFFF_FFFF) as u32;
        cmd[7] = (cq >> 32) as u32;
        cmd[10] = (((QDEPTH - 1) as u32) << 16) | 1; // qsize-1 | qid=1
        cmd[11] = 1; // PC=1, interrupts off
        self.admin.submit(&cmd);
        matches!(self.admin.wait(), Some(0))
    }

    unsafe fn create_io_sq(&mut self, sq: u64) -> bool {
        let mut cmd = [0u32; 16];
        cmd[0] = 0x01 | ((self.cid() as u32) << 16); // Create I/O SQ
        cmd[6] = (sq & 0xFFFF_FFFF) as u32;
        cmd[7] = (sq >> 32) as u32;
        cmd[10] = (((QDEPTH - 1) as u32) << 16) | 1; // qsize-1 | qid=1
        cmd[11] = (1u32 << 16) | 1; // CQID=1 | PC=1
        self.admin.submit(&cmd);
        matches!(self.admin.wait(), Some(0))
    }

    /// I/O Read(0x02)/Write(0x01) of `nlb` LBAs from `slba` via the data buffer.
    unsafe fn rw(&mut self, write: bool, slba: u64, nlb: u16) -> bool {
        let mut cmd = [0u32; 16];
        cmd[0] = if write { 0x01 } else { 0x02 } | ((self.cid() as u32) << 16);
        cmd[1] = 1; // NSID
        cmd[6] = (self.data & 0xFFFF_FFFF) as u32; // PRP1
        cmd[7] = (self.data >> 32) as u32;
        cmd[10] = (slba & 0xFFFF_FFFF) as u32; // SLBA low
        cmd[11] = (slba >> 32) as u32; // SLBA high
        cmd[12] = (nlb - 1) as u32; // 0-based number of LBAs
        self.io.submit(&cmd);
        matches!(self.io.wait(), Some(0))
    }
}

const DATA_MAX: usize = 4096;

/// Read `buf.len()` bytes (≤ 4096) from byte-LBA `lba` (512-byte sectors).
pub fn read_sectors(lba: u64, buf: &mut [u8]) -> bool {
    unsafe {
        let nv = match (*core::ptr::addr_of_mut!(NVME)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        let n = buf.len().min(DATA_MAX);
        let nlb = ((n + nv.lba_bytes as usize - 1) / nv.lba_bytes as usize).max(1) as u16;
        if !nv.rw(false, lba, nlb) {
            return false;
        }
        core::ptr::copy_nonoverlapping(nv.data as *const u8, buf.as_mut_ptr(), n);
        true
    }
}

/// Write `buf.len()` bytes (≤ 4096) from byte-LBA `lba`.
pub fn write_sectors(lba: u64, buf: &[u8]) -> bool {
    unsafe {
        let nv = match (*core::ptr::addr_of_mut!(NVME)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        let n = buf.len().min(DATA_MAX);
        core::ptr::copy_nonoverlapping(buf.as_ptr(), nv.data as *mut u8, n);
        let dlen = (n + nv.lba_bytes as usize - 1) / nv.lba_bytes as usize * nv.lba_bytes as usize;
        if dlen > n {
            core::ptr::write_bytes((nv.data + n as u64) as *mut u8, 0, dlen - n);
        }
        let nlb = (dlen / nv.lba_bytes as usize).max(1) as u16;
        nv.rw(true, lba, nlb)
    }
}

pub fn present() -> bool {
    unsafe { (*core::ptr::addr_of!(NVME)).is_some() }
}

/// Capacity in 512-byte sectors (normalized regardless of the LBA size).
pub fn capacity_sectors() -> u64 {
    unsafe {
        (*core::ptr::addr_of!(NVME))
            .as_ref()
            .map(|n| n.capacity * (n.lba_bytes as u64 / 512))
            .unwrap_or(0)
    }
}

/// SMART/Health log page (Get Log Page, LID=0x02): return (temperature in K,
/// percentage used). None if not available.
pub fn smart() -> Option<(u16, u8)> {
    unsafe {
        let nv = (*core::ptr::addr_of_mut!(NVME)).as_mut()?;
        let mut cmd = [0u32; 16];
        let numd = (512 / 4 - 1) as u32; // 512 bytes in dwords (0-based)
        cmd[0] = 0x02 | ((nv.cid() as u32) << 16); // Get Log Page
        cmd[1] = 0xFFFF_FFFF; // NSID = all
        cmd[6] = (nv.data & 0xFFFF_FFFF) as u32;
        cmd[7] = (nv.data >> 32) as u32;
        cmd[10] = 0x02 | (numd << 16); // LID=0x02 (SMART) | NUMDL
        nv.admin.submit(&cmd);
        if !matches!(nv.admin.wait(), Some(0)) {
            return None;
        }
        let d = core::slice::from_raw_parts(nv.data as *const u8, 512);
        let temp = u16::from_le_bytes([d[1], d[2]]); // composite temperature (K)
        let used = d[5]; // percentage used
        Some((temp, used))
    }
}

/// The COMPLETE SMART/Health log page (512 bytes) — for EuroHealth (Z) which parses
/// all fields (spare, wear, media errors, power-on hours).
pub fn smart_log() -> Option<[u8; 512]> {
    unsafe {
        let nv = (*core::ptr::addr_of_mut!(NVME)).as_mut()?;
        let mut cmd = [0u32; 16];
        let numd = (512 / 4 - 1) as u32;
        cmd[0] = 0x02 | ((nv.cid() as u32) << 16);
        cmd[1] = 0xFFFF_FFFF;
        cmd[6] = (nv.data & 0xFFFF_FFFF) as u32;
        cmd[7] = (nv.data >> 32) as u32;
        cmd[10] = 0x02 | (numd << 16);
        nv.admin.submit(&cmd);
        if !matches!(nv.admin.wait(), Some(0)) {
            return None;
        }
        let mut out = [0u8; 512];
        out.copy_from_slice(core::slice::from_raw_parts(nv.data as *const u8, 512));
        Some(out)
    }
}

/// EuroFS `BlockDevice` on top of the NVMe controller (4 KiB blocks = 8 × 512-byte
/// LBAs), so a EuroFS can be mounted directly on an NVMe disk (G2/B2).
#[derive(Clone, Copy)]
pub struct NvmeBlock {
    blocks: u64,
}

impl NvmeBlock {
    /// `None` if there is no NVMe controller.
    pub fn new() -> Option<Self> {
        if !present() {
            return None;
        }
        Some(NvmeBlock { blocks: capacity_sectors() / 8 }) // 512-byte sectors → 4 KiB blocks
    }
}

impl eurofs::BlockDevice for NvmeBlock {
    fn block_size(&self) -> u32 {
        4096
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> eurofs::BlockResult<()> {
        for i in 0..count as u64 {
            let o = (i * 4096) as usize;
            if !read_sectors((start + i) * 8, &mut buf[o..o + 4096]) {
                return Err(eurofs::BlockError::IoError);
            }
        }
        Ok(())
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> eurofs::BlockResult<()> {
        for i in 0..count as u64 {
            let o = (i * 4096) as usize;
            if !write_sectors((start + i) * 8, &buf[o..o + 4096]) {
                return Err(eurofs::BlockError::IoError);
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> eurofs::BlockResult<()> {
        // NVMe writes are synchronous (we poll for completion) → already durable.
        Ok(())
    }
}

/// Self-test: write a pattern to a high LBA, read it back, verify.
pub fn self_test() {
    if !present() {
        return;
    }
    let mut wbuf = [0u8; 512];
    for (i, b) in wbuf.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    let lba = 1000u64;
    if !write_sectors(lba, &wbuf) {
        crate::serial_println!("[nvme] self-test: write FAILED");
        return;
    }
    let mut rbuf = [0u8; 512];
    if !read_sectors(lba, &mut rbuf) {
        crate::serial_println!("[nvme] self-test: read FAILED");
        return;
    }
    let ok = wbuf == rbuf;
    crate::serial_println!("[nvme] self-test read/write @ LBA {lba}: {}", if ok { "OK ✓" } else { "MISMATCH ✗" });
    if let Some((temp, used)) = smart() {
        crate::serial_println!("[nvme] SMART: temperature {} K ({} °C), {}% used", temp, temp.saturating_sub(273), used);
    }
}
