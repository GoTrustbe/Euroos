//! megaraidd: the EuroOS MegaRAID (MFI) storage driver, in ring 3.
//!
//! The kernel's supervisor (`kernel/src/udrv_mr.rs`) starts this process under
//! the `driver` syscall profile with a device grant already mapped: the MFI
//! register pages of the memory BAR (uncached; the MSI-X table in the same BAR
//! is NOT granted) and a zeroed DMA window, both at virtual == physical, and
//! under VT-d an IOMMU domain that holds exactly that window. The MFI logic is
//! `euromegaraid` (host-tested); this file is the glue: the HAL over the two
//! windows, and a loop over EuroIPC messages in the `euromegaraid::blkif`
//! format.
//!
//! In (own port, only the kernel may send): `Read`/`Write`/`Flush` block
//! requests naming a data slot of the window, `Irq` (MSI-X), and the
//! self-test commands `Crash`, `CrashDuringWrite`, `DmaProbe`.
//! Out (the block port, the only destination the grant allows): `Disk` per
//! logical drive and `Ready` after bring-up, `Done` per request, `Report`.
#![no_std]
#![no_main]

use core::fmt::Write as _;

use eurodrv::abi::{self, DriverInfo};
use euromegaraid::blkif::{self, Io, Msg};
use euromegaraid::frame::status_name;
use euromegaraid::info::{ld_state_name, MAX_LD};
use euromegaraid::layout::{Layout, SLOTS, SLOT_BYTES};
use euromegaraid::mfi::{Hal, Mfi};
use euromegaraid::regs::{self, Family};

mod sys {
    /// One raw syscall: rax = number, rdi/rsi/rdx = arguments.
    #[inline(always)]
    pub fn call(n: u64, a1: u64, a2: u64, a3: u64) -> u64 {
        let r: u64;
        // SAFETY: the syscall instruction with the kernel's documented register
        // convention; the kernel validates every pointer against our arena.
        unsafe {
            core::arch::asm!("syscall", inlateout("rax") n => r, in("rdi") a1, in("rsi") a2, in("rdx") a3,
                lateout("rcx") _, lateout("r11") _, options(nostack));
        }
        r
    }
}

const SYS_LOG: u64 = 0x6107;
const SYS_CLOCK_MS: u64 = 0x6108;
const SYS_SLEEP_MS: u64 = 0x6109;
const SYS_EXIT: u64 = 0x610C;

/// How long the firmware may sit in one state during the handshake.
const HANDSHAKE_TIMEOUT_MS: u64 = 180_000;
/// Busy-wait rounds on the reply ring before sleeping until the next message.
const SPIN_ROUNDS: u32 = 400_000;
/// `user` values of the self-test probes (kernel tags stay below).
const PROBE_USER: u32 = 0xFFFF_FF00;

fn log(s: &str) {
    sys::call(SYS_LOG, s.as_ptr() as u64, s.len() as u64, 0);
}

