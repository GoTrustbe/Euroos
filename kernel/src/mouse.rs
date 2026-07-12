//! PS/2 mouse (i8042 auxiliary device), IRQ12 — drives the desktop cursor.
//!
//! The controller delivers 3-byte packets: [flags, dx, dy]. The IRQ12 handler
//! (interrupts.rs) pushes each byte here; we decode the packet and update
//! the cursor position + button state (atomically, so the desktop loop reads them).

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

use spin::Mutex;
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

/// Store the new button bitmap and latch a left-press edge (with cursor pos).
/// Callers must update MOUSE_X/MOUSE_Y *before* calling this so the latched
/// position is correct.
fn update_buttons(new: u8) {
    let old = BUTTONS.swap(new & 0x07, Ordering::Relaxed);
    if old & 0x01 == 0 && new & 0x01 != 0 {
        CLICK_X.store(MOUSE_X.load(Ordering::Relaxed), Ordering::Relaxed);
        CLICK_Y.store(MOUSE_Y.load(Ordering::Relaxed), Ordering::Relaxed);
        PRESS_PENDING.store(true, Ordering::Relaxed);
    }
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
    // try_lock, NOT lock (BUG-007 class): called from the PS/2 mouse IRQ and from the
    // USB-HID harvest in task context; a blocking acquire risks the same IRQ-vs-task
    // deadlock as push_scancode. Drop the byte on contention (mouse resync handles it).
    let mut p = match PACKET.try_lock() {
        Some(p) => p,
        None => return,
    };
    let phase = p.0;
    // Resynchronize: byte 0 must have bit 3 set.
    if phase == 0 && byte & 0x08 == 0 {
        return;
    }
    p.1[phase as usize] = byte;
    if phase < 2 {
        p.0 = phase + 1;
        return;
    }
    p.0 = 0;
    let flags = p.1[0];
    let dx = p.1[1] as i8 as i32;
    let dy = p.1[2] as i8 as i32;
    drop(p);

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
