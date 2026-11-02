//! Userspace MegaRAID: the supervisor, the device grant and the boot
//! self-tests (the driver syscalls, the vector pool and the IRQ stubs are the
//! shared ones in `udrv.rs`) for `megaraidd`
//! (docs/SPRINT-PLAN-MEGARAID.md). The block layer on top is `mrblk.rs`.
//!
//! The kernel has no MegaRAID driver of its own: a gen2 MFI controller
//! (SAS2108, `1000:0079`, QEMU `-device megasas-gen2`) is driven from ring 3
//! or not at all. A fusion controller (Dell PERC H730 = SAS3108 `1000:005d`
//! and relatives) is recognised, named and left untouched.
//!
//! What the kernel gives the driver, and nothing more (`eurodrv::grant`):
//! - **MMIO**: the register pages of the memory BAR, `USER|RW|NX|PCD|PWT`, at
//!   virtual == physical. The MSI-X table and PBA share that BAR on gen2; their
//!   pages are cut off the grant (`euromegaraid::grant`), the kernel programs them.
//! - **DMA**: a zeroed window of `layout::WINDOW_PAGES` pages below 4 GiB
//!   (gen2 posts frames through a 32-bit port), mapped at virtual == physical,
//!   and under VT-d the device's own IOMMU domain holding exactly that window.
//! - **IRQ**: MSI-X entry 0 on a vector from the userspace-driver pool
//!   (`udrv::alloc_vector`); its stub bumps megaraidd's EuroIPC notification
//!   slot (`udrv::RES`), the driver receives an `Irq` message (blkif format).
//! - **EuroIPC**: its own port (only the kernel may send) and the block port
//!   (only this driver's pid may send).
//!
//! When the driver dies, `supervise` quiesces the device (bus mastering off,
//! IOMMU domain back to the empty table), reaps the process, and keeps the
//! old DMA window **in quarantine** until the next instance has reset the
//! controller (its `Ready`): a controller that still holds addresses into the
//! old window cannot write into memory that has been handed to someone else,
//! even on a machine without an IOMMU.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use eurodrv::abi::{self, DriverInfo};
use eurodrv::grant::{Bdf, DeviceGrant, Window};
use eurodrv::restart::{Decision, Policy, Tracker};
use euromegaraid::blkif::{self, Msg};
use euromegaraid::grant::MsixPlace;
use euromegaraid::layout::{self, Layout};
use euromegaraid::regs::{self as mrregs, Family};
use euromm::FrameAllocator;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::euroipc;
use crate::pci::{self, PciDevice};
use crate::udrv::{self, Drv, SysGrant};

static MEGARAIDD_ELF: &[u8] = include_bytes!("../../userland/megaraidd.elf");
static MEGARAIDD_SIG: &[u8] = include_bytes!("../../userland/megaraidd.elf.sig");

/// EuroIPC ports: the driver's own, and the block layer's.
pub const DRIVER_PORT: u32 = Drv::Megaraidd.res().driver_port;
pub const BLK_PORT: u32 = Drv::Megaraidd.res().stack_port;
const DOMAIN: &str = "megaraidd";
const ARENA_MIB: u64 = 4;
const PID_BASE: u64 = Drv::Megaraidd.res().pid_base;
/// Storage restart policy: as for NICs, 5 deaths per minute, then give up.
const POLICY: Policy = Policy { max_restarts: 5, window_ticks: 6000, backoff_ticks: 2, max_backoff_ticks: 200 };
/// Logical drives the block layer exports.
pub const MAX_DISKS: usize = 8;

// ── State read by the data path (atomics/short locks) ────────────────────────
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The current instance said `Ready` (disks known, controller reset done).
static READY: AtomicBool = AtomicBool::new(false);
/// Bumped at every start: requests sent to an older epoch must be resent.
static EPOCH: AtomicU64 = AtomicU64::new(0);
static DRIVER_PID: AtomicU64 = AtomicU64::new(u64::MAX);
/// Base of the current instance's DMA window (0 while none runs).
static DMA_BASE: AtomicU64 = AtomicU64::new(0);
static READIES: AtomicU64 = AtomicU64::new(0);
static SEND_REFUSED_WHILE_DEAD: AtomicBool = AtomicBool::new(false);
/// The boot frame allocator, for supervision from inside a block wait (task 0 only).
static FALLOC: AtomicPtr<FrameAllocator> = AtomicPtr::new(core::ptr::null_mut());

