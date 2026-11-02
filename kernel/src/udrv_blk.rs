//! The first storage driver out of the kernel: the NVMe driver `nvmed` in
//! ring 3, serving a DATA disk (docs/SPRINT-PLAN-USERSPACE-STORAGE.md).
//!
//! Selected with `storage.nvme=user` (bootopt.rs; the default stays the
//! in-kernel `nvme.rs`). The root filesystem never goes through this path in
//! this sprint: with the option the kernel's NVMe driver leaves the controller
//! alone, so the root is chosen among the other disks (or RAM) as before, and
//! the NVMe disk becomes a EuroFS data mount at `/data`.
//!
//! The same primitives as the userspace NIC (udrv.rs), for a second device:
//! - **MMIO**: the register and doorbell pages of BAR 0 only. On QEMU's NVMe
//!   the MSI-X table and PBA sit in BAR 0 behind the registers; those pages are
//!   NOT granted, the kernel programs where the interrupts go.
//! - **DMA**: a window of `DMA_PAGES` pages (queues, identify page, 8 KiB data
//!   buffer), under VT-d the controller's own IOMMU domain holding exactly it.
//! - **IRQ**: an MSI-X vector from the userspace-driver pool (`udrv::alloc_vector`) on the BSP -> notification
//!   slot (`udrv::RES`) -> an `Irq` EuroIPC message to nvmed.
//! - **EuroIPC**: nvmed's port (only the kernel may send) and the disk port
//!   (only the pid holding the grant may send). Block requests and answers are
//!   messages of at most `BLK_CHUNK` bytes of data.
//! - **Restart**: on a death the device is quiesced (bus mastering off, IOMMU
//!   domain revoked) before the DMA window is freed; `Policy::DISK` decides
//!   when (and whether) a fresh nvmed starts, which resets the controller.
//!   Requests in flight when it died fail with an I/O error; the filesystem's
//!   CoW + A/B superblock commit decides what is on the disk afterwards.
//!
//! megaraidd (`udrv_mr.rs`, `mrblk.rs`) speaks a different block wire format
//! (`euromegaraid::blkif`: data in DMA-window slots, several requests in
//! flight); see `mrblk.rs` for why the two are not merged yet.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use eurodrv::abi::{self, DriverInfo};
use eurodrv::grant::{Bdf, DeviceGrant, Window};
use eurodrv::proto::{self, Msg, BLK_CHUNK};
use eurodrv::restart::{Decision, Policy, Tracker};
use eurofs::block::{BlockDevice, BlockError, BlockResult};
use euromm::FrameAllocator;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::euroipc;
use crate::pci::{self, PciDevice};
use crate::udrv::{self, Drv, SysGrant};

static NVMED_ELF: &[u8] = include_bytes!("../../userland/nvmed.elf");
static NVMED_SIG: &[u8] = include_bytes!("../../userland/nvmed.elf.sig");

/// EuroIPC ports: nvmed's own, and the kernel's disk port.
pub const DRIVER_PORT: u32 = Drv::Nvmed.res().driver_port;
pub const DISK_PORT: u32 = Drv::Nvmed.res().stack_port;
/// Queues (4 pages) + identify page + 8 KiB data buffer = 7 pages; one spare.
const DMA_PAGES: usize = 8;
const DOMAIN: &str = "nvmed";
const ARENA_MIB: u64 = 4;
/// Filesystem block size on the data disk (EuroFS).
const FS_BLOCK: usize = 4096;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static UP: AtomicBool = AtomicBool::new(false);
static SECTORS: AtomicU64 = AtomicU64::new(0);
static SECTOR_SIZE: AtomicU32 = AtomicU32::new(0);
static DRIVER_PID: AtomicU64 = AtomicU64::new(u64::MAX);
static HELLOS: AtomicU32 = AtomicU32::new(0);
static REQS: AtomicU64 = AtomicU64::new(0);
static REQ_ERRORS: AtomicU64 = AtomicU64::new(0);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
/// The task waiting for an answer (+1; 0 = nobody).
static WAITER: AtomicUsize = AtomicUsize::new(0);
static REPORT: Mutex<(u8, u64, u64)> = Mutex::new((0, 0, 0));

struct Instance {
    pid: u64,
    grant: DeviceGrant,
    iommu: bool,
}

struct Supervisor {
    dev: Option<PciDevice>,
    mmio: Window,
    inst: Option<Instance>,
    tracker: Tracker,
    restart_at: Option<u64>,
    gave_up: bool,
    starts: u32,
    msix: bool,
    last_death: Option<String>,
}

