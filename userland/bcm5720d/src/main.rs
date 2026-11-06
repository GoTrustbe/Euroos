//! bcm5720d: the EuroOS Broadcom BCM5720 driver, in ring 3.
//!
//! The kernel's supervisor (`kernel/src/udrv_bcm5720.rs`) starts this process
//! under the `driver` syscall profile with its grant already mapped: BAR 0
//! (registers + NIC SRAM window) and BAR 2 (the APE management processor),
//! both uncached, and a zeroed DMA window, all at virtual == physical; under
//! VT-d the device's own IOMMU domain holds exactly that window. The driver
//! logic is `eurobcm5720` (host-tested); this file is the glue: the HAL over
//! the three windows and a loop over EuroIPC messages.
//!
//! In: `Tx` frames, `Irq` notifications (MSI-X), and the self-test commands
//! `Crash` and `DmaEscape`. Out (the supervisor's port, the only destination
//! the grant allows): `Hello` (only when the PHY reports a link), `Rx` frames,
//! `Report`s (probe, link, loopback, escape, recovery).
#![no_std]
#![no_main]

use core::fmt::Write;

use eurobcm5720::{nvram_mac, wire, Bcm5720, Hal, Layout, Loopback, MacSource, BUF_SIZE, NRX};
use eurodrv::abi::{self, DriverInfo};
use eurodrv::proto::{self, Msg};

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

fn log(s: &str) {
    sys::call(SYS_LOG, s.as_ptr() as u64, s.len() as u64, 0);
}

fn exit(code: u64) -> ! {
    sys::call(SYS_EXIT, code, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

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

/// The device as the grant gives it. Every access is bounds-checked against
/// the grant; an out-of-range access is a driver bug and ends the process.
struct Hw {
    mmio: u64,
    mmio_len: u64,
    ape: u64,
    ape_len: u64,
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
        // SAFETY: inside the granted BAR 0, mapped uncached for this process.
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u32) }
    }

    fn reg_write(&mut self, off: u32, val: u32) {
        if off as u64 + 4 > self.mmio_len {
            exit(4);
        }
        // Descriptor writes must reach memory before the doorbell (x86 keeps
        // store order; this keeps the compiler from reordering).
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        // SAFETY: inside the granted BAR 0, mapped uncached for this process.
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u32, val) }
    }

    fn ape_read(&mut self, off: u32) -> u32 {
        if self.ape_len == 0 {
            return 0; // no APE window granted: reads as "no APE firmware"
        }
        if off as u64 + 4 > self.ape_len {
            exit(4);
        }
        // SAFETY: inside the granted BAR 2, mapped uncached for this process.
        unsafe { core::ptr::read_volatile((self.ape + off as u64) as *const u32) }
    }

    fn ape_write(&mut self, off: u32, val: u32) {
        if self.ape_len == 0 {
            return;
        }
        if off as u64 + 4 > self.ape_len {
            exit(4);
        }
        // SAFETY: inside the granted BAR 2, mapped uncached for this process.
        unsafe { core::ptr::write_volatile((self.ape + off as u64) as *mut u32, val) }
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

    /// No microsecond clock in ring 3: a register read crosses PCIe and takes
    /// about a microsecond, never less. Used for short, bounded waits only.
    fn delay_us(&mut self, us: u32) {
        for _ in 0..us {
            let _ = self.reg_read(eurobcm5720::regs::GRC_MODE);
        }
    }

    fn sleep_ms(&mut self, ms: u32) {
        sys::call(SYS_SLEEP_MS, ms as u64, 0, 0);
    }
}

fn send(port: u64, msg: &Msg, buf: &mut [u8]) -> bool {
    match proto::encode(msg, buf) {
        Ok(n) => (sys::call(abi::SYS_IPC_SEND, port, buf.as_ptr() as u64, n as u64) as i64) >= 0,
        Err(_) => false,
    }
}

fn report(port: u64, code: u8, a: u64, b: u64) {
    let mut out = [0u8; 32];
    send(port, &Msg::Report { code, a, b }, &mut out);
}