#[derive(Clone, Copy, Default)]
pub struct DiskInfo {
    pub target: u8,
    pub state: u8,
    pub sectors: u64,
}

static DISKS: Mutex<([DiskInfo; MAX_DISKS], usize)> = Mutex::new(([DiskInfo { target: 0, state: 0, sectors: 0 }; MAX_DISKS], 0));
/// The last `Report` from the driver: (code, a, b), code 0 = none.
static REPORT: Mutex<(u8, u64, u64)> = Mutex::new((0, 0, 0));
/// `Done` messages not yet picked up by the block layer: (tag, status, scsi).
static DONES: Mutex<Vec<(u32, u8, u8)>> = Mutex::new(Vec::new());

struct Instance {
    pid: u64,
    task: usize,
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
    /// A dead instance's DMA window, freed once the next instance is `Ready`.
    quarantine: Option<u64>,
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
    quarantine: None,
});

// ── The block port ───────────────────────────────────────────────────────────

/// Handle everything the driver sent to the block port.
pub fn pump() {
    for _ in 0..256 {
        let Some((sender, data)) = euroipc::take(BLK_PORT, |_| Vec::new()) else { return };
        if sender != DRIVER_PID.load(Ordering::Relaxed) {
            continue; // the port is restricted; belt and braces
        }
        match blkif::decode(&data) {
            Ok(Msg::Done { tag, status, scsi }) => without_interrupts(|| DONES.lock().push((tag, status, scsi))),
            Ok(Msg::Disk { index, target, state, sectors, sector_size }) => {
                if (index as usize) < MAX_DISKS && sector_size == 512 {
                    without_interrupts(|| {
                        let mut d = DISKS.lock();
                        d.0[index as usize] = DiskInfo { target, state, sectors };
                    });
                }
            }
            Ok(Msg::Ready { disks, slots, slot_bytes }) => {
                if slots as usize == layout::SLOTS && slot_bytes == layout::SLOT_BYTES {
                    without_interrupts(|| DISKS.lock().1 = (disks as usize).min(MAX_DISKS));
                    READIES.fetch_add(1, Ordering::Relaxed);
                    READY.store(true, Ordering::Release);
                } else {
                    crate::serial_println!("[mr] driver announced a slot geometry the kernel does not share ({slots} x {slot_bytes}): ignored");
                }
            }
            Ok(Msg::Report { code, a, b }) => without_interrupts(|| *REPORT.lock() = (code, a, b)),
            _ => {} // nothing else is valid from a driver
        }
    }
}

/// Take the completion for `tag`, if it arrived.
pub fn take_done(tag: u32) -> Option<(u8, u8)> {
    without_interrupts(|| {
        let mut d = DONES.lock();
        let i = d.iter().position(|e| e.0 == tag)?;
        let (_, s, c) = d.swap_remove(i);
        Some((s, c))
    })
}

/// Send a request to the driver. False when no instance is ready.
pub fn send_request(m: &Msg) -> bool {
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    let mut b = [0u8; blkif::MAX_MSG];
    let Ok(n) = blkif::encode(m, &mut b) else { return false };
    euroipc::send_with(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n], false) >= 0
}

pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

pub fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// The current instance's DMA layout (slots are addressed through it).
pub fn layout_now() -> Option<Layout> {
    let base = DMA_BASE.load(Ordering::Acquire);
    (base != 0).then(|| Layout { base, len: layout::WINDOW_BYTES })
}

pub fn disk(i: usize) -> Option<DiskInfo> {
    without_interrupts(|| {
        let d = DISKS.lock();
        (i < d.1).then(|| d.0[i])
    })
}

