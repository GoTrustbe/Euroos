//! PS/2 keyboard (i8042), scancode set 1. The active **keyboard layout** is
//! selectable (3F-4): US-QWERTY (default), BE/FR-AZERTY, DE-QWERTZ — decoded by
//! the host-tested [`eurokeymap`] crate.
//!
//! **IRQ-driven**: the IRQ1 handler (interrupts.rs) reads the scancode from port
//! 0x60 and pushes it into a ring buffer via [`push_scancode`]. The shell fetches
//! decoded characters with [`poll_key`]. This way no key is lost if
//! the shell is not running for a moment due to the scheduler (unlike with polling).

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use eurokeymap::Layout;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

static SHIFT: AtomicBool = AtomicBool::new(false);
/// The active layout, as `Layout::ALL` index. Default 0 = US QWERTY.
static LAYOUT: AtomicU8 = AtomicU8::new(0);

/// Set the active keyboard layout (installer keymap / `keymap` command).
pub fn set_layout(layout: Layout) {
    let idx = Layout::ALL.iter().position(|&l| l == layout).unwrap_or(0);
    LAYOUT.store(idx as u8, Ordering::Relaxed);
}

/// The active keyboard layout.
pub fn layout() -> Layout {
    Layout::ALL[LAYOUT.load(Ordering::Relaxed) as usize % Layout::ALL.len()]
}

/// Set the layout from a tag (`"be-azerty"`, `"de"`, …); `true` if recognised.
pub fn set_layout_tag(tag: &str) -> bool {
    match Layout::parse(tag) {
        Some(l) => {
            set_layout(l);
            true
        }
        None => false,
    }
}

/// `[3f4]` boot self-test — the same physical keys decode differently per active
/// layout (the AZERTY/QWERTZ transpositions), and the active layout is
/// switchable (installer keymap). Restores US-QWERTY afterwards.
pub fn keymap_selftest() {
    let saved = layout();
    // Scancode 0x10 = the physical 'Q' key; 0x2C = physical 'Z'; 0x15 = physical 'Y'.
    set_layout(Layout::UsQwerty);
    let us = eurokeymap::translate(layout(), 0x10, false); // 'q'
    set_layout(Layout::BeAzerty);
    let be = eurokeymap::translate(layout(), 0x10, false); // 'a'
    let be_digit = eurokeymap::translate(layout(), 0x02, true); // shift → '1'
    set_layout(Layout::DeQwertz);
    let de = eurokeymap::translate(layout(), 0x2C, false); // 'y'
    let switched = set_layout_tag("fr-azerty") && layout() == Layout::FrAzerty;
    set_layout(saved);

    let ok = us == Some('q') && be == Some('a') && be_digit == Some('1') && de == Some('y') && switched;
    crate::serial_println!(
        "[3f4] keyboard layouts (eurokeymap): US 'Q'-key={us:?}, AZERTY same key={be:?} (+shift-digit={be_digit:?}), QWERTZ 'Z'-key={de:?}, switch-by-tag={switched} → {}",
        if ok { "OK (US-QWERTY/BE-AZERTY/FR-AZERTY/DE-QWERTZ, installer-selectable) ✓" } else { "FAILED ✗" }
    );
}

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
    // IF-off + blocking lock (BUG-007 class, done right): this runs in IRQ context
    // AND in task context (the USB-HID harvest in `xhci::poll`, called from both the
    // desktop loop and the timer tick). The old `try_lock` avoided the IRQ-vs-task
    // deadlock but silently DROPPED the scancode on contention — a real make/break
    // code lost forever (the flaky-first-keystroke bug). Since every acquisition of
    // SCANCODES now happens with interrupts disabled, an IRQ can never preempt a
    // holder on this CPU: the blocking lock is deadlock-free AND lossless.
    without_interrupts(|| {
        let mut r = SCANCODES.lock();
        let next = (r.tail + 1) % RING_SIZE;
        if next != r.head {
            let tail = r.tail;
            r.buf[tail] = sc;
            r.tail = next;
        }
    });
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

/// Raw scancode (set 1) for a program that owns the keyboard — e.g. the DOOM
/// port, which needs press/release + arrow/ctrl keys that `poll_key`'s char
/// interface cannot express. Returns the next make/break code, or `None`.
/// Draining this starves `poll_key`, so callers use exactly one of the two.
pub fn poll_scancode() -> Option<u8> {
    pop_scancode()
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
                // Decode under the active layout (host-tested in eurokeymap).
                if let Some(c) = eurokeymap::translate(layout(), sc, SHIFT.load(Ordering::Relaxed)) {
                    return Some(c);
                }
            }
        }
    }
    None
}
