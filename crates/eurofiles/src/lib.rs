//! EuroFiles — de bestandsbeheerder van EuroOS (Sprint AC-1).
//!
//! Het pure model achter de grafische bestandsbeheerder: een maplijst met
//! **sorteren** en **filteren**, **padbewerkingen** (normaliseren, join, basename,
//! extensie), mensvriendelijke groottes, en de **soevereine badges** die EuroOS
//! per bestand toont (immutable 🔒, getekend, append-only, versleuteld). De kernel
//! vult het uit EuroFS; de compositor rendert het.
//!
//! Pure `no_std`-logica, host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Het soort item in een map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    File,
    Symlink,
}

/// Een soevereine EuroOS-eigenschap die per bestand getoond wordt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// Onveranderlijk (EuroFS immutable-vlag) — 🔒.
    Immutable,
    /// Alleen toevoegen (tamper-evident).
    AppendOnly,
    /// Ed25519-gesigneerd met geldig manifest.
    Signed,
    /// Versleuteld op schijf (FDE/per-bestand).
    Encrypted,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Immutable => "Onveranderlijk",
            Badge::AppendOnly => "Append-only",
            Badge::Signed => "Getekend",
            Badge::Encrypted => "Versleuteld",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Badge::Immutable => "🔒",
            Badge::AppendOnly => "➕",
            Badge::Signed => "✓",
            Badge::Encrypted => "🛡",
        }
    }
}

/// Eén item in een map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub modified: u64,
    pub badges: Vec<Badge>,
}

impl DirEntry {
    pub fn file(name: &str, size: u64) -> Self {
        DirEntry { name: name.to_string(), kind: FileKind::File, size, modified: 0, badges: Vec::new() }
    }
    pub fn dir(name: &str) -> Self {
        DirEntry { name: name.to_string(), kind: FileKind::Dir, size: 0, modified: 0, badges: Vec::new() }
    }
    pub fn with_badge(mut self, b: Badge) -> Self {
        if !self.badges.contains(&b) {
            self.badges.push(b);
        }
        self
    }
    pub fn modified_at(mut self, t: u64) -> Self {
        self.modified = t;
        self
    }
    /// Verborgen bestanden beginnen met een punt (Unix-conventie).
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
    /// De extensie (zonder punt), in kleine letters.
    pub fn extension(&self) -> Option<String> {
        extension(&self.name)
    }
}

/// Hoe te sorteren.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

/// Een maplijst: het pad + de items.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<DirEntry>,
}

impl Listing {
    pub fn new(path: &str, entries: Vec<DirEntry>) -> Self {
        Listing { path: normalize(path), entries }
    }

    /// Sorteer in plaats; mappen komen altijd vóór bestanden (verkennersconventie),
    /// daarna volgens de gekozen sleutel.
    pub fn sort(&mut self, key: SortKey, order: SortOrder) {
        self.entries.sort_by(|a, b| {
            // Mappen eerst.
            let dir_a = a.kind == FileKind::Dir;
            let dir_b = b.kind == FileKind::Dir;
            if dir_a != dir_b {
                return dir_b.cmp(&dir_a); // dir (true) eerst
            }
            let ord = match key {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.modified.cmp(&b.modified),
                SortKey::Kind => format_kind(a.kind).cmp(format_kind(b.kind)),
            };
            match order {
                SortOrder::Asc => ord,
                SortOrder::Desc => ord.reverse(),
            }
        });
    }

    /// Filter op een zoekterm (substring, hoofdletterongevoelig) en op het al dan
    /// niet tonen van verborgen bestanden. Geeft een nieuwe lijst van referenties.
    pub fn filter(&self, query: &str, show_hidden: bool) -> Vec<&DirEntry> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| show_hidden || !e.is_hidden())
            .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
            .collect()
    }

    /// Totale grootte van de bestanden (mappen tellen niet mee).
    pub fn total_size(&self) -> u64 {
        self.entries.iter().filter(|e| e.kind == FileKind::File).map(|e| e.size).sum()
    }

    /// Aantal mappen en bestanden.
    pub fn counts(&self) -> (usize, usize) {
        let dirs = self.entries.iter().filter(|e| e.kind == FileKind::Dir).count();
        (dirs, self.entries.len() - dirs)
    }
}

fn format_kind(k: FileKind) -> &'static str {
    match k {
        FileKind::Dir => "0dir",
        FileKind::Symlink => "1link",
        FileKind::File => "2file",
    }
}

// ── padbewerkingen ────────────────────────────────────────────────────────────