pub fn disk_count() -> usize {
    without_interrupts(|| DISKS.lock().1)
}

/// Supervision from inside a block-layer wait: only task 0 owns the frame
/// allocator. Anywhere else the desktop loop's supervision does the work.
pub fn supervise_from_wait() {
    if crate::sched::current() != 0 {
        return;
    }
    let p = FALLOC.load(Ordering::Acquire);
    if p.is_null() {
        return;
    }
    // SAFETY: set by `boot` from task 0's allocator, used only on task 0 (checked
    // above), never concurrently: the same contract as `crate::DMA_FALLOC`.
    supervise(unsafe { &mut *p });
}

// ── Supervisor ───────────────────────────────────────────────────────────────

fn find_device() -> Option<(PciDevice, Family)> {
    let d = pci::find(|d| d.vendor == 0x1000 && mrregs::family(d.vendor, d.device) != Family::Unknown)?;
    Some((d, mrregs::family(d.vendor, d.device)))
}

/// The first memory BAR: index, base, size (probed with decode off, PCI 6.2.5.1).
fn mem_bar(dev: &PciDevice) -> Option<(u8, u64, u64)> {
    let mut n = 0u8;
    while n < 6 {
        let off = 0x10 + n * 4;
        let v = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
        let is64 = v & 0x6 == 0x4;
        if v & 1 == 0 && dev.bar_addr(n) != 0 {
            let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
            pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd & !0x6);
            pci::cfg_write32(dev.bus, dev.dev, dev.func, off, 0xFFFF_FFFF);
            let probe = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
            pci::cfg_write32(dev.bus, dev.dev, dev.func, off, v);
            pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd);
            let size = eurodrv::grant::mem_bar_size(probe)?;
            return Some((n, dev.bar_addr(n), size));
        }
        n += if is64 && v & 1 == 0 { 2 } else { 1 };
    }
    None
}

/// Where the MSI-X table and PBA live (BAR index, offset).
fn msix_places(dev: &PciDevice) -> Vec<MsixPlace> {
    let Some(ptr) = dev.find_cap(0x11) else { return Vec::new() };
    let t = pci::cfg_read32(dev.bus, dev.dev, dev.func, ptr + 4);
    let p = pci::cfg_read32(dev.bus, dev.dev, dev.func, ptr + 8);
    alloc::vec![
        MsixPlace { bir: (t & 7) as u8, offset: (t & !7) as u64 },
        MsixPlace { bir: (p & 7) as u8, offset: (p & !7) as u64 },
    ]
}

fn bus_master(dev: &PciDevice, on: bool) {
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    let new = if on { cmd | 0x6 } else { cmd & !0x4 };
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, new);
}

fn free_window(falloc: &mut FrameAllocator, base: u64) {
    for i in 0..layout::WINDOW_PAGES as u64 {
        let _ = falloc.free(base + i * 4096);
    }
}

