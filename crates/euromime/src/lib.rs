//! **EuroMime** (3F-5) — MIME-type detection + default-app associations, so the
//! desktop knows what a file *is* and which app opens it. Detection is by **magic
//! bytes** first (authoritative, spoof-resistant) then by **extension** (a hint);
//! a [`Registry`] maps a MIME type to a default application, so double-clicking a
//! `.docx` opens EuroSuite Writer and a `.png` opens the image viewer.
//!
//! Pure `no_std` logic, host-tested — the sniffing tables and the association
//! resolution are independent of any GUI.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Detect the MIME type of `data` named `filename`. Magic bytes win over the
/// extension (a `.txt` that is really a PNG is reported as `image/png`); the
/// extension is the fallback when no signature matches.
pub fn detect(filename: &str, data: &[u8]) -> String {
    if let Some(m) = sniff_magic(data) {
        return m.to_string();
    }
    by_extension(filename).unwrap_or("application/octet-stream").to_string()
}

/// MIME type from the filename extension alone (no content).
pub fn by_extension(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit('.').next().filter(|e| !e.is_empty() && *e != filename)?;
    // Case-insensitive compare without allocating.
    let matches = |x: &str| ext.eq_ignore_ascii_case(x);
    Some(if matches("txt") || matches("log") || matches("md") {
        "text/plain"
    } else if matches("html") || matches("htm") {
        "text/html"
    } else if matches("json") {
        "application/json"
    } else if matches("png") {
        "image/png"
    } else if matches("jpg") || matches("jpeg") {
        "image/jpeg"
    } else if matches("gif") {
        "image/gif"
    } else if matches("bmp") {
        "image/bmp"
    } else if matches("qoi") {
        "image/qoi"
    } else if matches("pdf") {
        "application/pdf"
    } else if matches("zip") {
        "application/zip"
    } else if matches("gz") {
        "application/gzip"
    } else if matches("docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    } else if matches("xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else if matches("pptx") {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    } else if matches("odt") {
        "application/vnd.oasis.opendocument.text"
    } else if matches("wav") {
        "audio/wav"
    } else if matches("mp3") {
        "audio/mpeg"
    } else if matches("wasm") {
        "application/wasm"
    } else {
        return None;
    })
}

/// Detect from leading magic bytes. Returns a specific type, or `None` if no
/// signature is recognised. OOXML/ODF are ZIP-based, so a plain ZIP signature is
/// reported as `application/zip` (the extension refines it).
fn sniff_magic(data: &[u8]) -> Option<&'static str> {
    let b = data;
    let starts = |sig: &[u8]| b.len() >= sig.len() && &b[..sig.len()] == sig;
    if starts(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some("image/gif");
    }
    if starts(b"BM") {
        return Some("image/bmp");
    }
    if starts(b"qoif") {
        return Some("image/qoi");
    }
    if starts(b"%PDF-") {
        return Some("application/pdf");
    }
    if starts(&[0x1F, 0x8B]) {
        return Some("application/gzip");
    }
    if starts(&[0x00, 0x61, 0x73, 0x6D]) {
        return Some("application/wasm");
    }
    if starts(b"RIFF") && b.len() >= 12 && &b[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    // ZIP (also the container for OOXML/ODF) — the extension refines it.
    if starts(&[b'P', b'K', 0x03, 0x04]) || starts(&[b'P', b'K', 0x05, 0x06]) {
        return Some("application/zip");
    }
    None
}

/// Refined detection: prefer a specific Office/ODF type from the extension when
/// the content is a ZIP (magic can only say "it's a zip"). This is what the file
/// manager should call.
pub fn detect_refined(filename: &str, data: &[u8]) -> String {
    let base = detect(filename, data);
    if base == "application/zip" {
        // A ZIP that is really a .docx/.xlsx/.pptx/.odt → use the extension.
        if let Some(ext_type) = by_extension(filename) {
            if ext_type.contains("openxmlformats") || ext_type.contains("opendocument") {
                return ext_type.to_string();
            }
        }
    }
    base
}

/// The default-application registry: MIME type → app id.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    assoc: Vec<(String, String)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A sensible built-in association set for the EuroOS desktop apps.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        for (mime, app) in [
            ("text/plain", "eurotext"),
            ("text/html", "eurobrowser"),
            ("application/json", "eurotext"),
            ("image/png", "euroshot"),
            ("image/jpeg", "euroshot"),
            ("image/gif", "euroshot"),
            ("image/bmp", "euroshot"),
            ("image/qoi", "euroshot"),
            ("application/pdf", "eurobrowser"),
            ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "eurowriter"),
            ("application/vnd.oasis.opendocument.text", "eurowriter"),
            ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "eurocalc"),
            ("application/vnd.openxmlformats-officedocument.presentationml.presentation", "euroimpress"),
            ("audio/wav", "euromusic"),
            ("audio/mpeg", "euromusic"),
        ] {
            r.set_default(mime, app);
        }
        r
    }

    /// Set (or change) the default app for a MIME type.
    pub fn set_default(&mut self, mime: &str, app: &str) {
        self.assoc.retain(|(m, _)| m != mime);
        self.assoc.push((mime.to_string(), app.to_string()));
    }

    /// The default app for a MIME type, if any.
    pub fn default_app(&self, mime: &str) -> Option<&str> {
        self.assoc.iter().find(|(m, _)| m == mime).map(|(_, a)| a.as_str())
    }

    /// Resolve which app opens `filename` given its `data` — the "open with
    /// default" the file manager performs. `None` if nothing is associated.
    pub fn open_with(&self, filename: &str, data: &[u8]) -> Option<&str> {
        let mime = detect_refined(filename, data);
        self.default_app(&mime)
    }

    /// All associations `(mime, app)` for a settings view.
    pub fn list(&self) -> &[(String, String)] {
        &self.assoc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_beats_extension() {
        // A PNG mislabelled as .txt is still detected as image/png.
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(detect("photo.txt", &png), "image/png");
    }

    #[test]
    fn extension_is_the_fallback() {
        assert_eq!(detect("notes.md", b"# hello"), "text/plain");
        assert_eq!(detect("data.json", b"{}"), "application/json");
        assert_eq!(detect("unknown.xyz", b"\x00\x01\x02"), "application/octet-stream");
    }

    #[test]
    fn zip_refined_to_office_type_by_extension() {
        let zip = [b'P', b'K', 0x03, 0x04, 0, 0, 0, 0];
        assert_eq!(detect("report.docx", &zip), "application/zip"); // raw
        assert_eq!(
            detect_refined("report.docx", &zip),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        // A plain .zip stays application/zip.
        assert_eq!(detect_refined("archive.zip", &zip), "application/zip");
    }

    #[test]
    fn registry_resolves_default_app() {
        let r = Registry::with_defaults();
        let zip = [b'P', b'K', 0x03, 0x04, 0, 0, 0, 0];
        assert_eq!(r.open_with("report.docx", &zip), Some("eurowriter"));
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(r.open_with("a.png", &png), Some("euroshot"));
        assert_eq!(r.open_with("readme.txt", b"hi"), Some("eurotext"));
    }

    #[test]
    fn user_can_override_the_default() {
        let mut r = Registry::with_defaults();
        assert_eq!(r.default_app("text/plain"), Some("eurotext"));
        r.set_default("text/plain", "eurowriter");
        assert_eq!(r.default_app("text/plain"), Some("eurowriter"));
    }

    #[test]
    fn unassociated_type_returns_none() {
        let r = Registry::with_defaults();
        assert_eq!(r.open_with("blob.xyz", b"\x00"), None);
    }

    #[test]
    fn wasm_and_pdf_magic() {
        assert_eq!(detect("m.bin", &[0x00, 0x61, 0x73, 0x6D, 1, 0, 0, 0]), "application/wasm");
        assert_eq!(detect("doc", b"%PDF-1.7"), "application/pdf");
    }
}
