//! First-run tour: a small card that points at the dock, the launcher, the
//! Terminal, EuroBeheer and the help keys, in six steps.
//!
//! It must never be in the way:
//! - it is off in the self-test image and with the boot option `ui.tour=off`;
//! - it waits while the install wizard is open (a live medium with a blank
//!   disk opens EuroInstall on the first desktop) and starts when that window
//!   is closed;
//! - it does not hold the desktop: the loop keeps running underneath;
//! - Esc, "Skip", a click outside the card or any typed key closes it. A click
//!   outside and a typed key are NOT swallowed: they reach the desktop as if
//!   the card had not been there;
//! - it costs the boot one read of the user's desktop preferences.
//!
//! "Do not show again" (and finishing the tour) sets `tour.done=true` in the
//! user's desktop preferences (`prefs.rs`, `~/.config/euroos/desktop.conf`).
//! On the preview image the file system lives in memory, so the tour comes
//! back after a restart; on an installed system it stays away.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};
use crate::text;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not decided yet (first desktop-loop iteration decides).
    Unknown,
    Active(usize),
    Off,
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Unknown);
/// The "waiting for the install wizard" line was printed.
static WAIT_LOGGED: AtomicBool = AtomicBool::new(false);
/// A "do not show again" that still has to be written to the file system.
static REMEMBER: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    /// The column of app tiles.
    Dock,
    /// The EU mark (launcher).
    Brand,
    /// One dock tile.
    Tile(usize),
}

pub struct Step {
    pub title: &'static str,
    pub body: &'static str,
    target: Target,
}

pub const STEPS: [Step; 6] = [
    Step {
        title: "Welcome to EuroOS",
        body: "A tour of the desktop in six short steps. Next continues. Skip, Esc, a click outside this card or simply starting to type closes it.",
        target: Target::None,
    },
    Step {
        title: "The dock",
        body: "These tiles open the built-in apps: files, notes, clock, browser, terminal, settings, calculator, agent, text editor, monitor and log. Clicking the tile of the window in front hides that window.",
        target: Target::Dock,
    },
    Step {
        title: "The launcher",
        body: "The EU mark opens the launcher. Type to find apps, files and text inside files; Enter opens the first result. EuroView, EuroPaint, Chromium and the installer start from here.",
        target: Target::Brand,
    },
    Step {
        title: "Terminal",
        body: "The fifth tile is the command line. Type help for the commands by topic, or help ls for one command with an example.",
        target: Target::Tile(4),
    },
    Step {
        title: "Settings and privacy",
        body: "EuroBeheer, the sixth tile, shows what each program may access. Block a domain under EuroGuard; under Apps, cut a program off from the network or revoke its permissions.",
        target: Target::Tile(5),
    },
    Step {
        title: "Help",
        body: "F1 opens help for the window in front; the ? in a title bar does the same. F2 lists the keyboard shortcuts. On the preview image files are kept in memory only: see 'EuroOS at a glance' in help.",
        target: Target::None,
    },
];

// Dock geometry, as drawn by compositor::draw_sidebar (its constants are
// private). `selftest` checks these rectangles against the compositor's own
// hit tests, so a change there shows up as a failed `[help]` marker.
const TILE_X: usize = 24;
const TILE_Y0: usize = 78;
const TILE: usize = 42;
const TILE_PITCH: usize = 50;
const TILES: usize = 11;

fn target_rect(t: Target) -> Option<(usize, usize, usize, usize)> {
    match t {
        Target::None => None,
        Target::Dock => Some((TILE_X, TILE_Y0, TILE, (TILES - 1) * TILE_PITCH + TILE)),
        Target::Brand => Some((23, 20, 44, 44)),
        Target::Tile(i) => Some((TILE_X, TILE_Y0 + i * TILE_PITCH, TILE, TILE)),
    }
}

const CARD_W: usize = 420;
const PAD: usize = 20;

fn body_lines(step: usize) -> alloc::vec::Vec<String> {
    crate::help::wrap(STEPS[step].body, 13.0, CARD_W - PAD * 2)
}