/// Start one driver instance. Task 0 only (the boot frame allocator).
fn start(sup: &mut Supervisor, falloc: &mut FrameAllocator) -> Result<u64, &'static str> {
    let dev = sup.dev.ok_or("no device")?;
    if !crate::crypto::verify(MEGARAIDD_ELF, MEGARAIDD_SIG) {
        return Err("megaraidd.elf signature invalid: refused (verify-before-execute)");
    }
    let bdf = Bdf { bus: dev.bus, dev: dev.dev, func: dev.func };
    let pages = layout::WINDOW_PAGES;
    let dma_base = falloc.allocate_aligned(pages, 1).map_err(|_| "no memory for the DMA window")?;
    if Layout::in_window(dma_base, layout::WINDOW_BYTES).is_err() {
        free_window(falloc, dma_base);
        return Err("DMA window not below 4 GiB (gen2 posts frames through a 32-bit port)");
    }
    // SAFETY: fresh frames from the allocator, identity-mapped.
    unsafe { core::ptr::write_bytes(dma_base as *mut u8, 0, pages * 4096) };
    let dma = Window::new(dma_base, layout::WINDOW_BYTES);
    let ram = Window::new(0, falloc.total_bytes().min(0x8000_0000));
    let irq = if sup.msix { udrv::alloc_vector(Drv::Megaraidd) } else { None };
    let grant = match DeviceGrant::new(bdf, sup.mmio, dma, irq, &[ram], &[sup.mmio]) {
        Ok(g) => g,
        Err(_) => {
            free_window(falloc, dma_base);
            return Err("grant refused by eurodrv::grant");
        }
    };
    // IOMMU first: the device gets its own domain before bus mastering is on.
    let iommu = crate::iommu::attach_runtime(falloc, DOMAIN, (bdf.bus, bdf.dev, bdf.func), dma_base, pages).is_some();
    if crate::iommu::translating() && !iommu {
        free_window(falloc, dma_base);
        return Err("IOMMU translation is on but the driver domain could not be built: refused");
    }
    let pid = PID_BASE + sup.starts as u64;
    let info = DriverInfo {
        abi: abi::ABI_VERSION,
        vendor: dev.vendor as u64,
        device: dev.device as u64,
        bdf: bdf.sid() as u64,
        mmio_base: grant.mmio.base,
        mmio_len: grant.mmio.len,
        dma_base,
        dma_len: dma.len,
        own_port: DRIVER_PORT as u64,
        stack_port: BLK_PORT as u64,
        irq_vector: irq.unwrap_or(0) as u64,
        flags: if iommu { abi::FLAG_IOMMU } else { 0 } | if irq.is_some() { abi::FLAG_MSIX } else { 0 },
    };
    READY.store(false, Ordering::Release);
    without_interrupts(|| {
        DISKS.lock().1 = 0;
        DONES.lock().clear();
    });
    euroipc::unregister(DRIVER_PORT);
    euroipc::register_restricted(pid, DRIVER_PORT, euroipc::KERNEL_PID);
    euroipc::set_only_from(BLK_PORT, pid);
    euroipc::bind_notify(Drv::Megaraidd.res().notify_slot, DRIVER_PORT);
    DRIVER_PID.store(pid, Ordering::Relaxed);
    // The device may DMA (and interrupt) from here on: into its domain only.
    if let Some(v) = irq {
        crate::msix::enable(&dev, 0, v, crate::apic::lapic_id() as u8);
    }
    bus_master(&dev, true);
    let spawned = without_interrupts(|| -> Result<usize, &'static str> {
        let task = crate::ring3::spawn_euro_app(falloc, MEGARAIDD_ELF, pid, &[b"megaraidd"], ARENA_MIB, crate::ring3::CAP_CONSOLE, eurosandbox::Profile::Driver)
            .ok_or("spawn failed (memory)")?;
        let (_, pml4, arena, arena_bytes) = crate::ring3::bg_layout(pid).ok_or("spawned process not found")?;
        for pa in grant.mmio.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, true) {
                crate::sched::mark_dead(task);
                return Err("could not map the register pages");
            }
        }
        for pa in grant.dma.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, false) {
                crate::sched::mark_dead(task);
                return Err("could not map the DMA window");
            }
        }
        udrv::set_grant(Drv::Megaraidd, Some(SysGrant::new(Drv::Megaraidd, task, pid, pml4, arena, arena_bytes, info)));
        Ok(task)
    });
    let task = match spawned {
        Ok(t) => t,
        Err(e) => {
            bus_master(&dev, false);
            if iommu {
                crate::iommu::detach_runtime(falloc, DOMAIN);
            }
            free_window(falloc, dma_base);
            return Err(e);
        }
    };
    DMA_BASE.store(dma_base, Ordering::Release);
    EPOCH.fetch_add(1, Ordering::AcqRel);
    sup.starts += 1;
    sup.inst = Some(Instance { pid, task, grant, iommu });
    sup.restart_at = None;
    ACTIVE.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[mr] megaraidd started: pid {pid}, task {task}, profile driver · registers {:#x}+{:#x} ({} page(s); MSI-X table/PBA pages not granted) · DMA {:#x}+{:#x} ({} pages) · IRQ {} · IOMMU {}",
        grant.mmio.base, grant.mmio.len, grant.mmio.pages(), dma_base, dma.len, grant.dma.pages(),
        irq.map(|v| alloc::format!("MSI-X {v:#x} -> EuroIPC")).unwrap_or_else(|| String::from("none (polled)")),
        if iommu { "own domain (DMA confined)" } else { "absent: DMA NOT confined" }
    );
    Ok(pid)
}

