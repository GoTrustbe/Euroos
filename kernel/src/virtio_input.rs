//! virtio-input driver (virtio 1.x device type 18, PCI 1af4:1052): the
//! `virtio-tablet-pci` absolute pointer and `virtio-keyboard-pci` (and a
//! relative `virtio-mouse-pci`), as Kyvernon VMM and QEMU offer them.
//!
//! A tablet reports ABS_X/ABS_Y (0..32767 on Kyvernon and QEMU; the range is
//! read from the device's ABS_INFO) — the cursor is set to the host pointer's
//! position, so it cannot drift the way the relative PS/2 mouse does under a
//! VNC/remote console. Keys arrive as Linux evdev codes and are fed to the
//! PS/2 decoder as set-1 scancodes, so every consumer of the keyboard (shell,
//! desktop, X layer, lockscreen) sees them unchanged.
//!
//! Modern transport only (the device type has no legacy id). One event queue
//! (queue 0) with 64 eight-byte buffers; the status queue is not used. No
//! interrupt: the queue is drained from the 100 Hz timer tick and the desktop
//! loop (next to `xhci::poll`), with VRING_AVAIL_F_NO_INTERRUPT set.
//! Decoding (`eurovirtio::input`) is host-tested.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use euromm::FrameAllocator;
use eurovirtio::input::{self as vi, Action, AbsRange, Decoder, Event, Kind};

use crate::virtio_pci::{Queue, Transport, DESC_WRITE};

const MAX_DEV: usize = 4;
const NBUF: u16 = 64;
const EV_SIZE: u64 = 8;

struct InputDev {
    tp: Transport,
    q: Queue,
    bufs: u64, // NBUF * 8 bytes, buffer i at bufs + i*8
    kind: Kind,
    dec: Decoder,
}

static DEVS: spin::Mutex<[Option<InputDev>; MAX_DEV]> = spin::Mutex::new([None, None, None, None]);
static ANY: AtomicBool = AtomicBool::new(false);
/// Events consumed / pointer reports applied / keys pushed (proof counters).
pub static EVENTS: AtomicU64 = AtomicU64::new(0);
pub static POINTER_REPORTS: AtomicU64 = AtomicU64::new(0);
pub static KEYS: AtomicU64 = AtomicU64::new(0);
static DIAG_LEFT: AtomicU32 = AtomicU32::new(12);

/// Read `select/subsel` config data (up to `out.len()` bytes); returns the size.
fn cfg_query(tp: &Transport, select: u8, subsel: u8, out: &mut [u8]) -> usize {
    tp.cfg_write8(vi::CFG_SELECT as u16, select);
    tp.cfg_write8(vi::CFG_SUBSEL as u16, subsel);
    let size = (tp.cfg8(vi::CFG_SIZE as u16) as usize).min(128);
    let n = size.min(out.len());
    for (i, b) in out.iter_mut().enumerate().take(n) {
        *b = tp.cfg8(vi::CFG_DATA as u16 + i as u16);
    }
    size
}

fn abs_range(tp: &Transport, axis: u16) -> AbsRange {
    let mut b = [0u8; 20];
    if cfg_query(tp, vi::CFG_ABS_INFO, axis as u8, &mut b) >= 8 {
        let min = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let max = i32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        if max > min {
            return AbsRange { min, max };
        }
    }
    AbsRange::TABLET
}