fn card_rect(step: usize, sw: usize, sh: usize) -> (usize, usize, usize, usize) {
    let h = PAD + 26 + body_lines(step).len() * 19 + 18 + 30 + PAD - 4;
    let w = CARD_W.min(sw.saturating_sub(120));
    match target_rect(STEPS[step].target) {
        Some((_, ty, _, th)) => {
            let x = 100;
            let mid = ty + th.min(TILE) / 2;
            let y = mid.saturating_sub(48).max(14).min(sh.saturating_sub(h + 14));
            (x, y, w, h)
        }
        None => (sw.saturating_sub(w) / 2, sh.saturating_sub(h) / 2, w, h),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Next,
    Back,
    Skip,
    Never,
}

/// The card's buttons: (action, rect, label). Left: Skip, "Do not show again".
/// Right: Back (from step 2), Next / Done.
fn buttons(step: usize, card: (usize, usize, usize, usize)) -> [Option<(Act, (usize, usize, usize, usize), &'static str)>; 4] {
    let (x, y, w, h) = card;
    let by = y + h - PAD - 26;
    let last = step + 1 == STEPS.len();
    let next_w = 78;
    let next = (x + w - PAD - next_w, by, next_w, 28);
    let back = (next.0 - 8 - 62, by, 62, 28);
    let skip = (x + PAD, by, 54, 28);
    let never = (skip.0 + skip.2 + 8, by, 138, 28);
    [
        Some((Act::Next, next, if last { "Done" } else { "Next" })),
        if step > 0 { Some((Act::Back, back, "Back")) } else { None },
        if last { None } else { Some((Act::Skip, skip, "Skip")) },
        Some((Act::Never, if last { skip_wide(skip) } else { never }, "Do not show again")),
    ]
}

fn skip_wide(skip: (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
    (skip.0, skip.1, 138, skip.3)
}

pub fn is_active() -> bool {
    matches!(*PHASE.lock(), Phase::Active(_))
}

/// Close the tour for this session (no flag written).
pub fn dismiss() {
    let mut p = PHASE.lock();
    if matches!(*p, Phase::Active(_)) {
        *p = Phase::Off;
    }
}

/// Show the tour again (`help tour` in the Terminal).
pub fn restart() {
    *PHASE.lock() = Phase::Active(0);
}

/// The key in the user's desktop preferences (`~/.config/euroos/desktop.conf`,
/// `prefs.rs`) that says the tour was finished or switched off.
pub const DONE_KEY: &str = "tour.done";

/// Has the user of this session finished the tour or chosen "Do not show again"?
fn seen(fs: &mut dyn eurofs::FileSystem) -> bool {
    crate::prefs::open_user(fs, crate::prefs::DESKTOP).get_bool(DONE_KEY, false)
}

/// Remember it. Reads the file again first: the Settings panel writes the same file.
fn remember(fs: &mut dyn eurofs::FileSystem) {
    let mut p = crate::prefs::open_user(fs, crate::prefs::DESKTOP);
    p.set_bool(DONE_KEY, true);
    match p.save(fs) {
        Ok(_) => crate::serial_println!("[tour] do not show again: {DONE_KEY}=true saved in {}", p.path()),
        Err(e) => crate::serial_println!("[tour] do not show again: NOT saved ({e:?}); the tour returns at the next start"),
    }
}

fn apply(p: &mut Phase, act: Act) {
    let Phase::Active(step) = *p else { return };
    match act {
        Act::Next if step + 1 < STEPS.len() => *p = Phase::Active(step + 1),
        // Finishing the tour counts as "seen": do not show it again.
        Act::Next | Act::Never => {
            crate::serial_println!("[tour] closed at step {} of {}: {}", step + 1, STEPS.len(), if act == Act::Next { "finished" } else { "do not show again" });
            REMEMBER.store(true, Ordering::Relaxed);
            *p = Phase::Off;
        }
        Act::Back => *p = Phase::Active(step.saturating_sub(1)),
        Act::Skip => {
            crate::serial_println!("[tour] closed at step {} of {}: skipped (Esc or Skip)", step + 1, STEPS.len());
            *p = Phase::Off;
        }
    }
}

/// What a batch of pending scancodes means for the tour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keys {
    /// Only key releases / nothing of interest: leave the ring alone.
    Nothing,
    /// The user is typing (or holding a modifier): close, leave the keys.
    Typing,
    /// Only tour keys: consume them and apply these actions in order.
    Tour([Option<Act>; 4]),
}

fn classify(codes: &[u8]) -> Keys {
    let mut acts: [Option<Act>; 4] = [None; 4];
    let mut n = 0;
    let mut ext = false;
    let mut push = |a: Act, acts: &mut [Option<Act>; 4]| {
        if n < acts.len() {
            acts[n] = Some(a);
            n += 1;
        }
    };
    let mut consumed_any = false;
    for &sc in codes {
        if ext {
            ext = false;
            if sc & 0x80 == 0 {
                consumed_any = true;
                match sc {
                    0x4D | 0x50 => push(Act::Next, &mut acts), // Right, Down
                    0x4B | 0x48 => push(Act::Back, &mut acts), // Left, Up
                    _ => {}
                }
            }
            continue;
        }
        match sc {
            0xE0 => ext = true,
            s if s & 0x80 != 0 => {} // a key release
            0x01 => {
                consumed_any = true;
                push(Act::Skip, &mut acts); // Esc
            }
            0x3B | 0x3C => consumed_any = true, // F1 / F2: latched by ps2, help closes the tour
            _ => return Keys::Typing,
        }
    }
    if consumed_any { Keys::Tour(acts) } else { Keys::Nothing }
}

/// One call per desktop-loop iteration (from `help::pump`). `wizard_open`: the
/// install wizard's window is on screen. Returns true when the desktop must be
/// repainted.
pub fn pump(fs: &mut dyn eurofs::FileSystem, sw: usize, sh: usize, wizard_open: bool) -> bool {
    if REMEMBER.swap(false, Ordering::Relaxed) {
        remember(fs);
    }
    let mut p = PHASE.lock();
    if *p == Phase::Unknown {
        // The tour is not laid over the install wizard: it is decided, and shown,
        // once that window is closed. Nothing is read from the disk until then.
        if wizard_open && !cfg!(feature = "selftest") && !crate::bootopt::tour_off() {
            if !WAIT_LOGGED.swap(true, Ordering::Relaxed) {
                crate::serial_println!("[tour] waiting: the install wizard is open; the tour starts when it is closed");
            }
            return false;
        }
        let off = cfg!(feature = "selftest") || crate::bootopt::tour_off() || seen(fs);
        *p = if off { Phase::Off } else { Phase::Active(0) };
        if !off {
            crate::serial_println!("[tour] first-run tour shown (Esc, Skip, a click outside or typing closes it)");
        }
        return !off;
    }
    let Phase::Active(step) = *p else { return false };
    let mut dirty = false;

    // Mouse: a press on the card is ours; a press anywhere else closes the
    // tour and stays pending for the desktop.
    if let Some((mx, my)) = crate::mouse::peek_press() {
        let card = card_rect(step, sw, sh);
        let inside = |r: (usize, usize, usize, usize)| mx >= r.0 && mx < r.0 + r.2 && my >= r.1 && my < r.1 + r.3;
        if inside(card) {
            let _ = crate::mouse::take_press();
            if let Some((act, _, _)) = buttons(step, card).into_iter().flatten().find(|(_, r, _)| inside(*r)) {
                apply(&mut *p, act);
            }
        } else {
            crate::serial_println!("[tour] closed by a click at {mx},{my} outside the card (the click goes to the desktop)");
            *p = Phase::Off;
        }
        dirty = true;
    }

    if keyboard(&mut p) {
        dirty = true;
    }
    dirty
}

/// Keyboard: look, do not take, unless only tour keys are pending. True when the tour changed.
fn keyboard(p: &mut Phase) -> bool {
    if !matches!(*p, Phase::Active(_)) {
        return false;
    }
    let mut buf = [0u8; 32];
    let n = crate::ps2::peek_scancodes(&mut buf);
    match classify(&buf[..n]) {
        Keys::Nothing => false,
        Keys::Typing => {
            crate::serial_println!("[tour] closed by typing (scancodes {:02x?} stay for the desktop)", &buf[..n]);
            *p = Phase::Off;
            true
        }
        Keys::Tour(acts) => {
            for _ in 0..n {
                let _ = crate::ps2::poll_scancode();
            }
            for a in acts.into_iter().flatten() {
                apply(p, a);
            }
            true
        }
    }
}

/// The tour looks at the keys again, right before the desktop loop's own key readers drain the
/// ring (BUG-115). A key that arrived after the loop's first look (the virtio-input poll in the
/// timer tick, the poll right before the readers) was taken by a reader before the tour saw it: an
/// Esc went to the focused window and the tour stayed up. True when the desktop must repaint.
pub fn pump_keys() -> bool {
    let mut p = PHASE.lock();
    keyboard(&mut p)
}

/// Is the tour up? While it is, the desktop's key readers leave the ring alone: the tour decides
/// per batch whether the keys are its own (Esc, arrows: taken) or typing (it closes and the keys stay).
pub fn keys_reserved() -> bool {
    matches!(*PHASE.lock(), Phase::Active(_))
}

pub fn render(fb: &FrameBuffer, sw: usize, sh: usize) {
    let Phase::Active(step) = *PHASE.lock() else { return };
    let s = &STEPS[step];
    // Ring around what the step talks about.
    if let Some((tx, ty, tw, th)) = target_rect(s.target) {
        let (rx, ry, rw, rh) = (tx.saturating_sub(5), ty.saturating_sub(5), tw + 10, th + 10);
        fb.draw_border(rx, ry, rw, rh, 2, Color::ACCENT);
        fb.draw_border(rx.saturating_sub(1), ry.saturating_sub(1), rw + 2, rh + 2, 1, Color::ACCENT_SOFT);
    }
    let card = card_rect(step, sw, sh);
    let (x, y, w, h) = card;
    fb.drop_shadow(x, y, w, h, 18, 8, Color::rgb(0x1A, 0x22, 0x2C));
    fb.fill_rounded_rect(x, y, w, h, crate::eds::RADIUS_M, Color::SURFACE);
    fb.draw_border(x, y, w, h, 1, Color::BORDER);
    // A short connector from the card to the dock when the step has a target.
    if let Some((tx, ty, tw, th)) = target_rect(s.target) {
        let cy = (ty + th.min(TILE) / 2).max(y + 12).min(y + h - 12);
        let from = tx + tw + 6;
        if x > from {
            fb.fill_rect(from, cy, x - from, 2, Color::ACCENT);
        }
    }
    text::draw_px_styled(fb, x + PAD, y + PAD - 2, s.title, Color::INK, 16.0, text::STYLE_BOLD, 0);
    let count = alloc::format!("{} of {}", step + 1, STEPS.len());
    let cw = text::width_px(&count, 11.5);
    text::draw_px(fb, x + w - PAD - cw, y + PAD + 2, &count, Color::TEXT_DIM, 11.5);
    let mut ly = y + PAD + 26;
    for l in body_lines(step) {
        text::draw_px(fb, x + PAD, ly, &l, Color::INK, 13.0);
        ly += 19;
    }
    for (act, (bx, by, bw, bh), label) in buttons(step, card).into_iter().flatten() {
        let lw = text::width_px(label, 12.5);
        match act {
            Act::Next => {
                fb.fill_rounded_rect(bx, by, bw, bh, 8, Color::ACCENT);
                text::draw_px(fb, bx + (bw.saturating_sub(lw)) / 2, by + 7, label, Color::WHITE, 12.5);
            }
            Act::Back | Act::Skip => {
                fb.fill_rounded_rect(bx, by, bw, bh, 8, Color::SURFACE_3);
                text::draw_px(fb, bx + (bw.saturating_sub(lw)) / 2, by + 7, label, Color::INK, 12.5);
            }
            Act::Never => {
                text::draw_px(fb, bx + 4, by + 7, label, Color::TEXT_SEC, 12.5);
            }
        }
    }
}

/// Part of the `[help]` boot self-test: step count, the dock rectangles match
/// the compositor's hit tests, the key classifier lets typing through, and the
/// tour is off in this (self-test) image.
pub fn selftest() -> bool {
    let steps_ok = (4..=6).contains(&STEPS.len()) && STEPS.iter().all(|s| !s.title.is_empty() && !s.body.is_empty());
    let mut geom_ok = true;
    for i in 0..TILES {
        let (x, y, w, h) = target_rect(Target::Tile(i)).unwrap_or((0, 0, 0, 0));
        geom_ok &= crate::compositor::dock_icon_at(x + w / 2, y + h / 2) == Some(i);
        geom_ok &= crate::compositor::dock_icon_at(x + w / 2, y + 1) == Some(i);
        geom_ok &= crate::compositor::dock_icon_at(x + w / 2, y + h - 1) == Some(i);
    }
    if let Some((x, y, w, h)) = target_rect(Target::Brand) {
        geom_ok &= crate::compositor::brand_button_at(x + w / 2, y + h / 2);
    }
    let keys_ok = classify(&[]) == Keys::Nothing
        && classify(&[0x9C]) == Keys::Nothing // Enter released (left over from the lock screen)
        && classify(&[0x1E]) == Keys::Typing // 'a'
        && classify(&[0x9C, 0x2A, 0x23]) == Keys::Typing // release, Shift, 'h'
        && classify(&[0x1C]) == Keys::Typing // Enter belongs to the desktop
        && classify(&[0x01]) == Keys::Tour([Some(Act::Skip), None, None, None])
        && classify(&[0xE0, 0x4D, 0xE0, 0xCD]) == Keys::Tour([Some(Act::Next), None, None, None])
        && classify(&[0xE0, 0x4B]) == Keys::Tour([Some(Act::Back), None, None, None]);
    // Every card fits a 1280x720 screen with all its buttons inside it.
    let mut fits = true;
    for step in 0..STEPS.len() {
        let card = card_rect(step, 1280, 720);
        fits &= card.0 + card.2 <= 1280 && card.1 + card.3 <= 720;
        for (_, r, label) in buttons(step, card).into_iter().flatten() {
            fits &= r.0 >= card.0 && r.0 + r.2 <= card.0 + card.2 && r.1 + r.3 <= card.1 + card.3;
            fits &= text::width_px(label, 12.5) + 8 <= r.2;
        }
    }
    let off_here = !cfg!(feature = "selftest") || !is_active();
    let path_ok = crate::prefs::user_path("euro", crate::prefs::DESKTOP).as_deref() == Some("/home/euro/.config/euroos/desktop.conf");
    steps_ok && geom_ok && keys_ok && fits && off_here && path_ok
}