/// The driver died: quiesce the device first, then reap, then decide.
fn on_death(sup: &mut Supervisor, falloc: &mut FrameAllocator, inst: Instance) {
    let reason = crate::ring3::bg_end_reason(inst.pid).unwrap_or_else(|| String::from("terminated"));
    READY.store(false, Ordering::Release);
    SEND_REFUSED_WHILE_DEAD.store(!send_request(&Msg::Flush { tag: u32::MAX }), Ordering::Relaxed);
    udrv::set_grant(Drv::Megaraidd, None);
    DMA_BASE.store(0, Ordering::Release);
    // 1. No new DMA (and no MSI-X, which is a DMA write): bus mastering off.
    if let Some(dev) = sup.dev {
        bus_master(&dev, false);
    }
    // 2. No translation to the window any more: back to the empty table, flushed.
    let revoked = inst.iommu && crate::iommu::detach_runtime(falloc, DOMAIN);
    // 3. Its port and anything queued for it go.
    euroipc::unregister(DRIVER_PORT);
    // 4. The process; the window goes into quarantine (see the module docs).
    crate::ring3::reap_dead(falloc);
    if let Some(old) = sup.quarantine.replace(inst.grant.dma.base) {
        free_window(falloc, old); // two deaths before a Ready: the older one is long quiet
    }
    let decision = sup.tracker.on_death(crate::interrupts::ticks(), &POLICY);
    crate::serial_println!(
        "[mr] megaraidd pid {} died ({reason}) -> device quiesced: bus-master off, IOMMU domain {}, DMA window quarantined until the next instance has reset the controller; {}",
        inst.pid,
        if revoked { "revoked" } else { "n/a (no IOMMU)" },
        match decision {
            Decision::RestartAt(t) => alloc::format!("restart at tick {t} (death #{})", sup.tracker.total_deaths()),
            Decision::GiveUp => String::from("GIVING UP (too many deaths in the window; device stays off)"),
        }
    );
    sup.last_death = Some(reason);
    match decision {
        Decision::RestartAt(t) => sup.restart_at = Some(t),
        Decision::GiveUp => {
            sup.gave_up = true;
            udrv::release_vector(Drv::Megaraidd);
            if let Some(q) = sup.quarantine.take() {
                free_window(falloc, q); // bus mastering stays off: nothing can reach it
            }
        }
    }
}

/// Supervision tick: notice a dead driver, quiesce, restart on schedule,
/// release a quarantined window. Task 0 only.
pub fn supervise(falloc: &mut FrameAllocator) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    pump();
    let Some(mut sup) = SUP.try_lock() else { return };
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
                    crate::serial_println!("[mr] restart failed: {e}");
                    sup.restart_at = Some(crate::interrupts::ticks() + 100);
                }
            }
        }
    }
    if READY.load(Ordering::Acquire) {
        if let Some(q) = sup.quarantine.take() {
            free_window(falloc, q);
        }
    }
}

/// Wait (yielding) until `cond` holds or `ticks` pass. Task 0, interrupts on.
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
        crate::sched::yield_now();
        x86_64::instructions::hlt();
    }
}

fn send_test(m: &Msg) {
    let mut b = [0u8; blkif::MAX_MSG];
    if let Ok(n) = blkif::encode(m, &mut b) {
        euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    }
}

// ── Boot ─────────────────────────────────────────────────────────────────────

