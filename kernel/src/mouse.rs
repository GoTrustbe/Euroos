//! PS/2 mouse (i8042 auxiliary device), IRQ12 — drives the desktop cursor.
//!
//! The controller delivers 3-byte packets: [flags, dx, dy]. The IRQ12 handler
//! (interrupts.rs) pushes each byte here; we decode the packet and update
//! the cursor position + button state (atomically, so the desktop loop reads them).

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

static MOUSE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static BUTTONS: AtomicU8 = AtomicU8::new(0);
static SCREEN_W: AtomicUsize = AtomicUsize::new(0);
static SCREEN_H: AtomicUsize = AtomicUsize::new(0);
// Left-button press LATCH: set the instant a report shows a 0→1 edge, with the
// cursor position at that moment. The desktop loop consumes it via take_press(),
// so a click is never missed because the loop happened to sample between the
// press and release reports (which matters on a slow/emulated poll cycle).
static PRESS_PENDING: AtomicBool = AtomicBool::new(false);
static CLICK_X: AtomicI32 = AtomicI32::new(0);
static CLICK_Y: AtomicI32 = AtomicI32::new(0);
// Right-button press LATCH (same rationale as the left latch): opens the
// context menu at the exact spot the user right-clicked.
static RPRESS_PENDING: AtomicBool = AtomicBool::new(false);
static RCLICK_X: AtomicI32 = AtomicI32::new(0);
static RCLICK_Y: AtomicI32 = AtomicI32::new(0);

/// Store the new button bitmap and latch a left-press edge (with cursor pos).
/// Callers must update MOUSE_X/MOUSE_Y *before* calling this so the latched
/// position is correct.
/// Left-button EDGES, in order, with the position where each happened: (down, x, y).
/// A reader that samples the button LEVEL misses a whole click whenever it is not
/// running for the ~100 ms a real click lasts — which is exactly what happens while a
/// browser has the CPU. Every transition is queued here instead, so a click can be
/// late but never lost. Same principle as the scancode ring.
const BTN_RING: usize = 32;
static BTN_EVENTS: Mutex<([(bool, i32, i32); BTN_RING], usize, usize)> =
    Mutex::new(([(false, 0, 0); BTN_RING], 0, 0));

fn push_button_event(down: bool, x: i32, y: i32) {
    without_interrupts(|| {
        let mut r = BTN_EVENTS.lock();
        let (head, tail) = (r.1, r.2);
        let next = (tail + 1) % BTN_RING;
        if next != head {
            r.0[tail] = (down, x, y);
            r.2 = next;
        }
    });
}

/// Take the oldest queued left-button edge: (pressed, x, y).
pub fn take_button_event() -> Option<(bool, usize, usize)> {
    without_interrupts(|| {
        let mut r = BTN_EVENTS.lock();
        if r.1 == r.2 {
            return None;
        }
        let (down, x, y) = r.0[r.1];
        r.1 = (r.1 + 1) % BTN_RING;
        Some((down, x.max(0) as usize, y.max(0) as usize))
    })
}

fn update_buttons(new: u8) {
    let old = BUTTONS.swap(new & 0x07, Ordering::Relaxed);
    if old & 0x01 != new & 0x01 {
        push_button_event(new & 0x01 != 0,
                          MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed));
    }
    if old & 0x01 == 0 && new & 0x01 != 0 {
        CLICK_X.store(MOUSE_X.load(Ordering::Relaxed), Ordering::Relaxed);
        CLICK_Y.store(MOUSE_Y.load(Ordering::Relaxed), Ordering::Relaxed);
        PRESS_PENDING.store(true, Ordering::Relaxed);
    }
    // Right-button 0→1 edge (bit1): latch for the context menu.
    if old & 0x02 == 0 && new & 0x02 != 0 {
        RCLICK_X.store(MOUSE_X.load(Ordering::Relaxed), Ordering::Relaxed);
        RCLICK_Y.store(MOUSE_Y.load(Ordering::Relaxed), Ordering::Relaxed);
        RPRESS_PENDING.store(true, Ordering::Relaxed);
    }
}

