//! EuroNVMe: NVMe controller logic for the userspace storage driver, free of
//! hardware access.
//!
//! One admin queue pair and one I/O queue pair, polled completions with an
//! optional MSI-X vector on the I/O completion queue, reads and writes of up to
//! [`DATA_BYTES`] through PRP1/PRP2 (no PRP lists). Register offsets, command
//! layouts and status fields are hardware facts from the NVM Express Base
//! Specification 1.4 (the same values `kernel/src/nvme.rs` uses). Redox OS
//! `base/drivers/storage/nvmed` @ 6f881b09 was studied for the bring-up order
//! (disable, admin queues, CC, enable, identify, I/O queues); no code is taken
//! from it.
//!
//! The logic talks to the controller only through [`Hal`]: 32-bit register
//! reads and writes, and reads and writes of the driver's own DMA window. The
//! userspace driver (`userland/nvmed`) implements `Hal` over its granted MMIO
//! and DMA windows; the host tests implement it with a simulated controller
//! (`sim`). Nothing here is `unsafe`, nothing allocates.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

#[cfg(test)]
mod sim;

/// Controller register offsets (BAR 0).
pub mod reg {
    pub const CAP_LO: u32 = 0x00;
    pub const CAP_HI: u32 = 0x04;
    pub const VS: u32 = 0x08;
    pub const CC: u32 = 0x14;
    pub const CSTS: u32 = 0x1C;
    pub const AQA: u32 = 0x24;
    pub const ASQ_LO: u32 = 0x28;
    pub const ASQ_HI: u32 = 0x2C;
    pub const ACQ_LO: u32 = 0x30;
    pub const ACQ_HI: u32 = 0x34;
    /// First doorbell (admin SQ tail); the rest follow at the CAP.DSTRD stride.
    pub const DOORBELL_BASE: u32 = 0x1000;
}

pub const CC_EN: u32 = 1;
/// CC: I/O SQ entry 64 bytes (2^6), I/O CQ entry 16 bytes (2^4), 4 KiB pages, NVM command set.
pub const CC_CONFIG: u32 = (6 << 16) | (4 << 20);
pub const CSTS_RDY: u32 = 1;
pub const CSTS_CFS: u32 = 1 << 1;

/// Admin opcodes.
pub const ADM_CREATE_IO_SQ: u8 = 0x01;
pub const ADM_CREATE_IO_CQ: u8 = 0x05;
pub const ADM_IDENTIFY: u8 = 0x06;
/// NVM opcodes.
pub const IO_WRITE: u8 = 0x01;
pub const IO_READ: u8 = 0x02;

/// Entries per queue (both pairs).
pub const QDEPTH: u16 = 16;
pub const SQE: u64 = 64;
pub const CQE: u64 = 16;
const PAGE: u64 = 4096;
/// Largest transfer of one command: PRP1 + PRP2 = two pages.
pub const DATA_BYTES: usize = 8192;

/// Polls allowed for CSTS.RDY to change, and for one command to complete.
pub const READY_POLLS: u32 = 2_000_000;
pub const CMD_POLLS: u32 = 4_000_000;

/// The controller as the driver sees it: its registers and the DMA window.
pub trait Hal {
    fn reg_read(&mut self, off: u32) -> u32;
    fn reg_write(&mut self, off: u32, val: u32);
    /// Write `data` at bus address `iova` (in the driver's DMA window).
    fn dma_write(&mut self, iova: u64, data: &[u8]);
    /// Read `out.len()` bytes at bus address `iova` (in the DMA window).
    fn dma_read(&mut self, iova: u64, out: &mut [u8]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The DMA window is too small or not page-aligned.
    BadLayout,
    /// CSTS.RDY did not follow CC.EN.
    NotReady,
    /// The controller reports a fatal status (CSTS.CFS).
    Fatal,
    /// A command did not complete.
    Timeout,
    /// The controller completed a command with this status (SCT << 8 | SC).
    Status(u16),
    /// Namespace 1 reports a sector size this driver does not handle.
    BadSectorSize,
    /// LBA range outside the namespace.
    OutOfRange,
    /// More than DATA_BYTES, zero sectors, or a buffer of the wrong size.
    BadLength,
    /// A completion for a command id that is not outstanding.
    Unexpected,
}

/// Where the queues and buffers sit in the DMA window (bus addresses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub admin_sq: u64,
    pub admin_cq: u64,
    pub io_sq: u64,
    pub io_cq: u64,
    pub ident: u64,
    pub data: u64,
}

