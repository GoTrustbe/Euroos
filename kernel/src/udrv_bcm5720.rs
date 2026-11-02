//! Userspace driver for the Broadcom BCM5720 (`bcm5720d`): its supervisor,
//! grant and boot self-tests (docs/SPRINT-PLAN-BCM5720.md). Same model as
//! `udrv.rs` (e1000d), its own device state; the driver syscalls, the vector
//! pool and the IRQ->EuroIPC stubs are the shared ones in `udrv.rs`.
//!
//! What the kernel gives bcm5720d, and nothing more:
//! - **MMIO**: BAR 0 (registers and the NIC SRAM window) and BAR 2 (the APE
//!   management processor: its locks and driver-state block), both page-
//!   rounded, `USER|RW|NX|PCD|PWT` at virtual == physical. Not BAR 4 (the
//!   MSI-X table): the kernel programs where the chip's interrupts go. Both
//!   windows pass `eurodrv::grant` (not RAM, aligned, below the identity map,
//!   no overlap with each other or the DMA window).
//! - **DMA**: a zeroed window of `eurobcm5720::Layout::PAGES` pages, and under
//!   VT-d the function's own IOMMU domain holding exactly those pages.
//! - **IRQ**: MSI-X entry 0 -> a vector from the userspace-driver pool
//!   (`udrv::alloc_vector`) on the BSP; its stub bumps bcm5720d's EuroIPC
//!   notification slot (`udrv::RES`).
//! - **EuroIPC**: its own port (only the kernel may send) and the supervisor's
//!   port (only this driver's pid may send).
//!
//! Started with `net.driver=user` when a 14e4:165f is present. The netstack
//! (`nic.rs`) is not routed to it yet: without a cable on the test ports there
//! is no link to carry traffic (see the sprint plan).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use eurobcm5720::wire;
use eurodrv::abi::{self, DriverInfo};
use eurodrv::grant::{Bdf, DeviceGrant, Window};
use eurodrv::proto::{self, Msg};
use eurodrv::restart::{Decision, Policy, Tracker};
use euromm::FrameAllocator;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use crate::euroipc;
use crate::pci::{self, PciDevice};
use crate::udrv::{self, Drv, SysGrant};

static BCM5720D_ELF: &[u8] = include_bytes!("../../userland/bcm5720d.elf");
static BCM5720D_SIG: &[u8] = include_bytes!("../../userland/bcm5720d.elf.sig");

pub const DRIVER_PORT: u32 = Drv::Bcm5720d.res().driver_port;
pub const STACK_PORT: u32 = Drv::Bcm5720d.res().stack_port;
const DMA_PAGES: usize = eurobcm5720::Layout::PAGES as usize;
const DOMAIN: &str = "bcm5720d";
const ARENA_MIB: u64 = 4;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DRIVER_PID: AtomicU64 = AtomicU64::new(u64::MAX);
/// Reports from the driver, newest last (code, a, b). Bounded.
static REPORTS: Mutex<Vec<(u8, u64, u64)>> = Mutex::new(Vec::new());
static SELFTEST: AtomicBool = AtomicBool::new(false);

struct Instance {
    pid: u64,
    task: usize,
    grant: DeviceGrant,
    ape: Window,
    iommu: bool,
}

struct Supervisor {
    dev: Option<PciDevice>,
    bar0: Window,
    bar2: Window,
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
    bar0: Window { base: 0, len: 0 },
    bar2: Window { base: 0, len: 0 },
    inst: None,
    tracker: Tracker::new(),
    restart_at: None,
    gave_up: false,
    starts: 0,
    msix: false,
    last_death: None,
});

// ── Reports from the driver ──────────────────────────────────────────────────

/// Drain the supervisor port: keep the driver's reports (bounded), count frames.
fn pump() {
    for _ in 0..64 {
        let Some((sender, data)) = euroipc::take(STACK_PORT, |_| Vec::new()) else { return };
        if sender != DRIVER_PID.load(Ordering::Relaxed) {
            continue;
        }
        if let Ok(Msg::Report { code, a, b }) = proto::decode(&data) {
            without_interrupts(|| {
                let mut r = REPORTS.lock();
                if r.len() >= 64 {
                    r.remove(0);
                }
                r.push((code, a, b));
            });
        }
        // Hello / Rx: no netstack route yet (nic.rs is not wired to bcm5720d).
    }
}

