//! EuroClip — de klembordbeheerder van EuroOS (Sprint AC-2).
//!
//! Houdt een **geschiedenis** van gekopieerde items bij met vastmaken, zoeken en
//! dedup. **GDPR-native**: de geschiedenis wordt nooit naar schijf geschreven
//! tenzij een item expliciet is vastgemaakt, niet-vastgemaakte items verlopen
//! automatisch, en **wachtwoord-achtige inhoud wordt herkend en uitgesloten** van
//! de geschiedenis (zodat een gekopieerd wachtwoord niet blijft rondslingeren).
//!
//! Pure `no_std`-logica, host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Het soort klembord-item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Text,
    Image,
}

/// Eén item in de klembordgeschiedenis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    pub kind: ClipKind,
    /// Tekst-inhoud (voor afbeeldingen: een korte omschrijving/afmeting).
    pub text: String,
    /// Grootte in bytes (relevant voor afbeeldingen).
    pub bytes: usize,
    /// Vastgemaakt? Vastgemaakte items verlopen niet en mogen naar schijf.
    pub pinned: bool,
    /// Tijdstempel (kernel-tick of unix-seconde — eenheid bepaalt de aanroeper).
    pub ts: u64,
}

/// Het resultaat van een kopieer-actie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyResult {
    /// Toegevoegd aan de geschiedenis (vooraan).
    Stored,
    /// Verplaatst naar voren (was al aanwezig).
    Promoted,
    /// Geweigerd: lijkt op een wachtwoord → niet bewaard (privacy).
    RejectedSecret,
}

/// De klembordbeheerder.
#[derive(Debug, Clone)]
pub struct Clipboard {
    items: Vec<ClipItem>,
    max_items: usize,
    retain: u64,
}

impl Clipboard {
    /// `max_items` = maximale geschiedenislengte; `retain` = hoe lang (in dezelfde
    /// eenheid als `ts`) een niet-vastgemaakt item bewaard blijft.
    pub fn new(max_items: usize, retain: u64) -> Self {
        Clipboard { items: Vec::new(), max_items: max_items.max(1), retain }
    }

    /// Kopieer tekst naar het klembord. Dedup (verplaatst naar voren) en sluit
    /// wachtwoord-achtige inhoud uit.
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

    /// Kopieer een afbeelding (omschrijving + bytes) naar het klembord.
    pub fn copy_image(&mut self, desc: &str, bytes: usize, now: u64) -> CopyResult {
        self.items.insert(
            0,
            ClipItem { kind: ClipKind::Image, text: desc.to_string(), bytes, pinned: false, ts: now },
        );
        self.trim();
        CopyResult::Stored
    }

    /// Het meest recente item (de huidige klembordinhoud).
    pub fn current(&self) -> Option<&ClipItem> {
        self.items.first()
    }

    /// De volledige geschiedenis (meest recent eerst).
    pub fn history(&self) -> &[ClipItem] {
        &self.items
    }

    /// Maak een item vast / los (op index).
    pub fn set_pinned(&mut self, index: usize, pinned: bool) -> bool {
        if let Some(it) = self.items.get_mut(index) {
            it.pinned = pinned;
            true
        } else {
            false
        }
    }

    /// Zoek in de geschiedenis (substring, hoofdletterongevoelig).
    pub fn search(&self, query: &str) -> Vec<&ClipItem> {
        let q = query.to_lowercase();
        self.items
            .iter()
            .filter(|i| q.is_empty() || i.text.to_lowercase().contains(&q))
            .collect()
    }

    /// Verwijder niet-vastgemaakte items ouder dan `retain`. Vastgemaakte blijven.
    pub fn expire(&mut self, now: u64) {
        self.items.retain(|i| i.pinned || now.saturating_sub(i.ts) <= self.retain);
    }

    /// Wis de geschiedenis; vastgemaakte items blijven (privacy-wis).
    pub fn clear_unpinned(&mut self) {
        self.items.retain(|i| i.pinned);
    }

    /// Wis ALLES, inclusief vastgemaakt.
    pub fn clear_all(&mut self) {
        self.items.clear();
    }