static SUP: Mutex<Supervisor> = Mutex::new(Supervisor {
    dev: None,
    mmio: Window { base: 0, len: 0 },
    inst: None,
    tracker: Tracker::new(),
    restart_at: None,
    gave_up: false,
    starts: 0,
    msix: false,
    last_death: None,
});

// ── Interrupts and wake-ups ─────────────────────────────────────────────────

/// nvmed sent the kernel something: wake the task waiting for an answer.
pub fn wake() {
    let w = WAITER.load(Ordering::Acquire);
    if w != 0 {
        crate::sched::unblock_any(w - 1);
    }
}

/// Sleep a tick, or until nvmed answers (task context, interrupts on).
fn wait_tick() {
    let t = crate::sched::current_lockfree();
    WAITER.store(t + 1, Ordering::Release);
    crate::sched::sleep_ticks(1);
    crate::sched::yield_now();
    WAITER.store(0, Ordering::Release);
}

// ── The device, its grant, the supervisor ────────────────────────────────────

fn find_device() -> Option<PciDevice> {
    pci::find(|d| d.class == 0x01 && d.subclass == 0x08 && d.prog_if == 0x02)
}

/// Is there an NVMe controller for the userspace driver?
pub fn device_present() -> bool {
    find_device().is_some()
}

/// The pages of BAR 0 the driver may map: the registers and doorbells, and
/// nothing of the MSI-X table or PBA when those live in BAR 0. (MSI-X
/// capability: table offset/BIR at +4, PBA offset/BIR at +8, PCI 3.0 6.8.2.)
fn regs_window(dev: &PciDevice) -> Option<Window> {
    let bar = udrv::bar0_window(dev)?;
    let mut limit = bar.len;
    if let Some(cap) = dev.find_cap(0x11) {
        for reg in [4u8, 8u8] {
            let v = pci::cfg_read32(dev.bus, dev.dev, dev.func, cap + reg);
            if v & 7 == 0 {
                limit = limit.min((v & !7) as u64 & !0xFFF);
            }
        }
    }
    // The registers (4 KiB) and the doorbells of two queue pairs (0x1000..)
    // need two pages.
    if limit < 0x2000 {
        return None;
    }
    Some(Window::new(bar.base, limit))
}

fn free_dma(falloc: &mut FrameAllocator, base: u64) {
    for i in 0..DMA_PAGES as u64 {
        let _ = falloc.free(base + i * 4096);
    }
}