/// The newest report with `code` received after index `from`.
fn report_since(from: usize, code: u8) -> Option<(u64, u64)> {
    without_interrupts(|| REPORTS.lock().iter().skip(from).rev().find(|r| r.0 == code).map(|r| (r.1, r.2)))
}

fn reports_len() -> usize {
    without_interrupts(|| REPORTS.lock().len())
}

// ── Supervisor ───────────────────────────────────────────────────────────────

fn find_device() -> Option<PciDevice> {
    pci::find(|d| d.vendor == 0x14E4 && d.device == 0x165F)
}

/// Size of memory BAR `n` (64-bit BARs: the low dword decides the size here;
/// the 5720's BARs are 64 KiB), probed with decode off.
fn bar_window(dev: &PciDevice, n: u8) -> Option<Window> {
    let base = dev.bar_addr(n);
    let off = 0x10 + n * 4;
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd & !0x6);
    let orig = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, off, 0xFFFF_FFFF);
    let probe = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, off, orig);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd);
    let size = eurodrv::grant::mem_bar_size(probe)?;
    if base == 0 {
        return None;
    }
    Window::page_round(base, size)
}

fn bus_master(dev: &PciDevice, on: bool) {
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    let new = if on { cmd | 0x6 } else { cmd & !0x4 };
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, new);
}

/// PCIe device control: relaxed ordering and no-snoop off, as every 57xx
/// driver does (the status block must not overtake the data it describes).
fn pcie_strict_ordering(dev: &PciDevice) {
    if let Some(cap) = dev.find_cap(0x10) {
        let off = cap + 8;
        let v = pci::cfg_read32(dev.bus, dev.dev, dev.func, off);
        pci::cfg_write32(dev.bus, dev.dev, dev.func, off, v & !((1 << 4) | (1 << 11)));
    }
}

fn free_dma(falloc: &mut FrameAllocator, base: u64) {
    for i in 0..DMA_PAGES as u64 {
        let _ = falloc.free(base + i * 4096);
    }
}

