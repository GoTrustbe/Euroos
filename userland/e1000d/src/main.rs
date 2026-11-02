//! e1000d: the EuroOS e1000/e1000e driver, in ring 3.
//!
//! The kernel's supervisor (`kernel/src/udrv.rs`) starts this process under
//! the `driver` syscall profile with a device grant already mapped: BAR 0 of
//! the NIC (uncached) and a zeroed DMA window, both at virtual == physical,
//! and under VT-d an IOMMU domain that holds exactly that window. The driver
//! logic is `euroe1000` (host-tested); this file is the glue: the HAL over the
//! two windows, and a loop over EuroIPC messages.
//!
//! Messages in (own port, only the kernel may send): `Tx` frames from the
//! netstack, `Irq` notifications (MSI-X), and the self-test commands `Crash`
//! and `DmaEscape`. Messages out (the netstack port, the only destination the
//! grant allows): `Hello` (link up, MAC), `Rx` frames, `Report`.
#![no_std]
#![no_main]

use core::fmt::Write;

use eurodrv::abi::{self, DriverInfo};
use eurodrv::proto::{self, Msg};
use euroe1000::{Hal, Layout, BUF_SIZE, E1000};

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

impl Line {
    fn new() -> Line {
        Line { b: [0; 160], n: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("?")
    }
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
        let mut l = Line::new();
        let _ = write!(l, $($a)*);
        log(l.as_str());
    }};
}

/// The device as the grant gives it: registers in the MMIO window, rings and
/// buffers in the DMA window. Every access is bounds-checked against the
/// grant; an out-of-range access is a driver bug and ends the process.
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
        // SAFETY: inside the granted BAR, mapped uncached for this process.
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u32) }
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        if off as u64 + 4 > self.mmio_len {
            exit(4);
        }
        // Descriptor writes must reach memory before the doorbell (x86 keeps
        // store order; this keeps the compiler from reordering).
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: inside the granted BAR, mapped uncached for this process.
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

/// Fault on purpose (the crash self-test): a user write to a page that is not ours.
fn crash() -> ! {
    // SAFETY: deliberately invalid; the kernel kills this process on the fault.
    unsafe { core::arch::asm!("mov qword ptr [{0}], 0", in(reg) 0x1000u64, options(nostack)) };
    exit(9)
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
            log("DMA window too small for the rings");
            return 2;
        }
    };
    let msix = info.flags & abi::FLAG_MSIX != 0;
    let mut nic = match E1000::init(&mut hw, layout, msix) {
        Ok(n) => n,
        Err(e) => {
            logf!("device init failed: {e:?}");
            return 5;
        }
    };
    let m = nic.mac();
    logf!(
        "up: {:04x}:{:04x} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, {}, DMA {}",
        info.vendor, info.device, m[0], m[1], m[2], m[3], m[4], m[5],
        if msix { "MSI-X" } else { "polled" },
        if info.flags & abi::FLAG_IOMMU != 0 { "confined by the IOMMU" } else { "NOT confined (no IOMMU)" }
    );
    let mut out = [0u8; proto::MAX_MSG];
    send(info.stack_port, &Msg::Hello { mac: m }, &mut out);

    let mut msg = [0u8; proto::MAX_MSG];
    let mut frame = [0u8; BUF_SIZE];
    // An RX slot the self-test aimed elsewhere: its "frame" is not ours to pass on.
    let mut poisoned: Option<usize> = None;
    loop {
        // Everything that arrived goes to the netstack.
        loop {
            let slot = nic.rx_next_slot();
            match nic.receive(&mut hw, &mut frame) {
                Some(n) => {
                    if poisoned == Some(slot) {
                        poisoned = None; // the device's write there was refused; drop it
                        continue;
                    }
                    if (proto::MIN_FRAME..=proto::MAX_FRAME).contains(&n) {
                        send(info.stack_port, &Msg::Rx(&frame[..n]), &mut out);
                    }
                }
                None => break,
            }
        }
        // One message: a frame to send, an interrupt, or a test command. The
        // kernel sleeps us one tick when nothing is pending (EAGAIN).
        let r = sys::call(abi::SYS_IPC_RECV, msg.as_mut_ptr() as u64, msg.len() as u64, 0) as i64;
        if r <= 0 {
            continue;
        }
        match proto::decode(&msg[..r as usize]) {
            Ok(Msg::Tx(f)) => {
                if nic.transmit(&mut hw, f).is_err() {
                    log("transmit failed");
                }
            }
            Ok(Msg::Irq { .. }) => {
                let _ = nic.ack_irq(&mut hw);
            }
            Ok(Msg::Crash) => {
                log("crash requested by the self-test: faulting now");
                crash();
            }
            Ok(Msg::DmaEscape { rx_target, tx_target }) => {
                // Play a malicious driver: point the device at memory that is
                // not in our window. The IOMMU must refuse both accesses.
                let slot = nic.rx_next_slot();
                nic.aim_rx_slot(&mut hw, slot, rx_target);
                poisoned = Some(slot);
                let tx = nic.transmit_raw(&mut hw, tx_target, 64);
                logf!("DMA escape attempt: RX slot {slot} -> {rx_target:#x}, TX from {tx_target:#x} ({:?})", tx);
                let report = Msg::Report { code: proto::REPORT_DMA_ESCAPE_DONE, a: slot as u64, b: tx.is_err() as u64 };
                send(info.stack_port, &report, &mut out);
            }
            _ => {}
        }
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
