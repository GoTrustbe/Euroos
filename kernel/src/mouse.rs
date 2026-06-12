//! PS/2-muis (i8042 auxiliary device), IRQ12 — bestuurt de desktop-cursor.
//!
//! De controller levert 3-byte pakketten: [flags, dx, dy]. De IRQ12-handler
//! (interrupts.rs) duwt elke byte hierheen; we decoderen het pakket en werken
//! de cursorpositie + knopstatus bij (atomair, zodat de desktop-loop ze leest).

use core::sync::atomic::{AtomicI32, AtomicU8, AtomicUsize, Ordering};

use spin::Mutex;
use x86_64::instructions::port::Port;

static MOUSE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static BUTTONS: AtomicU8 = AtomicU8::new(0);
static SCREEN_W: AtomicUsize = AtomicUsize::new(0);
static SCREEN_H: AtomicUsize = AtomicUsize::new(0);

// Pakket-opbouw: (fase 0..3, bytes).
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

/// Initialiseer de PS/2-muis. Moet vóór het demaskeren van IRQ12 gebeuren.
pub fn init(width: usize, height: usize) {
    SCREEN_W.store(width, Ordering::Relaxed);
    SCREEN_H.store(height, Ordering::Relaxed);
    MOUSE_X.store((width / 2) as i32, Ordering::Relaxed);
    MOUSE_Y.store((height / 2) as i32, Ordering::Relaxed);

    let mut data = Port::<u8>::new(0x60);
    let mut cmd = Port::<u8>::new(0x64);
    let mut status = Port::<u8>::new(0x64);
    unsafe {
        // Schakel het auxiliary device (muis) in.
        wait_input_empty(&mut status);
        cmd.write(0xA8);
        // Lees config-byte. Zet BEIDE interrupts aan (bit0=toetsenbord IRQ1,
        // bit1=muis IRQ12) en BEIDE clocks aan (clear bit4=kbd, bit5=muis).
        wait_input_empty(&mut status);
        cmd.write(0x20);
        wait_output_full(&mut status);
        let mut cfg = data.read();
        cfg |= 0b11; // bit0 kbd-IRQ + bit1 muis-IRQ
        cfg &= !0b11_0000; // clear bit4 (kbd-clock) + bit5 (muis-clock) → beide aan
        wait_input_empty(&mut status);
        cmd.write(0x60);
        wait_input_empty(&mut status);
        data.write(cfg);
        // Toetsenbord: enable scanning (0xF4 direct naar de kbd) zodat toetsen
        // ECHT scancodes + IRQ1 genereren (anders kwam alleen de self-test-byte binnen).
        wait_input_empty(&mut status);
        data.write(0xF4);
        wait_output_full(&mut status);
        let _ack_kbd = data.read(); // 0xFA
        // Muis: defaults + data reporting aan (elk met 0xD4-prefix naar aux).
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

/// Door de IRQ12-handler aangeroepen met elke ontvangen byte.
pub fn push_byte(byte: u8) {
    let mut p = PACKET.lock();
    let phase = p.0;
    // Hersynchroniseer: byte 0 moet bit 3 gezet hebben.
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

    BUTTONS.store(flags & 0x07, Ordering::Relaxed);
    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    // Y is geïnverteerd: muis omhoog = positieve dy = cursor omhoog.
    let nx = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w - 1);
    let ny = (MOUSE_Y.load(Ordering::Relaxed) - dy).clamp(0, h - 1);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
}

/// Pas een relatieve USB-HID-muisbeweging + knopstatus toe (zelfde cursor-atomics
/// als de PS/2-muis, zodat de desktop transparant op USB-invoer werkt). `dy` is in
/// HID-conventie (omlaag positief), dus we tellen 'm direct op bij Y.
pub fn apply_usb(dx: i32, dy: i32, buttons: u8) {
    BUTTONS.store(buttons & 0x07, Ordering::Relaxed);
    let w = SCREEN_W.load(Ordering::Relaxed) as i32;
    let h = SCREEN_H.load(Ordering::Relaxed) as i32;
    if w == 0 || h == 0 {
        return;
    }
    let nx = (MOUSE_X.load(Ordering::Relaxed) + dx).clamp(0, w - 1);
    let ny = (MOUSE_Y.load(Ordering::Relaxed) + dy).clamp(0, h - 1);
    MOUSE_X.store(nx, Ordering::Relaxed);
    MOUSE_Y.store(ny, Ordering::Relaxed);
}

pub fn pos() -> (usize, usize) {
    (
        MOUSE_X.load(Ordering::Relaxed) as usize,
        MOUSE_Y.load(Ordering::Relaxed) as usize,
    )
}

/// True als de linkerknop ingedrukt is.
pub fn left_down() -> bool {
    BUTTONS.load(Ordering::Relaxed) & 0x01 != 0
}