fn start(sup: &mut Supervisor, falloc: &mut FrameAllocator) -> Result<u64, &'static str> {
    let dev = sup.dev.ok_or("no device")?;
    if !crate::crypto::verify(BCM5720D_ELF, BCM5720D_SIG) {
        return Err("bcm5720d.elf signature invalid: refused (verify-before-execute)");
    }
    let bdf = Bdf { bus: dev.bus, dev: dev.dev, func: dev.func };
    let dma_base = falloc.allocate_aligned(DMA_PAGES, 1).map_err(|_| "no memory for the DMA window")?;
    // SAFETY: fresh frames from the allocator, identity-mapped.
    unsafe { core::ptr::write_bytes(dma_base as *mut u8, 0, DMA_PAGES * 4096) };
    let dma = Window::new(dma_base, (DMA_PAGES * 4096) as u64);
    let ram = Window::new(0, falloc.total_bytes().min(0x8000_0000));
    let irq = if sup.msix { udrv::alloc_vector(Drv::Bcm5720d) } else { None };
    // Both MMIO windows go through the same checks; they may not overlap.
    let grant = DeviceGrant::new(bdf, sup.bar0, dma, irq, &[ram, sup.bar2], &[sup.bar0, sup.bar2]);
    let ape = DeviceGrant::new(bdf, sup.bar2, dma, irq, &[ram, sup.bar0], &[sup.bar0, sup.bar2]);
    let grant = match (grant, ape) {
        (Ok(g), Ok(_)) => g,
        (g, a) => {
            crate::serial_println!(
                "[udrv] bcm5720d grant: BAR0 {:#x}+{:#x} -> {:?}, BAR2 {:#x}+{:#x} -> {:?}, DMA {dma_base:#x}, RAM below {:#x}",
                sup.bar0.base, sup.bar0.len, g.err(), sup.bar2.base, sup.bar2.len, a.err(), ram.len
            );
            free_dma(falloc, dma_base);
            return Err("grant refused by eurodrv::grant");
        }
    };
    let iommu = crate::iommu::attach_runtime(falloc, DOMAIN, (bdf.bus, bdf.dev, bdf.func), dma_base, DMA_PAGES).is_some();
    if crate::iommu::translating() && !iommu {
        free_dma(falloc, dma_base);
        return Err("IOMMU translation is on but the driver domain could not be built: refused");
    }
    let pid = Drv::Bcm5720d.res().pid_base + sup.starts as u64;
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
        stack_port: STACK_PORT as u64,
        irq_vector: irq.unwrap_or(0) as u64,
        flags: if iommu { abi::FLAG_IOMMU } else { 0 } | if irq.is_some() { abi::FLAG_MSIX } else { 0 },
    };
    euroipc::unregister(DRIVER_PORT);
    euroipc::register_restricted(pid, DRIVER_PORT, euroipc::KERNEL_PID);
    euroipc::set_only_from(STACK_PORT, pid);
    euroipc::bind_notify(Drv::Bcm5720d.res().notify_slot, DRIVER_PORT);
    DRIVER_PID.store(pid, Ordering::Relaxed);
    let ape = sup.bar2;
    let spawned = without_interrupts(|| -> Result<usize, &'static str> {
        let task = crate::ring3::spawn_euro_app(falloc, BCM5720D_ELF, pid, &[b"bcm5720d"], ARENA_MIB, crate::ring3::CAP_CONSOLE, eurosandbox::Profile::Driver)
            .ok_or("spawn failed (memory)")?;
        let (_, pml4, arena, arena_bytes) = crate::ring3::bg_layout(pid).ok_or("spawned process not found")?;
        for pa in grant.mmio.page_addrs().chain(ape.page_addrs()) {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, true) {
                crate::sched::mark_dead(task);
                return Err("could not map a BAR");
            }
        }
        for pa in grant.dma.page_addrs() {
            if !crate::paging::map_user_identity_4k(falloc, pml4, pa, false) {
                crate::sched::mark_dead(task);
                return Err("could not map the DMA window");
            }
        }
        // SYS_DRV_INFO: the standard block, then the APE window and the options
        // (`eurobcm5720::wire`).
        let opts = if SELFTEST.load(Ordering::Relaxed) { wire::OPT_SELFTEST } else { 0 };
        let mut g = SysGrant::new(Drv::Bcm5720d, task, pid, pml4, arena, arena_bytes, info);
        g.extra[..wire::EXTRA_WORDS].copy_from_slice(&[ape.base, ape.len, opts]);
        g.extra_words = wire::EXTRA_WORDS;
        udrv::set_grant(Drv::Bcm5720d, Some(g));
        Ok(task)
    });
    let task = match spawned {
        Ok(t) => t,
        Err(e) => {
            if iommu {
                crate::iommu::detach_runtime(falloc, DOMAIN);
            }
            free_dma(falloc, dma_base);
            return Err(e);
        }
    };
    if let Some(v) = irq {
        crate::msix::enable(&dev, 0, v, crate::apic::lapic_id() as u8);
    }
    pcie_strict_ordering(&dev);
    bus_master(&dev, true);
    sup.starts += 1;
    sup.inst = Some(Instance { pid, task, grant, ape, iommu });
    sup.restart_at = None;
    ACTIVE.store(true, Ordering::Relaxed);
    crate::serial_println!(
        "[udrv] bcm5720d started: pid {pid}, task {task}, profile driver · BAR0 {:#x}+{:#x} · BAR2 (APE) {:#x}+{:#x} · DMA {:#x}+{:#x} ({} pages) · IRQ {} · IOMMU {}",
        grant.mmio.base, grant.mmio.len, ape.base, ape.len, dma_base, dma.len, DMA_PAGES,
        irq.map(|v| alloc::format!("MSI-X {v:#x} -> EuroIPC")).unwrap_or_else(|| String::from("none (polled)")),
        if iommu { "own domain (DMA confined)" } else { "absent: DMA NOT confined" }
    );
    Ok(pid)
}

