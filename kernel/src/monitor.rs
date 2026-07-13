//! **EuroMonitor** (Sprint 4) — a live system-monitor app. Shows REAL
//! kernel data: wall-clock time (RTC), RAM usage (frame allocator), active tasks
//! (scheduler), disk (virtio-blk capacity) and the number of security events
//! (audit log). No mock — every line is a direct measurement at draw time.

use crate::graphics::{Color, FrameBuffer};
use crate::{rtc, text};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const TITLEBAR_H: usize = 44;

// The frame-allocator stats need `&mut FrameAllocator`; the desktop loop
// snapshots them here (cheaply) so the render function can show them context-free.
static MEM_USABLE_MIB: AtomicU64 = AtomicU64::new(0);
static MEM_FREE_MIB: AtomicU64 = AtomicU64::new(0);
static MEM_FRAMES: AtomicUsize = AtomicUsize::new(0);

/// Called by the desktop loop with fresh frame-allocator stats.
pub fn set_mem(usable_mib: u64, free_mib: u64, free_frames: usize) {
    MEM_USABLE_MIB.store(usable_mib, Ordering::Relaxed);
    MEM_FREE_MIB.store(free_mib, Ordering::Relaxed);
    MEM_FRAMES.store(free_frames, Ordering::Relaxed);
}

fn bar(fb: &FrameBuffer, x: usize, y: usize, w: usize, frac: f32, c: Color) {
    fb.fill_rounded_rect(x, y, w, 12, 6, Color::rgb(0xE2, 0xE6, 0xEC));
    let fill = ((w as f32) * frac.clamp(0.0, 1.0)) as usize;
    if fill > 0 {
        fb.fill_rounded_rect(x, y, fill.max(12), 12, 6, c);
    }
}

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let _bh = h.saturating_sub(TITLEBAR_H);
    let accent = Color::rgb(0x1F, 0x9D, 0x6B);
    let ink = Color::rgb(0x20, 0x24, 0x2C);
    let dim = Color::rgb(0x60, 0x68, 0x74);

    fb.fill_rect(bx, by, bw, _bh, Color::rgb(0xFA, 0xFB, 0xFD));

    let t = rtc::now();
    let usable = MEM_USABLE_MIB.load(Ordering::Relaxed).max(1);
    let free = MEM_FREE_MIB.load(Ordering::Relaxed);
    let used = usable.saturating_sub(free);
    let frames = MEM_FRAMES.load(Ordering::Relaxed);
    let tasks = crate::sched::task_count();
    let sec_events = crate::audit::count();
    let (disk, dcount) = if crate::virtio_blk::present() {
        (crate::virtio_blk::capacity_sectors() * 512 / (1024 * 1024), crate::virtio_blk::device_count())
    } else {
        (0, 0)
    };
    let net = crate::nic::mac().is_some();

    let mut ty = by + 16;
    text::draw_px(fb, bx + 18, ty, "EuroMonitor — live system status", ink, 19.0);
    ty += 34;
    text::draw_px(
        fb,
        bx + 18,
        ty,
        &alloc::format!("{:04}-{:02}-{:02}  {:02}:{:02}:{:02}  (RTC)", t.year, t.month, t.day, t.hour, t.min, t.sec),
        dim,
        14.0,
    );
    ty += 34;

    // RAM with bar.
    text::draw_px(fb, bx + 18, ty, &alloc::format!("RAM   {used} / {usable} MiB used"), ink, 15.0);
    bar(fb, bx + 18, ty + 22, bw.saturating_sub(36), used as f32 / usable as f32, accent);
    ty += 50;
    text::draw_px(fb, bx + 18, ty, &alloc::format!("      {frames} free frames (4 KiB)"), dim, 13.0);
    ty += 30;

    for (label, value) in [
        (alloc::string::String::from("Tasks"), alloc::format!("{tasks} active (scheduler)")),
        (alloc::string::String::from("Disk"), if dcount > 0 { alloc::format!("{disk} MiB · {dcount} virtio-blk device(s)") } else { "no virtio-blk (live mode)".into() }),
        (alloc::string::String::from("Network"), if net { "virtio-net active".into() } else { "no NIC".into() }),
        (alloc::string::String::from("Security"), alloc::format!("{sec_events} audit event(s) (hash chain)")),
    ] {
        text::draw_px(fb, bx + 18, ty, &alloc::format!("{label:<11} {value}"), ink, 14.5);
        ty += 28;
    }
}