fn start(sup: &mut Supervisor, falloc: &mut FrameAllocator) -> Result<u64, &'static str> {
    let dev = sup.dev.ok_or("no device")?;
    if !crate::crypto::verify(NVMED_ELF, NVMED_SIG) {
        return Err("nvmed.elf signature invalid: refused (verify-before-execute)");
    }
    let bdf = Bdf { bus: dev.bus, dev: dev.dev, func: dev.func };
    let dma_base = falloc.allocate_aligned(DMA_PAGES, 1).map_err(|_| "no memory for the DMA window")?;
    // SAFETY: fresh frames from the allocator, identity-mapped.
    unsafe { core::ptr::write_bytes(dma_base as *mut u8, 0, DMA_PAGES * 4096) };
    let dma = Window::new(dma_base, (DMA_PAGES * 4096) as u64);
    let ram = Window::new(0, falloc.total_bytes().min(0x8000_0000));
    let irq = if sup.msix { udrv::alloc_vector(Drv::Nvmed) } else { None };
    let grant = match DeviceGrant::new(bdf, sup.mmio, dma, irq, &[ram], &[sup.mmio]) {
        Ok(g) => g,
        Err(e) => {
            crate::serial_println!("[udrv-blk] grant refused: {e:?} (BAR0 regs {:#x}+{:#x}, DMA {:#x}+{:#x})", sup.mmio.base, sup.mmio.len, dma.base, dma.len);
            free_dma(falloc, dma_base);
            return Err("grant refused by eurodrv::grant");
        }
    };
    let iommu = crate::iommu::attach_runtime(falloc, DOMAIN, (bdf.bus, bdf.dev, bdf.func), dma_base, DMA_PAGES).is_some();
    if crate::iommu::translating() && !iommu {
        free_dma(falloc, dma_base);
        return Err("IOMMU translation is on but the driver domain could not be built: refused");
    }
    let pid = Drv::Nvmed.res().pid_base + sup.starts as u64;
    let info = DriverInfo {
        abi: abi::ABI_VERSION,
        vendor: dev.vendor as u64,
        device: dev.device as u64,
        bdf: bdf.sid() as u64,
        mmio_base: grant.mmio_va(),
        mmio_len: grant.mmio.len,
        dma_base,
        dma_len: dma.len,
        own_port: DRIVER_PORT as u64,
        stack_port: DISK_PORT as u64,
        irq_vector: irq.unwrap_or(0) as u64,
        flags: if iommu { abi::FLAG_IOMMU } else { 0 } | if irq.is_some() { abi::FLAG_MSIX } else { 0 },
    };
    euroipc::unregister(DRIVER_PORT);
    euroipc::register_restricted(pid, DRIVER_PORT, euroipc::KERNEL_PID);
    euroipc::set_only_from(DISK_PORT, pid);
    euroipc::bind_notify(Drv::Nvmed.res().notify_slot, DRIVER_PORT);
    DRIVER_PID.store(pid, Ordering::Relaxed);
    let spawned = without_interrupts(|| -> Result<usize, &'static str> {
        let task = crate::ring3::spawn_euro_app(falloc, NVMED_ELF, pid, &[b"nvmed"], ARENA_MIB, crate::ring3::CAP_CONSOLE, eurosandbox::Profile::Driver)
            .ok_or("spawn failed (memory)")?;
        let (_, pml4, arena, arena_bytes) = crate::ring3::bg_layout(pid).ok_or("spawned process not found")?;
        let va0 = grant.mmio_va();
        for (i, pa) in grant.mmio.page_addrs().enumerate() {
            if !crate::paging::map_user_4k_at(falloc, pml4, va0 + i as u64 * 4096, pa, true) {
                crate::sched::mark_dead(task);
                return Err("could not map the BAR");
            }
        }
        for pa in grant.dma.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, false) {
                crate::sched::mark_dead(task);
                return Err("could not map the DMA window");
            }
        }
        udrv::set_grant(Drv::Nvmed, Some(SysGrant::new(Drv::Nvmed, task, pid, pml4, arena, arena_bytes, info)));
        // Interrupt routing and bus mastering before nvmed can run: its first
        // admin command is a DMA (the IOMMU domain is in place).
        if let Some(v) = irq {
            crate::msix::enable(&dev, 0, v, crate::apic::lapic_id() as u8);
        }
        udrv::bus_master(&dev, true);
        Ok(task)
    });
    if let Err(e) = spawned {
        if iommu {
            crate::iommu::detach_runtime(falloc, DOMAIN);
        }
        free_dma(falloc, dma_base);
        return Err(e);
    }
    sup.starts += 1;
    sup.inst = Some(Instance { pid, grant, iommu });
    sup.restart_at = None;
    ACTIVE.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[udrv-blk] nvmed started: pid {pid}, profile driver · BAR0 regs {:#x}+{:#x} ({} pages, MSI-X table not granted) · DMA {:#x}+{:#x} · IRQ {} · IOMMU {}",
        grant.mmio.base, grant.mmio.len, grant.mmio.pages(), dma_base, dma.len,
        irq.map(|v| alloc::format!("MSI-X {v:#x} -> EuroIPC")).unwrap_or_else(|| String::from("none (polled)")),
        if iommu { "own domain (DMA confined)" } else { "absent: DMA NOT confined" }
    );
    Ok(pid)
}

fn on_death(sup: &mut Supervisor, falloc: &mut FrameAllocator, inst: Instance) {
    let reason = crate::ring3::bg_end_reason(inst.pid).unwrap_or_else(|| String::from("terminated"));
    UP.store(false, Ordering::Relaxed);
    udrv::set_grant(Drv::Nvmed, None);
    if let Some(dev) = sup.dev {
        udrv::bus_master(&dev, false);
    }
    let revoked = inst.iommu && crate::iommu::detach_runtime(falloc, DOMAIN);
    euroipc::unregister(DRIVER_PORT);
    crate::ring3::reap_dead(falloc);
    free_dma(falloc, inst.grant.dma.base);
    let decision = sup.tracker.on_death(crate::interrupts::ticks(), &Policy::DISK);
    crate::serial_println!(
        "[udrv-blk] nvmed pid {} died ({reason}) -> controller quiesced: bus-master off, IOMMU domain {}, {DMA_PAGES} DMA pages freed; {}",
        inst.pid,
        if revoked { "revoked" } else { "n/a (no IOMMU)" },
        match decision {
            Decision::RestartAt(t) => alloc::format!("restart at tick {t} (death #{})", sup.tracker.total_deaths()),
            Decision::GiveUp => String::from("GIVING UP (too many deaths in the window; the disk stays off)"),
        }
    );
    sup.last_death = Some(reason);
    match decision {
        Decision::RestartAt(t) => sup.restart_at = Some(t),
        Decision::GiveUp => {
            sup.gave_up = true;
            ACTIVE.store(false, Ordering::Relaxed);
            udrv::release_vector(Drv::Nvmed);
        }
    }
}

