//! EuroMail — the e-mail core of EuroOS (Sprint AC-4).
//!
//! The pure, host-tested parser core of the mail client: **RFC822 headers**
//! (with unfolding), **address lists** (`Name <address>`), **MIME multipart**
//! parsing, and the decoders **base64**, **quoted-printable** and **RFC2047**
//! encoded-words (`=?utf-8?B?...?=`) for headers. The IMAP/SMTP transport layer
//! runs later on [`euronet`]/[`eurotls`]; this crate does not touch the network.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ── decoders ─────────────────────────────────────────────────────────────────

/// Decode base64 (ignores whitespace; tolerant of missing padding).
pub fn base64_decode(input: &str) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        if let Some(v) = val(c) {
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
    }
    out
}

/// Decode quoted-printable. `underscore_as_space` for the RFC2047 "Q" variant.
pub fn quoted_printable_decode(input: &str, underscore_as_space: bool) -> Vec<u8> {
    let b = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'=' => {
                // Soft line break "=\r\n" or "=\n".
                if i + 1 < b.len() && (b[i + 1] == b'\n' || b[i + 1] == b'\r') {
                    i += 1;
                    while i < b.len() && (b[i] == b'\n' || b[i] == b'\r') {
                        i += 1;
                    }
                    continue;
                }
                if i + 2 < b.len() {
                    if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                        out.push(h * 16 + l);
                        i += 3;
                        continue;
                    }
                }
                out.push(b'=');
                i += 1;
            }
            b'_' if underscore_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decode RFC2047 encoded-words in a header value to UTF-8 text.
pub fn decode_header(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    loop {
        let start = match rest.find("=?") {
            Some(s) => s,
            None => {
                out.push_str(rest);
                break;
            }
        };
        out.push_str(&rest[..start]);
        // Form: =?charset?enc?text?=  (text itself contains no '?').
        let body = &rest[start + 2..];
        let mut marks = body.match_indices('?');
        let p1 = marks.next(); // after charset
        let p2 = marks.next(); // after enc
        if let (Some((i1, _)), Some((i2, _))) = (p1, p2) {
            let enc = &body[i1 + 1..i2];
            let after_enc = &body[i2 + 1..];
            if let Some(term) = after_enc.find("?=") {
                let text = &after_enc[..term];
                let bytes = match enc {
                    "B" | "b" => base64_decode(text),
                    "Q" | "q" => quoted_printable_decode(text, true),
                    _ => text.as_bytes().to_vec(),
                };
                out.push_str(&String::from_utf8_lossy(&bytes));
                rest = &after_enc[term + 2..];
                continue;
            }
        }
        // No valid encoded-word → literal "=?".
        out.push_str("=?");
        rest = body;
    }
    out
}

// ── headers ──────────────────────────────────────────────────────────────────

/// A parsed address (`Name <address>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub email: String,
}

/// Split raw message text into (headers, body) at the first empty line.
fn split_headers(raw: &str) -> (&str, &str) {
    if let Some(p) = raw.find("\r\n\r\n") {
        (&raw[..p], &raw[p + 4..])
    } else if let Some(p) = raw.find("\n\n") {
        (&raw[..p], &raw[p + 2..])
    } else {
        (raw, "")
    }
}

/// Parse headers with unfolding (continuation lines start with space/tab).
pub fn parse_headers(head: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in head.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some(colon) = line.find(':') {
            out.push((line[..colon].trim().to_string(), line[colon + 1..].trim().to_string()));
        }
    }
    out
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Parse an address list (`A <a@x>, B <b@y>`), respecting quotes and `<>`.
pub fn parse_addresses(value: &str) -> Vec<Address> {
    let mut out = Vec::new();
    let mut depth_angle = 0;
    let mut in_quote = false;
    let mut cur = String::new();
    for c in value.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '<' if !in_quote => {
                depth_angle += 1;
                cur.push(c);
            }
            '>' if !in_quote => {
                depth_angle -= 1;
                cur.push(c);
            }
            ',' if !in_quote && depth_angle == 0 => {
                if let Some(a) = parse_one_address(&cur) {
                    out.push(a);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if let Some(a) = parse_one_address(&cur) {
        out.push(a);
    }
    out
}

fn parse_one_address(s: &str) -> Option<Address> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let (Some(lt), Some(gt)) = (s.find('<'), s.find('>')) {
        if lt < gt {
            let name = decode_header(s[..lt].trim().trim_matches('"').trim());
            let email = s[lt + 1..gt].trim().to_string();
            return Some(Address { name, email });
        }
    }
    Some(Address { name: String::new(), email: s.to_string() })
}

// ── MIME ─────────────────────────────────────────────────────────────────────

/// An attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// A parsed e-mail message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Email {
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub subject: String,
    pub date: String,
    pub text: String,
    pub html: String,
    pub attachments: Vec<Attachment>,
}

