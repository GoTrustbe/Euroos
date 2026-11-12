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
/// Alt held? Drives the Alt-Tab app switcher and Alt-1..4 workspace switch.
static ALT: AtomicBool = AtomicBool::new(false);
/// AltGr (right Alt, `E0 38`) held? Selects the third level of the layout
/// (BUG-052). It is never Alt: no Alt+Tab, no Alt+1..4.
static ALTGR: AtomicBool = AtomicBool::new(false);
/// An `0xE0` prefix was taken from the ring and its code has not been read yet.
/// Kept across calls: the two bytes of an extended key can arrive in different
/// loop iterations, and a prefix that was dropped turned `E0 38` into Alt.
static EXT: AtomicBool = AtomicBool::new(false);
/// Latched Alt+Tab presses (consumed by the desktop switcher).
static ALT_TAB: AtomicBool = AtomicBool::new(false);
/// Latched Alt+digit workspace request (0xFF = none, else 0-based workspace).
static WS_SWITCH: AtomicU8 = AtomicU8::new(0xFF);
/// Latched F1 press (help for the active window; consumed by `help::pump`).
static HELP_KEY: AtomicBool = AtomicBool::new(false);
/// Latched F2 press (keyboard shortcut overview).
static SHORTCUTS_KEY: AtomicBool = AtomicBool::new(false);
/// The active layout, as `Layout::ALL` index. Default 0 = US QWERTY.
static LAYOUT: AtomicU8 = AtomicU8::new(0);

/// Is the Alt key currently held?
pub fn alt_down() -> bool {
    ALT.load(Ordering::Relaxed)
}
/// Is AltGr (right Alt) currently held?
pub fn altgr_down() -> bool {
    ALTGR.load(Ordering::Relaxed)
}
/// Consume a pending Alt+Tab press.
pub fn take_alt_tab() -> bool {
    ALT_TAB.swap(false, Ordering::Relaxed)
}
/// Consume a pending Alt+digit workspace switch (0-based index).
pub fn take_ws_switch() -> Option<usize> {
    let v = WS_SWITCH.swap(0xFF, Ordering::Relaxed);
    if v == 0xFF { None } else { Some(v as usize) }
}

/// Consume a pending F1 press.
pub fn take_help_key() -> bool {
    HELP_KEY.swap(false, Ordering::Relaxed)
}
/// Consume a pending F2 press.
pub fn take_shortcuts_key() -> bool {
    SHORTCUTS_KEY.swap(false, Ordering::Relaxed)
}

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
    // F1 / F2 make codes are latched HERE, at the one place every keyboard
    // (PS/2, USB HID, virtio-input) passes, so help opens whichever consumer
    // drains the ring afterwards (char poll, rich poll, a program that owns
    // the keyboard). No layout maps these codes to a character.
    match sc {
        0x3B => HELP_KEY.store(true, Ordering::Relaxed),
        0x3C => SHORTCUTS_KEY.store(true, Ordering::Relaxed),
        _ => {}
    }
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

/// Copy the pending scancodes into `buf` WITHOUT consuming them; returns how
/// many. For an overlay that must decide whether the keys are meant for it
/// before anyone takes them (the first-run tour lets typing through).
pub fn peek_scancodes(buf: &mut [u8]) -> usize {
    without_interrupts(|| {
        let r = SCANCODES.lock();
        let mut i = r.head;
        let mut n = 0;
        while i != r.tail && n < buf.len() {
            buf[n] = r.buf[i];
            n += 1;
            i = (i + 1) % RING_SIZE;
        }
        n
    })
}

/// Take one pending Esc press out of the ring, wherever it sits, and say
/// whether there was one. For an overlay that closes on Esc while the keyboard
/// belongs to the window under it (the notification list, a context menu):
/// the window must not see that Esc as well. The break code stays; every
/// decoder ignores it.
pub fn take_pending_esc() -> bool {
    without_interrupts(|| {
        let mut r = SCANCODES.lock();
        let mut i = r.head;
        let mut prev = 0u8;
        let mut found = None;
        while i != r.tail {
            let sc = r.buf[i];
            if sc == 0x01 && prev != 0xE0 {
                found = Some(i);
                break;
            }
            prev = sc;
            i = (i + 1) % RING_SIZE;
        }
        let Some(mut i) = found else { return false };
        loop {
            let n = (i + 1) % RING_SIZE;
            if n == r.tail {
                break;
            }
            let v = r.buf[n];
            r.buf[i] = v;
            i = n;
        }
        r.tail = i;
        true
    })
}

