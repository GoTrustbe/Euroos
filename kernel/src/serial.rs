//! COM1 (16550 UART) serial port for debug output.
//!
//! Crucial for kernel bring-up: this also works after `ExitBootServices`, when
//! there is no longer a UEFI console. QEMU captures it with `-serial file:serial.log`.
//! That way, on a black screen we can see exactly how far the kernel got.

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
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut uart = UART.lock();
        struct Tee<'a>(&'a mut Uart);
        impl Write for Tee<'_> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                self.0.write_str(s)?;
                crate::klog::tee(s);
                Ok(())
            }
        }
        let _ = Tee(&mut uart).write_fmt(args);
    });
}

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