impl Layout {
    /// Bytes the layout needs: a page per queue, the identify page, the data pages.
    pub const BYTES: u64 = 5 * PAGE + DATA_BYTES as u64;

    /// Carve the layout out of a DMA window `[base, base + len)`.
    pub fn in_window(base: u64, len: u64) -> Result<Layout, Error> {
        if base % PAGE != 0 || len < Self::BYTES || base.checked_add(len).is_none() {
            return Err(Error::BadLayout);
        }
        Ok(Layout {
            admin_sq: base,
            admin_cq: base + PAGE,
            io_sq: base + 2 * PAGE,
            io_cq: base + 3 * PAGE,
            ident: base + 4 * PAGE,
            data: base + 5 * PAGE,
        })
    }
}

/// One queue pair's driver-side state.
#[derive(Clone, Copy, Debug)]
struct Queue {
    sq: u64,
    cq: u64,
    tail: u16,
    head: u16,
    phase: u16,
    sq_db: u32,
    cq_db: u32,
}

impl Queue {
    fn new(sq: u64, cq: u64, sq_db: u32, cq_db: u32) -> Queue {
        Queue { sq, cq, tail: 0, head: 0, phase: 1, sq_db, cq_db }
    }

    fn submit(&mut self, hal: &mut impl Hal, cmd: &[u32; 16]) {
        let mut b = [0u8; SQE as usize];
        for (i, w) in cmd.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        hal.dma_write(self.sq + self.tail as u64 * SQE, &b);
        self.tail = (self.tail + 1) % QDEPTH;
        hal.reg_write(self.sq_db, self.tail as u32);
    }

