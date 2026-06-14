//! EuroClip — the clipboard manager of EuroOS (Sprint AC-2).
//!
//! Keeps a **history** of copied items with pinning, search and dedup.
//! **GDPR-native**: the history is never written to disk unless an item is
//! explicitly pinned, unpinned items expire automatically, and
//! **password-like content is recognized and excluded** from the history (so a
//! copied password does not stay lying around).
//!
//! Pure `no_std` logic, host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The kind of clipboard item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Text,
    Image,
}

/// One item in the clipboard history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    pub kind: ClipKind,
    /// Text content (for images: a short description/dimensions).
    pub text: String,
    /// Size in bytes (relevant for images).
    pub bytes: usize,
    /// Pinned? Pinned items do not expire and may go to disk.
    pub pinned: bool,
    /// Timestamp (kernel tick or unix second — the unit is decided by the caller).
    pub ts: u64,
}

/// The result of a copy action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyResult {
    /// Added to the history (at the front).
    Stored,
    /// Moved to the front (was already present).
    Promoted,
    /// Rejected: looks like a password → not retained (privacy).
    RejectedSecret,
}

/// The clipboard manager.
#[derive(Debug, Clone)]
pub struct Clipboard {
    items: Vec<ClipItem>,
    max_items: usize,
    retain: u64,
}

impl Clipboard {
    /// `max_items` = maximum history length; `retain` = how long (in the same
    /// unit as `ts`) an unpinned item is retained.
    pub fn new(max_items: usize, retain: u64) -> Self {
        Clipboard { items: Vec::new(), max_items: max_items.max(1), retain }
    }

    /// Copy text to the clipboard. Dedup (moves to the front) and excludes
    /// password-like content.
    pub fn copy_text(&mut self, text: &str, now: u64) -> CopyResult {
        if looks_like_secret(text) {
            return CopyResult::RejectedSecret;
        }
        if let Some(pos) = self.items.iter().position(|i| i.kind == ClipKind::Text && i.text == text) {
            let mut item = self.items.remove(pos);
            item.ts = now;
            self.items.insert(0, item);
            return CopyResult::Promoted;
        }
        self.items.insert(
            0,
            ClipItem { kind: ClipKind::Text, text: text.to_string(), bytes: text.len(), pinned: false, ts: now },
        );
        self.trim();
        CopyResult::Stored
    }

    /// Copy an image (description + bytes) to the clipboard.
    pub fn copy_image(&mut self, desc: &str, bytes: usize, now: u64) -> CopyResult {
        self.items.insert(
            0,
            ClipItem { kind: ClipKind::Image, text: desc.to_string(), bytes, pinned: false, ts: now },
        );
        self.trim();
        CopyResult::Stored
    }

    /// The most recent item (the current clipboard content).
    pub fn current(&self) -> Option<&ClipItem> {
        self.items.first()
    }

    /// The full history (most recent first).
    pub fn history(&self) -> &[ClipItem] {
        &self.items
    }

    /// Pin / unpin an item (by index).
    pub fn set_pinned(&mut self, index: usize, pinned: bool) -> bool {
        if let Some(it) = self.items.get_mut(index) {
            it.pinned = pinned;
            true
        } else {
            false
        }
    }

    /// Search the history (substring, case-insensitive).
    pub fn search(&self, query: &str) -> Vec<&ClipItem> {
        let q = query.to_lowercase();
        self.items
            .iter()
            .filter(|i| q.is_empty() || i.text.to_lowercase().contains(&q))
            .collect()
    }

    /// Remove unpinned items older than `retain`. Pinned ones remain.
    pub fn expire(&mut self, now: u64) {
        self.items.retain(|i| i.pinned || now.saturating_sub(i.ts) <= self.retain);
    }

    /// Clear the history; pinned items remain (privacy wipe).
    pub fn clear_unpinned(&mut self) {
        self.items.retain(|i| i.pinned);
    }

    /// Clear EVERYTHING, including pinned.
    pub fn clear_all(&mut self) {
        self.items.clear();
    }

    /// The items that MAY go to disk (GDPR: only pinned ones persist).
    pub fn persistable(&self) -> Vec<&ClipItem> {
        self.items.iter().filter(|i| i.pinned).collect()
    }