/// The next scancode with its `0xE0` prefix resolved: (extended, code).
fn next_code() -> Option<(bool, u8)> {
    loop {
        let sc = pop_scancode()?;
        if sc == 0xE0 {
            EXT.store(true, Ordering::Relaxed);
            continue;
        }
        return Some((EXT.swap(false, Ordering::Relaxed), sc));
    }
}

/// `[altgr]` boot self-test (BUG-052): right Alt selects the third level and is
/// never Alt, in both decoders; left Alt still is. Runs on an empty ring and
/// leaves the layout and the latches as it found them.
pub fn altgr_selftest() {
    let saved = layout();
    while pop_scancode().is_some() {}
    EXT.store(false, Ordering::Relaxed);
    let feed = |codes: &[u8]| {
        for &c in codes {
            push_scancode(c);
        }
    };
    // (layout, scancode, expected third-level character)
    let cases: [(Layout, u8, char); 6] = [
        (Layout::BeAzerty, 0x03, '@'),
        (Layout::BeAzerty, 0x0A, '{'),
        (Layout::FrAzerty, 0x0B, '@'),
        (Layout::FrAzerty, 0x09, '\\'),
        (Layout::DeQwertz, 0x10, '@'),
        (Layout::DeQwertz, 0x56, '|'),
    ];
    let mut chars_ok = true;
    for (l, sc, want) in cases {
        set_layout(l);
        feed(&[0xE0, 0x38, sc, sc | 0x80, 0xE0, 0xB8]);
        let got = poll_key();
        let rest = poll_key();
        feed(&[0xE0, 0x38, sc, sc | 0x80, 0xE0, 0xB8]);
        let got_ex = poll_key_ex();
        let rest_ex = poll_key_ex();
        if got != Some(want) || rest.is_some() || got_ex != Some(Key::Char(want)) || rest_ex.is_some() {
            chars_ok = false;
        }
    }
    // AltGr+Tab and AltGr+1..4 switch nothing; AltGr leaves the Alt latch alone.
    set_layout(Layout::BeAzerty);
    let _ = take_alt_tab();
    let _ = take_ws_switch();
    feed(&[0xE0, 0x38, 0x0F, 0x8F, 0x02, 0x82, 0x05, 0x85, 0xE0, 0xB8]);
    let mut seen = alloc::vec::Vec::new();
    while let Some(c) = poll_key() {
        seen.push(c);
    }
    let no_switch = !take_alt_tab() && take_ws_switch().is_none() && !alt_down() && !altgr_down();
    let typed = seen == ['\t', '|'];
    feed(&[0xE0, 0x38, 0x0F, 0x8F, 0x03, 0x83, 0xE0, 0xB8]);
    let mut seen_ex = alloc::vec::Vec::new();
    while let Some(k) = poll_key_ex() {
        seen_ex.push(k);
    }
    let no_switch_ex = !take_alt_tab() && take_ws_switch().is_none() && !alt_down();
    let typed_ex = seen_ex == [Key::Tab, Key::Char('@')];
    // The prefix and its code in two separate polls (two loop iterations).
    feed(&[0xE0]);
    let none_yet = poll_key().is_none();
    feed(&[0x38, 0x03, 0x83, 0xE0, 0xB8]);
    let split = none_yet && poll_key() == Some('@') && !alt_down();
    while pop_scancode().is_some() {}
    // Left Alt is still Alt.
    feed(&[0x38, 0x0F, 0x8F, 0xB8]);
    while poll_key().is_some() {}
    let left_alt = take_alt_tab() && !alt_down();
    feed(&[0x38, 0x03, 0x83, 0xB8]);
    while poll_key_ex().is_some() {}
    let left_ws = take_ws_switch() == Some(1);
    set_layout(saved);
    let ok = chars_ok && no_switch && typed && no_switch_ex && typed_ex && split && left_alt && left_ws;
    crate::serial_println!(
        "[altgr] AltGr third level in both decoders (be, fr, de)={chars_ok}, AltGr+Tab/1..4 switch nothing={}, typed instead={}, split prefix={split}, left Alt still Alt+Tab={left_alt} Alt+2={left_ws} → {}",
        no_switch && no_switch_ex,
        typed && typed_ex,
        if ok { "OK ✓" } else { "FAILED ✗" }
    );
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
/// A decoded key: a character, or a navigation/edit key a text cursor needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Esc,
    /// Ctrl + a letter (lower-cased), e.g. Ctrl('s').
    Ctrl(char),
}

