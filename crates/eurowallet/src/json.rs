//! A compact JSON value model + parser/serializer, enough for SD-JWT payloads
//! and disclosures. The serializer uses `, ` / `: ` separators to match the
//! IETF SD-JWT reference serialization (so disclosure digests interoperate).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&[Json]> {
        if let Json::Arr(a) = self {
            Some(a)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        if let Json::Obj(o) = self {
            o.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u");
                let n = c as u32;
                for shift in [12, 8, 4, 0] {
                    out.push(core::char::from_digit((n >> shift) & 0xf, 16).unwrap());
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Serialize with `, ` and `: ` separators (SD-JWT reference style).
pub fn serialize(v: &Json) -> String {
    let mut s = String::new();
    write_value(v, &mut s);
    s
}
fn write_value(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&n.to_string()),
        Json::Str(s) => escape(s, out),
        Json::Arr(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(e, out);
            }
            out.push(']');
        }
        Json::Obj(o) => {
            out.push('{');
            for (i, (k, val)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                escape(k, out);
                out.push_str(": ");
                write_value(val, out);
            }
            out.push('}');
        }
    }
}

/// Parse a JSON document. `None` on malformed input.
pub fn parse(s: &str) -> Option<Json> {
    let bytes = s.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i == bytes.len() {
        Some(v)
    } else {
        None
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => self.number(),
        }
    }
    fn lit(&mut self, word: &str, val: Json) -> Option<Json> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Some(val)
        } else {
            None
        }
    }
    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        // Tolerate (and truncate) a fractional/exponent part to an integer.
        let int_end = self.i;
        if self.i < self.b.len() && matches!(self.b[self.i], b'.' | b'e' | b'E') {
            self.i += 1;
            while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'+' | b'-' | b'e' | b'E' | b'.') {
                self.i += 1;
            }
        }
        core::str::from_utf8(&self.b[start..int_end]).ok()?.parse::<i64>().ok().map(Json::Num)
    }
    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'u' => {
                            let hex = core::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                            let cp = u32::from_str_radix(hex, 16).ok()?;
                            self.i += 4;
                            out.push(char::from_u32(cp)?);
                        }
                        _ => return None,
                    }
                }
                _ => {
                    // Copy a full UTF-8 sequence starting at c.
                    let len = utf8_len(c);
                    let seg = self.b.get(self.i - 1..self.i - 1 + len)?;
                    out.push_str(core::str::from_utf8(seg).ok()?);
                    self.i += len - 1;
                }
            }
        }
        None
    }
    fn array(&mut self) -> Option<Json> {
        self.i += 1; // '['
        let mut a = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Some(Json::Arr(a));
        }
        loop {
            a.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => {
                    self.i += 1;
                }
                Some(&b']') => {
                    self.i += 1;
                    return Some(Json::Arr(a));
                }
                _ => return None,
            }
        }
    }
    fn object(&mut self) -> Option<Json> {
        self.i += 1; // '{'
        let mut o = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(Json::Obj(o));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            let v = self.value()?;
            o.push((k, v));
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => {
                    self.i += 1;
                }
                Some(&b'}') => {
                    self.i += 1;
                    return Some(Json::Obj(o));
                }
                _ => return None,
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_and_serialize_roundtrip() {
        let src = "{\"iss\": \"euro-id\", \"_sd\": [\"aaa\", \"bbb\"], \"age\": 42, \"ok\": true}";
        let v = parse(src).unwrap();
        assert_eq!(v.get("iss").unwrap().as_str(), Some("euro-id"));
        assert_eq!(v.get("_sd").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(serialize(&v), src);
    }
    #[test]
    fn disclosure_array_uses_comma_space() {
        let d = Json::Arr(alloc::vec![
            Json::Str("2GLC42sKQveCfGfryNRN9w".into()),
            Json::Str("given_name".into()),
            Json::Str("John".into()),
        ]);
        assert_eq!(serialize(&d), "[\"2GLC42sKQveCfGfryNRN9w\", \"given_name\", \"John\"]");
    }
}