    /// One look at the completion queue: `Some((command id, status))` when the
    /// next entry carries the expected phase (the head moves on, the doorbell
    /// is rung), `None` when nothing completed yet.
    ///
    /// The phase bit and the command id share DW3, and the controller writes
    /// the entry while the driver may be reading it. A HAL that reads bytes in
    /// ascending order could see the new phase with the old (zeroed) command
    /// id, so the phase and status half is read first and the id only after
    /// it showed the expected phase (the controller posts the entry as one
    /// write, so the id is there once the phase is).
    fn poll(&mut self, hal: &mut impl Hal) -> Option<(u16, u16)> {
        let at = self.cq + self.head as u64 * CQE;
        let mut hi = [0u8; 2];
        hal.dma_read(at + 14, &mut hi);
        let hi = u16::from_le_bytes(hi);
        if hi & 1 != self.phase {
            return None;
        }
        let mut id = [0u8; 2];
        hal.dma_read(at + 12, &mut id);
        let cid = u16::from_le_bytes(id);
        let status = (hi >> 1) & 0x7FF; // SC (8) + SCT (3)
        self.head = (self.head + 1) % QDEPTH;
        if self.head == 0 {
            self.phase ^= 1;
        }
        hal.reg_write(self.cq_db, self.head as u32);
        Some((cid, status))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub reads: u64,
    pub writes: u64,
    pub sectors_read: u64,
    pub sectors_written: u64,
    pub errors: u64,
}

/// An initialised controller with namespace 1 and one I/O queue pair.
#[derive(Debug)]
pub struct Nvme {
    layout: Layout,
    admin: Queue,
    io: Queue,
    next_cid: u16,
    /// The I/O command in flight (one at a time): (cid, write, sectors).
    pending: Option<(u16, bool, u16)>,
    /// Namespace 1: size in sectors and sector size in bytes.
    pub sectors: u64,
    pub sector_size: u32,
    pub model: [u8; 40],
    pub version: u32,
    pub stats: Stats,
}

fn cmd_base(opcode: u8, cid: u16, nsid: u32, prp1: u64, prp2: u64) -> [u32; 16] {
    let mut c = [0u32; 16];
    c[0] = opcode as u32 | (cid as u32) << 16;
    c[1] = nsid;
    c[6] = prp1 as u32;
    c[7] = (prp1 >> 32) as u32;
    c[8] = prp2 as u32;
    c[9] = (prp2 >> 32) as u32;
    c
}

/// Wait for CSTS.RDY == `want` (or a fatal status).
fn wait_ready(hal: &mut impl Hal, want: bool) -> Result<(), Error> {
    for _ in 0..READY_POLLS {
        let s = hal.reg_read(reg::CSTS);
        if s != u32::MAX && s & CSTS_CFS != 0 && want {
            return Err(Error::Fatal);
        }
        if (s & CSTS_RDY != 0) == want {
            return Ok(());
        }
    }
    Err(Error::NotReady)
}

impl Nvme {
    /// Reset the controller (whatever a previous driver instance left in it,
    /// a crashed one included, is gone after CC.EN=0), set up the admin queues,
    /// enable it, identify the controller and namespace 1, and create one I/O
    /// queue pair. `irq`: the I/O completion queue raises MSI-X vector 0.
    pub fn init(hal: &mut impl Hal, layout: Layout, irq: bool) -> Result<Nvme, Error> {
        let cap_hi = hal.reg_read(reg::CAP_HI);
        let stride = 4u32 << (cap_hi & 0xF); // CAP.DSTRD
        let db = |i: u32| reg::DOORBELL_BASE + i * stride;
        // Disable and wait until the controller is idle.
        hal.reg_write(reg::CC, 0);
        wait_ready(hal, false)?;
        for page in [layout.admin_sq, layout.admin_cq, layout.io_sq, layout.io_cq] {
            hal.dma_write(page, &[0u8; PAGE as usize]);
        }
        let q = (QDEPTH - 1) as u32;
        hal.reg_write(reg::AQA, (q << 16) | q);
        hal.reg_write(reg::ASQ_LO, layout.admin_sq as u32);
        hal.reg_write(reg::ASQ_HI, (layout.admin_sq >> 32) as u32);
        hal.reg_write(reg::ACQ_LO, layout.admin_cq as u32);
        hal.reg_write(reg::ACQ_HI, (layout.admin_cq >> 32) as u32);
        hal.reg_write(reg::CC, CC_CONFIG);
        hal.reg_write(reg::CC, CC_CONFIG | CC_EN);
        wait_ready(hal, true)?;
        let mut n = Nvme {
            layout,
            admin: Queue::new(layout.admin_sq, layout.admin_cq, db(0), db(1)),
            io: Queue::new(layout.io_sq, layout.io_cq, db(2), db(3)),
            next_cid: 1,
            pending: None,
            sectors: 0,
            sector_size: 0,
            model: [0; 40],
            version: hal.reg_read(reg::VS),
            stats: Stats::default(),
        };
        // Identify Controller (CNS 1): the model number at bytes 24..64.
        let c = cmd_identify(n.cid(), 1, 0, layout.ident);
        n.admin_cmd(hal, c)?;
        let mut id = [0u8; 64];
        hal.dma_read(layout.ident, &mut id);
        n.model.copy_from_slice(&id[24..64]);
        // Identify Namespace 1 (CNS 0): NSZE, and the LBA format in use.
        let c = cmd_identify(n.cid(), 0, 1, layout.ident);
        n.admin_cmd(hal, c)?;
        let mut ns = [0u8; 256];
        hal.dma_read(layout.ident, &mut ns);
        n.sectors = u64::from_le_bytes([ns[0], ns[1], ns[2], ns[3], ns[4], ns[5], ns[6], ns[7]]);
        let flbas = (ns[26] & 0xF) as usize;
        let lbads = ns[128 + flbas * 4 + 2];
        if !(9..=12).contains(&lbads) || n.sectors == 0 {
            return Err(Error::BadSectorSize);
        }
        n.sector_size = 1 << lbads;
        // I/O CQ 1 (physically contiguous, interrupts on vector 0 when wired),
        // then I/O SQ 1 bound to it.
        let mut c = cmd_base(ADM_CREATE_IO_CQ, n.cid(), 0, layout.io_cq, 0);
        c[10] = ((QDEPTH as u32 - 1) << 16) | 1;
        c[11] = if irq { (1 << 1) | 1 } else { 1 };
        n.admin_cmd(hal, c)?;
        let mut c = cmd_base(ADM_CREATE_IO_SQ, n.cid(), 0, layout.io_sq, 0);
        c[10] = ((QDEPTH as u32 - 1) << 16) | 1;
        c[11] = (1 << 16) | 1;
        n.admin_cmd(hal, c)?;
        Ok(n)
    }