    /// Trim to `max_items`, but never discard pinned items.
    fn trim(&mut self) {
        while self.items.len() > self.max_items {
            // Remove the oldest NON-pinned item.
            if let Some(pos) = self.items.iter().rposition(|i| !i.pinned) {
                self.items.remove(pos);
            } else {
                break; // everything pinned → nothing to trim
            }
        }
    }
}

/// Heuristic: does this text look like a password/secret? If so, do not retain.
/// One "word" (no spaces/newlines), 8–128 characters, with enough variety
/// (≥3 of: lowercase letter, uppercase letter, digit, symbol).
pub fn looks_like_secret(s: &str) -> bool {
    let t = s.trim();
    let len = t.chars().count();
    if !(8..=128).contains(&len) {
        return false;
    }
    if t.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    let mut symbol = false;
    for c in t.chars() {
        if c.is_ascii_lowercase() {
            lower = true;
        } else if c.is_ascii_uppercase() {
            upper = true;
        } else if c.is_ascii_digit() {
            digit = true;
        } else if c.is_ascii_punctuation() {
            symbol = true;
        }
    }
    let classes = [lower, upper, digit, symbol].iter().filter(|b| **b).count();
    classes >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_and_current() {
        let mut cb = Clipboard::new(10, 600);
        assert_eq!(cb.copy_text("hello", 1), CopyResult::Stored);
        assert_eq!(cb.copy_text("world", 2), CopyResult::Stored);
        assert_eq!(cb.current().unwrap().text, "world");
        assert_eq!(cb.history().len(), 2);
    }

    #[test]
    fn dedup_promotes_to_front() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("one", 1);
        cb.copy_text("two", 2);
        assert_eq!(cb.copy_text("one", 3), CopyResult::Promoted);
        assert_eq!(cb.current().unwrap().text, "one");
        assert_eq!(cb.history().len(), 2); // no duplicate
    }

    #[test]
    fn secrets_excluded() {
        let mut cb = Clipboard::new(10, 600);
        // Looks like a password (lower+upper+digit+symbol, no spaces).
        assert_eq!(cb.copy_text("Xq7!vR2p#Lm", 1), CopyResult::RejectedSecret);
        assert!(cb.current().is_none());
        // Ordinary text with spaces → retained.
        assert_eq!(cb.copy_text("this is a sentence", 2), CopyResult::Stored);
    }

    #[test]
    fn secret_heuristic_edges() {
        assert!(looks_like_secret("Abc123!@x")); // 4 classes
        assert!(!looks_like_secret("short1!")); // too short
        assert!(!looks_like_secret("alllowercase")); // 1 class
        assert!(!looks_like_secret("two words Ab1!")); // contains a space
        assert!(looks_like_secret("Hunter2-Pass")); // lower+upper+digit+symbol
    }

    #[test]
    fn pin_protects_from_expire_and_trim() {
        let mut cb = Clipboard::new(2, 100);
        cb.copy_text("old", 1);
        cb.set_pinned(0, true); // 'old' pinned
        cb.copy_text("a", 2);
        cb.copy_text("b", 3); // would trim 'old' away, but it is pinned
        assert!(cb.history().iter().any(|i| i.text == "old"));
        // Expire far in the future: pinned remains, the rest disappears.
        cb.expire(1000);
        let texts: Vec<&str> = cb.history().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, alloc::vec!["old"]);
    }

    #[test]
    fn search_and_clear() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("Report Q1", 1);
        cb.copy_text("note", 2);
        cb.copy_text("report Q2", 3);
        assert_eq!(cb.search("report").len(), 2);
        cb.set_pinned(0, true); // pin 'report Q2'
        cb.clear_unpinned();
        assert_eq!(cb.history().len(), 1);
        assert_eq!(cb.current().unwrap().text, "report Q2");
    }

    #[test]
    fn gdpr_only_pinned_persist() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("ephemeral", 1);
        cb.copy_text("keep me", 2);
        cb.set_pinned(0, true);
        let persist = cb.persistable();
        assert_eq!(persist.len(), 1);
        assert_eq!(persist[0].text, "keep me");
    }
}