/// Fault on purpose (the crash self-test): a user write to a page that is not ours.
fn crash() -> ! {
    // SAFETY: deliberately invalid; the kernel kills this process on the fault.
    unsafe { core::arch::asm!("mov qword ptr [{0}], 0", in(reg) 0x1000u64, options(nostack)) };
    exit(9)
}

fn mode_name(m: Loopback) -> &'static str {
    match m {
        Loopback::Phy => "PHY",
        Loopback::Mac => "MAC-internal",
    }
}

fn selftest_loopback(nic: &mut Bcm5720, hw: &mut Hw, port: u64, mode: Loopback, frames: u32) {
    match nic.loopback_test(hw, mode, frames) {
        Ok(r) => {
            logf!("{} loopback: {} sent, {} back, {} byte-identical", mode_name(mode), r.sent, r.received, r.matched);
            let (a, b) = wire::loopback(mode, &r);
            report(port, wire::LOOPBACK, a, b);
        }
        Err(e) => logf!("{} loopback failed: {e:?}", mode_name(mode)),
    }
}

fn run() -> u64 {
    let mut ib = [0u8; (abi::INFO_WORDS + wire::EXTRA_WORDS) * 8];
    if sys::call(abi::SYS_DRV_INFO, ib.as_mut_ptr() as u64, ib.len() as u64, 0) != 0 {
        log("no device grant: exiting");
        return 1;
    }
    let info = DriverInfo::read_bytes(&ib);
    let extra = |i: usize| {
        let s = (abi::INFO_WORDS + i) * 8;
        let mut w = [0u8; 8];
        w.copy_from_slice(&ib[s..s + 8]);
        u64::from_le_bytes(w)
    };
    let (ape_base, ape_len, options) = (extra(0), extra(1), extra(2));
    let port = info.stack_port;
    let mut hw = Hw { mmio: info.mmio_base, mmio_len: info.mmio_len, ape: ape_base, ape_len, dma: info.dma_base, dma_len: info.dma_len };
    let layout = match Layout::in_window(info.dma_base, info.dma_len) {
        Ok(l) => l,
        Err(_) => {
            log("DMA window too small for the rings");
            return 2;
        }
    };
    let t0 = sys::call(SYS_CLOCK_MS, 0, 0, 0);
    let mut nic = match Bcm5720::init(&mut hw, layout) {
        Ok(n) => n,
        Err(e) => {
            logf!("device init failed: {e:?}");
            report(port, wire::INIT_FAILED, e.code(), 0);
            return 5;
        }
    };
    let ms = sys::call(SYS_CLOCK_MS, 0, 0, 0).saturating_sub(t0);
    let ci = nic.info();
    let m = nic.mac();
    let nv = nvram_mac(&mut hw, ci.pci_func);
    let v = ci.ape_version;
    logf!(
        "up in {ms} ms: BCM5720 chip {:#010x}, PCI fn {}, APE {} {}.{}.{}.{}, bootcode ASF {}, DMA {}, IRQ {}",
        ci.chip_id, ci.pci_func,
        if ci.ape { if ci.ape_ncsi { "NCSI" } else { "firmware" } } else { "absent" },
        v >> 24, (v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff,
        if ci.asf { "on" } else { "off" },
        if info.flags & abi::FLAG_IOMMU != 0 { "confined by the IOMMU" } else { "NOT confined (no IOMMU)" },
        if info.flags & abi::FLAG_MSIX != 0 { "MSI-X" } else { "polled" }
    );
    logf!(
        "MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} from the {}; NVRAM (offset {:#x}) says {}",
        m[0], m[1], m[2], m[3], m[4], m[5],
        match nic.mac_source() {
            MacSource::SramMailbox => "bootcode SRAM mailbox",
            MacSource::Nvram => "NVRAM",
            MacSource::Registers => "MAC registers",
        },
        if ci.pci_func & 1 == 1 { 0xcc } else { 0x7c },
        match nv {
            Some(n) if n == m => "the same",
            Some(_) => "something else",
            None => "nothing readable",
        }
    );
    let (a, b) = wire::probe(&ci, m, nic.mac_source(), nv == Some(m));
    report(port, wire::PROBE, a, b);
    match nic.phy_id(&mut hw) {
        Ok(id) => report(port, wire::PHY_ID, id as u64, ci.phy_addr as u64),
        Err(e) => logf!("PHY id unreadable: {e:?}"),
    }
    let mut link_up = false;
    match nic.link(&mut hw) {
        Ok(l) => {
            link_up = l.up;
            if l.up {
                logf!("link up: {} Mb/s {}", l.mbps, if l.full_duplex { "full duplex" } else { "half duplex" });
            } else {
                log("no link (PHY reports link down: no cable or no partner)");
            }
            report(port, wire::LINK, l.up as u64, l.mbps as u64 | (l.full_duplex as u64) << 16);
        }
        Err(e) => logf!("link state unreadable: {e:?}"),
    }
    if options & wire::OPT_SELFTEST != 0 {
        selftest_loopback(&mut nic, &mut hw, port, Loopback::Phy, 16);
        selftest_loopback(&mut nic, &mut hw, port, Loopback::Mac, 4);
    }
    let mut out = [0u8; proto::MAX_MSG];
    if link_up {
        send(port, &Msg::Hello { mac: m }, &mut out);
    }

    let mut msg = [0u8; proto::MAX_MSG];
    let mut frame = [0u8; BUF_SIZE];
    let mut last_link_check = sys::call(SYS_CLOCK_MS, 0, 0, 0);
    loop {
        // Everything that arrived goes to the supervisor (only with a link:
        // without one nothing can arrive but loopback leftovers).
        while let Some((n, _buf)) = nic.receive(&mut hw, &mut frame) {
            if link_up && (proto::MIN_FRAME..=proto::MAX_FRAME).contains(&n) {
                send(port, &Msg::Rx(&frame[..n]), &mut out);
            }
        }
        // The PHY is polled once a second (no link interrupt routing yet).
        let now = sys::call(SYS_CLOCK_MS, 0, 0, 0);
        if now.saturating_sub(last_link_check) >= 1000 {
            last_link_check = now;
            if let Ok(l) = nic.link(&mut hw) {
                if l.up != link_up {
                    link_up = l.up;
                    logf!("link {}", if l.up { "up" } else { "down" });
                    report(port, wire::LINK, l.up as u64, l.mbps as u64 | (l.full_duplex as u64) << 16);
                    if l.up {
                        send(port, &Msg::Hello { mac: m }, &mut out);
                    }
                }
            }
        }
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
            Ok(Msg::Irq { .. }) => nic.ack_irq(&mut hw),
            Ok(Msg::Crash) => {
                log("crash requested by the self-test: faulting now");
                crash();
            }
            Ok(Msg::DmaEscape { rx_target, tx_target }) => {
                // Play a malicious driver: an RX buffer and a TX source outside
                // our window. The IOMMU must refuse both. The RX descriptor is
                // used after the NRX buffers posted before it, so drive that
                // many frames through the PHY loopback.
                nic.post_rx_escape(&mut hw, rx_target);
                let rx = nic.loopback_test(&mut hw, Loopback::Phy, NRX as u32 + 2);
                let tx = nic.transmit_raw(&mut hw, tx_target, 64);
                let (sent, back) = rx.map(|r| (r.sent, r.received)).unwrap_or((0, 0));
                logf!("DMA escape attempt: RX buffer at {rx_target:#x} ({sent} loopback frames sent, {back} back), TX from {tx_target:#x} ({tx:?})");
                report(port, wire::ESCAPE_DONE, (sent as u64) << 32 | back as u64, tx.is_err() as u64);
                // A refused DMA read stops the chip's send engine; the core reset
                // of a fresh init brings it back.
                match Bcm5720::init(&mut hw, layout) {
                    Ok(n) => {
                        nic = n;
                        let r = nic.loopback_test(&mut hw, Loopback::Phy, 8);
                        let (s, mt) = r.map(|r| (r.sent, r.matched)).unwrap_or((0, 0));
                        logf!("re-initialised after the escape: PHY loopback {mt}/{s} byte-identical");
                        report(port, wire::RECOVERED, (s as u64) << 32 | mt as u64, 0);
                    }
                    Err(e) => {
                        logf!("re-init after the escape failed: {e:?}");
                        report(port, wire::INIT_FAILED, e.code(), 1);
                        return 6;
                    }
                }
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
