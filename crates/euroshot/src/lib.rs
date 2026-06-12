//! EuroShot — de schermafbeelding-tool van EuroOS (Sprint AC-1).
//!
//! De pure kern achter de screenshot-app: **regio-geometrie** (volledig scherm,
//! venster of selectie, met clamping), **annotaties** (pijl, kader, markering,
//! tekst), en — de soevereine sleutel — een **canoniek manifest** dat met Ed25519
//! ondertekend kan worden zodat een screenshot bewijsbaar ongewijzigd is ("gemaakt
//! op <ts>, <b>×<h>, inhoudshash …"). De kernel levert de pixels + de handtekening;
//! deze crate blijft crypto-vrij en host-testbaar.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een rechthoekige regio in schermpixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Region {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Region { x, y, w, h }
    }
    /// Klem de regio binnen het scherm (`sw`×`sh`).
    pub fn clamp_to(self, sw: u32, sh: u32) -> Region {
        let x = self.x.min(sw);
        let y = self.y.min(sh);
        Region { x, y, w: self.w.min(sw - x), h: self.h.min(sh - y) }
    }
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn area(self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

/// Wat er vastgelegd wordt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    FullScreen,
    Window,
    Region,
}

/// Bepaal de uiteindelijke (geklemde) regio voor een opname.
pub fn capture_region(target: Target, sel: Region, screen_w: u32, screen_h: u32) -> Region {
    match target {
        Target::FullScreen => Region::new(0, 0, screen_w, screen_h),
        Target::Window | Target::Region => sel.clamp_to(screen_w, screen_h),
    }
}

/// Het soort annotatie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotKind {
    Arrow,
    Box,
    Highlight,
    Text,
}

/// Eén annotatie op een screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub kind: AnnotKind,
    pub x: u32,
    pub y: u32,
    pub x2: u32,
    pub y2: u32,
    pub color: u32,
    pub text: String,
}

impl Annotation {
    pub fn arrow(x: u32, y: u32, x2: u32, y2: u32, color: u32) -> Self {
        Annotation { kind: AnnotKind::Arrow, x, y, x2, y2, color, text: String::new() }
    }
    pub fn boxed(x: u32, y: u32, x2: u32, y2: u32, color: u32) -> Self {
        Annotation { kind: AnnotKind::Box, x, y, x2, y2, color, text: String::new() }
    }
    pub fn label(x: u32, y: u32, text: &str, color: u32) -> Self {
        Annotation { kind: AnnotKind::Text, x, y, x2: x, y2: y, color, text: text.to_string() }
    }
}

/// Het manifest van een screenshot — het canoniek-ondertekenbare bewijs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShotManifest {
    pub width: u32,
    pub height: u32,
    /// Tijdstip van opname (unix-seconden).
    pub taken_at: u64,
    /// Inhoudshash van de pixels (FNV-1a-64) — wijzigt de afbeelding → wijzigt de hash.
    pub content_hash: u64,
    pub annotated: bool,
}

impl ShotManifest {
    /// Bouw een manifest uit de pixel-bytes (RGBA of QOI — wat de aanroeper hasht).
    pub fn from_pixels(width: u32, height: u32, taken_at: u64, pixels: &[u8], annotated: bool) -> Self {
        ShotManifest { width, height, taken_at, content_hash: fnv1a_64(pixels), annotated }
    }

    /// De **canonieke bytes** die ondertekend worden (Ed25519). Deterministisch en
    /// stabiel formaat zodat verificatie elders exact reproduceerbaar is.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str("EuroShot-v1\n");
        s.push_str(&alloc::format!("size={}x{}\n", self.width, self.height));
        s.push_str(&alloc::format!("taken_at={}\n", self.taken_at));
        s.push_str(&alloc::format!("content={:016x}\n", self.content_hash));
        s.push_str(&alloc::format!("annotated={}\n", self.annotated));
        s.into_bytes()
    }

    /// Mensvriendelijke samenvatting (voor de eigenschappen-weergave).
    pub fn summary(&self) -> String {
        alloc::format!(
            "{}×{} \u{00B7} gemaakt op {} \u{00B7} hash {:016x}{}",
            self.width,
            self.height,
            self.taken_at,
            self.content_hash,
            if self.annotated { " \u{00B7} geannoteerd" } else { "" }
        )
    }
}

/// FNV-1a 64-bit hash — klein, snel, geen afhankelijkheden (geen crypto-claim;
/// de integriteit komt van de Ed25519-handtekening over [`ShotManifest::canonical_bytes`]).
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_clamp() {
        let r = Region::new(1900, 1000, 200, 200).clamp_to(1920, 1080);
        assert_eq!(r, Region::new(1900, 1000, 20, 80));
        assert!(!r.is_empty());
        assert_eq!(Region::new(0, 0, 0, 50).clamp_to(800, 600).is_empty(), true);
    }

    #[test]
    fn capture_targets() {
        assert_eq!(capture_region(Target::FullScreen, Region::default(), 1920, 1080), Region::new(0, 0, 1920, 1080));
        let sel = Region::new(100, 100, 300, 200);
        assert_eq!(capture_region(Target::Region, sel, 1920, 1080), sel);
    }

    #[test]
    fn manifest_hash_changes_with_pixels() {
        let a = ShotManifest::from_pixels(4, 4, 1000, &[1, 2, 3, 4], false);
        let b = ShotManifest::from_pixels(4, 4, 1000, &[1, 2, 3, 5], false);
        assert_ne!(a.content_hash, b.content_hash);
        // Zelfde pixels + meta → zelfde manifest (deterministisch).
        let c = ShotManifest::from_pixels(4, 4, 1000, &[1, 2, 3, 4], false);
        assert_eq!(a, c);
    }

    #[test]
    fn canonical_payload_stable_and_signed_shape() {
        let m = ShotManifest::from_pixels(1920, 1080, 1_700_000_000, b"pixels", true);
        let bytes = m.canonical_bytes();
        let s = alloc::string::String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("EuroShot-v1\n"));
        assert!(s.contains("size=1920x1080\n"));
        assert!(s.contains("taken_at=1700000000\n"));
        assert!(s.contains("annotated=true\n"));
        // Tampering met de afbeelding wijzigt de payload → handtekening ongeldig.
        let m2 = ShotManifest::from_pixels(1920, 1080, 1_700_000_000, b"PIXELS", true);
        assert_ne!(m.canonical_bytes(), m2.canonical_bytes());
    }

    #[test]
    fn annotations_build() {
        let a = Annotation::arrow(0, 0, 10, 10, 0xFF0000);
        assert_eq!(a.kind, AnnotKind::Arrow);
        let t = Annotation::label(5, 5, "Let op", 0x000000);
        assert_eq!(t.kind, AnnotKind::Text);
        assert_eq!(t.text, "Let op");
    }

    #[test]
    fn fnv_known_vector() {
        // FNV-1a van de lege invoer = de offset basis.
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        // Stabiel over aanroepen.
        assert_eq!(fnv1a_64(b"EuroOS"), fnv1a_64(b"EuroOS"));
    }
}
