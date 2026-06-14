//! PS/2 keyboard (i8042), scancode set 1 — US QWERTY.
//!
//! **IRQ-driven**: the IRQ1 handler (interrupts.rs) reads the scancode from port
//! 0x60 and pushes it into a ring buffer via [`push_scancode`]. The shell fetches
//! decoded characters with [`poll_key`]. This way no key is lost if
//! the shell is not running for a moment due to the scheduler (unlike with polling).

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

static SHIFT: AtomicBool = AtomicBool::new(false);

const RING_SIZE: usize = 256;

struct Ring {
    buf: [u8; RING_SIZE],
    head: usize,
    tail: usize,
}

static SCANCODES: Mutex<Ring> = Mutex::new(Ring {
    buf: [0; RING_SIZE],
    head: 0,
    tail: 0,
});

/// Called from the IRQ1 handler (interrupts already disabled): buffer the scancode.
pub fn push_scancode(sc: u8) {
    let mut r = SCANCODES.lock();
    let next = (r.tail + 1) % RING_SIZE;
    if next != r.head {
        let tail = r.tail;
        r.buf[tail] = sc;
        r.tail = next;
    }
    // Buffer full → drop the newest scancode (should very rarely happen).
}

fn pop_scancode() -> Option<u8> {
    without_interrupts(|| {
        let mut r = SCANCODES.lock();
        if r.head == r.tail {
            None
        } else {
            let sc = r.buf[r.head];
            r.head = (r.head + 1) % RING_SIZE;
            Some(sc)
        }
    })
}

/// Fetch the next decoded character from the buffer (or `None`).
/// Returns `'\r'` (enter), `'\u{8}'` (backspace) or a printable character.
pub fn poll_key() -> Option<char> {
    while let Some(sc) = pop_scancode() {
        match sc {
            0x2A | 0x36 => SHIFT.store(true, Ordering::Relaxed),
            0xAA | 0xB6 => SHIFT.store(false, Ordering::Relaxed),
            _ if sc & 0x80 != 0 => {} // ignore other break codes
            _ => {
                if let Some(c) = translate(sc, SHIFT.load(Ordering::Relaxed)) {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn translate(sc: u8, shift: bool) -> Option<char> {
    let base = match sc {
        0x02 => '1', 0x03 => '2', 0x04 => '3', 0x05 => '4', 0x06 => '5',
        0x07 => '6', 0x08 => '7', 0x09 => '8', 0x0A => '9', 0x0B => '0',
        0x0C => '-', 0x0D => '=',
        0x0E => '\u{8}', // backspace
        0x0F => '\t',
        0x10 => 'q', 0x11 => 'w', 0x12 => 'e', 0x13 => 'r', 0x14 => 't',
        0x15 => 'y', 0x16 => 'u', 0x17 => 'i', 0x18 => 'o', 0x19 => 'p',
        0x1A => '[', 0x1B => ']',
        0x1C => '\r', // enter
        0x1E => 'a', 0x1F => 's', 0x20 => 'd', 0x21 => 'f', 0x22 => 'g',
        0x23 => 'h', 0x24 => 'j', 0x25 => 'k', 0x26 => 'l', 0x27 => ';',
        0x28 => '\'', 0x29 => '`', 0x2B => '\\',
        0x2C => 'z', 0x2D => 'x', 0x2E => 'c', 0x2F => 'v', 0x30 => 'b',
        0x31 => 'n', 0x32 => 'm', 0x33 => ',', 0x34 => '.', 0x35 => '/',
        0x39 => ' ',
        _ => return None,
    };
    if shift && base.is_ascii_alphabetic() {
        Some(base.to_ascii_uppercase())
    } else if shift {
        Some(shifted_symbol(base))
    } else {
        Some(base)
    }
}

fn shifted_symbol(c: char) -> char {
    match c {
        '1' => '!', '2' => '@', '3' => '#', '4' => '$', '5' => '%',
        '6' => '^', '7' => '&', '8' => '*', '9' => '(', '0' => ')',
        '-' => '_', '=' => '+', '/' => '?', '.' => '>', ',' => '<',
        ';' => ':', '\'' => '"', '\\' => '|', '[' => '{', ']' => '}', '`' => '~',
        _ => c,
    }
}