/// Find a MegaRAID controller and, for a gen2 MFI one, start megaraidd and run
/// the self-tests. Call from task 0 after the scheduler runs, interrupts are on
/// and `iommu::program` has run.
pub fn boot(falloc: &mut FrameAllocator) {
    let Some((dev, fam)) = find_device() else { return };
    let bdf = alloc::format!("{:02x}:{:02x}.{}", dev.bus, dev.dev, dev.func);
    match fam {
        Family::MfiGen2 => {}
        Family::Fusion(name) => {
            crate::serial_println!(
                "[mr] {bdf} {:04x}:{:04x} {name}: fusion (MPT request-descriptor) interface; megaraidd drives the MFI frame path only: device left untouched ⬜",
                dev.vendor, dev.device
            );
            return;
        }
        other => {
            crate::serial_println!("[mr] {bdf} {:04x}:{:04x} {other:?}: not driven (only MFI gen2 is implemented and proven) ⬜", dev.vendor, dev.device);
            return;
        }
    }
    let Some((bar, bar_base, bar_len)) = mem_bar(&dev) else {
        crate::serial_println!("[mr] {bdf}: no memory BAR: not started");
        return;
    };
    let places = msix_places(&dev);
    let (base, len) = match euromegaraid::grant::register_window(bar, bar_base, bar_len, &places) {
        Ok(w) => w,
        Err(e) => {
            crate::serial_println!("[mr] {bdf}: register window refused ({e:?}): not started");
            return;
        }
    };
    let Some(mmio) = Window::page_round(base, len) else { return };
    pci::claim(dev.bus, dev.dev, dev.func, "megaraidd (userspace)");
    euroipc::register_restricted(euroipc::KERNEL_PID, BLK_PORT, u64::MAX);
    let msix = !places.is_empty();
    FALLOC.store(falloc as *mut _, Ordering::Release);
    crate::serial_println!(
        "[mr] {bdf} {:04x}:{:04x} MegaRAID MFI gen2 (SAS2108): BAR{bar} {bar_base:#x}+{bar_len:#x}, MSI-X table at BAR{} +{:#x} -> grant {:#x}+{:#x}",
        dev.vendor, dev.device,
        places.first().map(|p| p.bir).unwrap_or(0), places.first().map(|p| p.offset).unwrap_or(0),
        mmio.base, mmio.len
    );
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
            crate::serial_println!("[mr-grant] megaraidd not started: {e} → FAILED");
            return;
        }
    };
    selftest_grant(pid);
    if !wait_for(1500, falloc, ready) {
        crate::serial_println!("[mr-info] megaraidd pid {pid} did not report Ready within 15 s → FAILED");
        return;
    }
    selftest_info();
    #[cfg(feature = "selftest")]
    {
        crate::mrblk::selftest_io(falloc);
        selftest_irq(msix);
        crate::mrblk::selftest_fs(falloc);
        crate::mrblk::selftest_crash(falloc);
        selftest_dma(falloc);
    }
    crate::serial_println!("[mr] {}", status_line());
}

