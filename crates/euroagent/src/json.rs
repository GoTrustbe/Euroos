//! Minimale JSON — net genoeg voor JSON-RPC 2.0 over de MCP-socket.
//!
//! `no_std`, geen externe crate. Ondersteunt object/array/string/number/bool/null,
//! met escapes in strings. Getallen worden als `f64`-vrije `i64`/raw bewaard via
//! een tekstrepresentatie zodat we geen floating-point nodig hebben in de kernel.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een JSON-waarde.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// Numeriek, bewaard als brontekst (bv. "30000", "-1.5").
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => n.parse().ok(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// Zoek een sleutel in een object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Serialiseer naar compacte JSON-tekst.
    pub fn to_string(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => out.push_str(n),
            Json::Str(s) => write_string(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Parse JSON-tekst.
    pub fn parse(input: &str) -> Result<Json, &'static str> {
        let bytes = input.as_bytes();
        let mut p = Parser { b: bytes, i: 0, depth: 0 };
        p.skip_ws();
        let v = p.value()?;
        p.skip_ws();
        if p.i != bytes.len() {
            return Err("trailing");
        }
        Ok(v)
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    /// Geneste object/array-diepte — begrensd tegen stack-overflow (audit H4):
    /// de JSON komt van onvertrouwde MCP-/AF_UNIX-/LLM-invoer.
    depth: usize,
}

/// Maximale nesting-diepte van een JSON-document.
const MAX_DEPTH: usize = 128;

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    /// Diepte-bewaakte wrapper: elke geneste waarde verhoogt de teller.
    fn value(&mut self) -> Result<Json, &'static str> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("nesting too deep");
        }
        let r = self.value_inner();
        self.depth -= 1;
        r
    }
    fn value_inner(&mut self) -> Result<Json, &'static str> {
        self.skip_ws();
        if self.i >= self.b.len() {
            return Err("eof");
        }
        match self.b[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Json::Str(self.string()?)),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => self.number(),
        }
    }
    fn lit(&mut self, word: &str, v: Json) -> Result<Json, &'static str> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err("literal")
        }
    }
    fn number(&mut self) -> Result<Json, &'static str> {
        let start = self.i;
        while self.i < self.b.len()
            && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
        {
            self.i += 1;
        }
        if self.i == start {
            return Err("number");
        }
        let s = core::str::from_utf8(&self.b[start..self.i]).map_err(|_| "utf8")?;
        Ok(Json::Num(s.to_string()))
    }
    fn string(&mut self) -> Result<String, &'static str> {
        self.i += 1; // openende "
        let mut s = String::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            self.i += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    if self.i >= self.b.len() {
                        return Err("escape");
                    }
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'b' => s.push('\u{08}'),
                        b'f' => s.push('\u{0C}'),
                        b'u' => {
                            if self.i + 4 > self.b.len() {
                                return Err("u-escape");
                            }
                            let hex = core::str::from_utf8(&self.b[self.i..self.i + 4])
                                .map_err(|_| "u-utf8")?;
                            let cp = u32::from_str_radix(hex, 16).map_err(|_| "u-hex")?;
                            s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            self.i += 4;
                        }
                        _ => return Err("bad-escape"),
                    }
                }
                _ => {
                    // Voeg de (mogelijk multi-byte) UTF-8 byte direct toe.
                    s.push(c as char);
                }
            }
        }
        Err("unterminated")
    }
    fn array(&mut self) -> Result<Json, &'static str> {
        self.i += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == b']' {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.b.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err("array-sep"),
            }
        }
    }
    fn object(&mut self) -> Result<Json, &'static str> {
        self.i += 1; // {
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err("obj-key");
            }
            let key = self.string()?;
            self.skip_ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err("obj-colon");
            }
            self.i += 1;
            let val = self.value()?;
            pairs.push((key, val));
            self.skip_ws();
            match self.b.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(pairs));
                }
                _ => return Err("obj-sep"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_object() {
        let src = r#"{"a":1,"b":"x","c":[true,null,2],"d":{"e":false}}"#;
        let v = Json::parse(src).unwrap();
        assert_eq!(v.get("a").unwrap().as_i64(), Some(1));
        assert_eq!(v.get("b").unwrap().as_str(), Some("x"));
        assert_eq!(v.get("d").unwrap().get("e").unwrap().as_bool(), Some(false));
        // Compacte re-serialisatie is stabiel.
        assert_eq!(v.to_string(), src);
    }

    #[test]
    fn string_escapes() {
        let v = Json::parse(r#""a\nb\t\"c\"""#).unwrap();
        assert_eq!(v.as_str(), Some("a\nb\t\"c\""));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Json::parse("{bad}").is_err());
        assert!(Json::parse("[1,2").is_err());
        assert!(Json::parse("nul").is_err());
    }
}