/// Normaliseer een pad: dubbele slashes weg, `.` weg, `..` lost een segment op.
/// Behoudt of het pad absoluut is (begint met `/`).
pub fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(stack.last(), Some(&s) if s != "..") {
                    stack.pop();
                } else if !absolute {
                    stack.push("..");
                }
            }
            s => stack.push(s),
        }
    }
    let joined = stack.join("/");
    if absolute {
        let mut out = String::from("/");
        out.push_str(&joined);
        out
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Voeg twee padcomponenten samen en normaliseer.
pub fn join(base: &str, child: &str) -> String {
    if child.starts_with('/') {
        return normalize(child);
    }
    let mut s = String::from(base);
    if !s.ends_with('/') {
        s.push('/');
    }
    s.push_str(child);
    normalize(&s)
}

/// De laatste padcomponent.
pub fn basename(path: &str) -> String {
    let n = normalize(path);
    match n.rsplit('/').next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => n,
    }
}

/// Het ouderpad.
pub fn parent(path: &str) -> String {
    let n = normalize(path);
    if n == "/" {
        return "/".to_string();
    }
    match n.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => n[..i].to_string(),
        None => ".".to_string(),
    }
}

/// De extensie (zonder punt), in kleine letters.
pub fn extension(name: &str) -> Option<String> {
    let base = basename(name);
    let dot = base.rfind('.')?;
    if dot == 0 || dot + 1 >= base.len() {
        return None; // ".dotfile" of "naam." → geen extensie
    }
    Some(base[dot + 1..].to_lowercase())
}

/// Mensvriendelijke grootte (binaire prefixen, 1024-basis).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return alloc::format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    // Eén decimaal, afgekapt voor determinisme.
    let scaled = (v * 10.0) as u64;
    alloc::format!("{}.{} {}", scaled / 10, scaled % 10, UNITS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        assert_eq!(normalize("/a//b/./c/"), "/a/b/c");
        assert_eq!(normalize("/a/b/../../x"), "/x");
        assert_eq!(normalize("a/b/../c"), "a/c");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("./x"), "x");
    }

    #[test]
    fn join_basename_parent() {
        assert_eq!(join("/home/user", "docs/report.txt"), "/home/user/docs/report.txt");
        assert_eq!(join("/home/user", "../root"), "/home/root");
        assert_eq!(join("/a", "/b/c"), "/b/c");
        assert_eq!(basename("/home/user/file.md"), "file.md");
        assert_eq!(parent("/home/user/file.md"), "/home/user");
        assert_eq!(parent("/file"), "/");
    }

    #[test]
    fn extensions() {
        assert_eq!(extension("report.PDF"), Some("pdf".to_string()));
        assert_eq!(extension("/a/b/photo.jpeg"), Some("jpeg".to_string()));
        assert_eq!(extension(".bashrc"), None);
        assert_eq!(extension("Makefile"), None);
        assert_eq!(extension("archive.tar.gz"), Some("gz".to_string()));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn sort_dirs_first_then_name() {
        let mut l = Listing::new(
            "/x",
            alloc::vec![
                DirEntry::file("zebra.txt", 10),
                DirEntry::dir("src"),
                DirEntry::file("apple.txt", 20),
                DirEntry::dir("Assets"),
            ],
        );
        l.sort(SortKey::Name, SortOrder::Asc);
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        // Mappen eerst (alfabetisch), dan bestanden (alfabetisch).
        assert_eq!(names, alloc::vec!["Assets", "src", "apple.txt", "zebra.txt"]);
    }

    #[test]
    fn sort_by_size_desc() {
        let mut l = Listing::new(
            "/x",
            alloc::vec![
                DirEntry::file("a", 100),
                DirEntry::file("b", 300),
                DirEntry::file("c", 200),
            ],
        );
        l.sort(SortKey::Size, SortOrder::Desc);
        let sizes: Vec<u64> = l.entries.iter().map(|e| e.size).collect();
        assert_eq!(sizes, alloc::vec![300, 200, 100]);
    }

    #[test]
    fn filter_query_and_hidden() {
        let l = Listing::new(
            "/x",
            alloc::vec![
                DirEntry::file("report.txt", 1),
                DirEntry::file(".secret", 1),
                DirEntry::file("Report-2.txt", 1),
                DirEntry::dir("reports"),
            ],
        );
        // Zoek 'report' (hoofdletterongevoelig), verberg dotfiles.
        let r = l.filter("report", false);
        assert_eq!(r.len(), 3); // report.txt, Report-2.txt, reports
        assert!(!r.iter().any(|e| e.name == ".secret"));
        // Toon verborgen + lege query → alles.
        assert_eq!(l.filter("", true).len(), 4);
    }

    #[test]
    fn badges_and_counts() {
        let l = Listing::new(
            "/etc",
            alloc::vec![
                DirEntry::file("kernel.efi", 2_500_000).with_badge(Badge::Immutable).with_badge(Badge::Signed),
                DirEntry::file("audit.log", 4096).with_badge(Badge::AppendOnly),
                DirEntry::dir("conf"),
            ],
        );
        let (dirs, files) = l.counts();
        assert_eq!((dirs, files), (1, 2));
        assert_eq!(l.total_size(), 2_500_000 + 4096);
        let kernel = &l.entries[0];
        assert!(kernel.badges.contains(&Badge::Immutable));
        assert_eq!(Badge::Immutable.glyph(), "🔒");
    }
}