    /// De items die naar schijf MOGEN (GDPR: enkel vastgemaakte persisteren).
    pub fn persistable(&self) -> Vec<&ClipItem> {
        self.items.iter().filter(|i| i.pinned).collect()
    }

    /// Trim tot `max_items`, maar gooi vastgemaakte items nooit weg.
    fn trim(&mut self) {
        while self.items.len() > self.max_items {
            // Verwijder het oudste NIET-vastgemaakte item.
            if let Some(pos) = self.items.iter().rposition(|i| !i.pinned) {
                self.items.remove(pos);
            } else {
                break; // alles vastgemaakt → niets te trimmen
            }
        }
    }
}

/// Heuristiek: lijkt deze tekst op een wachtwoord/geheim? Zo ja, niet bewaren.
/// Eén "woord" (geen spaties/regeleinden), 8–128 tekens, met voldoende variatie
/// (≥3 van: kleine letter, hoofdletter, cijfer, symbool).
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
        assert_eq!(cb.copy_text("hallo", 1), CopyResult::Stored);
        assert_eq!(cb.copy_text("wereld", 2), CopyResult::Stored);
        assert_eq!(cb.current().unwrap().text, "wereld");
        assert_eq!(cb.history().len(), 2);
    }

    #[test]
    fn dedup_promotes_to_front() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("een", 1);
        cb.copy_text("twee", 2);
        assert_eq!(cb.copy_text("een", 3), CopyResult::Promoted);
        assert_eq!(cb.current().unwrap().text, "een");
        assert_eq!(cb.history().len(), 2); // geen duplicaat
    }

    #[test]
    fn secrets_excluded() {
        let mut cb = Clipboard::new(10, 600);
        // Lijkt op een wachtwoord (lower+upper+digit+symbol, geen spaties).
        assert_eq!(cb.copy_text("Xq7!vR2p#Lm", 1), CopyResult::RejectedSecret);
        assert!(cb.current().is_none());
        // Gewone tekst met spaties → wél bewaard.
        assert_eq!(cb.copy_text("dit is een zin", 2), CopyResult::Stored);
    }

    #[test]
    fn secret_heuristic_edges() {
        assert!(looks_like_secret("Abc123!@x")); // 4 klassen
        assert!(!looks_like_secret("short1!")); // te kort
        assert!(!looks_like_secret("alllowercase")); // 1 klasse
        assert!(!looks_like_secret("twee woorden Ab1!")); // bevat spatie
        assert!(looks_like_secret("Hunter2-Pass")); // lower+upper+digit+symbol
    }

    #[test]
    fn pin_protects_from_expire_and_trim() {
        let mut cb = Clipboard::new(2, 100);
        cb.copy_text("oud", 1);
        cb.set_pinned(0, true); // 'oud' vastgemaakt
        cb.copy_text("a", 2);
        cb.copy_text("b", 3); // zou 'oud' wegtrimmen, maar die is vastgemaakt
        assert!(cb.history().iter().any(|i| i.text == "oud"));
        // Expire ver in de toekomst: vastgemaakt blijft, rest verdwijnt.
        cb.expire(1000);
        let texts: Vec<&str> = cb.history().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, alloc::vec!["oud"]);
    }

    #[test]
    fn search_and_clear() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("Rapport Q1", 1);
        cb.copy_text("notitie", 2);
        cb.copy_text("rapport Q2", 3);
        assert_eq!(cb.search("rapport").len(), 2);
        cb.set_pinned(0, true); // pin 'rapport Q2'
        cb.clear_unpinned();
        assert_eq!(cb.history().len(), 1);
        assert_eq!(cb.current().unwrap().text, "rapport Q2");
    }

    #[test]
    fn gdpr_only_pinned_persist() {
        let mut cb = Clipboard::new(10, 600);
        cb.copy_text("vluchtig", 1);
        cb.copy_text("bewaar mij", 2);
        cb.set_pinned(0, true);
        let persist = cb.persistable();
        assert_eq!(persist.len(), 1);
        assert_eq!(persist[0].text, "bewaar mij");
    }
}
