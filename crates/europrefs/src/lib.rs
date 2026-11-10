//! **EuroPrefs** — the settings-file format of EuroOS.
//!
//! One file per scope and app, plain `key=value` lines:
//!
//! ```text
//! # EuroOS settings
//! time.zone=Europe/Brussels
//! clock.24h=true
//! ```
//!
//! The kernel side (`kernel/src/prefs.rs`) decides WHERE a file lives
//! (`/etc/euroos/<app>.conf`, `/home/<user>/.config/euroos/<app>.conf`) and
//! writes it atomically; this crate is the format: parse, typed access with
//! defaults, serialize. It is host-tested and has no dependencies.
//!
//! Rules of the format:
//! - A line is `key=value`, a comment (`#` or `;` first), or blank.
//! - Keys: 1..=64 chars of `a-z A-Z 0-9 . _ -`. Values: the rest of the line,
//!   trimmed, at most 4096 bytes.
//! - Escapes in values: `\n` newline, `\r` carriage return, `\t` tab, `\\`
//!   backslash, `\s` a space that must survive trimming. An unknown escape is
//!   kept as written.
//! - A later line with the same key replaces the earlier one.
//! - Anything else is a broken line: it is skipped and counted
//!   ([`Conf::rejected`]), never an error. Parsing cannot fail and cannot panic.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub mod tz;
pub mod validate;

/// Input beyond this many bytes is ignored (counted as one rejected line).
pub const MAX_BYTES: usize = 64 * 1024;
/// At most this many `key=value` entries per file.
pub const MAX_ENTRIES: usize = 512;
/// Longest key, in bytes.
pub const MAX_KEY: usize = 64;
/// Longest value, in bytes (after unescaping).
pub const MAX_VALUE: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// A comment or blank line, kept verbatim so a hand-edited file keeps its notes.
    Comment(String),
    Pair(String, String),
}

/// A parsed settings file. Order and comments are preserved on a round trip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conf {
    lines: Vec<Line>,
    rejected: usize,
}

/// Is `key` a valid settings key?
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// Trim what an editor may leave around a line, key or value: spaces, tabs and
/// the CR of a CRLF file. Deliberately not Unicode whitespace, so a value keeps
/// e.g. a leading no-break space.
fn trim_ascii(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r')
}

fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let last = v.chars().count().saturating_sub(1);
    for (i, c) in v.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Only the outer spaces need protecting from the trim on parse.
            ' ' if i == 0 || i == last => out.push_str("\\s"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('s') => out.push(' '),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            // Unknown escape or a lone trailing backslash: keep what was written.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

impl Conf {
    /// An empty file.
    pub const fn new() -> Self {
        Conf { lines: Vec::new(), rejected: 0 }
    }

    /// An empty file that starts with `# <header>` comment lines.
    pub fn with_header(header: &str) -> Self {
        let mut c = Conf::new();
        for l in header.lines() {
            c.lines.push(Line::Comment(alloc::format!("# {}", l.trim())));
        }
        c
    }

    /// Parse a settings file. Never fails: broken lines are skipped and counted.
    pub fn parse(text: &str) -> Self {
        let mut c = Conf::new();
        let mut text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if text.len() > MAX_BYTES {
            let mut cut = MAX_BYTES;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            // Drop the (probably cut-off) last line as well.
            cut = text[..cut].rfind('\n').map(|i| i + 1).unwrap_or(0);
            text = &text[..cut];
            c.rejected += 1;
        }
        let mut pairs = 0usize;
        for raw in text.split('\n') {
            let line = trim_ascii(raw);
            if line.is_empty() {
                c.lines.push(Line::Comment(String::new()));
                continue;
            }
            if line.starts_with('#') || line.starts_with(';') {
                c.lines.push(Line::Comment(String::from(line)));
                continue;
            }
            let Some(eq) = line.find('=') else {
                c.rejected += 1;
                continue;
            };
            let key = trim_ascii(&line[..eq]);
            let value = unescape(trim_ascii(&line[eq + 1..]));
            if !valid_key(key) || value.len() > MAX_VALUE {
                c.rejected += 1;
                continue;
            }
            if let Some(slot) = c.slot_mut(key) {
                *slot = value; // last one wins
                continue;
            }
            if pairs >= MAX_ENTRIES {
                c.rejected += 1;
                continue;
            }
            pairs += 1;
            c.lines.push(Line::Pair(String::from(key), value));
        }
        // `split` yields one empty piece after a final newline: not a blank line.
        if matches!(c.lines.last(), Some(Line::Comment(s)) if s.is_empty()) {
            c.lines.pop();
        }
        c
    }

    /// Parse raw file bytes. Invalid UTF-8 is replaced, so a damaged file still
    /// yields every line that is intact.
    pub fn parse_bytes(bytes: &[u8]) -> Self {
        let over = bytes.len() > MAX_BYTES;
        let bytes = if over { &bytes[..MAX_BYTES] } else { bytes };
        let mut c = Conf::parse(&String::from_utf8_lossy(bytes));
        if over && c.rejected == 0 {
            c.rejected = 1;
        }
        c
    }

    /// The file as text: comments and entries in their original order.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            match l {
                Line::Comment(s) => out.push_str(s),
                Line::Pair(k, v) => {
                    out.push_str(k);
                    out.push('=');
                    out.push_str(&escape(v));
                }
            }
            out.push('\n');
        }
        out
    }

    /// How many broken lines the parser skipped.
    pub fn rejected(&self) -> usize {
        self.rejected
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.lines.iter().filter(|l| matches!(l, Line::Pair(..))).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// All keys, in file order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().filter_map(|l| match l {
            Line::Pair(k, _) => Some(k.as_str()),
            _ => None,
        })
    }

    fn slot_mut(&mut self, key: &str) -> Option<&mut String> {
        self.lines.iter_mut().find_map(|l| match l {
            Line::Pair(k, v) if k == key => Some(v),
            _ => None,
        })
    }

    // ── Typed getters: a missing or unreadable value yields the default ──────

    /// The raw value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            Line::Pair(k, v) if k == key => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn get_str(&self, key: &str, default: &str) -> String {
        String::from(self.get(key).unwrap_or(default))
    }

    /// `true/false`, `yes/no`, `on/off`, `1/0` (any case); anything else → default.
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some(v) => {
                let v = v.trim();
                let is = |x: &str| v.eq_ignore_ascii_case(x);
                if is("true") || is("yes") || is("on") || is("1") {
                    true
                } else if is("false") || is("no") || is("off") || is("0") {
                    false
                } else {
                    default
                }
            }
            None => default,
        }
    }

    pub fn get_i64(&self, key: &str, default: i64) -> i64 {
        self.get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
    }

    /// An integer limited to `min..=max`; out of range or unreadable → default.
    pub fn get_i64_in(&self, key: &str, default: i64, min: i64, max: i64) -> i64 {
        match self.get(key).and_then(|v| v.trim().parse::<i64>().ok()) {
            Some(v) if v >= min && v <= max => v,
            _ => default,
        }
    }

    pub fn get_u64(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
    }

    /// The index of the stored value in `options` (exact match, ASCII case
    /// ignored); a value that is not an option → `default`.
    pub fn get_choice(&self, key: &str, options: &[&str], default: usize) -> usize {
        self.get(key)
            .and_then(|v| options.iter().position(|o| o.eq_ignore_ascii_case(v.trim())))
            .unwrap_or(default)
    }

    /// A comma-separated list; items are trimmed, empty ones dropped.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(v) => v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
            None => Vec::new(),
        }
    }

    // ── Setters: `false` means the key or value was refused, nothing changed ─

    pub fn set(&mut self, key: &str, value: &str) -> bool {
        if !valid_key(key) || value.len() > MAX_VALUE {
            return false;
        }
        if let Some(slot) = self.slot_mut(key) {
            *slot = String::from(value);
            return true;
        }
        if self.len() >= MAX_ENTRIES {
            return false;
        }
        self.lines.push(Line::Pair(String::from(key), String::from(value)));
        true
    }

    pub fn set_bool(&mut self, key: &str, value: bool) -> bool {
        self.set(key, if value { "true" } else { "false" })
    }

    pub fn set_i64(&mut self, key: &str, value: i64) -> bool {
        self.set(key, &alloc::format!("{value}"))
    }

    pub fn set_u64(&mut self, key: &str, value: u64) -> bool {
        self.set(key, &alloc::format!("{value}"))
    }

    /// Store a list. Items may not be empty or contain a comma or a line break;
    /// such a list is refused as a whole.
    pub fn set_list<S: AsRef<str>>(&mut self, key: &str, items: &[S]) -> bool {
        if items.iter().any(|s| s.as_ref().contains([',', '\n', '\r']) || s.as_ref().trim().is_empty()) {
            return false;
        }
        let mut v = String::new();
        for (i, s) in items.iter().enumerate() {
            if i > 0 {
                v.push(',');
            }
            v.push_str(s.as_ref().trim());
        }
        self.set(key, &v)
    }

    /// Remove an entry; `true` if it existed.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.lines.len();
        self.lines.retain(|l| !matches!(l, Line::Pair(k, _) if k == key));
        self.lines.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_order_comments_and_values() {
        let src = "# EuroOS settings\n\ntime.zone=Europe/Brussels\nclock.24h=true\n; note\nui.text_scale=110\n";
        let c = Conf::parse(src);
        assert_eq!(c.rejected(), 0);
        assert_eq!(c.len(), 3);
        assert_eq!(c.to_text(), src);
        assert_eq!(Conf::parse(&c.to_text()), c);
    }

    #[test]
    fn typed_getters_fall_back_to_defaults() {
        let c = Conf::parse("a=yes\nb=OFF\nc=maybe\nn=42\nneg=-7\nbad=4x\nbig=999\n");
        assert!(c.get_bool("a", false));
        assert!(!c.get_bool("b", true));
        assert!(c.get_bool("c", true) && !c.get_bool("c", false));
        assert!(c.get_bool("missing", true));
        assert_eq!(c.get_i64("n", 0), 42);
        assert_eq!(c.get_i64("neg", 0), -7);
        assert_eq!(c.get_i64("bad", 5), 5);
        assert_eq!(c.get_u64("neg", 9), 9);
        assert_eq!(c.get_i64_in("big", 100, 80, 140), 100);
        assert_eq!(c.get_i64_in("n", 100, 0, 50), 42);
        assert_eq!(c.get_str("missing", "dflt"), "dflt");
        assert_eq!(c.get_choice("a", &["no", "YES"], 0), 1);
        assert_eq!(c.get_choice("c", &["no", "yes"], 0), 0);
    }

    #[test]
    fn broken_lines_are_skipped_not_fatal() {
        let src = "good=1\nno equals sign here\n=novalue\nbad key=2\nkey with spaces = 3\n\u{1F600}=4\nalso.good = spaced value \r\n====\n";
        let c = Conf::parse(src);
        assert_eq!(c.get("good"), Some("1"));
        assert_eq!(c.get("also.good"), Some("spaced value"));
        assert_eq!(c.len(), 2);
        assert_eq!(c.rejected(), 6);
        // What is written back is clean.
        let again = Conf::parse(&c.to_text());
        assert_eq!(again.rejected(), 0);
        assert_eq!(again.len(), 2);
    }

    #[test]
    fn last_duplicate_wins_and_keeps_first_position() {
        let c = Conf::parse("a=1\nb=2\na=3\n");
        assert_eq!(c.get("a"), Some("3"));
        assert_eq!(c.to_text(), "a=3\nb=2\n");
    }

    #[test]
    fn escapes_round_trip() {
        let mut c = Conf::new();
        for v in ["two\nlines", " lead and trail ", "back\\slash", "\\n literal", "tab\tinside", "\ttab outside\t", "", " ", "  ", "a=b=c", "ünïcödé €", "\\", "\\s", "\u{a0}nbsp\u{a0}"] {
            assert!(c.set("k", v));
            let back = Conf::parse(&c.to_text());
            assert_eq!(back.get("k"), Some(v), "value {v:?} via {:?}", c.to_text());
            assert_eq!(back.rejected(), 0);
        }
        // Unknown escapes and a trailing backslash survive as written.
        let c = Conf::parse("p=C:\\dir\\x\nq=end\\\n");
        assert_eq!(c.get("p"), Some("C:\\dir\\x"));
        assert_eq!(c.get("q"), Some("end\\"));
    }

    #[test]
    fn setters_validate() {
        let mut c = Conf::new();
        assert!(!c.set("", "x"));
        assert!(!c.set("has space", "x"));
        assert!(!c.set("a/b", "x"));
        assert!(!c.set(&"k".repeat(MAX_KEY + 1), "x"));
        assert!(!c.set("k", &"v".repeat(MAX_VALUE + 1)));
        assert!(c.is_empty());
        assert!(c.set_bool("flag", true) && c.set_i64("n", -3) && c.set_u64("u", 7));
        assert_eq!(c.to_text(), "flag=true\nn=-3\nu=7\n");
        assert!(c.remove("n") && !c.remove("n"));
        assert_eq!(c.keys().collect::<Vec<_>>(), ["flag", "u"]);
    }

    #[test]
    fn lists() {
        let mut c = Conf::new();
        assert!(c.set_list("d", &["ads.example.com", " tracker.example "]));
        assert_eq!(c.get_list("d"), ["ads.example.com", "tracker.example"]);
        assert!(!c.set_list("d", &["a,b"]));
        assert!(!c.set_list("d", &["ok", ""]));
        assert_eq!(c.get_list("d").len(), 2);
        assert!(c.set_list::<&str>("d", &[]));
        assert!(c.get_list("d").is_empty());
        assert_eq!(Conf::parse("l= a, ,b ,,\n").get_list("l"), ["a", "b"]);
    }

    #[test]
    fn limits_hold() {
        let mut src = String::new();
        for i in 0..MAX_ENTRIES + 10 {
            src.push_str(&format!("k{i}=v\n"));
        }
        let c = Conf::parse(&src);
        assert_eq!(c.len(), MAX_ENTRIES);
        assert_eq!(c.rejected(), 10);
        // Oversized input: the tail is ignored, the head still parses.
        let huge = format!("first=1\n{}", "x=y\n".repeat(MAX_BYTES));
        let c = Conf::parse(&huge);
        assert_eq!(c.get("first"), Some("1"));
        assert!(c.rejected() >= 1);
        let c = Conf::parse_bytes(huge.as_bytes());
        assert_eq!(c.get("first"), Some("1"));
        assert!(c.rejected() >= 1);
        let mut full = Conf::parse(&src);
        assert!(!full.set("one.more", "x"));
        assert!(full.set("k0", "changed"));
    }

    #[test]
    fn header_and_bom_and_crlf() {
        let mut c = Conf::with_header("EuroOS settings\nedit with care");
        c.set("a", "1");
        assert_eq!(c.to_text(), "# EuroOS settings\n# edit with care\na=1\n");
        let c = Conf::parse("\u{feff}a=1\r\nb=2\r\n");
        assert_eq!((c.get("a"), c.get("b"), c.rejected()), (Some("1"), Some("2"), 0));
    }

    #[test]
    fn garbage_never_panics() {
        // A small xorshift keeps the test deterministic without a dependency.
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for round in 0..400 {
            let len = (next() % 600) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    // Bias toward the bytes the format cares about.
                    match r % 8 {
                        0 => b'=',
                        1 => b'\n',
                        2 => b'\\',
                        3 => b'#',
                        _ => (r >> 8) as u8,
                    }
                })
                .collect();
            let c = Conf::parse_bytes(&bytes);
            let text = c.to_text();
            let again = Conf::parse(&text);
            assert_eq!(again.rejected(), 0, "round {round}: serialized form must be clean");
            assert_eq!(again.len(), c.len());
            for k in c.keys() {
                assert_eq!(again.get(k), c.get(k), "round {round} key {k:?}");
            }
        }
        assert_eq!(Conf::parse_bytes(&[0xFF, 0xFE, 0x00, b'=', 0x80]).len(), 0);
        assert!(Conf::parse("").is_empty());
    }
}