/// Synthesize a left-press at (x,y) — a deterministic click for self-tests (routing a
/// click into a hosted X app's window without relying on flaky QMP mouse injection).
pub fn inject_press(x: usize, y: usize) {
    MOUSE_X.store(x as i32, Ordering::Relaxed);
    MOUSE_Y.store(y as i32, Ordering::Relaxed);
    CLICK_X.store(x as i32, Ordering::Relaxed);
    CLICK_Y.store(y as i32, Ordering::Relaxed);
    PRESS_PENDING.store(true, Ordering::Relaxed);
    // A synthetic click is a full click: press AND release, queued like a real one.
    push_button_event(true, x as i32, y as i32);
    push_button_event(false, x as i32, y as i32);
}

/// Consume a pending left-button press → the screen position where it happened.
pub fn take_press() -> Option<(usize, usize)> {
    if PRESS_PENDING.swap(false, Ordering::Relaxed) {
        Some((
            CLICK_X.load(Ordering::Relaxed).max(0) as usize,
            CLICK_Y.load(Ordering::Relaxed).max(0) as usize,
        ))
    } else {
        None
    }
}

// Packet assembly: (phase 0..3, bytes).
static PACKET: Mutex<(u8, [u8; 3])> = Mutex::new((0, [0; 3]));

fn wait_input_empty(status: &mut Port<u8>) {
    for _ in 0..100_000 {
        if unsafe { status.read() } & 0x02 == 0 {
            return;
        }
    }
}
fn wait_output_full(status: &mut Port<u8>) {
    for _ in 0..100_000 {
        if unsafe { status.read() } & 0x01 != 0 {
            return;
        }
    }
}

/// Initialize the PS/2 mouse. Must happen before unmasking IRQ12.
pub fn init(width: usize, height: usize) {
    SCREEN_W.store(width, Ordering::Relaxed);
    SCREEN_H.store(height, Ordering::Relaxed);
    MOUSE_X.store((width / 2) as i32, Ordering::Relaxed);
    MOUSE_Y.store((height / 2) as i32, Ordering::Relaxed);

    let mut data = Port::<u8>::new(0x60);
    let mut cmd = Port::<u8>::new(0x64);
    let mut status = Port::<u8>::new(0x64);
    unsafe {
        // Enable the auxiliary device (mouse).
        wait_input_empty(&mut status);
        cmd.write(0xA8);
        // Read config byte. Turn BOTH interrupts on (bit0=keyboard IRQ1,
        // bit1=mouse IRQ12) and BOTH clocks on (clear bit4=kbd, bit5=mouse).
        wait_input_empty(&mut status);
        cmd.write(0x20);
        wait_output_full(&mut status);
        let mut cfg = data.read();
        cfg |= 0b11; // bit0 kbd-IRQ + bit1 mouse-IRQ
        cfg &= !0b11_0000; // clear bit4 (kbd-clock) + bit5 (mouse-clock) → both on
        wait_input_empty(&mut status);
        cmd.write(0x60);
        wait_input_empty(&mut status);
        data.write(cfg);
        // Keyboard: enable scanning (0xF4 directly to the kbd) so keys
        // ACTUALLY generate scancodes + IRQ1 (otherwise only the self-test byte arrived).
        wait_input_empty(&mut status);
        data.write(0xF4);
        wait_output_full(&mut status);
        let _ack_kbd = data.read(); // 0xFA
        // Mouse: defaults + data reporting on (each with 0xD4 prefix to aux).
        mouse_write(&mut cmd, &mut data, &mut status, 0xF6); // set defaults
        mouse_write(&mut cmd, &mut data, &mut status, 0xF4); // enable reporting
    }
}

unsafe fn mouse_write(cmd: &mut Port<u8>, data: &mut Port<u8>, status: &mut Port<u8>, value: u8) {
    wait_input_empty(status);
    cmd.write(0xD4);
    wait_input_empty(status);
    data.write(value);
    wait_output_full(status);
    let _ack = data.read(); // 0xFA
}

