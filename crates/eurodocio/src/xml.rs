//! A minimal, in-house XML pull-parser — enough for OOXML/ODF (WordprocessingML,
//! OpenDocument). No external crate: a sovereign office suite parses its
//! own formats. Yields a stream of [`Event`]s (open/close/text), with attribute
//! parsing and the five standard entities.

use alloc::string::String;
use alloc::vec::Vec;

/// An XML event from the pull-parser.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    /// `<name attr="x">` — for a self-closing tag `<name/>` a `Close` follows immediately.
    Open { name: String, attrs: Vec<(String, String)> },
    /// `</name>`
    Close { name: String },
    /// Text content between tags (entities already decoded).
    Text(String),
}

/// Parse an XML document into a list of events. Tolerates an XML declaration,
/// comments and self-closing tags. Non-strict (no DTD/namespace resolution —
/// the prefix stays part of the name, e.g. `w:p`).
pub fn parse(input: &str) -> Vec<Event> {
    let b = input.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i] == b'<' {
            // Skip comment / declaration / processing-instruction.
            if b[i..].starts_with(b"<!--") {
                if let Some(end) = find(b, i + 4, b"-->") {
                    i = end + 3;
                    continue;
                }
                break;
            }
            // A bare `<` at the end of the input (audit M5): do not index b[i+1].
            if i + 1 >= b.len() {
                break;
            }
            if b[i + 1] == b'?' || b[i + 1] == b'!' {
                if let Some(gt) = memchr(b, i, b'>') {
                    i = gt + 1;
                    continue;
                }
                break;
            }
            // A tag.
            let gt = match memchr(b, i, b'>') {
                Some(p) => p,
                None => break,
            };
            let inner = &input[i + 1..gt];
            if let Some(name) = inner.strip_prefix('/') {
                out.push(Event::Close { name: String::from(name.trim()) });
            } else {
                let self_closing = inner.ends_with('/');
                let inner = inner.trim_end_matches('/').trim();
                let (name, attrs) = parse_tag(inner);
                out.push(Event::Open { name: name.clone(), attrs });
                if self_closing {
                    out.push(Event::Close { name });
                }
            }
            i = gt + 1;
        } else {
            // Text up to the next `<`.
            let start = i;
            while i < b.len() && b[i] != b'<' {
                i += 1;
            }
            let raw = &input[start..i];
            if !raw.trim().is_empty() {
                out.push(Event::Text(decode_entities(raw)));
            }
        }
    }
    out
}

/// Split `name attr1="x" attr2='y'` into the tag name + the attributes.
fn parse_tag(inner: &str) -> (String, Vec<(String, String)>) {
    let mut chars = inner.char_indices();
    let mut name_end = inner.len();
    for (idx, c) in chars.by_ref() {
        if c.is_whitespace() {
            name_end = idx;
            break;
        }
    }
    let name = String::from(&inner[..name_end]);
    let mut attrs = Vec::new();
    let rest = &inner[name_end..];
    let rb = rest.as_bytes();
    let mut j = 0;
    while j < rb.len() {
        // Skip whitespace.
        while j < rb.len() && rb[j].is_ascii_whitespace() {
            j += 1;
        }
        let key_start = j;
        while j < rb.len() && rb[j] != b'=' && !rb[j].is_ascii_whitespace() {
            j += 1;
        }
        if key_start == j {
            break;
        }
        let key = &rest[key_start..j];
        // Expect =."..."
        while j < rb.len() && (rb[j] == b'=' || rb[j].is_ascii_whitespace()) {
            j += 1;
        }
        if j >= rb.len() || (rb[j] != b'"' && rb[j] != b'\'') {
            break;
        }
        let quote = rb[j];
        j += 1;
        let val_start = j;
        while j < rb.len() && rb[j] != quote {
            j += 1;
        }
        let val = &rest[val_start..j.min(rest.len())];
        attrs.push((String::from(key), decode_entities(val)));
        j += 1;
    }
    (name, attrs)
}

/// Decode the five standard XML entities.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            match ent {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    if let Ok(cp) = u32::from_str_radix(&ent[2..], 16) {
                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    }
                }
                _ if ent.starts_with('#') => {
                    if let Ok(cp) = ent[1..].parse::<u32>() {
                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    }
                }
                _ => {
                    out.push('&');
                    out.push_str(ent);
                    out.push(';');
                }
            }
            rest = &tail[semi + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Encode text for XML output (the five entities).
pub fn encode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn memchr(b: &[u8], from: usize, c: u8) -> Option<usize> {
    b[from..].iter().position(|&x| x == c).map(|p| p + from)
}

fn find(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if from > b.len() {
        return None;
    }
    b[from..].windows(pat.len()).position(|w| w == pat).map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_events() {
        let ev = parse(r#"<a x="1"><b>hi</b></a>"#);
        assert_eq!(ev[0], Event::Open { name: "a".into(), attrs: alloc::vec![("x".into(), "1".into())] });
        assert_eq!(ev[1], Event::Open { name: "b".into(), attrs: alloc::vec![] });
        assert_eq!(ev[2], Event::Text("hi".into()));
        assert_eq!(ev[3], Event::Close { name: "b".into() });
        assert_eq!(ev[4], Event::Close { name: "a".into() });
    }

    #[test]
    fn self_closing_and_decl() {
        let ev = parse(r#"<?xml version="1.0"?><br/><!-- x --><img src="a"/>"#);
        assert_eq!(ev[0], Event::Open { name: "br".into(), attrs: alloc::vec![] });
        assert_eq!(ev[1], Event::Close { name: "br".into() });
        assert_eq!(ev[2], Event::Open { name: "img".into(), attrs: alloc::vec![("src".into(), "a".into())] });
        assert_eq!(ev[3], Event::Close { name: "img".into() });
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#65;"), "a & b <c> A");
        assert_eq!(encode_entities("a & b <c>"), "a &amp; b &lt;c&gt;");
        let ev = parse("<t>a &amp; b</t>");
        assert_eq!(ev[1], Event::Text("a & b".into()));
    }
}
