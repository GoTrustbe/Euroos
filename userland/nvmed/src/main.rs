//! nvmed: the EuroOS NVMe driver, in ring 3 (the first storage driver out of
//! the kernel, docs/SPRINT-PLAN-USERSPACE-STORAGE.md).
//!
//! The kernel's supervisor (`kernel/src/udrv_blk.rs`) starts this process under
//! the `driver` syscall profile with a device grant already mapped: the
//! register and doorbell pages of BAR 0 (uncached; not the MSI-X table) and a
//! zeroed DMA window, both at virtual == physical, and under VT-d an IOMMU
//! domain that holds exactly that window. The controller logic is `euronvme`
//! (host-tested); this file is the glue: the HAL over the two windows and a
//! loop over EuroIPC messages.
//!
//! Messages in (own port, only the kernel may send): `BlkRead`, `BlkWrite`,
//! `Irq` (MSI-X of the I/O completion queue), and the self-test commands
//! `Crash`, `CrashAfterWrites` and `DmaProbe`. Messages out (the kernel's disk
//! port, the only destination the grant allows): `BlkInfo` (disk up), `BlkDone`,
//! `Report`.
#![no_std]
#![no_main]

use core::fmt::Write;

use eurodrv::abi::{self, DriverInfo};
use eurodrv::proto::{self, Msg, BLK_CHUNK};
use euronvme::{Hal, Layout, Nvme};

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
const SYS_EXIT: u64 = 0x610C;

/// BlkDone status for a request this driver refuses (bad length, out of range).
const ST_INVALID: u16 = 0xFFFF;
/// BlkDone status when the controller did not complete in time.
const ST_TIMEOUT: u16 = 0xFFFE;

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
    b: [u8; 160],
    n: usize,
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let take = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + take].copy_from_slice(&s.as_bytes()[..take]);
        self.n += take;
        Ok(())
    }
}

macro_rules! logf {
    ($($a:tt)*) => {{
        let mut l = Line { b: [0; 160], n: 0 };
        let _ = write!(l, $($a)*);
        log(core::str::from_utf8(&l.b[..l.n]).unwrap_or("?"));
    }};
}

/// The controller as the grant gives it. Every access is bounds-checked
/// against the grant; an out-of-range access is a driver bug and ends the process.
struct Hw {
    mmio: u64,
    mmio_len: u64,
    dma: u64,
    dma_len: u64,
}

impl Hw {
    fn check_dma(&self, iova: u64, len: usize) {
        let ok = iova >= self.dma && iova.checked_add(len as u64).is_some_and(|e| e <= self.dma + self.dma_len);
        if !ok {
            logf!("BUG: DMA access {iova:#x}+{len} outside the window");
            exit(3);
        }
    }
}

impl Hal for Hw {
    fn reg_read(&mut self, off: u32) -> u32 {
        if off as u64 + 4 > self.mmio_len {
            exit(4);
        }
        // SAFETY: inside the granted BAR pages, mapped uncached for this process.
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u32) }
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        if off as u64 + 4 > self.mmio_len {
            exit(4);
        }
        // Queue entries must reach memory before the doorbell.
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: inside the granted BAR pages, mapped uncached for this process.
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u32, val) }
    }

    fn dma_write(&mut self, iova: u64, data: &[u8]) {
        self.check_dma(iova, data.len());
        // SAFETY: inside the granted DMA window (IOVA == our virtual address).
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), iova as *mut u8, data.len()) };
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }

    fn dma_read(&mut self, iova: u64, out: &mut [u8]) {
        self.check_dma(iova, out.len());
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: inside the granted DMA window; volatile: the device writes it.
            *b = unsafe { core::ptr::read_volatile((iova + i as u64) as *const u8) };
        }
    }
}