fn exit(code: u64) -> ! {
    sys::call(SYS_EXIT, code, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// A fixed buffer for formatted log lines (no heap in this process).
struct Line {
    b: [u8; 200],
    n: usize,
}

impl Line {
    fn new() -> Line {
        Line { b: [0; 200], n: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("?")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let take = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}

macro_rules! logf {
    ($($a:tt)*) => {{
        let mut l = Line::new();
        let _ = write!(l, $($a)*);
        log(l.as_str());
    }};
}

/// The device as the grant gives it. Every access is bounds-checked against
/// the grant; an out-of-range access is a driver bug and ends the process.
struct Hw {
    mmio: u64,
    mmio_len: u64,
    dma: u64,
    dma_len: u64,
}

impl Hw {
    fn check_dma(&self, addr: u64, len: usize) {
        let ok = addr >= self.dma && addr.checked_add(len as u64).is_some_and(|e| e <= self.dma + self.dma_len);
        if !ok {
            logf!("BUG: DMA access {addr:#x}+{len} outside the window");
            exit(3);
        }
    }
    fn check_reg(&self, off: u32) {
        if off as u64 + 4 > self.mmio_len {
            exit(4);
        }
    }
}

impl Hal for Hw {
    fn read32(&mut self, off: u32) -> u32 {
        self.check_reg(off);
        // SAFETY: inside the granted register pages, mapped uncached for this process.
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u32) }
    }

    fn write32(&mut self, off: u32, val: u32) {
        self.check_reg(off);
        // Frame writes must reach memory before the queue-port doorbell (x86
        // keeps store order; this keeps the compiler from reordering).
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: inside the granted register pages, mapped uncached for this process.
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u32, val) }
    }

    fn dma_read(&mut self, addr: u64, out: &mut [u8]) {
        self.check_dma(addr, out.len());
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: inside the granted DMA window; volatile: the device writes it.
            *b = unsafe { core::ptr::read_volatile((addr + i as u64) as *const u8) };
        }
    }

    fn dma_write(&mut self, addr: u64, data: &[u8]) {
        self.check_dma(addr, data.len());
        for (i, &b) in data.iter().enumerate() {
            // SAFETY: inside the granted DMA window (IOVA == our virtual address).
            unsafe { core::ptr::write_volatile((addr + i as u64) as *mut u8, b) };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }

    fn now_ms(&mut self) -> u64 {
        sys::call(SYS_CLOCK_MS, 0, 0, 0)
    }

    fn relax(&mut self) {
        sys::call(SYS_SLEEP_MS, 1, 0, 0);
    }
}

fn send(port: u64, msg: &Msg) -> bool {
    let mut buf = [0u8; blkif::MAX_MSG];
    match blkif::encode(msg, &mut buf) {
        Ok(n) => (sys::call(abi::SYS_IPC_SEND, port, buf.as_ptr() as u64, n as u64) as i64) >= 0,
        Err(_) => false,
    }
}

/// Fault on purpose (the crash self-tests): a user write to a page that is not ours.
fn crash() -> ! {
    // SAFETY: deliberately invalid; the kernel kills this process on the fault.
    unsafe { core::arch::asm!("mov qword ptr [{0}], 0", in(reg) 0x1000u64, options(nostack)) };
    exit(9)
}

#[derive(Clone, Copy, Default)]
struct Disk {
    target: u8,
    sectors: u64,
}