    fn cid(&mut self) -> u16 {
        let c = self.next_cid;
        self.next_cid = if c == u16::MAX { 1 } else { c + 1 };
        c
    }

    fn admin_cmd(&mut self, hal: &mut impl Hal, cmd: [u32; 16]) -> Result<(), Error> {
        let cid = cmd[0] >> 16;
        self.admin.submit(hal, &cmd);
        for _ in 0..CMD_POLLS {
            if let Some((c, status)) = self.admin.poll(hal) {
                if c as u32 != cid {
                    return Err(Error::Unexpected);
                }
                return if status == 0 { Ok(()) } else { Err(Error::Status(status)) };
            }
        }
        Err(Error::Timeout)
    }

    /// Bytes of `count` sectors, checked against the namespace and the buffer.
    fn check(&self, lba: u64, count: u16) -> Result<usize, Error> {
        let bytes = count as usize * self.sector_size as usize;
        if count == 0 || bytes > DATA_BYTES {
            return Err(Error::BadLength);
        }
        if lba.checked_add(count as u64).is_none_or(|end| end > self.sectors) {
            return Err(Error::OutOfRange);
        }
        Ok(bytes)
    }

    /// Start one I/O command (the driver waits for it with [`Nvme::complete`],
    /// on an interrupt or by polling). A write takes its data from `data`.
    pub fn submit(&mut self, hal: &mut impl Hal, write: bool, lba: u64, count: u16, data: &[u8]) -> Result<(), Error> {
        if self.pending.is_some() {
            return Err(Error::Unexpected);
        }
        let bytes = self.check(lba, count)?;
        if write {
            if data.len() != bytes {
                return Err(Error::BadLength);
            }
            hal.dma_write(self.layout.data, data);
        }
        let prp2 = if bytes > PAGE as usize { self.layout.data + PAGE } else { 0 };
        let cid = self.cid();
        let mut c = cmd_base(if write { IO_WRITE } else { IO_READ }, cid, 1, self.layout.data, prp2);
        c[10] = lba as u32;
        c[11] = (lba >> 32) as u32;
        c[12] = (count - 1) as u32;
        self.io.submit(hal, &c);
        self.pending = Some((cid, write, count));
        Ok(())
    }

    /// Is a command in flight?
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// One look for the in-flight command's completion: `None` while it runs.
    /// A read's data is copied into `out` (exactly the requested length).
    pub fn complete(&mut self, hal: &mut impl Hal, out: &mut [u8]) -> Option<Result<(), Error>> {
        let (cid, write, count) = self.pending?;
        let (c, status) = self.io.poll(hal)?;
        self.pending = None;
        if c != cid {
            self.stats.errors += 1;
            return Some(Err(Error::Unexpected));
        }
        if status != 0 {
            self.stats.errors += 1;
            return Some(Err(Error::Status(status)));
        }
        let bytes = count as usize * self.sector_size as usize;
        if write {
            self.stats.writes += 1;
            self.stats.sectors_written += count as u64;
        } else {
            if out.len() != bytes {
                return Some(Err(Error::BadLength));
            }
            hal.dma_read(self.layout.data, out);
            self.stats.reads += 1;
            self.stats.sectors_read += count as u64;
        }
        Some(Ok(()))
    }

    /// Submit and poll until done (bounded).
    fn rw(&mut self, hal: &mut impl Hal, write: bool, lba: u64, count: u16, data: &[u8], out: &mut [u8]) -> Result<(), Error> {
        self.submit(hal, write, lba, count, data)?;
        for _ in 0..CMD_POLLS {
            if let Some(r) = self.complete(hal, out) {
                return r;
            }
        }
        self.pending = None;
        Err(Error::Timeout)
    }

    /// Read `count` sectors at `lba` into `out` (polled).
    pub fn read(&mut self, hal: &mut impl Hal, lba: u64, count: u16, out: &mut [u8]) -> Result<(), Error> {
        self.rw(hal, false, lba, count, &[], out)
    }

    /// Write `data` (whole sectors) at `lba` (polled).
    pub fn write(&mut self, hal: &mut impl Hal, lba: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() || data.len() % self.sector_size as usize != 0 {
            return Err(Error::BadLength);
        }
        let count = (data.len() / self.sector_size as usize) as u16;
        self.rw(hal, true, lba, count, data, &mut [])
    }

