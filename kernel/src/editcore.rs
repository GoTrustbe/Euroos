//! A real text-editing buffer with a cursor: insertion anywhere, arrow/Home/End
//! navigation, backspace/delete at the cursor, and line splitting on Enter.
//! Shared by EuroText and EuroNotes so both behave like an actual editor rather
//! than an append-only log.

use alloc::string::String;
use alloc::vec::Vec;
use crate::ps2::Key;

pub struct Buffer {
    pub lines: Vec<String>,
    pub row: usize, // cursor line
    pub col: usize, // cursor column (in chars)
    pub dirty: bool,
}

impl Buffer {
    pub fn new() -> Self {
        Buffer { lines: alloc::vec![String::new()], row: 0, col: 0, dirty: false }
    }

    /// Load text, splitting on '\n'. Cursor goes to the start.
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = 0;
        self.col = 0;
        self.dirty = false;
    }

    /// The whole buffer as one string (for saving / parsing).
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn clamp_col(&mut self) {
        let len = self.lines.get(self.row).map(|l| l.chars().count()).unwrap_or(0);
        if self.col > len {
            self.col = len;
        }
    }

    fn byte_at(line: &str, col: usize) -> usize {
        line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len())
    }

    /// Apply one key. Returns true if anything changed (repaint needed).
    pub fn key(&mut self, k: Key) -> bool {
        match k {
            Key::Char(c) => {
                let line = &mut self.lines[self.row];
                let b = Self::byte_at(line, self.col);
                line.insert(b, c);
                self.col += 1;
                self.dirty = true;
            }
            Key::Enter => {
                let line = &mut self.lines[self.row];
                let b = Self::byte_at(line, self.col);
                let tail = line.split_off(b);
                self.lines.insert(self.row + 1, tail);
                self.row += 1;
                self.col = 0;
                self.dirty = true;
            }
            Key::Tab => {
                let line = &mut self.lines[self.row];
                let b = Self::byte_at(line, self.col);
                line.insert_str(b, "    ");
                self.col += 4;
                self.dirty = true;
            }
            Key::Backspace => {
                if self.col > 0 {
                    let line = &mut self.lines[self.row];
                    let prev = Self::byte_at(line, self.col - 1);
                    let cur = Self::byte_at(line, self.col);
                    line.replace_range(prev..cur, "");
                    self.col -= 1;
                } else if self.row > 0 {
                    let cur = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                    self.lines[self.row].push_str(&cur);
                } else {
                    return false;
                }
                self.dirty = true;
            }
            Key::Delete => {
                let len = self.lines[self.row].chars().count();
                if self.col < len {
                    let line = &mut self.lines[self.row];
                    let cur = Self::byte_at(line, self.col);
                    let nxt = Self::byte_at(line, self.col + 1);
                    line.replace_range(cur..nxt, "");
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                } else {
                    return false;
                }
                self.dirty = true;
            }
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                } else {
                    return false;
                }
            }
            Key::Right => {
                let len = self.lines[self.row].chars().count();
                if self.col < len {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                } else {
                    return false;
                }
            }
            Key::Up => {
                if self.row == 0 {
                    return false;
                }
                self.row -= 1;
                self.clamp_col();
            }
            Key::Down => {
                if self.row + 1 >= self.lines.len() {
                    return false;
                }
                self.row += 1;
                self.clamp_col();
            }
            Key::Home => self.col = 0,
            Key::End => self.col = self.lines[self.row].chars().count(),
            Key::PageUp => {
                self.row = self.row.saturating_sub(12);
                self.clamp_col();
            }
            Key::PageDown => {
                self.row = (self.row + 12).min(self.lines.len() - 1);
                self.clamp_col();
            }
            _ => return false,
        }
        true
    }
}