static CTRL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Rich key poll: like [`poll_key`] but also returns arrows, Home/End,
/// Page keys, Delete and Ctrl combos. Draining this starves `poll_key`, so a
/// consumer uses exactly one of the two.
pub fn poll_key_ex() -> Option<Key> {
    while let Some((ext, sc)) = next_code() {
        if ext {
            // Extended (`E0`-prefixed): right Alt is AltGr, the rest are the
            // navigation/edit keys; break codes and everything else are dropped.
            let k = match sc {
                0x38 => {
                    ALTGR.store(true, Ordering::Relaxed);
                    continue;
                }
                0xB8 => {
                    ALTGR.store(false, Ordering::Relaxed);
                    continue;
                }
                0x4B => Key::Left,
                0x4D => Key::Right,
                0x48 => Key::Up,
                0x50 => Key::Down,
                0x47 => Key::Home,
                0x4F => Key::End,
                0x49 => Key::PageUp,
                0x51 => Key::PageDown,
                0x53 => Key::Delete,
                _ => continue,
            };
            return Some(k);
        }
        match sc {
            0x2A | 0x36 => SHIFT.store(true, Ordering::Relaxed),
            0xAA | 0xB6 => SHIFT.store(false, Ordering::Relaxed),
            0x1D => CTRL.store(true, Ordering::Relaxed),
            0x9D => CTRL.store(false, Ordering::Relaxed),
            0x38 => ALT.store(true, Ordering::Relaxed),
            0xB8 => ALT.store(false, Ordering::Relaxed),
            // BUG-044: Alt+Tab (app switcher) and Alt+1..4 (workspace) are desktop
            // shortcuts whoever has the keyboard: latched as in `poll_key`, never
            // handed to the editor, the notes, the Settings panel or the installer.
            0x0F if ALT.load(Ordering::Relaxed) => {
                ALT_TAB.store(true, Ordering::Relaxed);
                continue;
            }
            0x02..=0x05 if ALT.load(Ordering::Relaxed) => {
                WS_SWITCH.store(sc - 0x02, Ordering::Relaxed);
                continue;
            }
            _ if sc & 0x80 != 0 => {} // ignore other break codes
            _ => {
                let ctrl = CTRL.load(Ordering::Relaxed);
                if let Some(c) = eurokeymap::translate_ex(layout(), sc, SHIFT.load(Ordering::Relaxed), ALTGR.load(Ordering::Relaxed)) {
                    return Some(match c {
                        '\r' | '\n' => Key::Enter,
                        '\u{8}' | '\u{7f}' => Key::Backspace,
                        '\t' => Key::Tab,
                        '\u{1b}' => Key::Esc,
                        c if ctrl && c.is_ascii_alphabetic() => Key::Ctrl(c.to_ascii_lowercase()),
                        c => Key::Char(c),
                    });
                }
            }
        }
    }
    None
}

pub fn poll_key() -> Option<char> {
    while let Some((ext, sc)) = next_code() {
        if ext {
            // BUG-052: `E0 38` is AltGr, not Alt. Without this arm the prefix was
            // dropped as a break code and the 0x38 after it set the Alt latch.
            // The keypad's Enter and slash are characters; the navigation keys
            // have no character and are dropped, as before.
            match sc {
                0x38 => ALTGR.store(true, Ordering::Relaxed),
                0xB8 => ALTGR.store(false, Ordering::Relaxed),
                0x1C => return Some('\r'),
                0x35 => return Some('/'),
                _ => {}
            }
            continue;
        }
        match sc {
            0x2A | 0x36 => SHIFT.store(true, Ordering::Relaxed),
            0xAA | 0xB6 => SHIFT.store(false, Ordering::Relaxed),
            0x38 => ALT.store(true, Ordering::Relaxed),
            0xB8 => ALT.store(false, Ordering::Relaxed),
            // Alt+Tab → app switcher; Alt+1..4 → workspace. Intercepted (no char).
            0x0F if ALT.load(Ordering::Relaxed) => ALT_TAB.store(true, Ordering::Relaxed),
            0x02..=0x05 if ALT.load(Ordering::Relaxed) => {
                WS_SWITCH.store(sc - 0x02, Ordering::Relaxed);
            }
            _ if sc & 0x80 != 0 => {} // ignore other break codes
            _ => {
                // Decode under the active layout (host-tested in eurokeymap).
                if let Some(c) = eurokeymap::translate_ex(layout(), sc, SHIFT.load(Ordering::Relaxed), ALTGR.load(Ordering::Relaxed)) {
                    return Some(c);
                }
            }
        }
    }
    None
}
