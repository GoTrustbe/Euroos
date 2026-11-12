//! **EuroOS GUI lockscreen** (Sprint AG) — an interactive login screen that
//! authenticates the desktop session via EuroID (Argon2id) before the desktop
//! becomes interactive. It closes the identity story graphically: no longer an
//! auto-session as `euro`, but a real password check against the sovereign
//! identity store, with must-change handling.
//!
//! The screen reuses the compositor primitives (EDS theme, ab_glyph text) and
//! the PS/2 keyboard pad (`ps2::poll_key`). For an UNATTENDED/CI boot without
//! keyboard input it automatically logs in after a short grace period as the
//! desktop user — honestly logged — so the boot/screenshot/e2e keeps working.

use alloc::string::{String, ToString};

use crate::graphics::{Color, FrameBuffer};
use crate::{eds, text};

/// After this many 100 Hz ticks (≈ guest seconds) without a successful login, an
/// unattended boot automatically logs in as the desktop user (demo/CI convenience).
const UNATTENDED_AUTOLOGIN_TICKS: u64 = 60; // ~0.6 s guest time (small boot effect)

#[derive(PartialEq)]
enum Phase {
    /// Enter password for `user`.
    Password,
    /// Account requires a change: enter a NEW password.
    MustChange,
}

struct Lock {
    user: String,
    input: String,         // current input (shown masked)
    saved_old: String,     // saved old password during must-change
    phase: Phase,
    status: String,        // hint or error line below the field
    error: bool,           // show status in red?
}

/// The colour of background pixel (`col`, `row`): the 150° gradient of
/// `FrameBuffer::fill_rounded_rect_grad`, the same expression operation for
/// operation, so both give the same bits.
#[inline(always)]
fn background_at(col: usize, row: usize, maxp: f32, c0: Color, c1: Color) -> Color {
    let t = ((col as f32) * 0.5f32 + (row as f32) * 0.866f32) / maxp;
    c0.lerp(c1, t)
}