/// Handle what nvmed sent that is not an answer to a request (BlkInfo, Report).
fn pump_control() {
    while let Some((sender, data)) = euroipc::take(DISK_PORT, |_| Vec::new()) {
        if sender != DRIVER_PID.load(Ordering::Relaxed) {
            continue;
        }
        note_control(&data);
    }
}

fn note_control(data: &[u8]) {
    match proto::decode(data) {
        Ok(Msg::BlkInfo { sectors, sector_size }) => {
            SECTORS.store(sectors, Ordering::Relaxed);
            SECTOR_SIZE.store(sector_size, Ordering::Relaxed);
            UP.store(true, Ordering::Release);
            HELLOS.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Msg::Report { code, a, b }) => without_interrupts(|| *REPORT.lock() = (code, a, b)),
        _ => {} // a late answer to a request that already gave up
    }
}

/// Supervision tick: notice a dead nvmed, quiesce, restart on schedule.
/// Task 0 only (the boot and the desktop loop).
pub fn supervise(falloc: &mut FrameAllocator) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let mut sup = SUP.lock();
    if let Some(inst) = sup.inst.as_ref() {
        if !crate::ring3::bg_alive(inst.pid) {
            let inst = sup.inst.take().unwrap();
            on_death(&mut sup, falloc, inst);
        }
    }
    if sup.inst.is_none() && !sup.gave_up {
        if let Some(t) = sup.restart_at {
            if crate::interrupts::ticks() >= t {
                if let Err(e) = start(&mut sup, falloc) {
                    crate::serial_println!("[udrv-blk] restart failed: {e}");
                    sup.restart_at = Some(crate::interrupts::ticks() + 100);
                }
            }
        }
    }
    drop(sup);
    if !UP.load(Ordering::Relaxed) {
        pump_control();
    }
}

fn wait_for(ticks: u64, falloc: &mut FrameAllocator, mut cond: impl FnMut() -> bool) -> bool {
    let end = crate::interrupts::ticks() + ticks;
    loop {
        supervise(falloc);
        if cond() {
            return true;
        }
        if crate::interrupts::ticks() >= end {
            return false;
        }
        wait_tick();
    }
}

// ── Block requests ──────────────────────────────────────────────────────────

/// One request to nvmed and its answer. `out`: where a read's data goes.
/// An error when the driver is down, dies meanwhile, refuses, or does not
/// answer within 5 s. Task context with interrupts on.
fn request(msg: &Msg, id: u32, out: Option<&mut [u8]>) -> Result<(), BlockError> {
    if !UP.load(Ordering::Acquire) {
        return Err(BlockError::IoError);
    }
    REQS.fetch_add(1, Ordering::Relaxed);
    let pid = DRIVER_PID.load(Ordering::Relaxed);
    let mut b = [0u8; proto::MAX_MSG];
    let n = proto::encode(msg, &mut b).map_err(|_| BlockError::NotAligned)?;
    if euroipc::send_with(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n], false) < 0 {
        REQ_ERRORS.fetch_add(1, Ordering::Relaxed);
        return Err(BlockError::IoError);
    }
    let end = crate::interrupts::ticks() + 500;
    let mut out = out;
    loop {
        while let Some((sender, data)) = euroipc::take(DISK_PORT, |_| Vec::new()) {
            if sender != pid {
                continue;
            }
            match proto::decode(&data) {
                Ok(Msg::BlkDone { id: got, status, data: d }) if got == id => {
                    if status != 0 {
                        REQ_ERRORS.fetch_add(1, Ordering::Relaxed);
                        return Err(BlockError::IoError);
                    }
                    if let Some(o) = out.as_deref_mut() {
                        if d.len() != o.len() {
                            REQ_ERRORS.fetch_add(1, Ordering::Relaxed);
                            return Err(BlockError::IoError);
                        }
                        o.copy_from_slice(d);
                    }
                    return Ok(());
                }
                _ => note_control(&data),
            }
        }
        if !crate::ring3::bg_alive(pid) || crate::interrupts::ticks() >= end {
            REQ_ERRORS.fetch_add(1, Ordering::Relaxed);
            return Err(BlockError::IoError);
        }
        wait_tick();
    }
}

fn next_id() -> u32 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Read `out.len()` bytes (whole chunks) at byte offset `off` of the disk.
fn read_bytes(off: u64, out: &mut [u8]) -> Result<(), BlockError> {
    let ss = SECTOR_SIZE.load(Ordering::Relaxed) as u64;
    if ss == 0 || off % ss != 0 || out.len() as u64 % ss != 0 {
        return Err(BlockError::NotAligned);
    }
    let mut done = 0usize;
    while done < out.len() {
        let n = (out.len() - done).min(BLK_CHUNK);
        let lba = (off + done as u64) / ss;
        let id = next_id();
        request(&Msg::BlkRead { id, lba, count: (n as u64 / ss) as u16 }, id, Some(&mut out[done..done + n]))?;
        done += n;
    }
    Ok(())
}