fn selftest_grant(pid: u64) {
    let Some((_, pml4, _, _)) = crate::ring3::bg_layout(pid) else { return };
    let sup = SUP.lock();
    let Some(inst) = sup.inst.as_ref() else { return };
    let g = inst.grant;
    let regs_first = crate::paging::user_accessible_in(pml4, g.mmio.base);
    let regs_last = crate::paging::user_accessible_in(pml4, g.mmio.base + g.mmio.len - 4096);
    let msix_page = crate::paging::user_accessible_in(pml4, g.mmio.base + g.mmio.len);
    let before = crate::paging::user_accessible_in(pml4, g.mmio.base - 4096);
    let dma_in = crate::paging::user_accessible_in(pml4, g.dma.base) && crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len - 4096);
    let dma_past = crate::paging::user_accessible_in(pml4, g.dma.base + g.dma.len);
    let lapic = crate::paging::user_accessible_in(pml4, 0xFEE0_0000);
    let kernel_code = crate::paging::user_accessible_in(pml4, (selftest_grant as *const () as usize as u64) & !0xFFF);
    let ok = regs_first && regs_last && !msix_page && !before && dma_in && !dma_past && !lapic && !kernel_code;
    crate::serial_println!(
        "[mr-grant] megaraidd pid {pid}: register pages {:#x}..{:#x} mapped for ring 3 (first={regs_first}, last={regs_last}), next page (MSI-X table)={msix_page}, page before={before}; DMA window {} pages mapped={dma_in}, page after={dma_past}; LAPIC={lapic}, kernel code={kernel_code} → {}",
        g.mmio.base, g.mmio.base + g.mmio.len, g.dma.pages(),
        if ok { "OK (exactly the granted ranges) ✓" } else { "FAILED" }
    );
    let pages = crate::iommu::domain_pages(DOMAIN);
    if inst.iommu {
        crate::serial_println!(
            "[mr-iommu] device {:02x}:{:02x}.{} has its own VT-d domain '{DOMAIN}' with {pages} page(s) = the DMA window, nothing else → {}",
            g.bdf.bus, g.bdf.dev, g.bdf.func,
            if pages == layout::WINDOW_PAGES { "OK ✓" } else { "FAILED" }
        );
    } else {
        crate::serial_println!("[mr-iommu] no IOMMU translation on this machine: megaraidd's DMA is NOT confined (boot QEMU with -device intel-iommu,intremap=off) ⬜");
    }
}

fn selftest_info() {
    let n = disk_count();
    let mut lines = Vec::new();
    for i in 0..n {
        if let Some(d) = disk(i) {
            lines.push(alloc::format!("LD{i}=target {} {} sectors ({} MiB) {}", d.target, d.sectors, d.sectors / 2048, euromegaraid::info::ld_state_name(d.state)));
        }
    }
    crate::serial_println!(
        "[mr-info] controller info + LD list via DCMD (see the megaraidd lines above), exported to the block layer over EuroIPC: {} → {}",
        lines.join(", "),
        if n > 0 { "OK ✓" } else { "FAILED (no logical drive)" }
    );
}

#[cfg(feature = "selftest")]
fn selftest_irq(msix: bool) {
    let raised = udrv::irqs(Drv::Megaraidd);
    let delivered = udrv::irqs_delivered(Drv::Megaraidd);
    if !msix {
        crate::serial_println!("[mr-irq] device without MSI-X: megaraidd polls its reply ring, no interrupt path to prove ⬜");
        return;
    }
    crate::serial_println!(
        "[mr-irq] MSI-X vector 0x{:x}: {raised} interrupt(s) raised by reply-queue completions, {delivered} delivered to megaraidd as EuroIPC Irq messages → {}",
        udrv::vector_of(Drv::Megaraidd).unwrap_or(0),
        if raised > 0 && delivered > 0 { "OK ✓" } else { "FAILED" }
    );
}

/// Ask the driver to crash right after posting its `nth` write (self-test).
pub fn arm_crash_during_write(nth: u32) {
    send_test(&Msg::CrashDuringWrite { nth });
}

pub fn death_count() -> u64 {
    SUP.lock().tracker.total_deaths()
}

pub fn driver_pid() -> u64 {
    DRIVER_PID.load(Ordering::Relaxed)
}

pub fn readies() -> u64 {
    READIES.load(Ordering::Relaxed)
}

pub fn last_death() -> String {
    SUP.lock().last_death.clone().unwrap_or_default()
}

pub fn sends_refused_while_dead() -> bool {
    SEND_REFUSED_WHILE_DEAD.load(Ordering::Relaxed)
}

