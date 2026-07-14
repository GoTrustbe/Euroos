//! **BB-6** — EuroAgent **dispatch panel**: the sovereign agent-first front-end.
//! Type a command (intent); the runtime routes it to an agent and runs the
//! agent loop (model → tool → result) through the REAL MCP gateway. The panel shows
//! every tool call LIVE with the capability decision: allowed (green, audited)
//! or denied (red) with a **capability grant prompt** for elevated rights.
//! The model talks via EuroNet TCP to a local Ollama (BB-1).

use core::sync::atomic::{AtomicBool, Ordering};

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

const TITLEBAR_H: usize = 44;

static EDITING: AtomicBool = AtomicBool::new(false);
static INTENT: Mutex<String> = Mutex::new(String::new());
/// Transcript of the last run: (line, color).
static LOG: Mutex<Vec<(String, Color)>> = Mutex::new(Vec::new());
static NEEDS_GRANT: AtomicBool = AtomicBool::new(false);

pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

pub fn begin_edit() {
    EDITING.store(true, Ordering::Relaxed);
    INTENT.lock().clear();
}

/// Process a key in the intent field. Returns Some(intent) on Enter → dispatch.
pub fn edit_key(ch: char) -> Option<String> {
    if !EDITING.load(Ordering::Relaxed) {
        return None;
    }
    match ch {
        '\r' => {
            let s = INTENT.lock().clone();
            EDITING.store(false, Ordering::Relaxed);
            if !s.trim().is_empty() {
                return Some(s.trim().into());
            }
        }
        '\u{1b}' => EDITING.store(false, Ordering::Relaxed),
        '\u{8}' | '\u{7f}' => {
            INTENT.lock().pop();
        }
        c if !c.is_control() => INTENT.lock().push(c),
        _ => {}
    }
    None
}

/// y-offset (from win_y) of the intent input field.
fn field_y() -> usize {
    TITLEBAR_H + 64
}

/// Click on the intent input field?
pub fn field_at(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> bool {
    let fx = win_x + 24;
    let fy = win_y + field_y();
    mx >= fx && mx < fx + win_w.saturating_sub(48) && my + 4 >= fy && my < fy + 32
}

/// Run the demo agent for `intent` and build the live, colored transcript.
pub fn dispatch(intent: &str) {
    let (routed, run) = crate::agent::run_intent(intent);
    let mut log = LOG.lock();
    log.clear();
    match &routed {
        Some(a) => log.push((alloc::format!("intent routed  →  agent '{a}'"), Color::ACCENT)),
        None => log.push((String::from("no agent matches this intent (demo agent runs anyway)"), Color::TEXT_DIM)),
    }
    let mut grant = false;
    for line in &run.log {
        if line.contains("GEWEIGERD") {
            log.push((alloc::format!("\u{2717}  {line}"), Color::RED));
            grant = true;
        } else {
            log.push((alloc::format!("\u{2713}  {line}"), Color::SUCCESS));
        }
    }
    if !run.answer.is_empty() {
        log.push((alloc::format!("final answer:  {}", run.answer), Color::INK));
    }
    log.push((
        alloc::format!("{} tool calls, {} denied — all immutably audited (P3)", run.tool_calls, run.denied),
        Color::TEXT_DIM,
    ));
    NEEDS_GRANT.store(grant, Ordering::Relaxed);
}

/// Render the dispatch panel body (live runtime state).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);
    fb.fill_rect(x, y, w, h, Color::SURFACE);

    text::draw_px(fb, x + 24, y + 16, "EuroAgent", Color::INK, 20.0);
    text::draw_px(
        fb,
        x + 24,
        y + 44,
        "Give the agent a command. Every tool runs through EuroGuard (cap gate) + immutable audit.",
        Color::TEXT_SEC,
        12.5,
    );

    // Intent input field.
    let fy = win_y + field_y();
    let fw = w.saturating_sub(48);
    let edit = EDITING.load(Ordering::Relaxed);
    fb.fill_rounded_rect(x + 24, fy, fw, 32, crate::eds::RADIUS_S, Color::SURFACE_3);
    fb.draw_border(x + 24, fy, fw, 32, if edit { 2 } else { 1 }, if edit { Color::ACCENT } else { Color::BORDER });
    let mut shown = INTENT.lock().clone();
    if edit {
        shown.push('|');
    } else if shown.is_empty() {
        shown.push_str("e.g. record meeting and summarize");
    }
    let c = if edit || !INTENT.lock().is_empty() { Color::INK } else { Color::TEXT_DIM };
    text::draw_px(fb, x + 34, fy + 8, &shown, c, 14.0);
    text::draw_px(
        fb,
        x + 24,
        fy + 42,
        "type + Enter \u{2192} run intent (scripted demo model \u{2014} no live LLM/tool effects)",
        Color::TEXT_DIM,
        11.5,
    );

    // Transcript of the last run.
    let mut ty = fy + 72;
    let log = LOG.lock();
    if log.is_empty() {
        text::draw_px(fb, x + 24, ty, "No run yet. Type a command above and press Enter.", Color::TEXT_DIM, 13.0);
    } else {
        for (line, col) in log.iter() {
            if ty > y + h - 56 {
                break;
            }
            text::draw_px(fb, x + 24, ty, line, *col, 13.0);
            ty += 23;
        }
    }

    // Capability grant prompt when a tool requested elevated rights.
    if NEEDS_GRANT.load(Ordering::Relaxed) {
        let py = (ty + 6).min(y + h - 46);
        fb.fill_rounded_rect(x + 24, py, fw, 38, crate::eds::RADIUS_M, Color::SURFACE_3);
        fb.draw_border(x + 24, py, fw, 38, 1, Color::GOLD);
        text::draw_px(fb, x + 36, py + 11, "\u{26A0} 'exec' needs elevated rights \u{2014} the capability gate denied it (buttons illustrative):", Color::INK, 12.5);
        // Two buttons (visual only — the cap-gate decision above is automatic): allow / deny.
        let bw = 96usize;
        let bx2 = x + 24 + fw - bw - 12;
        let bx1 = bx2 - bw - 8;
        fb.fill_rounded_rect(bx1, py + 6, bw, 26, crate::eds::RADIUS_S, Color::SUCCESS_SOFT);
        text::draw_px(fb, bx1 + 16, py + 11, "Allow", Color::SUCCESS, 12.5);
        fb.fill_rounded_rect(bx2, py + 6, bw, 26, crate::eds::RADIUS_S, Color::SURFACE);
        fb.draw_border(bx2, py + 6, bw, 26, 1, Color::BORDER);
        text::draw_px(fb, bx2 + 18, py + 11, "Deny", Color::TEXT_SEC, 12.5);
    }
}