fn write_bytes(off: u64, data: &[u8]) -> Result<(), BlockError> {
    let ss = SECTOR_SIZE.load(Ordering::Relaxed) as u64;
    if ss == 0 || off % ss != 0 || data.len() as u64 % ss != 0 {
        return Err(BlockError::NotAligned);
    }
    let mut done = 0usize;
    while done < data.len() {
        let n = (data.len() - done).min(BLK_CHUNK);
        let lba = (off + done as u64) / ss;
        let id = next_id();
        request(&Msg::BlkWrite { id, lba, data: &data[done..done + n] }, id, None)?;
        done += n;
    }
    Ok(())
}

/// The data disk as a EuroFS block device, through nvmed.
#[derive(Clone, Copy)]
pub struct UserBlk {
    blocks: u64,
}

impl UserBlk {
    pub fn new() -> Option<UserBlk> {
        if !UP.load(Ordering::Acquire) {
            return None;
        }
        let bytes = SECTORS.load(Ordering::Relaxed) * SECTOR_SIZE.load(Ordering::Relaxed) as u64;
        Some(UserBlk { blocks: bytes / FS_BLOCK as u64 })
    }
}

impl BlockDevice for UserBlk {
    fn block_size(&self) -> u32 {
        FS_BLOCK as u32
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }
    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        if buf.len() != count as usize * FS_BLOCK {
            return Err(BlockError::NotAligned);
        }
        if start.checked_add(count as u64).is_none_or(|e| e > self.blocks) {
            return Err(BlockError::OutOfBounds);
        }
        read_bytes(start * FS_BLOCK as u64, buf)
    }
    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        if buf.len() != count as usize * FS_BLOCK {
            return Err(BlockError::NotAligned);
        }
        if start.checked_add(count as u64).is_none_or(|e| e > self.blocks) {
            return Err(BlockError::OutOfBounds);
        }
        write_bytes(start * FS_BLOCK as u64, buf)
    }
    fn flush(&mut self) -> BlockResult<()> {
        // Every write is acknowledged by nvmed only after the controller
        // completed it. No NVMe Flush command is sent yet (see the plan's open
        // points): a device with a volatile write cache could still lose it.
        Ok(())
    }
}

// ── Boot and self-tests ─────────────────────────────────────────────────────

/// Boot: with `storage.nvme=user`, start nvmed, prove each step, and mount the
/// disk's EuroFS at `/data`. Task 0, after the scheduler, interrupts and
/// `iommu::program`.
pub fn boot(falloc: &mut FrameAllocator, vfs: &mut crate::kvfs::KernelVfs) {
    if !crate::bootopt::nvme_user_driver() {
        return;
    }
    crate::serial_println!("[udrv-blk] storage.nvme=user: the NVMe driver runs in ring 3 (nvmed) for a DATA disk; the root never goes through it in this sprint");
    let Some(dev) = find_device() else {
        crate::serial_println!("[udrv-blk] no NVMe controller present: nothing to drive");
        return;
    };
    let Some(mmio) = regs_window(&dev) else {
        crate::serial_println!("[udrv-blk] NVMe BAR0 unusable (not memory, or the MSI-X table leaves no room for the registers): not started");
        return;
    };
    pci::claim(dev.bus, dev.dev, dev.func, "nvmed (userspace)");
    euroipc::register_restricted(euroipc::KERNEL_PID, DISK_PORT, u64::MAX);
    let msix = dev.find_cap(0x11).is_some();
    {
        let mut sup = SUP.lock();
        sup.dev = Some(dev);
        sup.mmio = mmio;
        sup.msix = msix;
    }
    let started = {
        let mut sup = SUP.lock();
        start(&mut sup, falloc)
    };
    let pid = match started {
        Ok(p) => p,
        Err(e) => {
            crate::serial_println!("[udrv-blk-grant] nvmed not started: {e} → FAILED");
            return;
        }
    };
    let bar_full = udrv::bar0_window(&dev).map(|w| w.len).unwrap_or(0);
    selftest_grant(pid, bar_full);
    if !wait_for(1000, falloc, || HELLOS.load(Ordering::Relaxed) >= 1) {
        crate::serial_println!("[udrv-blk] nvmed pid {pid} did not report the disk within 10 s → FAILED");
        return;
    }
    // A restart may already have replaced the first instance.
    let pid = DRIVER_PID.load(Ordering::Relaxed);
    let (sectors, ss) = (SECTORS.load(Ordering::Relaxed), SECTOR_SIZE.load(Ordering::Relaxed));
    crate::serial_println!(
        "[udrv-blk] nvmed (pid {pid}, ring 3, profile driver) brought the NVMe {:04x}:{:04x} up: {sectors} sectors x {ss} B = {} MiB, reported over EuroIPC → OK ✓",
        dev.vendor, dev.device, sectors * ss as u64 / (1024 * 1024)
    );
    selftest_rw(sectors, ss);
    selftest_irq(msix);
    if cfg!(feature = "selftest") {
        selftest_dma(falloc);
    }
    let fs = selftest_fs_and_crash(falloc);
    if let Some(fs) = fs {
        vfs.mount("/data", alloc::boxed::Box::new(fs));
        use eurofs::FileSystem;
        let msg: &[u8] = b"written through /data: VFS -> EuroFS -> nvmed (ring 3) -> NVMe\n";
        let wrote = vfs.write_file("/data/hello.txt", msg);
        let read = vfs.read_file("/data/hello.txt");
        let ok = wrote.is_ok() && read.as_ref().map(|d| d.as_slice() == msg).unwrap_or(false);
        if !ok {
            crate::serial_println!("[udrv-blk-mount] write {:?}, read {:?}", wrote, read.map(|d| d.len()));
        }
        crate::serial_println!(
            "[udrv-blk-mount] EuroFS on the userspace-driven NVMe mounted at /data; VFS write+read through it → {}",
            if ok { "OK ✓" } else { "FAILED" }
        );
    }
    crate::serial_println!("[udrv-blk] {}", status_line());
}