fn run() -> u64 {
    let mut ib = [0u8; abi::INFO_WORDS * 8];
    if sys::call(abi::SYS_DRV_INFO, ib.as_mut_ptr() as u64, ib.len() as u64, 0) != 0 {
        log("no device grant: exiting");
        return 1;
    }
    let info = DriverInfo::read_bytes(&ib);
    if regs::family(info.vendor as u16, info.device as u16) != Family::MfiGen2 {
        logf!("{:04x}:{:04x} is not an MFI gen2 controller: refusing to touch it", info.vendor, info.device);
        return 2;
    }
    let mut hw = Hw { mmio: info.mmio_base, mmio_len: info.mmio_len, dma: info.dma_base, dma_len: info.dma_len };
    let lay = match Layout::in_window(info.dma_base, info.dma_len) {
        Ok(l) => l,
        Err(e) => {
            logf!("DMA window unusable: {e:?}");
            return 2;
        }
    };
    let t0 = hw.now_ms();
    let before = hw.read32(regs::OSP0);
    // A generation per instance: completions of a previous instance are recognised.
    let gen = ((t0 / 10) as u16) ^ 0x5A5A;
    let mut mfi = match Mfi::bring_up(&mut hw, lay, gen, HANDSHAKE_TIMEOUT_MS) {
        Ok(m) => m,
        Err(e) => {
            logf!("firmware bring-up failed: {e:?} (status was {before:#010x})");
            return 5;
        }
    };
    logf!(
        "firmware: status {before:#010x} -> READY -> init frame OK -> OPERATIONAL in {} ms; {} commands, {} SGEs, reply ring {} entries, generation {:#06x}",
        hw.now_ms() - t0, mfi.fw.max_cmds(), mfi.fw.max_sge(), mfi.fw.max_cmds(), mfi.generation()
    );
    match mfi.ctrl_info(&mut hw) {
        Ok(ci) => {
            logf!(
                "controller: '{}' {:04x}:{:04x} (sub {:04x}:{:04x}), serial '{}', firmware package '{}', {} '{}'",
                ci.product.as_str(), ci.vendor, ci.device, ci.subvendor, ci.subdevice, ci.serial.as_str(),
                ci.package.as_str(), ci.fw_name.as_str(), ci.fw_version.as_str()
            );
            logf!(
                "controller: max {} commands, {} SGEs, max request {} sectors, {} LD(s) present ({} degraded, {} offline), {} PD(s), memory {} MiB",
                ci.max_cmds, ci.max_sge, ci.max_request_size, ci.lds_present, ci.lds_degraded, ci.lds_offline, ci.pd_present, ci.memory_mib
            );
        }
        Err(e) => {
            logf!("DCMD CTRL_GET_INFO failed: {e:?}");
            return 6;
        }
    }
    let list = match mfi.ld_list(&mut hw) {
        Ok(l) => l,
        Err(e) => {
            logf!("DCMD LD_GET_LIST failed: {e:?}");
            return 7;
        }
    };
    let mut disks = [Disk::default(); MAX_LD];
    let mut nd = 0usize;
    // Export in target-id order (the firmware's list order is its own business),
    // so "LD n" means the same drive across boots and restarts.
    let mut order = list;
    order.lds[..order.count].sort_unstable_by_key(|l| l.target);
    for ld in order.iter() {
        let (cap, bs) = match mfi.read_capacity(&mut hw, ld.target) {
            Ok(c) => c,
            Err(e) => {
                logf!("LD target {}: READ CAPACITY(16) failed: {e:?}; not exported", ld.target);
                continue;
            }
        };
        let agree = cap * bs as u64 == ld.sectors * 512;
        logf!(
            "LD {nd}: target {}, {}, {} sectors ({} MiB); SCSI pass-through READ CAPACITY(16): {} x {} B -> {}",
            ld.target, ld_state_name(ld.state), ld.sectors, ld.sectors / 2048, cap, bs,
            if agree { "matches the LD list" } else { "DIFFERS from the LD list" }
        );
        if bs != 512 {
            logf!("LD target {}: {} B sectors are not supported yet; not exported", ld.target, bs);
            continue;
        }
        disks[nd] = Disk { target: ld.target, sectors: ld.sectors };
        send(info.stack_port, &Msg::Disk { index: nd as u8, target: ld.target, state: ld.state, sectors: ld.sectors, sector_size: bs });
        nd += 1;
    }
    if list.clamped {
        log("the controller reported more LDs than the list holds: clamped");
    }
    // Drain: whatever an earlier instance left in the controller completes first.
    if let Err(e) = mfi.flush_sync(&mut hw) {
        logf!("cache flush failed: {e:?}");
    }
    send(info.stack_port, &Msg::Ready { disks: nd as u8, slots: SLOTS as u8, slot_bytes: SLOT_BYTES });

    let mut msg = [0u8; blkif::MAX_MSG];
    let mut crash_on_write: Option<u32> = None;
    let mut writes_seen = 0u32;
    let mut irqs = 0u64;
    loop {
        // Completions first: one `Done` per finished request.
        drain(&mut mfi, &mut hw, info.stack_port);
        // Commands in flight: watch the ring for a moment before sleeping.
        if mfi.in_flight() > 0 {
            let mut k = 0;
            while k < SPIN_ROUNDS && !mfi.pending(&mut hw) {
                core::hint::spin_loop();
                k += 1;
            }
            if k < SPIN_ROUNDS {
                continue;
            }
        }
        // One message; the kernel sleeps us one tick when nothing is pending.
        let r = sys::call(abi::SYS_IPC_RECV, msg.as_mut_ptr() as u64, msg.len() as u64, 0) as i64;
        if r <= 0 {
            continue;
        }
        match blkif::decode(&msg[..r as usize]) {
            Ok(Msg::Read(io)) => submit(&mut mfi, &mut hw, &lay, &disks[..nd], io, false, info.stack_port),
            Ok(Msg::Write(io)) => {
                submit(&mut mfi, &mut hw, &lay, &disks[..nd], io, true, info.stack_port);
                writes_seen += 1;
                if crash_on_write == Some(writes_seen) {
                    logf!("crash requested by the self-test: faulting with write #{writes_seen} (tag {}) in flight at the controller", io.tag);
                    crash();
                }
            }
            Ok(Msg::Flush { tag }) => {
                if mfi.post_flush(&mut hw, tag).is_err() {
                    send(info.stack_port, &Msg::Done { tag, status: blkif::DONE_NO_FRAME, scsi: 0 });
                }
            }
            Ok(Msg::Irq { count }) => {
                irqs += count as u64;
                mfi.ack_irq(&mut hw);
            }
            Ok(Msg::Crash) => {
                log("crash requested by the self-test: faulting now");
                crash();
            }
            Ok(Msg::CrashDuringWrite { nth }) => {
                writes_seen = 0;
                crash_on_write = Some(nth);
                logf!("armed: will fault right after posting write #{nth}");
            }
            Ok(Msg::DmaProbe { kind, target }) => probe(&mut mfi, &mut hw, kind, target, info.stack_port),
            _ => {}
        }
        let _ = irqs;
    }
}

