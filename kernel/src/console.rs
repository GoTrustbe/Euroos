//! Simple text console on the framebuffer: scrollback + prompt line.
//! Uses the 8x8 font (scale 1). Redraws the console area on each keystroke.

use alloc::string::String;
use alloc::vec::Vec;

use crate::font::{draw_string, CHAR_HEIGHT, CHAR_WIDTH};
use crate::graphics::{Color, FrameBuffer};

pub struct Console<'a> {
    fb: &'a FrameBuffer,
    x: usize,
    y: usize,
    cols: usize,
    rows: usize,
    lines: Vec<String>,
    prompt: String,
}

impl<'a> Console<'a> {
    pub fn new(fb: &'a FrameBuffer, x: usize, y: usize, w: usize, h: usize, prompt: &str) -> Self {
        let cols = (w / CHAR_WIDTH).max(1);
        let rows = (h / (CHAR_HEIGHT + 2)).max(2);
        Self {
            fb,
            x,
            y,
            cols,
            rows,
            lines: Vec::new(),
            prompt: String::from(prompt),
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Add a line; long lines are hard-wrapped at `cols`.
    pub fn println(&mut self, s: &str) {
        if s.is_empty() {
            self.lines.push(String::new());
            return;
        }
        for chunk in CharChunks::new(s, self.cols) {
            self.lines.push(chunk);
        }
    }

    /// Draw the console area: last lines + the active input line.
    pub fn render(&self, input: &str) {
        let line_h = CHAR_HEIGHT + 2;
        let area_h = self.rows * line_h;
        let area_w = self.cols * CHAR_WIDTH;
        // Background (slightly lighter than bg → "card").
        self.fb.fill_rect(self.x - 6, self.y - 6, area_w + 12, area_h + 12, Color::CARD);

        let visible_rows = self.rows - 1;
        let start = self.lines.len().saturating_sub(visible_rows);
        let mut row = 0;
        for line in &self.lines[start..] {
            draw_string(
                self.fb,
                self.x,
                self.y + row * line_h,
                line,
                Color::TEXT_SEC,
                1,
            );
            row += 1;
        }
        // Prompt line at the bottom.
        let prompt_y = self.y + visible_rows * line_h;
        self.fb.fill_rect(self.x - 6, prompt_y - 1, area_w + 12, line_h, Color::SURFACE);
        draw_string(self.fb, self.x, prompt_y, &self.prompt, Color::ACCENT, 1);
        let cursor_x = self.x + self.prompt.len() * CHAR_WIDTH;
        draw_string(self.fb, cursor_x, prompt_y, input, Color::WHITE, 1);
        // Non-blinking block cursor.
        let cx = cursor_x + input.chars().count() * CHAR_WIDTH;
        self.fb.fill_rect(cx, prompt_y, CHAR_WIDTH, CHAR_HEIGHT, Color::ACCENT);
    }
}

/// Split a string into chunks of at most `n` chars (byte-safe for ASCII).
struct CharChunks<'a> {
    s: &'a str,
    n: usize,
}
impl<'a> CharChunks<'a> {
    fn new(s: &'a str, n: usize) -> Self {
        Self { s, n }
    }
}
impl<'a> Iterator for CharChunks<'a> {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        if self.s.is_empty() {
            return None;
        }
        let take = self.s.chars().take(self.n).collect::<String>();
        let consumed = take.len();
        self.s = &self.s[consumed..];
        Some(take)
    }
}
