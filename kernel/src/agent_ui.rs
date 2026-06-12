//! **BB-6** — EuroAgent **dispatch-paneel**: het soevereine agent-first front-end.
//! Typ een opdracht (intent); de runtime routeert ze naar een agent en draait de
//! agent-lus (model → tool → resultaat) door de ECHTE MCP-gateway. Het paneel toont
//! elke tool-aanroep LIVE met de capability-beslissing: toegestaan (groen, geaudit)
//! of geweigerd (rood) met een **capability-grant-prompt** voor verhoogde rechten.
//! Het model praat via EuroNet-TCP met een lokale Ollama (BB-1).

use core::sync::atomic::{AtomicBool, Ordering};

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

const TITLEBAR_H: usize = 44;

static EDITING: AtomicBool = AtomicBool::new(false);
static INTENT: Mutex<String> = Mutex::new(String::new());
/// Transcript van de laatste run: (regel, kleur).
static LOG: Mutex<Vec<(String, Color)>> = Mutex::new(Vec::new());
static NEEDS_GRANT: AtomicBool = AtomicBool::new(false);

pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

pub fn begin_edit() {
    EDITING.store(true, Ordering::Relaxed);
    INTENT.lock().clear();
}

/// Verwerk een toets in het intent-veld. Geeft Some(intent) bij Enter → dispatch.
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

/// y-offset (vanaf win_y) van het intent-invoerveld.
fn field_y() -> usize {
    TITLEBAR_H + 64
}

/// Klik op het intent-invoerveld?
pub fn field_at(win_x: usize, win_y: usize, win_w: usize, mx: usize, my: usize) -> bool {
    let fx = win_x + 24;
    let fy = win_y + field_y();
    mx >= fx && mx < fx + win_w.saturating_sub(48) && my + 4 >= fy && my < fy + 32
}

/// Draai de demo-agent voor `intent` en bouw het live, gekleurde transcript op.
pub fn dispatch(intent: &str) {
    let (routed, run) = crate::agent::run_intent(intent);
    let mut log = LOG.lock();
    log.clear();
    match &routed {
        Some(a) => log.push((alloc::format!("intent gerouteerd  →  agent '{a}'"), Color::ACCENT)),
        None => log.push((String::from("geen agent matcht deze intent (demo-agent draait toch)"), Color::TEXT_DIM)),
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
        log.push((alloc::format!("eindantwoord:  {}", run.answer), Color::INK));
    }
    log.push((
        alloc::format!("{} tool-aanroepen, {} geweigerd — alles onveranderlijk geaudit (P3)", run.tool_calls, run.denied),
        Color::TEXT_DIM,
    ));
    NEEDS_GRANT.store(grant, Ordering::Relaxed);
}

/// Render het dispatch-paneel-lichaam (live runtime-toestand).
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
        "Geef de agent een opdracht. Elke tool loopt door EuroGuard (cap-gate) + onveranderlijke audit.",
        Color::TEXT_SEC,
        12.5,
    );

    // Intent-invoerveld.
    let fy = win_y + field_y();
    let fw = w.saturating_sub(48);
    let edit = EDITING.load(Ordering::Relaxed);
    fb.fill_rounded_rect(x + 24, fy, fw, 32, crate::eds::RADIUS_S, Color::SURFACE_3);
    fb.draw_border(x + 24, fy, fw, 32, if edit { 2 } else { 1 }, if edit { Color::ACCENT } else { Color::BORDER });
    let mut shown = INTENT.lock().clone();
    if edit {
        shown.push('|');
    } else if shown.is_empty() {
        shown.push_str("bv. vergadering opnemen en samenvatten");
    }
    let c = if edit || !INTENT.lock().is_empty() { Color::INK } else { Color::TEXT_DIM };
    text::draw_px(fb, x + 34, fy + 8, &shown, c, 14.0);
    text::draw_px(
        fb,
        x + 24,
        fy + 42,
        "typ + Enter \u{2192} verzend naar de lokale agent (model via EuroNet-TCP, BB-1)",
        Color::TEXT_DIM,
        11.5,
    );

    // Transcript van de laatste run.
    let mut ty = fy + 72;
    let log = LOG.lock();
    if log.is_empty() {
        text::draw_px(fb, x + 24, ty, "Nog geen run. Typ hierboven een opdracht en druk Enter.", Color::TEXT_DIM, 13.0);
    } else {
        for (line, col) in log.iter() {
            if ty > y + h - 56 {
                break;
            }
            text::draw_px(fb, x + 24, ty, line, *col, 13.0);
            ty += 23;
        }
    }

    // Capability-grant-prompt wanneer een tool verhoogde rechten vroeg.
    if NEEDS_GRANT.load(Ordering::Relaxed) {
        let py = (ty + 6).min(y + h - 46);
        fb.fill_rounded_rect(x + 24, py, fw, 38, crate::eds::RADIUS_M, Color::SURFACE_3);
        fb.draw_border(x + 24, py, fw, 38, 1, Color::GOLD);
        text::draw_px(fb, x + 36, py + 11, "\u{26A0} 'exec' vraagt verhoogde rechten \u{2014} capability-grant vereist:", Color::INK, 12.5);
        // Twee knoppen (visueel): toestaan / weigeren.
        let bw = 96usize;
        let bx2 = x + 24 + fw - bw - 12;
        let bx1 = bx2 - bw - 8;
        fb.fill_rounded_rect(bx1, py + 6, bw, 26, crate::eds::RADIUS_S, Color::SUCCESS_SOFT);
        text::draw_px(fb, bx1 + 16, py + 11, "Toestaan", Color::SUCCESS, 12.5);
        fb.fill_rounded_rect(bx2, py + 6, bw, 26, crate::eds::RADIUS_S, Color::SURFACE);
        fb.draw_border(bx2, py + 6, bw, 26, 1, Color::BORDER);
        text::draw_px(fb, bx2 + 18, py + 11, "Weigeren", Color::TEXT_SEC, 12.5);
    }
}