fn send(port: u64, msg: &Msg, buf: &mut [u8]) -> bool {
    match proto::encode(msg, buf) {
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

/// Messages that arrived while a command was in flight, served next (FIFO).
struct Stash {
    m: [[u8; proto::MAX_MSG]; 4],
    len: [usize; 4],
    n: usize,
}

impl Stash {
    fn push(&mut self, b: &[u8]) -> bool {
        if self.n == self.m.len() {
            return false;
        }
        self.m[self.n][..b.len()].copy_from_slice(b);
        self.len[self.n] = b.len();
        self.n += 1;
        true
    }
    fn pop(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        let l = self.len[0];
        out[..l].copy_from_slice(&self.m[0][..l]);
        for i in 1..self.n {
            self.m[i - 1] = self.m[i];
            self.len[i - 1] = self.len[i];
        }
        self.n -= 1;
        Some(l)
    }
}

/// Wait for the in-flight command: poll the completion queue, and between
/// polls take messages: an `Irq` is the device's completion interrupt
/// (counted), anything else is stashed for later. Bounded.
fn wait_done(nv: &mut Nvme, hw: &mut Hw, out: &mut [u8], stash: &mut Stash, irqs: &mut u64) -> Result<(), u16> {
    let mut msg = [0u8; proto::MAX_MSG];
    for _round in 0..2_000 {
        for _ in 0..2_000 {
            if let Some(r) = nv.complete(hw, out) {
                return r.map_err(|e| match e {
                    euronvme::Error::Status(s) => s.max(1),
                    _ => ST_INVALID,
                });
            }
            core::hint::spin_loop();
        }
        // Not yet: wait for the interrupt (the receive sleeps a tick when
        // nothing is pending).
        let r = sys::call(abi::SYS_IPC_RECV, msg.as_mut_ptr() as u64, msg.len() as u64, 0) as i64;
        if r > 0 {
            match proto::decode(&msg[..r as usize]) {
                Ok(Msg::Irq { count }) => *irqs += count as u64,
                Ok(_) => {
                    if !stash.push(&msg[..r as usize]) {
                        log("stash full: a request was dropped");
                    }
                }
                Err(_) => {}
            }
        }
    }
    Err(ST_TIMEOUT)
}

fn run() -> u64 {
    let mut ib = [0u8; abi::INFO_WORDS * 8];
    if sys::call(abi::SYS_DRV_INFO, ib.as_mut_ptr() as u64, ib.len() as u64, 0) != 0 {
        log("no device grant: exiting");
        return 1;
    }
    let info = DriverInfo::read_bytes(&ib);
    let mut hw = Hw { mmio: info.mmio_base, mmio_len: info.mmio_len, dma: info.dma_base, dma_len: info.dma_len };
    let layout = match Layout::in_window(info.dma_base, info.dma_len) {
        Ok(l) => l,
        Err(_) => {
            log("DMA window too small for the queues");
            return 2;
        }
    };
    let msix = info.flags & abi::FLAG_MSIX != 0;
    let mut nv = match Nvme::init(&mut hw, layout, msix) {
        Ok(n) => n,
        Err(e) => {
            logf!("controller init failed: {e:?}");
            return 5;
        }
    };
    logf!(
        "up: {:04x}:{:04x} '{}' NVMe {}.{}, {} sectors x {} B, {}, DMA {}",
        info.vendor, info.device, nv.model_str(), nv.version >> 16, (nv.version >> 8) & 0xff,
        nv.sectors, nv.sector_size,
        if msix { "MSI-X" } else { "polled" },
        if info.flags & abi::FLAG_IOMMU != 0 { "confined by the IOMMU" } else { "NOT confined (no IOMMU)" }
    );
    let mut out = [0u8; proto::MAX_MSG];
    send(info.stack_port, &Msg::BlkInfo { sectors: nv.sectors, sector_size: nv.sector_size }, &mut out);

    let mut msg = [0u8; proto::MAX_MSG];
    let mut data = [0u8; BLK_CHUNK];
    let mut stash = Stash { m: [[0; proto::MAX_MSG]; 4], len: [0; 4], n: 0 };
    let mut irqs = 0u64;
    let mut crash_after: Option<u32> = None;
    loop {
        let r = match stash.pop(&mut msg) {
            Some(l) => l as i64,
            None => sys::call(abi::SYS_IPC_RECV, msg.as_mut_ptr() as u64, msg.len() as u64, 0) as i64,
        };
        if r <= 0 {
            continue;
        }
        let m = msg;
        match proto::decode(&m[..r as usize]) {
            Ok(Msg::BlkRead { id, lba, count }) => {
                let bytes = count as usize * nv.sector_size as usize;
                let status = if count == 0 || bytes > BLK_CHUNK {
                    ST_INVALID
                } else {
                    match nv.submit(&mut hw, false, lba, count, &[]) {
                        Ok(()) => wait_done(&mut nv, &mut hw, &mut data[..bytes], &mut stash, &mut irqs).err().unwrap_or(0),
                        Err(_) => ST_INVALID,
                    }
                };
                let d = if status == 0 { &data[..bytes] } else { &data[..0] };
                send(info.stack_port, &Msg::BlkDone { id, status, data: d }, &mut out);
            }
            Ok(Msg::BlkWrite { id, lba, data: w }) => {
                let status = if w.len() % nv.sector_size as usize != 0 {
                    ST_INVALID
                } else {
                    let count = (w.len() / nv.sector_size as usize) as u16;
                    match nv.submit(&mut hw, true, lba, count, w) {
                        Ok(()) => wait_done(&mut nv, &mut hw, &mut [], &mut stash, &mut irqs).err().unwrap_or(0),
                        Err(_) => ST_INVALID,
                    }
                };
                if let Some(n) = crash_after {
                    if n <= 1 {
                        logf!("crash requested after this write (lba {lba}): faulting now, before reporting it");
                        crash();
                    }
                    crash_after = Some(n - 1);
                }
                send(info.stack_port, &Msg::BlkDone { id, status, data: &[] }, &mut out);
            }
            Ok(Msg::Irq { count }) => irqs += count as u64,
            Ok(Msg::Crash) => {
                log("crash requested by the self-test: faulting now");
                crash();
            }
            Ok(Msg::CrashAfterWrites { n }) => {
                logf!("will fault after {n} more write(s) (self-test)");
                crash_after = Some(n.max(1));
            }
            Ok(Msg::DmaProbe { target }) => {
                // Play a malicious driver: a read whose data pointer is outside
                // our window. The IOMMU must refuse the device's write.
                let r = nv.read_into_foreign(&mut hw, 0, target);
                logf!("DMA probe: read of LBA 0 aimed at {target:#x}: device says {r:?}");
                let st = match r {
                    Ok(()) => 0,
                    Err(euronvme::Error::Status(s)) => s as u64,
                    Err(_) => 0xFFFF,
                };
                send(info.stack_port, &Msg::Report { code: proto::REPORT_DMA_PROBE_DONE, a: r.is_err() as u64, b: st }, &mut out);
            }
            _ => {}
        }
        let _ = irqs;
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