fn on_death(sup: &mut Supervisor, falloc: &mut FrameAllocator, inst: Instance) {
    let reason = crate::ring3::bg_end_reason(inst.pid).unwrap_or_else(|| String::from("terminated"));
    udrv::set_grant(Drv::Bcm5720d, None);
    if let Some(dev) = sup.dev {
        bus_master(&dev, false);
    }
    let revoked = inst.iommu && crate::iommu::detach_runtime(falloc, DOMAIN);
    euroipc::unregister(DRIVER_PORT);
    crate::ring3::reap_dead(falloc);
    free_dma(falloc, inst.grant.dma.base);
    let now = crate::interrupts::ticks();
    let decision = sup.tracker.on_death(now, &Policy::NIC);
    crate::serial_println!(
        "[udrv] bcm5720d pid {} died ({reason}) -> device quiesced: bus-master off, IOMMU domain {}, {DMA_PAGES} DMA pages freed; {}",
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
            ACTIVE.store(false, Ordering::Relaxed);
            udrv::release_vector(Drv::Bcm5720d);
        }
    }
}

/// Supervision tick (task 0): notice a dead driver, quiesce, restart on schedule.
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
                    crate::serial_println!("[udrv] bcm5720d restart failed: {e}");
                    sup.restart_at = Some(crate::interrupts::ticks() + 100);
                }
            }
        }
    }
    drop(sup);
    pump();
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
        crate::sched::yield_now();
        x86_64::instructions::hlt();
    }
}

fn macfmt(m: [u8; 6]) -> String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

// ── Boot and self-tests ──────────────────────────────────────────────────────