fn selftest_grant(pid: u64, bar_full: u64) {
    let Some((_, pml4, _, _)) = crate::ring3::bg_layout(pid) else { return };
    let sup = SUP.lock();
    let Some(inst) = sup.inst.as_ref() else { return };
    let g = inst.grant;
    let va = g.mmio_va();
    let regs = crate::paging::user_accessible_in(pml4, va) && crate::paging::user_accessible_in(pml4, va + g.mmio.len - 4096);
    let past = crate::paging::user_accessible_in(pml4, va + g.mmio.len);
    let before = crate::paging::user_accessible_in(pml4, va - 4096);
    let dma_in = crate::paging::user_accessible_in(pml4, g.dma.base) && crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len - 4096);
    let dma_past = crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len);
    let lapic = crate::paging::user_accessible_in(pml4, 0xFEE0_0000);
    let kernel_code = crate::paging::user_accessible_in(pml4, (selftest_grant as *const () as usize as u64) & !0xFFF);
    let ok = regs && !past && !before && dma_in && !dma_past && !lapic && !kernel_code;
    crate::serial_println!(
        "[udrv-blk-grant] nvmed pid {pid}: BAR0 {:#x}..{:#x} ({} of the BAR's {} KiB: registers + doorbells) mapped at {va:#x}={regs}, next page (MSI-X table side)={past}, page before={before}; DMA window {} pages mapped={dma_in}, page after={dma_past}; LAPIC={lapic}, kernel code={kernel_code} → {}",
        g.mmio.base, g.mmio.base + g.mmio.len, g.mmio.len / 1024, bar_full / 1024, g.dma.pages(),
        if ok { "OK (exactly the granted ranges) ✓" } else { "FAILED" }
    );
    let pages = crate::iommu::domain_pages(DOMAIN);
    if inst.iommu {
        crate::serial_println!(
            "[udrv-blk-iommu] NVMe {:02x}:{:02x}.{} has its own VT-d domain '{DOMAIN}' with {pages} page(s) = the DMA window, nothing else → {}",
            g.bdf.bus, g.bdf.dev, g.bdf.func,
            if pages == DMA_PAGES { "OK ✓" } else { "FAILED" }
        );
    } else {
        crate::serial_println!("[udrv-blk-iommu] no IOMMU translation on this machine: nvmed's DMA is NOT confined (boot QEMU with -device intel-iommu,intremap=off) ⬜");
    }
}

/// Raw sectors through nvmed: the last 8 KiB of the disk are saved, overwritten
/// with a pattern, read back, and restored.
fn selftest_rw(sectors: u64, ss: u32) {
    let len = 8192usize;
    let off = sectors * ss as u64 - len as u64;
    let mut saved = alloc::vec![0u8; len];
    let pattern: Vec<u8> = (0..len).map(|i| (i as u8) ^ 0xA5).collect();
    let mut back = alloc::vec![0u8; len];
    let r = read_bytes(off, &mut saved)
        .and_then(|_| write_bytes(off, &pattern))
        .and_then(|_| read_bytes(off, &mut back));
    let same = r.is_ok() && back == pattern;
    let restored = write_bytes(off, &saved).is_ok();
    crate::serial_println!(
        "[udrv-blk-rw] raw sectors via EuroIPC through nvmed: 8 KiB at sector {} written + read back identical={same}, original restored={restored} ({} requests so far) → {}",
        off / ss as u64,
        REQS.load(Ordering::Relaxed),
        if same && restored { "OK ✓" } else { "FAILED" }
    );
}

