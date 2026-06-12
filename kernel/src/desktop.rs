//! EuroOS boot-/login-splash (Track 1 demo, in de huisstijl van het UI-prototype).
//!
//! Dit is bewust een statisch scherm met dezelfde visuele taal als het React-
//! prototype (donker palet, EURO+OS-wordmark, accent, soevereiniteits-footer).
//! De interactieve desktop (sidebar, vensters, login) is Track 5 en draait op
//! de compositor — niet op deze bare-metal 8x8-renderer.
#![allow(dead_code)] // statische splash, vervangen door de interactieve shell

use alloc::string::String;

use crate::font::{draw_string, draw_string_centered, text_width, CHAR_HEIGHT};
use crate::graphics::{Color, FrameBuffer};

const TASKBAR_H: usize = 30;

pub fn render(fb: &FrameBuffer, info: &[String], clock: &str) {
    let w = fb.width();
    let h = fb.height();

    // Achtergrond
    fb.clear(Color::BACKGROUND);

    // Bovenlabel: "EUROKERNEL OS v0.1"
    let top_y = h / 6;
    draw_string_centered(fb, 0, w, top_y, "EUROKERNEL OS v0.1", Color::ACCENT, 1);

    // Wordmark "EURO" (wit) + "OS" (accent), scale 4, als één geheel gecentreerd.
    let scale = 4;
    let euro_w = text_width("EURO", scale);
    let os_w = text_width("OS", scale);
    let mark_x = (w - (euro_w + os_w)) / 2;
    let mark_y = top_y + 22;
    draw_string(fb, mark_x, mark_y, "EURO", Color::WHITE, scale);
    draw_string(fb, mark_x + euro_w, mark_y, "OS", Color::ACCENT, scale);

    // Subtitel
    draw_string_centered(
        fb,
        0,
        w,
        mark_y + CHAR_HEIGHT * scale + 12,
        "Europees Soeverein Besturingssysteem",
        Color::TEXT_SEC,
        1,
    );

    // Infokaart (donker, accent-rand) — login-card-esthetiek.
    let card_w = (w * 9 / 20).max(420).min(w - 80);
    let card_h = 24 + info.len().max(1) * 18 + 24;
    let card_x = (w - card_w) / 2;
    let card_y = mark_y + CHAR_HEIGHT * scale + 56;

    fb.fill_rect(card_x, card_y, card_w, card_h, Color::CARD);
    fb.draw_border(card_x, card_y, card_w, card_h, 1, Color::BORDER);
    fb.fill_rect(card_x, card_y, 3, card_h, Color::ACCENT); // accent-balk links

    let mut line_y = card_y + 16;
    for line in info {
        draw_string(fb, card_x + 18, line_y, line, Color::TEXT_SEC, 1);
        line_y += 18;
    }

    // Soevereiniteits-footer, gecentreerd boven de taakbalk.
    draw_string_centered(
        fb,
        0,
        w,
        h - TASKBAR_H - 26,
        "Versleuteld  -  Geen telemetrie  -  Europees",
        Color::TEXT_DIM,
        1,
    );

    // Taakbalk + accentlijn
    let bar_y = h - TASKBAR_H;
    fb.fill_rect(0, bar_y, w, TASKBAR_H, Color::TASKBAR);
    fb.fill_rect(0, bar_y, w, 1, Color::BORDER);
    let bar_text_y = bar_y + (TASKBAR_H - CHAR_HEIGHT) / 2;
    draw_string(fb, 12, bar_text_y, "EuroKernel v0.1", Color::TEXT_SEC, 1);

    // Online-bolletje + klok rechts
    let clk_x = w.saturating_sub(text_width(clock, 1) + 12);
    draw_string(fb, clk_x, bar_text_y, clock, Color::WHITE, 1);
    fb.fill_rect(clk_x - 16, bar_text_y + 1, 6, 6, Color::SUCCESS);
}