/// [mr-dma]: the driver turns malicious on request and aims the controller at
/// kernel memory twice: a DCMD whose data buffer is a kernel canary (the device
/// would WRITE 2 KiB of controller info there) and a frame address on a kernel
/// page (the device would READ it as a command and write status bytes into
/// it). Under VT-d both must fault, both pages must stay intact, the driver
/// must keep working.
#[cfg(feature = "selftest")]
fn selftest_dma(falloc: &mut FrameAllocator) {
    let iommu = SUP.lock().inst.as_ref().map(|i| i.iommu).unwrap_or(false);
    if !iommu {
        crate::serial_println!("[mr-dma] no IOMMU translation: skipped (the escape would succeed; that is the unconfined case) ⬜");
        return;
    }
    let (Ok(canary), Ok(secret)) = (falloc.allocate(), falloc.allocate()) else {
        crate::serial_println!("[mr-dma] no frames for the probe → FAILED");
        return;
    };
    // SAFETY: two fresh, identity-mapped frames of our own.
    unsafe {
        core::ptr::write_bytes(canary as *mut u8, 0xA5, 4096);
        core::ptr::write_bytes(secret as *mut u8, 0x5E, 4096);
    }
    // SAFETY (closure): our own two frames, identity-mapped, 4 KiB each.
    let intact = |p: u64, v: u8| unsafe { core::slice::from_raw_parts(p as *const u8, 4096) }.iter().all(|&b| b == v);
    let _ = crate::iommu::poll_faults();
    let run = |kind: u8, target: u64, falloc: &mut FrameAllocator| -> (bool, u64, usize) {
        without_interrupts(|| *REPORT.lock() = (0, 0, 0));
        send_test(&Msg::DmaProbe { kind, target });
        let reported = wait_for(500, falloc, || {
            let r = without_interrupts(|| *REPORT.lock());
            r.0 == blkif::REPORT_PROBE_DONE && r.1 == kind as u64
        });
        let _ = wait_for(10, falloc, || false);
        let b = without_interrupts(|| REPORT.lock().2);
        (reported, b, crate::iommu::poll_faults())
    };
    let (rep_w, status_w, faults_w) = run(blkif::PROBE_DEVICE_WRITE, canary, falloc);
    let (rep_r, strays, faults_r) = run(blkif::PROBE_DEVICE_READ, secret, falloc);
    let canary_ok = intact(canary, 0xA5);
    let secret_ok = intact(secret, 0x5E);
    // Still working afterwards: a read through the same driver.
    let still = crate::mrblk::probe_read(0);
    let ok = rep_w && rep_r && faults_w >= 1 && faults_r >= 1 && canary_ok && secret_ok && still;
    crate::serial_println!(
        "[mr-dma] malicious megaraidd: DCMD data buffer aimed at kernel canary {canary:#x} (completed, status {status_w:#x}) -> IOMMU fault records {faults_w}, canary intact={canary_ok}; frame address = kernel page {secret:#x} -> fault records {faults_r}, page intact={secret_ok}, {strays} stray completion(s) ignored · driver still works={still} → {}",
        if ok { "OK (controller DMA confined to the driver's own window) ✓" } else { "FAILED" }
    );
    let _ = falloc.free(canary);
    let _ = falloc.free(secret);
}

/// One status line for the log / hwprobe.
pub fn status_line() -> String {
    if !ACTIVE.load(Ordering::Relaxed) {
        return String::from("userspace megaraidd: none");
    }
    let sup = SUP.lock();
    alloc::format!(
        "userspace megaraidd: {} (starts {}, deaths {}, Ready messages {}), {} LD(s), ready={}, irq {}",
        match sup.inst.as_ref() { Some(i) => alloc::format!("pid {} task {}", i.pid, i.task), None => String::from("not running") },
        sup.starts,
        sup.tracker.total_deaths(),
        readies(),
        disk_count(),
        ready(),
        udrv::irqs(Drv::Megaraidd)
    )
}

/// Wait until a driver instance is `Ready` (self-tests, task 0).
pub fn wait_ready_from_task0(falloc: &mut FrameAllocator, ticks: u64) -> bool {
    wait_for(ticks, falloc, ready)
}

/// Is a completion waiting on the block port or in the done list?
pub fn has_done() -> bool {
    pump();
    without_interrupts(|| !DONES.lock().is_empty())
}
