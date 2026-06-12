//! COM1 (16550 UART) seriële poort voor debug-output.
//!
//! Cruciaal voor kernel-bring-up: dit werkt óók ná `ExitBootServices`, wanneer
//! er geen UEFI-console meer is. QEMU vangt dit op met `-serial file:serial.log`.
//! Zo zien we bij een zwart scherm precies tot waar de kernel kwam.

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
            ier.write(0x00); // interrupts uit
            lcr.write(0x80); // DLAB aan
            self.data.write(0x03); // divisor lo = 3 (38400 baud)
            ier.write(0x00); // divisor hi
            lcr.write(0x03); // 8N1, DLAB uit
            fcr.write(0xC7); // FIFO aan, clear, 14-byte threshold
            mcr.write(0x0B); // RTS/DSR/OUT2
        }
    }

    fn write_byte(&mut self, b: u8) {
        unsafe {
            // Wacht tot de transmit-holding-register leeg is (LSR bit 5).
            while self.lsr.read() & 0x20 == 0 {}
            self.data.write(b);
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
    // Tee: schrijf naar de UART én naar de kmsg-ring (S1 observability), zodat
    // `dmesg` en de panic-handler de recente historie hebben. Lock-volgorde is
    // altijd UART -> RING (klog::tee neemt geen UART-lock), dus geen deadlock.
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
}

/// Schrijf ruwe bytes RECHTSTREEKS naar de UART (panic-veilig: `try_lock`, en géén
/// tee terug naar de ring — voorkomt herlock van RING tijdens een paniek-dump).
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
