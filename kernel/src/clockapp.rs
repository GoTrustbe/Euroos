//! **EuroClock** (AC-2): world time, timer, stopwatch, alarm. Core: [`euroclock`].
//! Contains the boot self-test and the desktop GUI (`render`) which pulls the REAL
//! wall-clock time (RTC) through the `euroclock` engine for local time + EU world clocks.

use crate::graphics::{Color, FrameBuffer};
use crate::serial_println;
use crate::{rtc, text};
use euroclock::{format_time, time_of_day, Alarm, Stopwatch, Timer, WorldClock};

/// Equal to `compositor::TITLEBAR_H` (window title-bar height).
const TITLEBAR_H: usize = 44;

/// Desktop GUI: local time (large) + date, and a grid of EU world clocks —
/// all derived from the REAL RTC epoch via the `euroclock` engine. No mock.
pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    let accent = Color::rgb(0x6A, 0x4B, 0xD0); // violet (clock-app accent)
    fb.fill_rect(bx, by, bw, bh, Color::SURFACE);

    // ── Hero panel: local wall-clock time (RTC) ─────────────────────────────
    let hero_h = (bh * 38 / 100).clamp(120, 220);
    fb.fill_rect(bx, by, bw, hero_h, accent);
    let dt = rtc::now();
    let big = alloc::format!("{:02}:{:02}", dt.hour, dt.min);
    let secs = alloc::format!(":{:02}", dt.sec);
    let bigsz = 76.0f32;
    let bw_px = text::width_px(&big, bigsz);
    let sw_px = text::width_px(&secs, 30.0);
    let total = bw_px + sw_px + 6;
    let tx = bx + (bw.saturating_sub(total)) / 2;
    let ty = by + hero_h / 2 - 42;
    text::draw_px(fb, tx, ty, &big, Color::WHITE, bigsz);
    text::draw_px(fb, tx + bw_px + 6, ty + 40, &secs, Color::rgb(0xCD, 0xC2, 0xF0), 30.0);
    // Date below, centered.
    let date = rtc::date_string();
    let dw = text::width_px(&date, 16.0);
    text::draw_px(fb, bx + (bw - dw) / 2, ty + 60, &date, Color::rgb(0xE7, 0xE2, 0xFA), 16.0);

    // ── World-clocks grid (EU standard zones, live) ─────────────────────────
    let head_y = by + hero_h + 18;
    text::draw_px(fb, bx + 24, head_y, "World clocks", Color::INK, 15.0);
    let epoch = rtc::epoch();
    let wc = WorldClock::eu_default();
    let cols = 2usize;
    let pad = 24usize;
    let gap = 14usize;
    let card_w = (bw.saturating_sub(pad * 2 + gap * (cols - 1))) / cols;
    let card_h = 58usize;
    let grid_y = head_y + 22;
    for (i, z) in wc.zones.iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        let cxp = bx + pad + c * (card_w + gap);
        let cyp = grid_y + r * (card_h + gap);
        if cyp + card_h > by + bh - 30 {
            break;
        }
        fb.fill_rounded_rect(cxp, cyp, card_w, card_h, 12, Color::CARD);
        fb.draw_border(cxp, cyp, card_w, card_h, 1, Color::BORDER);
        // Accent dot + zone label.
        fb.fill_rounded_rect(cxp + 14, cyp + card_h / 2 - 4, 8, 8, 4, accent);
        text::draw_px(fb, cxp + 30, cyp + 12, &z.label, Color::INK, 13.5);
        let off_h = z.offset_min / 60;
        let utc = alloc::format!("UTC{}{}", if off_h >= 0 { "+" } else { "-" }, off_h.abs());
        text::draw_px(fb, cxp + 30, cyp + 32, &utc, Color::TEXT_DIM, 11.0);
        // Time, right-aligned, large.
        let t = z.formatted(epoch, true);
        let tw = text::width_px(&t, 22.0);
        text::draw_px(fb, cxp + card_w.saturating_sub(tw + 16), cyp + 16, &t, accent, 22.0);
    }

    // Status bar.
    let sy = by + bh - 26;
    fb.fill_rect(bx, sy, bw, 26, accent);
    text::draw_px(fb, bx + 14, sy + 6, "EuroClock  ·  live RTC", Color::WHITE, 11.5);
    let right = alloc::format!("{}  ·  {} zones", rtc::clock_string(), wc.zones.len());
    let rw = text::width_px(&right, 11.5);
    text::draw_px(fb, bx + bw - rw - 14, sy + 6, &right, Color::WHITE, 11.5);
}

pub fn selftest() {
    // World time: 10:00 UTC → Brussels 11:00, New York 05:00.
    let wc = WorldClock::eu_default();
    let bru = wc.zones[0].formatted(36_000, true) == "11:00";
    let nyc = wc.zones[3].formatted(36_000, true) == "05:00";
    let ampm = format_time(15, 42, false) == "3:42 PM";
    let (h, _, _) = time_of_day(43_200, 60);
    let tz = h == 13;

    // Timer: 60s, 10s elapsed → 50s remaining.
    let mut t = Timer::new(60);
    t.start(100);
    let timer_ok = t.remaining(110) == 50 && t.is_done(160);

    // Stopwatch + lap.
    let mut sw = Stopwatch::new();
    sw.start(0);
    sw.lap(10);
    let sw_ok = sw.elapsed(25) == 25 && sw.laps == alloc::vec![10];

    // Alarm fires when 07:00 is passed.
    let a = Alarm::new(7, 0, "Wake up");
    let alarm_ok = a.fires_between(25_190, 25_210, 0) && !a.fires_between(25_210, 25_260, 0);

    let ok = bru && nyc && ampm && tz && timer_ok && sw_ok && alarm_ok;
    serial_println!(
        "[ck] EuroClock: Brussels=11:00({}) NYC=05:00({}) 12h={} TZ+1={} | timer={} stopwatch={} alarm={} {}",
        bru, nyc, ampm, tz, timer_ok, sw_ok, alarm_ok,
        if ok { "✓" } else { "✗ FAIL" }
    );
}
