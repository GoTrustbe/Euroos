//! COM1 (16550 UART) serial port for debug output.
//!
//! Crucial for kernel bring-up: this also works after `ExitBootServices`, when
//! there is no longer a UEFI console. QEMU captures it with `-serial file:serial.log`.
//! That way, on a black screen we can see exactly how far the kernel got.

use core::sync::atomic::Ordering;
use core::fmt::{self, Write};

use spin::Mutex;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3F8;

struct Uart {
    data: Port<u8>,
    lsr: Port<u8>,
}

impl Uart {
    const fn new() -> Self {
        Self {
            data: Port::new(COM1),
            lsr: Port::new(COM1 + 5),
        }
    }

    fn init(&mut self) {
        let mut ier = Port::<u8>::new(COM1 + 1);
        let mut fcr = Port::<u8>::new(COM1 + 2);
        let mut lcr = Port::<u8>::new(COM1 + 3);
        let mut mcr = Port::<u8>::new(COM1 + 4);
        unsafe {
            ier.write(0x00); // interrupts off
            lcr.write(0x80); // DLAB on
            self.data.write(0x03); // divisor lo = 3 (38400 baud)
            ier.write(0x00); // divisor hi
            lcr.write(0x03); // 8N1, DLAB off
            fcr.write(0xC7); // FIFO on, clear, 14-byte threshold
            mcr.write(0x0B); // RTS/DSR/OUT2
        }
    }

    fn write_byte(&mut self, b: u8) {
        unsafe {
            // Wait until the transmit-holding register is empty (LSR bit 5).
            while self.lsr.read() & 0x20 == 0 {}
            self.data.write(b);
        }
    }

    /// Non-blocking read of one byte from the UART; `None` if no data is ready
    /// (LSR bit 0 = Data Ready). Powers the host-driven serial console (COM1 input).
    fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            if self.lsr.read() & 0x01 != 0 {
                Some(self.data.read())
            } else {
                None
            }
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}

static UART: Mutex<Uart> = Mutex::new(Uart::new());

pub fn init() {
    UART.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    // Tee: write to the UART and to the kmsg ring (S1 observability), so that
    // `dmesg` and the panic handler have the recent history. Lock order is
    // always UART -> RING (klog::tee takes no UART lock), so no deadlock.
    //
    // Interrupts OFF while the UART lock is held (BUG-007 class): interrupt
    // handlers print too (the xHCI MSI-X harvest logs its first reports); if one
    // preempts a task mid-print, it spins forever on this lock with interrupts
    // disabled — a silent boot hang. With the lock only ever held under IF=0,
    // an IRQ-context print can never see it taken on this CPU.
    //
    // Re-entrancy on the SAME cpu is the case IF=0 cannot cover: a page fault
    // while the line is being formatted (an argument that reads user memory), or
    // an NMI, runs a handler that prints too, and that print would spin forever
    // on a lock its own cpu holds. Run 37 on the NUC wedged exactly there
    // (serial::_print+0x2a, IF=0, on a fork child's CR3), and the NMI probe
    // printed nothing for the same reason. A nested print writes past the lock:
    // the holder is suspended underneath, so the port is free in practice, and a
    // garbled line beats a dead machine. It skips the kmsg tee (the ring lock
    // may be held by the same suspended frame).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let me = crate::apic::lapic_id().wrapping_add(1);
        if PRINTING_CPU.load(Ordering::Acquire) == me {
            // A second handle on the same port: the Uart is only the two port
            // numbers, the outer frame holding the lock cannot run until we return,
            // and the UART registers tolerate the interleaving.
            let mut uart = Uart::new();
            let _ = uart.write_fmt(args);
            return;
        }
        // A bounded wait: the holder is one task on this core with interrupts off,
        // so a lock still taken after 200 million spins belongs to a task that died
        // or slept while printing (runs 37 and 38 sat here forever, on a fork
        // child's CR3 and on the boot CR3). Force it open, say whose it was, and
        // go on: the next run names the path that leaves the lock behind.
        let mut spins = 0u64;
        let mut uart = loop {
            if let Some(g) = UART.try_lock() {
                break g;
            }
            core::hint::spin_loop();
            spins += 1;
            if spins == 200_000_000 {
                let holder = PRINTING_TASK.load(Ordering::Relaxed);
                let cpu = PRINTING_CPU.load(Ordering::Relaxed);
                // SAFETY: the holder cannot be running (single core, IF=0 here) and
                // will never release; the port is idle.
                unsafe { UART.force_unlock() };
                let mut u = Uart::new();
                let _ = u.write_fmt(format_args!(
                    "\n[serial] UART lock held by task {holder} (cpu {cpu}) through 200M spins: forced open, the holder died or slept while printing\n"
                ));
            }
        };
        PRINTING_CPU.store(me, Ordering::Release);
        // Lock-free on purpose: the census prints with SCHED held, and current()
        // takes SCHED (run 39 wedged in exactly that nested print).
        PRINTING_TASK.store(crate::sched::current_lockfree(), Ordering::Relaxed);
        struct Tee<'a>(&'a mut Uart);
        impl Write for Tee<'_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0.write_str(s)?;
                crate::klog::tee(s);
                Ok(())
            }
        }
        let _ = Tee(&mut uart).write_fmt(args);
        PRINTING_CPU.store(0, Ordering::Release);
    });
}

/// LAPIC id + 1 of the cpu that holds the UART lock inside `_print` (0 = none),
/// so a print nested on the same cpu can tell "held by me, suspended" from "held
/// by another core, about to be released".
static PRINTING_CPU: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Scheduler task index of that holder, for the forced-open message.
static PRINTING_TASK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Non-blocking read of one input byte from COM1 (`None` if nothing pending).
/// Used by the host-driven serial console to stream shell commands in.
pub fn read_byte() -> Option<u8> {
    UART.try_lock().and_then(|mut u| u.read_byte())
}

/// Write raw bytes DIRECTLY to the UART (panic-safe: `try_lock`, and no
/// tee back to the ring — prevents re-locking RING during a panic dump).
pub fn write_raw(bytes: &[u8]) {
    if let Some(mut uart) = UART.try_lock() {
        for &b in bytes {
            if b == b'\n' {
                uart.write_byte(b'\r');
            }
            uart.write_byte(b);
        }
    }
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