/// Boot: with `net.driver=user` and a BCM5720 present, start bcm5720d and
/// prove every step. Task 0, after the scheduler, interrupts and `iommu::program`.
pub fn boot(falloc: &mut FrameAllocator) {
    if !crate::bootopt::net_user_driver() {
        return;
    }
    let Some(dev) = find_device() else { return };
    crate::serial_println!(
        "[udrv-bcm] Broadcom BCM5720 (14e4:165f) at {:02x}:{:02x}.{}: driven from ring 3 by bcm5720d",
        dev.bus, dev.dev, dev.func
    );
    let (Some(bar0), Some(bar2)) = (bar_window(&dev, 0), bar_window(&dev, 2)) else {
        crate::serial_println!("[udrv-bcm] BAR0/BAR2 not usable memory BARs: not started → FAILED");
        return;
    };
    pci::claim(dev.bus, dev.dev, dev.func, "bcm5720d (userspace)");
    euroipc::register_restricted(euroipc::KERNEL_PID, STACK_PORT, u64::MAX);
    let msix = dev.find_cap(0x11).is_some();
    SELFTEST.store(true, Ordering::Relaxed);
    {
        let mut sup = SUP.lock();
        sup.dev = Some(dev);
        sup.bar0 = bar0;
        sup.bar2 = bar2;
        sup.msix = msix;
    }
    let from = reports_len();
    let started = {
        let mut sup = SUP.lock();
        start(&mut sup, falloc)
    };
    let pid = match started {
        Ok(p) => p,
        Err(e) => {
            crate::serial_println!("[udrv-bcm-grant] bcm5720d not started: {e} → FAILED");
            return;
        }
    };
    selftest_grant(pid);
    // The driver resets the chip (~0.1 s), probes, and runs the loopback self-test.
    let up = wait_for(1500, falloc, || report_since(from, wire::PROBE).is_some() || report_since(from, wire::INIT_FAILED).is_some());
    if let Some((code, _)) = report_since(from, wire::INIT_FAILED) {
        crate::serial_println!("[udrv-bcm] bcm5720d could not initialise the chip (error {code:#x}) → FAILED");
        return;
    }
    let Some((a, b)) = report_since(from, wire::PROBE).filter(|_| up) else {
        crate::serial_println!("[udrv-bcm] bcm5720d pid {pid} sent no probe report within 15 s → FAILED");
        return;
    };
    let p = wire::parse_probe(a, b);
    let phy = report_since(from, wire::PHY_ID).map(|(id, _)| id as u32);
    let _ = wait_for(500, falloc, || report_since(from, wire::LINK).is_some());
    let link = report_since(from, wire::LINK);
    let v = p.ape_version;
    let ok = p.chip_id >> 12 == 0x5720 && p.mac != [0; 6] && phy.is_some() && link.is_some();
    crate::serial_println!(
        "[udrv-bcm] chip {:#010x} (5720 A{}), APE firmware {} v{}.{}.{}.{}, bootcode handshake done (ASF {}); MAC {} from the {}, NVRAM {}; PHY id {}; link {} → {}",
        p.chip_id, p.chip_id & 0xF,
        if p.ape { if p.ncsi { "NCSI" } else { "present" } } else { "absent" },
        v >> 24, (v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff,
        if p.asf { "on" } else { "off" },
        macfmt(p.mac),
        match p.source { 0 => "bootcode SRAM mailbox", 1 => "NVRAM", _ => "MAC registers" },
        if p.nvram_agrees { "agrees" } else { "disagrees or unreadable" },
        phy.map(|i| alloc::format!("{:04x}:{:04x}", i >> 16, i & 0xffff)).unwrap_or_else(|| String::from("?")),
        match link {
            Some((1, s)) => alloc::format!("UP {} Mb/s", s & 0xffff),
            Some(_) => String::from("DOWN (no cable or no partner)"),
            None => String::from("?"),
        },
        if ok { "OK ✓" } else { "FAILED" }
    );
    selftest_loopback(falloc, from);
    selftest_irq(msix);
    if matches!(link, Some((1, _))) {
        crate::serial_println!("[udrv-bcm-net] link up, but the netstack is not routed to bcm5720d yet (nic.rs) ⬜");
    } else {
        crate::serial_println!("[udrv-bcm-net] no link on this port: DHCP and HTTP not attempted ⬜");
    }
    selftest_dma(falloc);
    selftest_crash(falloc, pid);
    crate::serial_println!("[udrv-bcm] {}", status_line());
}

fn selftest_grant(pid: u64) {
    let Some((_, pml4, _, _)) = crate::ring3::bg_layout(pid) else { return };
    let sup = SUP.lock();
    let Some(inst) = sup.inst.as_ref() else { return };
    let g = inst.grant;
    let ape = inst.ape;
    let acc = |a| crate::paging::user_accessible_in(pml4, a);
    let bar0 = acc(g.mmio.base) && acc(g.mmio.base + g.mmio.len - 4096);
    let bar2 = acc(ape.base) && acc(ape.base + ape.len - 4096);
    // The page just outside each window, unless it is the other window.
    let outside = |a: u64| !(g.mmio.contains(a, 1) || ape.contains(a, 1)) && acc(a);
    let leak = outside(g.mmio.base - 4096) || outside(g.mmio.base + g.mmio.len) || outside(ape.base - 4096) || outside(ape.base + ape.len);
    let dma_in = acc(g.dma.base) && acc(g.dma.base + g.dma.len - 4096);
    let dma_past = acc(g.dma.base + g.dma.len);
    let msix_tbl = sup.dev.map(|d| d.bar_addr(4)).filter(|&a| a != 0).map(|a| acc(a)).unwrap_or(false);
    let lapic = acc(0xFEE0_0000);
    let kernel_code = acc((selftest_grant as *const () as usize as u64) & !0xFFF);
    let ok = bar0 && bar2 && !leak && dma_in && !dma_past && !msix_tbl && !lapic && !kernel_code;
    crate::serial_println!(
        "[udrv-bcm-grant] bcm5720d pid {pid}: BAR0 {:#x}+{:#x} mapped={bar0}, BAR2 (APE) {:#x}+{:#x} mapped={bar2}, a page next to either={leak}; DMA {} pages mapped={dma_in}, page after={dma_past}; MSI-X table (BAR4)={msix_tbl}, LAPIC={lapic}, kernel code={kernel_code} → {}",
        g.mmio.base, g.mmio.len, ape.base, ape.len, g.dma.pages(),
        if ok { "OK (exactly the granted ranges) ✓" } else { "FAILED" }
    );
    let pages = crate::iommu::domain_pages(DOMAIN);
    if inst.iommu {
        crate::serial_println!(
            "[udrv-bcm-iommu] device {:02x}:{:02x}.{} has its own VT-d domain '{DOMAIN}' with {pages} page(s) = the DMA window, nothing else → {}",
            g.bdf.bus, g.bdf.dev, g.bdf.func,
            if pages == DMA_PAGES { "OK ✓" } else { "FAILED" }
        );
    } else {
        crate::serial_println!("[udrv-bcm-iommu] no IOMMU translation: bcm5720d's DMA is NOT confined ⬜");
    }
}

fn selftest_loopback(falloc: &mut FrameAllocator, from: usize) {
    // PHY then MAC loopback; both report.
    let both = |f| {
        without_interrupts(|| REPORTS.lock().iter().skip(f).filter(|r| r.0 == wire::LOOPBACK).count()) >= 2
    };
    let _ = wait_for(1000, falloc, || both(from));
    let reps: Vec<(u64, u64)> = without_interrupts(|| REPORTS.lock().iter().skip(from).filter(|r| r.0 == wire::LOOPBACK).map(|r| (r.1, r.2)).collect());
    let mut phy_ok = false;
    for (a, b) in reps {
        let (mode, r) = wire::parse_loopback(a, b);
        match mode {
            eurobcm5720::Loopback::Phy => {
                phy_ok = r.sent > 0 && r.matched == r.sent;
                crate::serial_println!(
                    "[udrv-bcm-loop] PHY loopback (1000/full): {} frames out through the send ring, {} back through the return ring, {} byte-identical → {}",
                    r.sent, r.received, r.matched,
                    if phy_ok { "OK (TX DMA read + RX DMA write through the IOMMU) ✓" } else { "FAILED" }
                );
            }
            eurobcm5720::Loopback::Mac => crate::serial_println!(
                "[udrv-bcm-loop] MAC-internal loopback: {} sent, {} back ({}) — informational",
                r.sent, r.received,
                if r.received == 0 { "Broadcom deprecates it on chips with a CPMU; Linux tg3 skips it on the 5720" } else { "works on this chip" }
            ),
        }
    }
    if !phy_ok {
        crate::serial_println!("[udrv-bcm-loop] no successful PHY loopback report → FAILED");
    }
}

fn selftest_irq(msix: bool) {
    let raised = udrv::irqs(Drv::Bcm5720d);
    let delivered = udrv::irqs_delivered(Drv::Bcm5720d);
    if !msix {
        crate::serial_println!("[udrv-bcm-irq] no MSI-X capability: polled ⬜");
        return;
    }
    crate::serial_println!(
        "[udrv-bcm-irq] MSI-X vector 0x{:x}: {raised} interrupt(s) raised by the chip, {delivered} delivered to bcm5720d as EuroIPC Irq messages → {}",
        udrv::vector_of(Drv::Bcm5720d).unwrap_or(0),
        if raised > 0 && delivered > 0 { "OK ✓" } else { "FAILED" }
    );
}

/// [udrv-bcm-dma]: the driver turns malicious on request and aims the chip at
/// two kernel frames: an RX buffer at a canary (the chip would WRITE a frame
/// there) and a TX source at a secret page (the chip would READ it out). With
/// QEMU's vIOMMU in front of a VFIO device the refusal happens in the host's
/// VT-d (QEMU never mapped those addresses); the guest sees: canary intact,
/// the aimed frame never returns, the send engine stops on the refused read.
fn selftest_dma(falloc: &mut FrameAllocator) {
    let iommu = SUP.lock().inst.as_ref().map(|i| i.iommu).unwrap_or(false);
    if !iommu {
        crate::serial_println!("[udrv-bcm-dma] no IOMMU translation: skipped (the escape would succeed) ⬜");
        return;
    }
    let (Ok(canary), Ok(secret)) = (falloc.allocate(), falloc.allocate()) else {
        crate::serial_println!("[udrv-bcm-dma] no frames for the probe → FAILED");
        return;
    };
    // SAFETY: two fresh, identity-mapped frames of our own.
    unsafe {
        core::ptr::write_bytes(canary as *mut u8, 0xA5, 4096);
        core::ptr::write_bytes(secret as *mut u8, 0x5E, 4096);
    }
    let _ = crate::iommu::poll_faults();
    let faults_before = crate::iommu::faults_total();
    let from = reports_len();
    let mut b = [0u8; 32];
    let n = proto::encode(&Msg::DmaEscape { rx_target: canary, tx_target: secret }, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    let done = wait_for(1500, falloc, || report_since(from, wire::RECOVERED).is_some() || report_since(from, wire::INIT_FAILED).is_some());
    let esc = report_since(from, wire::ESCAPE_DONE);
    let rec = report_since(from, wire::RECOVERED);
    let _ = crate::iommu::poll_faults();
    let guest_faults = crate::iommu::faults_total() - faults_before;
    // SAFETY: our own frame.
    let changed = unsafe { core::slice::from_raw_parts(canary as *const u8, 4096) }.iter().any(|&x| x != 0xA5);
    let (sent, back) = esc.map(|(a, _)| ((a >> 32) as u32, a as u32)).unwrap_or((0, 0));
    let tx_refused = esc.map(|(_, b)| b == 1).unwrap_or(false);
    let (rs, rm) = rec.map(|(a, _)| ((a >> 32) as u32, a as u32)).unwrap_or((0, 0));
    let ok = done && esc.is_some() && !changed && sent > 0 && back + 1 == sent && tx_refused && rs > 0 && rm == rs;
    crate::serial_println!(
        "[udrv-bcm-dma] malicious bcm5720d: RX buffer aimed at kernel canary {canary:#x}, TX from kernel page {secret:#x} · loopback frames {sent} sent, {back} back (the aimed one lost) · TX from the secret page completed={} · canary changed={changed} · guest vIOMMU fault records {guest_faults} (the host VT-d refuses VFIO DMA; see its DMAR log) · after a core reset: PHY loopback {rm}/{rs} → {}",
        !tx_refused,
        if ok { "OK (device DMA confined to the driver's own window) ✓" } else { "FAILED" }
    );
    let _ = falloc.free(canary);
    let _ = falloc.free(secret);
}

/// [udrv-bcm-crash]: a real ring-3 fault in the driver, a restart, the chip back.
fn selftest_crash(falloc: &mut FrameAllocator, _pid: u64) {
    let pid = DRIVER_PID.load(Ordering::Relaxed);
    let from = reports_len();
    let mut b = [0u8; 4];
    let n = proto::encode(&Msg::Crash, &mut b).unwrap_or(0);
    euroipc::send(euroipc::KERNEL_PID, DRIVER_PORT, &b[..n]);
    let died = wait_for(500, falloc, || !crate::ring3::bg_alive(pid));
    let back = wait_for(2000, falloc, || {
        let n = without_interrupts(|| REPORTS.lock().iter().skip(from).filter(|r| r.0 == wire::LOOPBACK).count());
        n >= 2
    });
    let new_pid = DRIVER_PID.load(Ordering::Relaxed);
    let probe = report_since(from, wire::PROBE).map(|(a, b)| wire::parse_probe(a, b));
    let lp = without_interrupts(|| REPORTS.lock().iter().skip(from).find(|r| r.0 == wire::LOOPBACK).map(|r| (r.1, r.2)));
    let (mode, r) = lp.map(|(a, b)| wire::parse_loopback(a, b)).unwrap_or((eurobcm5720::Loopback::Mac, Default::default()));
    let reason = SUP.lock().last_death.clone().unwrap_or_default();
    let ok = died && back && new_pid != pid && probe.is_some() && mode == eurobcm5720::Loopback::Phy && r.sent > 0 && r.matched == r.sent;
    crate::serial_println!(
        "[udrv-bcm-crash] bcm5720d pid {pid} killed by a real fault ({reason}); kernel kept running; restarted as pid {new_pid}: chip reset again, MAC {}, PHY loopback {}/{} → {}",
        probe.map(|p| macfmt(p.mac)).unwrap_or_default(),
        r.matched, r.sent,
        if ok { "OK (driver crash = process exit, chip back) ✓" } else { "FAILED" }
    );
}

pub fn status_line() -> String {
    if !ACTIVE.load(Ordering::Relaxed) {
        return String::from("userspace bcm5720d: not running");
    }
    let sup = SUP.lock();
    alloc::format!(
        "userspace bcm5720d: {} (starts {}, deaths {}), irq {}",
        match sup.inst.as_ref() { Some(i) => alloc::format!("pid {} task {}", i.pid, i.task), None => String::from("not running") },
        sup.starts,
        sup.tracker.total_deaths(),
        udrv::irqs(Drv::Bcm5720d)
    )
}
