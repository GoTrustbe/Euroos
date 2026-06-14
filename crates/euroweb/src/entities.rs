//! Character-reference decoder (`&amp;`, `&#169;`, `&#x20AC;`, `&eacute;`, ...).
//!
//! Covers the XML basics, commonly used Latin-1/typographic entities and numeric
//! references (decimal + hex), including the Windows-1252 C1 corrections that the
//! HTML spec prescribes. No ICU, no table generator — a fixed, readable set.

use alloc::string::{String, ToString};

/// Named entities with semicolon (the canonical form). Key = name without `&`, with `;`.
static NAMED: &[(&str, &str)] = &[
    ("amp;", "&"), ("lt;", "<"), ("gt;", ">"), ("quot;", "\""), ("apos;", "'"),
    ("nbsp;", "\u{A0}"), ("copy;", "©"), ("reg;", "®"), ("trade;", "™"),
    ("hellip;", "…"), ("mdash;", "—"), ("ndash;", "–"), ("lsquo;", "\u{2018}"),
    ("rsquo;", "\u{2019}"), ("ldquo;", "\u{201C}"), ("rdquo;", "\u{201D}"),
    ("laquo;", "«"), ("raquo;", "»"), ("euro;", "€"), ("pound;", "£"),
    ("cent;", "¢"), ("yen;", "¥"), ("sect;", "§"), ("para;", "¶"),
    ("middot;", "·"), ("bull;", "•"), ("dagger;", "†"), ("Dagger;", "‡"),
    ("deg;", "°"), ("plusmn;", "±"), ("times;", "×"), ("divide;", "÷"),
    ("frac12;", "½"), ("frac14;", "¼"), ("frac34;", "¾"), ("micro;", "µ"),
    ("aacute;", "á"), ("eacute;", "é"), ("iacute;", "í"), ("oacute;", "ó"),
    ("uacute;", "ú"), ("agrave;", "à"), ("egrave;", "è"), ("ugrave;", "ù"),
    ("acirc;", "â"), ("ecirc;", "ê"), ("ocirc;", "ô"), ("auml;", "ä"),
    ("euml;", "ë"), ("iuml;", "ï"), ("ouml;", "ö"), ("uuml;", "ü"),
    ("ccedil;", "ç"), ("ntilde;", "ñ"), ("szlig;", "ß"), ("aring;", "å"),
    ("oslash;", "ø"), ("aelig;", "æ"), ("Auml;", "Ä"), ("Ouml;", "Ö"),
    ("Uuml;", "Ü"), ("Eacute;", "É"), ("AElig;", "Æ"),
    ("larr;", "←"), ("rarr;", "→"), ("uarr;", "↑"), ("darr;", "↓"),
    ("harr;", "↔"), ("hearts;", "♥"), ("spades;", "♠"), ("clubs;", "♣"),
    ("diams;", "♦"), ("check;", "✓"), ("cross;", "✗"), ("star;", "★"),
];

/// Legacy entities without semicolon (HTML allows these in text; in attributes
/// extra conditions apply).
static LEGACY: &[(&str, &str)] = &[
    ("amp", "&"), ("lt", "<"), ("gt", ">"), ("quot", "\""),
    ("nbsp", "\u{A0}"), ("copy", "©"), ("reg", "®"),
];

/// Windows-1252 C1 corrections for numeric references 0x80–0x9F (HTML spec).
fn c1_override(n: u32) -> Option<char> {
    let ch = match n {
        0x80 => '€', 0x82 => '\u{201A}', 0x83 => 'ƒ', 0x84 => '\u{201E}',
        0x85 => '…', 0x86 => '†', 0x87 => '‡', 0x88 => 'ˆ', 0x89 => '‰',
        0x8A => 'Š', 0x8B => '\u{2039}', 0x8C => 'Œ', 0x8E => 'Ž',
        0x91 => '\u{2018}', 0x92 => '\u{2019}', 0x93 => '\u{201C}', 0x94 => '\u{201D}',
        0x95 => '•', 0x96 => '–', 0x97 => '—', 0x98 => '˜', 0x99 => '™',
        0x9A => 'š', 0x9B => '\u{203A}', 0x9C => 'œ', 0x9E => 'ž', 0x9F => 'Ÿ',
        _ => return None,
    };
    Some(ch)
}

fn codepoint_to_string(n: u32) -> String {
    if let Some(ch) = c1_override(n) {
        return ch.to_string();
    }
    // Null, out-of-range and surrogates → U+FFFD (REPLACEMENT CHARACTER).
    let ch = if n == 0 || n > 0x10_FFFF || (0xD800..=0xDFFF).contains(&n) {
        '\u{FFFD}'
    } else {
        char::from_u32(n).unwrap_or('\u{FFFD}')
    };
    ch.to_string()
}

/// Decode a character reference that starts at index `start` (right after the `&`).
///
/// Returns `(decoded_text, new_position)`. On an invalid reference the
/// literal `&` is returned and `start` is unchanged, so that the tokenizer emits the
/// `&` as ordinary text and continues.
pub fn decode_entity(chars: &[char], start: usize, in_attr: bool) -> (String, usize) {
    if start >= chars.len() {
        return ("&".to_string(), start);
    }

    // Numeric reference: &#123; or &#x1F600;
    if chars[start] == '#' {
        let mut i = start + 1;
        let hex = i < chars.len() && (chars[i] == 'x' || chars[i] == 'X');
        if hex {
            i += 1;
        }
        let digits_start = i;
        let mut value: u32 = 0;
        let mut overflow = false;
        while i < chars.len() {
            let d = if hex {
                chars[i].to_digit(16)
            } else {
                chars[i].to_digit(10)
            };
            match d {
                Some(v) => {
                    value = value.saturating_mul(if hex { 16 } else { 10 }).saturating_add(v);
                    if value > 0x10_FFFF {
                        overflow = true;
                    }
                    i += 1;
                }
                None => break,
            }
        }
        if i == digits_start {
            // No digits → invalid.
            return ("&".to_string(), start);
        }
        // Optional trailing semicolon.
        if i < chars.len() && chars[i] == ';' {
            i += 1;
        }
        let n = if overflow { 0x11_0000 } else { value };
        return (codepoint_to_string(n), i);
    }

    // Named reference: read the alphanumeric name (max 32).
    let mut name = String::new();
    let mut i = start;
    while i < chars.len() && chars[i].is_ascii_alphanumeric() && name.len() < 32 {
        name.push(chars[i]);
        i += 1;
    }
    if name.is_empty() {
        return ("&".to_string(), start);
    }

    // 1) Semicolon-terminated form.
    if i < chars.len() && chars[i] == ';' {
        let key_len = name.len() + 1;
        for (k, v) in NAMED {
            if k.len() == key_len && k[..k.len() - 1] == name && k.ends_with(';') {
                return (v.to_string(), i + 1);
            }
        }
    }

    // 2) Legacy without semicolon: longest prefix that matches.
    let name_chars: alloc::vec::Vec<char> = name.chars().collect();
    for len in (1..=name_chars.len()).rev() {
        let prefix: String = name_chars[..len].iter().collect();
        if let Some((_, v)) = LEGACY.iter().find(|(k, _)| *k == prefix) {
            // Attribute exception: if the next char is '=' or alphanumeric,
            // then this is not a reference (historical compat rule).
            if in_attr {
                let next = start + len;
                if next < chars.len() && (chars[next] == '=' || chars[next].is_ascii_alphanumeric()) {
                    break;
                }
            }
            return (v.to_string(), start + len);
        }
    }

    ("&".to_string(), start)
}