/// Consume the reply ring and tell the kernel: one `Done` per request, one
/// `Report` per self-test probe.
fn drain(mfi: &mut Mfi, hw: &mut Hw, port: u64) {
    let mut done: [(u32, u8, u8); 32] = [(0, 0, 0); 32];
    let mut n = 0;
    mfi.reap(hw, |c| {
        if n < done.len() {
            done[n] = (c.user, c.status, c.scsi);
            n += 1;
        }
    });
    for &(user, status, scsi) in &done[..n] {
        if user >= PROBE_USER {
            logf!("DMA probe (device writes controller info to the target): completed with '{}'", status_name(status));
            send(port, &Msg::Report { code: blkif::REPORT_PROBE_DONE, a: (user - PROBE_USER) as u64, b: status as u64 });
        } else {
            send(port, &Msg::Done { tag: user, status, scsi });
        }
    }
}

fn submit(mfi: &mut Mfi, hw: &mut Hw, lay: &Layout, disks: &[Disk], io: Io, write: bool, port: u64) {
    let disk = disks.get(io.disk as usize);
    let ok = disk.is_some_and(|d| io.valid(d.sectors, SLOTS, SLOT_BYTES));
    let (Some(d), true, Some(buf)) = (disk, ok, lay.slot(io.slot as usize)) else {
        send(port, &Msg::Done { tag: io.tag, status: blkif::DONE_BAD_REQUEST, scsi: 0 });
        return;
    };
    if mfi.post_ld_io(hw, io.tag, write, d.target, io.lba, io.sectors, buf).is_err() {
        send(port, &Msg::Done { tag: io.tag, status: blkif::DONE_NO_FRAME, scsi: 0 });
    }
}

/// Play a malicious driver (self-test): aim the controller's DMA at memory
/// outside our window. The IOMMU must refuse it.
fn probe(mfi: &mut Mfi, hw: &mut Hw, kind: u8, target: u64, port: u64) {
    match kind {
        blkif::PROBE_DEVICE_WRITE => {
            logf!("DMA probe: DCMD CTRL_GET_INFO with its data buffer at {target:#x} (outside the window)");
            if mfi.post_info_to(hw, PROBE_USER + kind as u32, target).is_err() {
                send(port, &Msg::Report { code: blkif::REPORT_PROBE_DONE, a: kind as u64, b: 0xFF });
            }
        }
        blkif::PROBE_DEVICE_READ => {
            let stray = mfi.stats.stray;
            logf!("DMA probe: posting {target:#x} (outside the window) as a frame address");
            let posted = mfi.post_foreign_frame(hw, target).is_ok();
            // Whatever the controller completes for it carries no context of ours.
            for _ in 0..20 {
                hw.relax();
                drain(mfi, hw, port);
            }
            let strays = mfi.stats.stray - stray;
            logf!("DMA probe: foreign frame posted={posted}; {strays} stray completion(s) ignored");
            send(port, &Msg::Report { code: blkif::REPORT_PROBE_DONE, a: kind as u64, b: strays });
        }
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    exit(run())
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("panic");
    exit(101)
}