/// Paint the full-screen background gradient (BUG-056).
///
/// This used to be `fb.fill_rounded_rect_grad(0, 0, w, h, 0, c0, c1)`: a
/// function written for icon tiles that computes the gradient in `f32` and
/// stores one pixel at a time. The kernel has no hardware float, so that was
/// about 30 soft-float calls per pixel, two million pixels per frame: 25 to
/// 45 s per frame under TCG.
///
/// The two colours are a few steps apart per channel, so a row is a handful of
/// runs of one colour. Each channel is monotone along a row (every float
/// operation in `background_at` is monotone in `col`), so the end of a run is
/// found by bisection with the same expression, and the run is filled in one
/// go. The picture is the same pixel for pixel; a row costs about a hundred
/// evaluations instead of `w`, and the fills are plain row fills.
fn paint_background(fb: &FrameBuffer, c0: Color, c1: Color) {
    let (w, h) = (fb.width(), fb.height());
    if w == 0 || h == 0 {
        return;
    }
    let maxp = (w as f32) * 0.5f32 + (h as f32) * 0.866f32;
    for row in 0..h {
        let last = background_at(w - 1, row, maxp, c0, c1);
        let mut x = 0usize;
        let mut cx = background_at(0, row, maxp, c0, c1);
        loop {
            if cx == last {
                fb.fill_rect(x, row, w - x, 1, cx);
                break;
            }
            // Invariant: column `lo` has colour `cx`, column `hi` has another one.
            let (mut lo, mut hi) = (x, w - 1);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if background_at(mid, row, maxp, c0, c1) == cx {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            fb.fill_rect(x, row, lo + 1 - x, 1, cx);
            x = hi;
            cx = background_at(hi, row, maxp, c0, c1);
        }
    }
}

/// Self-test half of BUG-056: a few rows of the painted frame above and below
/// the card, every pixel against the per-pixel expression.
fn background_rows_exact(fb: &FrameBuffer, c0: Color, c1: Color) -> (usize, usize) {
    let (w, h) = (fb.width(), fb.height());
    if w == 0 || h < 400 {
        return (0, 0);
    }
    let maxp = (w as f32) * 0.5f32 + (h as f32) * 0.866f32;
    let card_top = (h - 280) / 2;
    let rows = [0, 1, card_top / 3, card_top / 2 + 1, card_top - 1, h - card_top / 2, h - 2, h - 1];
    let (mut checked, mut wrong) = (0usize, 0usize);
    for &row in rows.iter() {
        for col in 0..w {
            checked += 1;
            if fb.get_pixel(col, row) != background_at(col, row, maxp, c0, c1) {
                wrong += 1;
            }
        }
    }
    (checked, wrong)
}

/// Draw the lockscreen frame: sand background + centered card with wordmark,
/// username, masked password field (with cursor), an EuroID/Argon2id badge
/// and the status/hint line.
fn render(fb: &FrameBuffer, lk: &Lock, clock: &str) {
    let (w, h) = (fb.width(), fb.height());
    // Background in the house style (sand → slightly darker at the bottom).
    paint_background(fb, Color::BACKGROUND, Color::PAPER_2);

    // Card, centered.
    let cw = 380usize;
    let ch = 280usize;
    let cx = (w - cw) / 2;
    let cy = (h - ch) / 2;
    fb.fill_rounded_rect(cx + 6, cy + 8, cw, ch, eds::RADIUS_XL, Color::rgb(0xE2, 0xDC, 0xD1)); // soft shadow
    fb.fill_rounded_rect(cx, cy, cw, ch, eds::RADIUS_XL, Color::SURFACE);

    let pad = eds::eu(7); // 28
    let mut y = cy + pad;

    // Wordmark + accent dot.
    text::draw_px(fb, cx + pad, y, "EuroOS", Color::INK, 30.0);
    let wm = text::width_px("EuroOS", 30.0);
    fb.fill_rounded_rect(cx + pad + wm + 8, y + 8, 10, 10, 5, Color::ACCENT);
    y += 44;
    text::draw_px(fb, cx + pad, y, "Sign in to your sovereign session", Color::TEXT_SEC, 14.0);
    y += 34;

    // Username line.
    text::draw_px(fb, cx + pad, y, "User", Color::TEXT_DIM, 12.0);
    text::draw_px(fb, cx + pad + 90, y, &lk.user, Color::INK, 14.0);
    y += 28;

    // Password field.
    let label = if lk.phase == Phase::Password { "Password" } else { "New password" };
    text::draw_px(fb, cx + pad, y, label, Color::TEXT_DIM, 12.0);
    y += 20;
    let fw = cw - 2 * pad;
    fb.fill_rounded_rect(cx + pad, y, fw, 36, eds::RADIUS_M, Color::SURFACE_3);
    // Accent border (focus).
    fb.fill_rect(cx + pad, y + 34, fw, 2, Color::ACCENT);
    // Masked input (• per character) + non-blinking cursor.
    let mut masked = String::new();
    for _ in lk.input.chars() {
        masked.push('•');
    }
    masked.push('|');
    text::draw_px(fb, cx + pad + 12, y + 9, &masked, Color::INK, 16.0);
    y += 50;

    // Status / hint.
    let (msg, col) = if !lk.status.is_empty() {
        (lk.status.as_str(), if lk.error { Color::RED } else { Color::TEXT_SEC })
    } else {
        ("Press Enter to sign in", Color::TEXT_DIM)
    };
    text::draw_px(fb, cx + pad, y, msg, col, 13.0);

    // EuroID badge at the bottom + clock.
    let by = cy + ch - 30;
    fb.fill_rounded_rect(cx + pad, by, 200, 20, eds::RADIUS_S, Color::SUCCESS_SOFT);
    text::draw_px(fb, cx + pad + 8, by + 2, "EuroID · Argon2id", Color::SUCCESS, 12.0);
    let cwid = text::width_px(clock, 13.0);
    text::draw_px(fb, cx + cw - pad - cwid, by + 2, clock, Color::TEXT_DIM, 13.0);

    fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() {
        crate::virtio_gpu::present_frame(bb, bw, bh, bs);
    }
}

/// Show the lockscreen and block until the user signs in (or, on an
/// unattended boot, until the auto-login grace expires). Sets the session via
/// EuroAuth and returns the signed-in username.
pub fn gate(fb: &FrameBuffer, default_user: &str) -> String {
    // Release image on a live medium (the demo store): the session is signed in
    // without anyone typing, so drawing the lockscreen and waiting out the grace
    // period only delays the desktop (0.6 s under KVM, several seconds under TCG
    // for the two full-screen renders). An installed system never comes here: it
    // waits for its password. The self-test image keeps the grace period, which
    // its harnesses rely on.
    if !cfg!(feature = "selftest") && crate::euroid::unattended_login_allowed() {
        return unattended_login(default_user);
    }
    let mut lk = Lock {
        user: default_user.to_string(),
        input: String::new(),
        saved_old: String::new(),
        phase: Phase::Password,
        status: String::new(),
        error: false,
    };
    let start = crate::interrupts::ticks();
    render(fb, &lk, &crate::rtc::clock_string());

    loop {
        crate::xhci::poll(); // also service the USB keyboard
        crate::virtio_input::poll(); // virtio-tablet / virtio-keyboard (polled)
        let mut dirty = false;
        while let Some(c) = crate::ps2::poll_key() {
            dirty = true;
            match c {
                '\r' => {
                    if lk.phase == Phase::Password {
                        match crate::euroid::login(&lk.user, &lk.input) {
                            Ok(ok) => {
                                set_session(&ok.name, ok.uid);
                                crate::serial_println!("[lock] login successful: {} (uid={})", ok.name, ok.uid);
                                return ok.name;
                            }
                            Err(reason) => {
                                if reason.contains("must be changed") {
                                    // must-change: ask for a new password.
                                    lk.saved_old = core::mem::take(&mut lk.input);
                                    lk.phase = Phase::MustChange;
                                    lk.status = "Password expired — choose a new one".to_string();
                                    lk.error = false;
                                } else {
                                    crate::serial_println!("[lock] login rejected for {}: {reason}", lk.user);
                                    lk.status = reason;
                                    lk.error = true;
                                    lk.input.clear();
                                }
                            }
                        }
                    } else {
                        // MustChange: set the new password and then sign in.
                        let newpw = core::mem::take(&mut lk.input);
                        match crate::euroid::change_own_password(&lk.user, &lk.saved_old, &newpw) {
                            Ok(()) => match crate::euroid::login(&lk.user, &newpw) {
                                Ok(ok) => {
                                    set_session(&ok.name, ok.uid);
                                    crate::serial_println!("[lock] password changed + signed in: {}", ok.name);
                                    return ok.name;
                                }
                                Err(e) => {
                                    lk.status = e;
                                    lk.error = true;
                                    lk.phase = Phase::Password;
                                }
                            },
                            Err(e) => {
                                lk.status = e;
                                lk.error = true;
                            }
                        }
                    }
                }
                '\u{8}' | '\u{7f}' => {
                    lk.input.pop();
                }
                '\u{1b}' => {
                    lk.input.clear();
                }
                ch if !ch.is_control() => {
                    if lk.input.len() < 128 {
                        lk.input.push(ch);
                    }
                }
                _ => {}
            }
        }

        // Unattended boot: after the grace period, auto-login as the desktop user.
        // Only on a live medium (demo store) or in self-test builds: an INSTALLED
        // system in a release build always waits for the real password.
        if crate::euroid::unattended_login_allowed()
            && crate::interrupts::ticks().saturating_sub(start) > UNATTENDED_AUTOLOGIN_TICKS
        {
            return unattended_login(default_user);
        }

        if dirty {
            render(fb, &lk, &crate::rtc::clock_string());
        }
        x86_64::instructions::hlt();
    }
}

/// The unattended sign-in: a real login with the demo password (live) or the
/// dev install password (self-test standalone boot); if that fails, the
/// existing session default (uid 1000).
fn unattended_login(default_user: &str) -> String {
    let attempt = crate::euroid::login(default_user, "euro")
        .or_else(|_| crate::euroid::login(default_user, crate::instexec::DEV_INSTALL_PASSWORD));
    if let Ok(ok) = attempt {
        set_session(&ok.name, ok.uid);
        crate::serial_println!(
            "[lock] unattended boot — no input within grace → auto-login as {} (demo/CI)",
            ok.name
        );
        return ok.name;
    }
    crate::serial_println!("[lock] unattended boot — auto-login fell back to default session");
    default_user.to_string()
}

fn set_session(name: &str, uid: u32) {
    // gid from /etc/passwd mapping if available; otherwise the uid.
    crate::auth::set_session(uid, uid, name);
}

/// `[ag-lock]` boot self-test — proves the auth wiring of the lockscreen without
/// an interactive keyboard: the correct password signs in, a wrong one is
/// rejected, and rendering the screen does not panic (drawn headless).
pub fn selftest(fb: &FrameBuffer) {
    if crate::euroid::store_from_disk() {
        crate::serial_println!("[ag-lock] GUI lockscreen: installed system (identity store from disk) — demo-credential check skipped");
        return;
    }
    let good = crate::euroid::login("euro", "euro").is_ok();
    let bad = crate::euroid::login("euro", "fout").is_err();
    // Render one frame headless (proves the draw routine does not panic).
    let lk = Lock {
        user: "euro".to_string(),
        input: "secret".to_string(),
        saved_old: String::new(),
        phase: Phase::Password,
        status: String::new(),
        error: false,
    };
    let t0 = crate::interrupts::ticks();
    render(fb, &lk, &crate::rtc::clock_string());
    let frame_ms = crate::interrupts::ticks().saturating_sub(t0) * 10;
    let rendered = true;
    // BUG-056: the background is painted in runs; it must be the per-pixel gradient.
    let (checked, wrong) = background_rows_exact(fb, Color::BACKGROUND, Color::PAPER_2);
    let exact = wrong == 0;

    let ok = good && bad && rendered && exact;
    crate::serial_println!(
        "[ag-lock] GUI lockscreen: correct-password-signs-in={good}, wrong-rejected={bad}, screen-drawn={rendered}, background {wrong} of {checked} sampled pixels off the per-pixel gradient, frame {frame_ms} ms → {}",
        if ok { "OK (desktop session behind EuroID-Argon2id instead of auto-euro) ✓" } else { "FAILED" }
    );
}