    /// The IOMMU self-test: a one-sector READ whose data pointer is `target`,
    /// an address outside the DMA window. Under an IOMMU domain the device's
    /// write there must be refused. Returns the command's outcome as the
    /// device reports it (the kernel judges the memory, not this).
    pub fn read_into_foreign(&mut self, hal: &mut impl Hal, lba: u64, target: u64) -> Result<(), Error> {
        self.check(lba, 1)?;
        let cid = self.cid();
        let mut c = cmd_base(IO_READ, cid, 1, target, 0);
        c[10] = lba as u32;
        c[11] = (lba >> 32) as u32;
        self.io.submit(hal, &c);
        for _ in 0..CMD_POLLS {
            if let Some((cc, status)) = self.io.poll(hal) {
                if cc != cid {
                    return Err(Error::Unexpected);
                }
                return if status == 0 { Ok(()) } else { Err(Error::Status(status)) };
            }
        }
        Err(Error::Timeout)
    }

    /// The model string (trimmed ASCII).
    pub fn model_str(&self) -> &str {
        let s = core::str::from_utf8(&self.model).unwrap_or("?");
        s.trim_matches(|c: char| c == ' ' || c == '\0')
    }
}

fn cmd_identify(cid: u16, cns: u32, nsid: u32, buf: u64) -> [u32; 16] {
    let mut c = cmd_base(ADM_IDENTIFY, cid, nsid, buf, 0);
    c[10] = cns;
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::SimNvme;

    const BASE: u64 = 0x10_0000;

    fn up(sectors: u64) -> (SimNvme, Nvme) {
        let mut s = SimNvme::new(BASE, 8 * 4096, sectors);
        let l = Layout::in_window(BASE, 8 * 4096).unwrap();
        let n = Nvme::init(&mut s, l, true).unwrap();
        (s, n)
    }

    #[test]
    fn init_identifies_controller_and_namespace() {
        let (s, n) = up(2048);
        assert_eq!(n.sectors, 2048);
        assert_eq!(n.sector_size, 512);
        assert_eq!(n.model_str(), "EuroSim NVMe");
        assert!(s.enabled());
        assert_eq!(s.io_queues(), (true, true));
        assert!(s.io_cq_irq(), "the I/O CQ asks for interrupts when irq=true");
    }

    #[test]
    fn write_then_read_roundtrip_one_and_two_pages() {
        let (mut s, mut n) = up(4096);
        let one: [u8; 512] = core::array::from_fn(|i| (i * 7) as u8);
        n.write(&mut s, 3, &one).unwrap();
        let mut back = [0u8; 512];
        n.read(&mut s, 3, 1, &mut back).unwrap();
        assert_eq!(back, one);
        // 16 sectors = 8 KiB: PRP1 + PRP2.
        let big: [u8; DATA_BYTES] = core::array::from_fn(|i| (i % 251) as u8);
        n.write(&mut s, 100, &big).unwrap();
        let mut back = [0u8; DATA_BYTES];
        n.read(&mut s, 100, 16, &mut back).unwrap();
        assert_eq!(back[..], big[..]);
        assert_eq!(&s.disk()[100 * 512..116 * 512], &big[..]);
        assert_eq!(n.stats.sectors_written, 17);
        assert_eq!(n.stats.sectors_read, 17);
    }

    #[test]
    fn requests_outside_the_namespace_or_buffer_are_refused_before_the_device() {
        let (mut s, mut n) = up(64);
        let before = s.commands();
        let mut out = [0u8; 512];
        assert_eq!(n.read(&mut s, 64, 1, &mut out), Err(Error::OutOfRange));
        assert_eq!(n.read(&mut s, 63, 2, &mut [0u8; 1024]), Err(Error::OutOfRange));
        assert_eq!(n.read(&mut s, u64::MAX, 1, &mut out), Err(Error::OutOfRange));
        assert_eq!(n.read(&mut s, 0, 17, &mut [0u8; 17 * 512]), Err(Error::BadLength));
        assert_eq!(n.read(&mut s, 0, 0, &mut []), Err(Error::BadLength));
        assert_eq!(n.write(&mut s, 0, &[1u8; 100]), Err(Error::BadLength));
        assert_eq!(s.commands(), before, "nothing reached the controller");
    }

    #[test]
    fn many_commands_wrap_the_queues_and_flip_the_phase() {
        let (mut s, mut n) = up(512);
        for i in 0..(5 * QDEPTH as u64) {
            let d = [(i & 0xff) as u8; 512];
            n.write(&mut s, i % 512, &d).unwrap();
            let mut b = [0u8; 512];
            n.read(&mut s, i % 512, 1, &mut b).unwrap();
            assert_eq!(b, d);
        }
        assert_eq!(n.stats.writes, 5 * QDEPTH as u64);
    }

    #[test]
    fn device_errors_are_reported_not_hidden() {
        let (mut s, mut n) = up(64);
        s.fail_next_io(0x0281); // media error status
        let mut out = [0u8; 512];
        assert_eq!(n.read(&mut s, 1, 1, &mut out), Err(Error::Status(0x0281 & 0x7FF)));
        assert_eq!(n.stats.errors, 1);
        // The queue is still usable afterwards.
        n.read(&mut s, 1, 1, &mut out).unwrap();
    }

    #[test]
    fn a_controller_that_never_gets_ready_or_is_fatal_fails_init() {
        let mut s = SimNvme::new(BASE, 8 * 4096, 64);
        s.set_dead(true);
        let l = Layout::in_window(BASE, 8 * 4096).unwrap();
        assert_eq!(Nvme::init(&mut s, l, false).unwrap_err(), Error::NotReady);
        let mut s = SimNvme::new(BASE, 8 * 4096, 64);
        s.set_fatal(true);
        assert_eq!(Nvme::init(&mut s, l, false).unwrap_err(), Error::Fatal);
    }

    #[test]
    fn layout_needs_an_aligned_window_of_enough_pages() {
        assert_eq!(Layout::in_window(BASE + 8, 8 * 4096), Err(Error::BadLayout));
        assert_eq!(Layout::in_window(BASE, Layout::BYTES - 1), Err(Error::BadLayout));
        assert_eq!(Layout::in_window(u64::MAX - 4095, 8 * 4096), Err(Error::BadLayout));
        let l = Layout::in_window(BASE, Layout::BYTES).unwrap();
        assert_eq!(l.data + DATA_BYTES as u64, BASE + Layout::BYTES);
    }

    #[test]
    fn reinit_after_a_crash_resets_a_live_controller() {
        // A driver instance died with the controller enabled and a command in
        // flight; the next instance's init starts from CC.EN=0.
        let (mut s, mut n) = up(64);
        n.submit(&mut s, true, 5, 1, &[9u8; 512]).unwrap();
        assert!(n.busy());
        let l = Layout::in_window(BASE, 8 * 4096).unwrap();
        let mut n2 = Nvme::init(&mut s, l, true).unwrap();
        assert!(s.resets() >= 1, "the second init reset a live controller");
        let mut b = [0u8; 512];
        n2.read(&mut s, 5, 1, &mut b).unwrap();
        n2.write(&mut s, 6, &[3u8; 512]).unwrap();
        n2.read(&mut s, 6, 1, &mut b).unwrap();
        assert_eq!(b, [3u8; 512]);
    }

    #[test]
    fn only_one_command_in_flight_and_completions_are_matched() {
        let (mut s, mut n) = up(64);
        n.submit(&mut s, false, 1, 1, &[]).unwrap();
        assert_eq!(n.submit(&mut s, false, 2, 1, &[]), Err(Error::Unexpected));
        let mut out = [0u8; 512];
        let mut r = None;
        for _ in 0..10 {
            r = n.complete(&mut s, &mut out);
            if r.is_some() {
                break;
            }
        }
        assert_eq!(r, Some(Ok(())));
        assert!(!n.busy());
        assert_eq!(n.complete(&mut s, &mut out), None, "nothing in flight");
    }

    #[test]
    fn a_foreign_data_pointer_goes_to_the_device_as_given() {
        // The IOMMU self-test: the logic does not second-guess the target; the
        // simulated device records where it was told to write.
        let (mut s, mut n) = up(64);
        let r = n.read_into_foreign(&mut s, 0, 0xdead_0000);
        assert_eq!(r, Err(Error::Status(0x0006)), "the sim refuses DMA outside its window like an IOMMU would");
        assert_eq!(s.last_foreign(), Some(0xdead_0000));
        let mut out = [0u8; 512];
        n.read(&mut s, 0, 1, &mut out).unwrap();
    }
}