/// Called by the IRQ12 handler with each received byte.
pub fn push_byte(byte: u8) {
    // IF-off + blocking lock (BUG-007 class, same rule as push_scancode): called
    // from the PS/2 mouse IRQ and from task context. Every PACKET acquisition
    // happens with interrupts disabled, so an IRQ can never preempt a holder on
    // this CPU — deadlock-free without silently dropping bytes on contention.
    let done = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut p = PACKET.lock();
        let phase = p.0;
        // Resynchronize: byte 0 must have bit 3 set.
        if phase == 0 && byte & 0x08 == 0 {
            return None;
        }
        p.1[phase as usize] = byte;
        if phase < 2 {
            p.0 = phase + 1;
            return None;
        }
        p.0 = 0;
        Some((p.1[0], p.1[1] as i8 as i32, p.1[2] as i8 as i32))
    });
    let Some((flags, dx, dy)) = done else { return };

    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    // Y is inverted: mouse up = positive dy = cursor up.
    let nx = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w - 1);
    let ny = (MOUSE_Y.load(Ordering::Relaxed) - dy).clamp(0, h - 1);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
    update_buttons(flags); // position updated first, so the press latch is correct
}

/// Apply a relative USB-HID mouse movement + button state (same cursor atomics
/// as the PS/2 mouse, so the desktop works transparently on USB input). `dy` is in
/// HID convention (down positive), so we add it directly to Y.
pub fn apply_usb(dx: i32, dy: i32, buttons: u8) {
    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    if w == 0 || h == 0 {
        return;
    }
    let nx = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w - 1);
    let ny = (MOUSE_Y.load(Ordering::Relaxed) + dy).clamp(0, h - 1);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
    update_buttons(buttons);
}

/// Apply an ABSOLUTE USB-HID pointer report (usb-tablet / touchscreen). The
/// device reports X/Y in a fixed logical range (0..=0x7FFF) which we scale to
/// the framebuffer, setting the cursor directly. Unlike the relative mouse this
/// tracks a VNC/remote pointer exactly — no drift, no "two cursors".
pub fn apply_usb_abs(x_abs: u16, y_abs: u16, buttons: u8) {
    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    if w == 0 || h == 0 {
        return;
    }
    let nx = ((x_abs as i32) * (w - 1) / 0x7FFF).clamp(0, w - 1);
    let ny = ((y_abs as i32) * (h - 1) / 0x7FFF).clamp(0, h - 1);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
    update_buttons(buttons);
}

/// Put the cursor at an absolute screen position — X11 WarpPointer, and anything
/// else that moves the pointer without a hardware packet. Clamped to the screen.
pub fn set_pos(x: usize, y: usize) {
    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    MOUSE_X.store((x as i32).clamp(0, (w - 1).max(0)), Ordering::Relaxed);
    MOUSE_Y.store((y as i32).clamp(0, (h - 1).max(0)), Ordering::Relaxed);
}

pub fn pos() -> (usize, usize) {
    (
        MOUSE_X.load(Ordering::Relaxed) as usize,
        MOUSE_Y.load(Ordering::Relaxed) as usize,
    )
}

/// True if the left button is pressed.
pub fn left_down() -> bool {
    BUTTONS.load(Ordering::Relaxed) & 0x01 != 0
}

/// The raw button bitmap (bit0 left, bit1 right, bit2 middle) — for apps that
/// read the pointer via the app-graphics bridge (e.g. the browser).
pub fn buttons() -> u8 {
    BUTTONS.load(Ordering::Relaxed)
}

/// Consume a pending right-button press → the screen position where it happened
/// (drives the context menu).
pub fn take_right_press() -> Option<(usize, usize)> {
    if RPRESS_PENDING.swap(false, Ordering::Relaxed) {
        Some((
            RCLICK_X.load(Ordering::Relaxed).max(0) as usize,
            RCLICK_Y.load(Ordering::Relaxed).max(0) as usize,
        ))
    } else {
        None
    }
}