fn param(value: &str, key: &str) -> Option<String> {
    // e.g. `multipart/mixed; boundary="abc"` → param "boundary" = abc
    for seg in value.split(';').skip(1) {
        let seg = seg.trim();
        if let Some(eq) = seg.find('=') {
            if seg[..eq].trim().eq_ignore_ascii_case(key) {
                return Some(seg[eq + 1..].trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn mime_type(value: &str) -> String {
    value.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn decode_body(body: &str, encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => base64_decode(body),
        "quoted-printable" => quoted_printable_decode(body, false),
        _ => body.as_bytes().to_vec(),
    }
}

/// Process one MIME part (recursive for multipart), up to depth `depth`.
fn walk_part(raw: &str, email: &mut Email, depth: u32) {
    if depth > 16 {
        return;
    }
    let (head, body) = split_headers(raw);
    let headers = parse_headers(head);
    let ctype = header(&headers, "Content-Type").unwrap_or("text/plain");
    let mtype = mime_type(ctype);
    let cte = header(&headers, "Content-Transfer-Encoding").unwrap_or("7bit");
    let disposition = header(&headers, "Content-Disposition").unwrap_or("");

    if mtype.starts_with("multipart/") {
        if let Some(boundary) = param(ctype, "boundary") {
            let delim = alloc::format!("--{boundary}");
            let mut first = true;
            for seg in body.split(delim.as_str()) {
                if first {
                    first = false;
                    continue; // preamble before the first boundary
                }
                let seg = seg.trim_start_matches(['\r', '\n']);
                if seg.starts_with("--") {
                    break; // closing boundary
                }
                walk_part(seg, email, depth + 1);
            }
        }
        return;
    }

    // Leaf part.
    let filename = param(ctype, "name").or_else(|| param(disposition, "filename"));
    if let Some(fname) = filename {
        email.attachments.push(Attachment {
            filename: decode_header(&fname),
            content_type: mtype,
            data: decode_body(body, cte),
        });
    } else if mtype == "text/html" {
        email.html = String::from_utf8_lossy(&decode_body(body, cte)).into_owned();
    } else {
        // text/plain (or unknown) → text.
        let t = String::from_utf8_lossy(&decode_body(body, cte)).into_owned();
        if email.text.is_empty() {
            email.text = t;
        }
    }
}

/// Parse a complete RFC822/MIME message.
pub fn parse(raw: &str) -> Email {
    let (head, _) = split_headers(raw);
    let headers = parse_headers(head);
    let mut email = Email {
        from: header(&headers, "From").map(parse_addresses).unwrap_or_default(),
        to: header(&headers, "To").map(parse_addresses).unwrap_or_default(),
        subject: header(&headers, "Subject").map(decode_header).unwrap_or_default(),
        date: header(&headers, "Date").unwrap_or("").to_string(),
        ..Default::default()
    };
    walk_part(raw, &mut email, 0);
    email
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_known() {
        assert_eq!(base64_decode("aGVsbG8="), b"hello");
        assert_eq!(base64_decode("RXVyb09T"), b"EuroOS");
        // whitespace ignored
        assert_eq!(base64_decode("aGVs\r\nbG8="), b"hello");
    }

    #[test]
    fn quoted_printable_decode_basics() {
        assert_eq!(quoted_printable_decode("a=3Db", false), b"a=b");
        assert_eq!(quoted_printable_decode("caf=C3=A9", false), "café".as_bytes());
        // soft line break
        assert_eq!(quoted_printable_decode("one=\r\ntwo", false), b"onetwo");
        // Q variant: _ = space
        assert_eq!(quoted_printable_decode("a_b", true), b"a b");
    }

    #[test]
    fn rfc2047_header_decode() {
        assert_eq!(decode_header("=?utf-8?B?RXVyb09T?="), "EuroOS");
        assert_eq!(decode_header("=?utf-8?Q?caf=C3=A9?="), "café");
        assert_eq!(decode_header("plain text"), "plain text");
        assert_eq!(decode_header("Pre =?utf-8?B?RXVybw==?= post"), "Pre Euro post");
    }

    #[test]
    fn headers_unfold() {
        let h = "Subject: long\r\n line\r\nFrom: a@b\r\n";
        let parsed = parse_headers(h);
        assert_eq!(parsed[0], ("Subject".into(), "long line".into()));
        assert_eq!(parsed[1], ("From".into(), "a@b".into()));
    }

    #[test]
    fn address_list_parsing() {
        let a = parse_addresses("Jan Vandenberg <jan@euro-os.eu>, anna@x.be");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0], Address { name: "Jan Vandenberg".into(), email: "jan@euro-os.eu".into() });
        assert_eq!(a[1].email, "anna@x.be");
        // comma inside quotes/angles does not split
        let b = parse_addresses("\"Company, Ltd\" <info@company.be>");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].email, "info@company.be");
    }

    #[test]
    fn parse_simple_message() {
        let raw = "From: Jan <jan@euro-os.eu>\r\nTo: anna@x.be\r\nSubject: =?utf-8?B?SGFsbG8=?=\r\nContent-Type: text/plain\r\n\r\nThis is the text.\r\n";
        let e = parse(raw);
        assert_eq!(e.from[0].email, "jan@euro-os.eu");
        assert_eq!(e.to[0].email, "anna@x.be");
        assert_eq!(e.subject, "Hallo");
        assert!(e.text.contains("This is the text."));
    }

    #[test]
    fn parse_multipart_with_attachment() {
        let raw = "From: a@b\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n\
            --XX\r\nContent-Type: text/plain\r\n\r\nHello world\r\n\
            --XX\r\nContent-Type: text/html\r\n\r\n<p>Hello</p>\r\n\
            --XX\r\nContent-Type: application/octet-stream; name=\"data.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nRXVyb09T\r\n\
            --XX--\r\n";
        let e = parse(raw);
        assert!(e.text.contains("Hello world"));
        assert!(e.html.contains("<p>Hello</p>"));
        assert_eq!(e.attachments.len(), 1);
        assert_eq!(e.attachments[0].filename, "data.bin");
        assert_eq!(e.attachments[0].data, b"EuroOS");
    }
}