fn selftest_irq(msix: bool) {
    let raised = udrv::irqs(Drv::Nvmed);
    let delivered = udrv::irqs_delivered(Drv::Nvmed);
    if !msix {
        crate::serial_println!("[udrv-blk-irq] controller without MSI-X: nvmed polls, no interrupt path to prove ⬜");
        return;
    }
    crate::serial_println!(
        "[udrv-blk-irq] MSI-X vector 0x{:x}: {raised} completion interrupt(s) raised, {delivered} delivered to nvmed as EuroIPC Irq messages → {}",
        udrv::vector_of(Drv::Nvmed).unwrap_or(0),
        if raised > 0 && delivered > 0 { "OK ✓" } else { "FAILED" }
    );
}

/// [udrv-blk-dma]: nvmed turns malicious on request and points a READ's data
/// pointer at a kernel canary frame (the controller would WRITE a sector
/// there). Under VT-d the write must fault, the canary must stay intact, and
/// the disk must keep working.
fn selftest_dma(falloc: &mut FrameAllocator) {
    let iommu = SUP.lock().inst.as_ref().map(|i| i.iommu).unwrap_or(false);
    if !iommu {
        crate::serial_println!("[udrv-blk-dma] no IOMMU translation: skipped (the write would land; that is the unconfined case) ⬜");
        return;
    }
    let Ok(canary) = falloc.allocate() else {
        crate::serial_println!("[udrv-blk-dma] no frame for the probe → FAILED");
        return;
    };
    // SAFETY: a fresh, identity-mapped frame of our own.
    unsafe { core::ptr::write_bytes(canary as *mut u8, 0xA5, 4096) };
    let _ = crate::iommu::poll_faults();
    without_interrupts(|| *REPORT.lock() = (0, 0, 0));
    let mut b = [0u8; 16];
    let n = proto::encode(&Msg::DmaProbe { target: canary }, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    let reported = wait_for(500, falloc, || {
        pump_control();
        without_interrupts(|| REPORT.lock().0) == proto::REPORT_DMA_PROBE_DONE
    });
    let (_, dev_err, status) = without_interrupts(|| *REPORT.lock());
    let _ = wait_for(20, falloc, || false);
    let faults = crate::iommu::poll_faults();
    // SAFETY: our own frame.
    let changed = unsafe { core::slice::from_raw_parts(canary as *const u8, 4096) }.iter().any(|&x| x != 0xA5);
    let mut sector = [0u8; 512];
    let still = read_bytes(0, &mut sector).is_ok();
    let ok = reported && faults >= 1 && !changed && still;
    crate::serial_println!(
        "[udrv-blk-dma] malicious nvmed: READ of LBA 0 aimed at kernel canary {canary:#x} · driver report={reported} (device error={}, status {status:#x}) · IOMMU fault records {faults} · canary changed={changed} · disk still works={still} → {}",
        dev_err != 0,
        if ok { "OK (controller DMA confined to the driver's own window) ✓" } else { "FAILED" }
    );
    let _ = falloc.free(canary);
}

/// Is the start of the disk all zeros (never used)? Only such a disk is formatted.
fn disk_is_blank() -> bool {
    let mut b = alloc::vec![0u8; 64 * 1024];
    read_bytes(0, &mut b).is_ok() && b.iter().all(|&x| x == 0)
}

/// EuroFS on the data disk through nvmed, then a driver crash in the middle of
/// a write: after the restart the filesystem must mount from the disk, scrub
/// clean, keep the file committed before the crash byte for byte, and hold
/// the file being written either whole or not at all. Returns the mounted
/// filesystem for /data.
fn selftest_fs_and_crash(falloc: &mut FrameAllocator) -> Option<eurofs::EuroFs<UserBlk>> {
    use eurofs::FileSystem;
    let now = crate::rtc::epoch();
    let dev = UserBlk::new()?;
    let mut fs = match eurofs::EuroFs::mount(dev, now) {
        Ok(f) => {
            crate::serial_println!("[udrv-blk-fs] existing EuroFS on the data disk mounted through nvmed");
            f
        }
        Err(_) if disk_is_blank() => match eurofs::EuroFs::format(dev, [0xDA; 16], now) {
            Ok(f) => {
                crate::serial_println!("[udrv-blk-fs] blank data disk: EuroFS formatted through nvmed");
                f
            }
            Err(e) => {
                crate::serial_println!("[udrv-blk-fs] EuroFS format through nvmed failed: {e:?} → FAILED");
                return None;
            }
        },
        Err(_) => {
            crate::serial_println!("[udrv-blk-fs] the data disk holds something that is not EuroFS and is not blank: left untouched, not mounted ⬜");
            return None;
        }
    };
    let a: Vec<u8> = (0..20_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let wrote = fs.write_file("/committed.bin", &a).is_ok();
    let read_ok = fs.read_file("/committed.bin").map(|d| d == a).unwrap_or(false);
    let s1 = fs.scrub();
    let fs_ok = wrote && read_ok && s1.errors == 0 && s1.superblock_ok;
    crate::serial_println!(
        "[udrv-blk-fs] EuroFS through nvmed: 20000-byte file written={wrote}, read back identical={read_ok}; scrub: {} object(s), {} error(s), superblock ok={} → {}",
        s1.objects, s1.errors, s1.superblock_ok,
        if fs_ok { "OK ✓" } else { "FAILED" }
    );
    if !cfg!(feature = "selftest") {
        return Some(fs);
    }
    // The crash: nvmed faults right after its 24th next write, before it
    // reports it, in the middle of a 256 KiB file.
    let pid = DRIVER_PID.load(Ordering::Relaxed);
    let hellos = HELLOS.load(Ordering::Relaxed);
    let mut b = [0u8; 8];
    let n = proto::encode(&Msg::CrashAfterWrites { n: 24 }, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    let big: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 253) as u8 ^ 0x3C).collect();
    let errs0 = REQ_ERRORS.load(Ordering::Relaxed);
    let write_failed = fs.write_file("/in-flight.bin", &big).is_err();
    drop(fs);
    let died = wait_for(500, falloc, || !crate::ring3::bg_alive(pid));
    let back = wait_for(1500, falloc, || HELLOS.load(Ordering::Relaxed) > hellos && UP.load(Ordering::Acquire));
    let new_pid = DRIVER_PID.load(Ordering::Relaxed);
    if !(died && back) {
        crate::serial_println!("[udrv-blk-crash] nvmed died={died}, came back={back} → FAILED");
        return None;
    }
    let dev = UserBlk::new()?;
    let fs2 = match eurofs::EuroFs::mount(dev, crate::rtc::epoch()) {
        Ok(f) => f,
        Err(e) => {
            crate::serial_println!("[udrv-blk-crash] EuroFS does not mount after the crash: {e:?} → FAILED");
            return None;
        }
    };
    let s2 = fs2.scrub();
    let a_ok = fs2.read_file("/committed.bin").map(|d| d == a).unwrap_or(false);
    let b_state = match fs2.read_file("/in-flight.bin") {
        Err(_) => "absent (the commit never happened)",
        Ok(d) if d == big => "complete",
        Ok(_) => "TORN",
    };
    let reason = SUP.lock().last_death.clone().unwrap_or_default();
    let ok = write_failed && s2.errors == 0 && s2.superblock_ok && s2.bitmap_ok && a_ok && b_state != "TORN";
    crate::serial_println!(
        "[udrv-blk-crash] nvmed pid {pid} killed by a real fault mid-write ({reason}); the 256 KiB write failed={write_failed} ({} request error(s)); restarted as pid {new_pid}; EuroFS remounted from the disk: scrub {} object(s), {} error(s), superblock ok={}, bitmap ok={}; committed file intact={a_ok}; file being written: {b_state} → {}",
        REQ_ERRORS.load(Ordering::Relaxed) - errs0,
        s2.objects, s2.errors, s2.superblock_ok, s2.bitmap_ok,
        if ok { "OK (driver crash = I/O error, the filesystem stays consistent) ✓" } else { "FAILED" }
    );
    Some(fs2)
}

/// One status line for `hwprobe`/the System window.
pub fn status_line() -> String {
    if !ACTIVE.load(Ordering::Relaxed) {
        return String::from("userspace storage drivers: none");
    }
    let sup = SUP.lock();
    alloc::format!(
        "userspace nvmed: {} (starts {}, deaths {}), disk {}, {} sectors, requests {} (errors {}), irq {}",
        match sup.inst.as_ref() { Some(i) => alloc::format!("pid {}", i.pid), None => String::from("not running") },
        sup.starts,
        sup.tracker.total_deaths(),
        if UP.load(Ordering::Relaxed) { "up" } else { "down" },
        SECTORS.load(Ordering::Relaxed),
        REQS.load(Ordering::Relaxed),
        REQ_ERRORS.load(Ordering::Relaxed),
        udrv::irqs(Drv::Nvmed)
    )
}