/// Find and start every virtio-input device. Returns the number started.
pub fn init(falloc: &mut FrameAllocator) -> usize {
    let mut started = 0usize;
    let mut slots = DEVS.lock();
    for dev in crate::virtio_pci::find_all(18) {
        if started >= MAX_DEV {
            break;
        }
        let Some(mut tp) = Transport::probe(&dev) else { continue };
        if !tp.is_modern() {
            continue;
        }
        tp.begin();
        if tp.negotiate(0).is_none() {
            crate::serial_println!("[vinput] {:02x}:{:02x}.{}: feature negotiation refused", dev.bus, dev.dev, dev.func);
            continue;
        }
        let mut name = [0u8; 40];
        let nlen = cfg_query(&tp, vi::CFG_ID_NAME, 0, &mut name).min(name.len());
        let mut abs = [0u8; 8];
        let mut rel = [0u8; 2];
        let mut key = [0u8; 16];
        cfg_query(&tp, vi::CFG_EV_BITS, vi::EV_ABS as u8, &mut abs);
        cfg_query(&tp, vi::CFG_EV_BITS, vi::EV_REL as u8, &mut rel);
        cfg_query(&tp, vi::CFG_EV_BITS, vi::EV_KEY as u8, &mut key);
        let kind = vi::classify(&abs, &rel, &key);
        let (xr, yr) = if kind == Kind::Tablet {
            (abs_range(&tp, vi::ABS_X), abs_range(&tp, vi::ABS_Y))
        } else {
            (AbsRange::TABLET, AbsRange::TABLET)
        };

        let Some(q) = tp.setup_queue(0, NBUF, &mut |pages| falloc.allocate_contiguous(pages).ok()) else { continue };
        let Ok(bufs) = falloc.allocate() else { continue };
        let n = q.size.min(NBUF);
        unsafe {
            core::ptr::write_bytes(bufs as *mut u8, 0, 4096);
            // Polled queue: ask the device not to interrupt (avail.flags bit 0).
            (q.avail as *mut u16).write_volatile(1);
            for i in 0..n {
                let d = q.desc(i);
                (*d).addr = bufs + i as u64 * EV_SIZE;
                (*d).len = EV_SIZE as u32;
                (*d).flags = DESC_WRITE;
                (*d).next = 0;
                q.avail_ring(i).write_volatile(i);
            }
            core::sync::atomic::fence(Ordering::SeqCst);
            q.avail_idx_ptr().write_volatile(n);
        }
        tp.driver_ok();
        tp.notify(&q);
        crate::pci::claim(dev.bus, dev.dev, dev.func, "virtio-input");
        let nm = core::str::from_utf8(&name[..nlen]).unwrap_or("?").trim_end_matches('\0');
        crate::serial_println!(
            "[vinput] virtio-input {:02x}:{:02x}.{} \"{}\": {} ({}), {} event buffers, polled ✓",
            dev.bus, dev.dev, dev.func, nm,
            match kind { Kind::Tablet => "tablet, absolute pointer", Kind::Mouse => "mouse, relative", Kind::Keyboard => "keyboard", Kind::Other => "other (events ignored)" },
            if kind == Kind::Tablet {
                alloc::format!("ABS_X {}..{}, ABS_Y {}..{}", xr.min, xr.max, yr.min, yr.max)
            } else {
                tp.describe()
            },
            n
        );
        slots[started] = Some(InputDev { tp, q, bufs, kind, dec: Decoder::new(xr, yr) });
        started += 1;
    }
    if started > 0 {
        ANY.store(true, Ordering::Relaxed);
    }
    started
}

pub fn present() -> bool {
    ANY.load(Ordering::Relaxed)
}

fn apply(kind: Kind, a: Action) {
    match a {
        Action::Key { extended, code, pressed } => {
            if extended {
                crate::ps2::push_scancode(0xE0);
            }
            crate::ps2::push_scancode(if pressed { code } else { code | 0x80 });
            KEYS.fetch_add(1, Ordering::Relaxed);
        }
        Action::Pointer { abs, dx, dy, wheel: _, buttons } => {
            if kind == Kind::Other {
                return;
            }
            match abs {
                Some((x, y)) => crate::mouse::apply_usb_abs(x, y, buttons),
                None => crate::mouse::apply_usb(dx, dy, buttons),
            }
            POINTER_REPORTS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if DIAG_LEFT.load(Ordering::Relaxed) > 0 {
        DIAG_LEFT.fetch_sub(1, Ordering::Relaxed);
        let (x, y) = crate::mouse::pos();
        crate::serial_println!("[vinput] {a:?} → cursor {x},{y}");
    }
}

/// Drain every event queue. Safe from the timer tick and from task context:
/// a second caller while one is busy simply returns.
pub fn poll() {
    if !present() {
        return;
    }
    let Some(mut devs) = DEVS.try_lock() else { return };
    for slot in devs.iter_mut() {
        let Some(d) = slot.as_mut() else { continue };
        let mut any = false;
        let mut budget = 256;
        while d.q.used_idx() != d.q.last_used && budget > 0 {
            budget -= 1;
            let (id, len) = d.q.used_elem(d.q.last_used % d.q.size);
            d.q.last_used = d.q.last_used.wrapping_add(1);
            let id = id as u16;
            if id >= d.q.size.min(NBUF) {
                continue; // not a descriptor we posted
            }
            if len as u64 >= EV_SIZE {
                let mut b = [0u8; 8];
                unsafe {
                    core::ptr::copy_nonoverlapping((d.bufs + id as u64 * EV_SIZE) as *const u8, b.as_mut_ptr(), 8);
                }
                if let Some(ev) = Event::parse(&b) {
                    EVENTS.fetch_add(1, Ordering::Relaxed);
                    if let Some(a) = d.dec.feed(ev) {
                        apply(d.kind, a);
                    }
                }
            }
            d.q.push_avail(id);
            any = true;
        }
        if any {
            d.tp.notify(&d.q);
        }
    }
}

/// `vinput` shell line / boot summary.
pub fn summary() -> alloc::string::String {
    alloc::format!(
        "virtio-input: {} event(s), {} pointer report(s), {} key event(s)",
        EVENTS.load(Ordering::Relaxed),
        POINTER_REPORTS.load(Ordering::Relaxed),
        KEYS.load(Ordering::Relaxed)
    )
}
