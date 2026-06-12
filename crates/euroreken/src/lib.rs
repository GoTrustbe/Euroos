//! EuroReken — de rekenmachine van EuroOS (Sprint AC-1).
//!
//! Eén expressie-evaluator met drie gezichten:
//! - **Standaard**: `+ - * / % ^`, haakjes, decimale getallen.
//! - **Wetenschappelijk**: idem + functies (`sin cos tan sqrt ln log exp abs
//!   floor round`) en constanten (`pi`, `e`), op de soevereine [`math`]-kern
//!   (geen `libm`).
//! - **Programmeur**: gehele getallen in `0x`/`0o`/`0b`/decimaal, bitwise
//!   `& | xor ~ << >>`, en uitvoer in hex/oct/bin/dec.
//!
//! Plus [`convert`] voor eenhedenconversie (lengte/massa/temperatuur/data).
//! Pure `no_std`-logica, host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod math;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// De modus bepaalt vooral de invoer-/uitvoervorm; de evaluator is gedeeld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Standard,
    Scientific,
    Programmer,
}

/// Fouten bij het evalueren van een expressie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalcError {
    Syntax(String),
    UnknownIdent(String),
    DivByZero,
    BadNumber(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret, // machtsverheffing
    Amp,
    Pipe,
    Tilde,
    Shl,
    Shr,
    LParen,
    RParen,
    Comma,
}

fn lex(input: &str) -> Result<Vec<Tok>, CalcError> {
    let chars: Vec<char> = input.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '+' => {
                toks.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                toks.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                // '**' = machtsverheffing alias.
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    toks.push(Tok::Caret);
                    i += 2;
                } else {
                    toks.push(Tok::Star);
                    i += 1;
                }
            }
            '/' => {
                toks.push(Tok::Slash);
                i += 1;
            }
            '%' => {
                toks.push(Tok::Percent);
                i += 1;
            }
            '^' => {
                toks.push(Tok::Caret);
                i += 1;
            }
            '&' => {
                toks.push(Tok::Amp);
                i += 1;
            }
            '|' => {
                toks.push(Tok::Pipe);
                i += 1;
            }
            '~' => {
                toks.push(Tok::Tilde);
                i += 1;
            }
            '<' if i + 1 < chars.len() && chars[i + 1] == '<' => {
                toks.push(Tok::Shl);
                i += 2;
            }
            '>' if i + 1 < chars.len() && chars[i + 1] == '>' => {
                toks.push(Tok::Shr);
                i += 2;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            _ if c.is_ascii_digit() || c == '.' => {
                let (tok, ni) = lex_number(&chars, i)?;
                toks.push(tok);
                i = ni;
            }
            _ if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            _ => return Err(CalcError::Syntax(alloc::format!("onbekend teken '{c}'"))),
        }
    }
    Ok(toks)
}

fn lex_number(chars: &[char], start: usize) -> Result<(Tok, usize), CalcError> {
    // Basis-prefixen 0x / 0o / 0b.
    if chars[start] == '0' && start + 1 < chars.len() {
        let p = chars[start + 1].to_ascii_lowercase();
        let radix = match p {
            'x' => Some(16),
            'o' => Some(8),
            'b' => Some(2),
            _ => None,
        };
        if let Some(r) = radix {
            let mut i = start + 2;
            let ds = i;
            let mut val: i64 = 0;
            while i < chars.len() {
                let d = chars[i].to_digit(r as u32);
                match d {
                    Some(v) => {
                        val = val
                            .checked_mul(r)
                            .and_then(|x| x.checked_add(v as i64))
                            .ok_or_else(|| CalcError::BadNumber("overflow".to_string()))?;
                        i += 1;
                    }
                    None => break,
                }
            }
            if i == ds {
                return Err(CalcError::BadNumber(chars[start..i].iter().collect()));
            }
            return Ok((Tok::Num(val as f64), i));
        }
    }
    // Decimaal met fractie en exponent.
    let mut i = start;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
        // Exponent enkel als er een cijfer (met optioneel teken) volgt.
        let mut j = i + 1;
        if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
            j += 1;
        }
        if j < chars.len() && chars[j].is_ascii_digit() {
            i = j;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    let s: String = chars[start..i].iter().collect();
    let val: f64 = s.parse().map_err(|_| CalcError::BadNumber(s.clone()))?;
    Ok((Tok::Num(val), i))
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    // Precedentieladder, laagste eerst.
    fn parse(&mut self) -> Result<f64, CalcError> {
        let v = self.or()?;
        if self.pos != self.toks.len() {
            return Err(CalcError::Syntax("overtollige invoer".to_string()));
        }
        Ok(v)
    }

    fn or(&mut self) -> Result<f64, CalcError> {
        let mut v = self.xor()?;
        while self.eat(&Tok::Pipe) {
            let r = self.xor()?;
            v = ((v as i64) | (r as i64)) as f64;
        }
        Ok(v)
    }
    fn xor(&mut self) -> Result<f64, CalcError> {
        let mut v = self.and()?;
        while matches!(self.peek(), Some(Tok::Ident(s)) if s == "xor") {
            self.pos += 1;
            let r = self.and()?;
            v = ((v as i64) ^ (r as i64)) as f64;
        }
        Ok(v)
    }
    fn and(&mut self) -> Result<f64, CalcError> {
        let mut v = self.shift()?;
        while self.eat(&Tok::Amp) {
            let r = self.shift()?;
            v = ((v as i64) & (r as i64)) as f64;
        }
        Ok(v)
    }
    fn shift(&mut self) -> Result<f64, CalcError> {
        let mut v = self.add()?;
        loop {
            if self.eat(&Tok::Shl) {
                let r = self.add()?;
                v = ((v as i64) << (r as i64)) as f64;
            } else if self.eat(&Tok::Shr) {
                let r = self.add()?;
                v = ((v as i64) >> (r as i64)) as f64;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn add(&mut self) -> Result<f64, CalcError> {
        let mut v = self.mul()?;
        loop {
            if self.eat(&Tok::Plus) {
                v += self.mul()?;
            } else if self.eat(&Tok::Minus) {
                v -= self.mul()?;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn mul(&mut self) -> Result<f64, CalcError> {
        let mut v = self.unary()?;
        loop {
            if self.eat(&Tok::Star) {
                v *= self.unary()?;
            } else if self.eat(&Tok::Slash) {
                let r = self.unary()?;
                if r == 0.0 {
                    return Err(CalcError::DivByZero);
                }
                v /= r;
            } else if self.eat(&Tok::Percent) {
                let r = self.unary()?;
                if r == 0.0 {
                    return Err(CalcError::DivByZero);
                }
                v %= r;
            } else if matches!(self.peek(), Some(Tok::Ident(s)) if s == "mod") {
                self.pos += 1;
                let r = self.unary()?;
                if r == 0.0 {
                    return Err(CalcError::DivByZero);
                }
                v %= r;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn unary(&mut self) -> Result<f64, CalcError> {
        if self.eat(&Tok::Minus) {
            return Ok(-self.unary()?);
        }
        if self.eat(&Tok::Plus) {
            return self.unary();
        }
        if self.eat(&Tok::Tilde) {
            let v = self.unary()?;
            return Ok(!(v as i64) as f64);
        }
        self.power()
    }
    fn power(&mut self) -> Result<f64, CalcError> {
        let base = self.atom()?;
        if self.eat(&Tok::Caret) {
            // rechts-associatief
            let exp = self.unary()?;
            return Ok(math::pow(base, exp));
        }
        Ok(base)
    }
    fn atom(&mut self) -> Result<f64, CalcError> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::LParen) => {
                let v = self.or()?;
                if !self.eat(&Tok::RParen) {
                    return Err(CalcError::Syntax("')' verwacht".to_string()));
                }
                Ok(v)
            }
            Some(Tok::Ident(name)) => {
                // Constante of functie-aanroep.
                match name.as_str() {
                    "pi" => return Ok(core::f64::consts::PI),
                    "e" => return Ok(core::f64::consts::E),
                    "tau" => return Ok(core::f64::consts::TAU),
                    _ => {}
                }
                if self.eat(&Tok::LParen) {
                    let arg = self.or()?;
                    if !self.eat(&Tok::RParen) {
                        return Err(CalcError::Syntax("')' na functie-argument".to_string()));
                    }
                    return apply_fn(&name, arg);
                }
                Err(CalcError::UnknownIdent(name))
            }
            other => Err(CalcError::Syntax(alloc::format!("onverwacht: {other:?}"))),
        }
    }
}

fn apply_fn(name: &str, x: f64) -> Result<f64, CalcError> {
    let v = match name {
        "sin" => math::sin(x),
        "cos" => math::cos(x),
        "tan" => math::tan(x),
        "sqrt" => math::sqrt(x),
        "ln" => math::ln(x),
        "log" => math::log10(x),
        "exp" => math::exp(x),
        "abs" => math::fabs(x),
        "floor" => math::floor(x),
        "round" => math::round(x),
        _ => return Err(CalcError::UnknownIdent(name.to_string())),
    };
    Ok(v)
}

/// Evalueer een expressie naar een `f64` (Standaard/Wetenschappelijk).
pub fn eval(expr: &str) -> Result<f64, CalcError> {
    let toks = lex(expr)?;
    if toks.is_empty() {
        return Err(CalcError::Syntax("lege invoer".to_string()));
    }
    let mut p = Parser { toks, pos: 0 };
    p.parse()
}

/// Evalueer in programmeursmodus naar een geheel getal (`i64`). Bitwise + bases.
pub fn eval_programmer(expr: &str) -> Result<i64, CalcError> {
    let v = eval(expr)?;
    Ok(v as i64)
}

/// Formatteer een geheel getal in een gegeven basis met EuroOS-prefix.
pub fn format_base(value: i64, radix: u32) -> String {
    let prefix = match radix {
        16 => "0x",
        8 => "0o",
        2 => "0b",
        _ => "",
    };
    if value == 0 {
        return alloc::format!("{prefix}0");
    }
    let neg = value < 0;
    let mut n = (value as i128).unsigned_abs();
    let mut digits = Vec::new();
    let r = radix as u128;
    while n > 0 {
        let d = (n % r) as u32;
        let ch = core::char::from_digit(d, radix).unwrap();
        digits.push(ch.to_ascii_uppercase());
        n /= r;
    }
    let body: String = digits.iter().rev().collect();
    if neg {
        alloc::format!("-{prefix}{body}")
    } else {
        alloc::format!("{prefix}{body}")
    }
}

/// Eenhedenconversie tussen twee eenheden van dezelfde categorie.
pub fn convert(value: f64, from: &str, to: &str) -> Result<f64, CalcError> {
    // Temperatuur is niet-lineair: apart.
    let from_l = from.to_ascii_lowercase();
    let to_l = to.to_ascii_lowercase();
    if let (Some(cf), Some(_ct)) = (temp_kind(&from_l), temp_kind(&to_l)) {
        let kelvin = match cf {
            't' if from_l == "c" => value + 273.15,
            't' if from_l == "f" => (value - 32.0) * 5.0 / 9.0 + 273.15,
            _ => value, // k
        };
        let out = match to_l.as_str() {
            "c" => kelvin - 273.15,
            "f" => (kelvin - 273.15) * 9.0 / 5.0 + 32.0,
            _ => kelvin,
        };
        return Ok(out);
    }

    let (fcat, ffac) = unit_factor(&from_l).ok_or_else(|| CalcError::UnknownIdent(from.to_string()))?;
    let (tcat, tfac) = unit_factor(&to_l).ok_or_else(|| CalcError::UnknownIdent(to.to_string()))?;
    if fcat != tcat {
        return Err(CalcError::Syntax(alloc::format!(
            "onverenigbare eenheden: {from} ({fcat}) ↔ {to} ({tcat})"
        )));
    }
    Ok(value * ffac / tfac)
}

fn temp_kind(u: &str) -> Option<char> {
    matches!(u, "c" | "f" | "k").then_some('t')
}

/// (categorie, factor naar basiseenheid).
fn unit_factor(u: &str) -> Option<(&'static str, f64)> {
    let v = match u {
        // lengte (basis: meter)
        "m" => ("len", 1.0),
        "km" => ("len", 1000.0),
        "cm" => ("len", 0.01),
        "mm" => ("len", 0.001),
        "mi" => ("len", 1609.344),
        "yd" => ("len", 0.9144),
        "ft" => ("len", 0.3048),
        "in" => ("len", 0.0254),
        // massa (basis: kg)
        "kg" => ("mass", 1.0),
        "g" => ("mass", 0.001),
        "mg" => ("mass", 1e-6),
        "t" => ("mass", 1000.0),
        "lb" => ("mass", 0.453_592_37),
        "oz" => ("mass", 0.028_349_523_125),
        // data (basis: byte, binaire prefixen)
        "b" => ("data", 1.0),
        "kb" => ("data", 1024.0),
        "mb" => ("data", 1024.0 * 1024.0),
        "gb" => ("data", 1024.0 * 1024.0 * 1024.0),
        "tb" => ("data", 1024.0 * 1024.0 * 1024.0 * 1024.0),
        _ => return None,
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        math::fabs(a - b) <= 1e-9 * (1.0 + math::fabs(b))
    }

    #[test]
    fn standard_arithmetic_precedence() {
        assert!(close(eval("1 + 2 * 3").unwrap(), 7.0));
        assert!(close(eval("(1 + 2) * 3").unwrap(), 9.0));
        assert!(close(eval("2 ^ 10").unwrap(), 1024.0));
        assert!(close(eval("2 ^ 3 ^ 2").unwrap(), 512.0)); // rechts-assoc
        assert!(close(eval("-3 + 4").unwrap(), 1.0));
        assert!(close(eval("10 % 3").unwrap(), 1.0));
        assert!(close(eval("7 / 2").unwrap(), 3.5));
    }

    #[test]
    fn scientific_functions_and_constants() {
        assert!(close(eval("sqrt(2)").unwrap(), 1.4142135623730951));
        assert!(close(eval("sin(pi/2)").unwrap(), 1.0));
        assert!(close(eval("cos(0)").unwrap(), 1.0));
        assert!(close(eval("ln(e)").unwrap(), 1.0));
        assert!(close(eval("log(1000)").unwrap(), 3.0));
        assert!(close(eval("exp(0)").unwrap(), 1.0));
        assert!(close(eval("abs(0 - 5)").unwrap(), 5.0));
        assert!(close(eval("2 * pi").unwrap(), core::f64::consts::TAU));
    }

    #[test]
    fn programmer_bases_and_bitwise() {
        assert_eq!(eval_programmer("0xFF").unwrap(), 255);
        assert_eq!(eval_programmer("0b1010").unwrap(), 10);
        assert_eq!(eval_programmer("0o17").unwrap(), 15);
        assert_eq!(eval_programmer("0xF0 | 0x0F").unwrap(), 255);
        assert_eq!(eval_programmer("0xFF & 0x0F").unwrap(), 15);
        assert_eq!(eval_programmer("5 xor 1").unwrap(), 4);
        assert_eq!(eval_programmer("1 << 8").unwrap(), 256);
        assert_eq!(eval_programmer("256 >> 2").unwrap(), 64);
        assert_eq!(eval_programmer("~0").unwrap(), -1);
    }

    #[test]
    fn base_formatting() {
        assert_eq!(format_base(255, 16), "0xFF");
        assert_eq!(format_base(10, 2), "0b1010");
        assert_eq!(format_base(15, 8), "0o17");
        assert_eq!(format_base(0, 16), "0x0");
        assert_eq!(format_base(-255, 16), "-0xFF");
    }

    #[test]
    fn errors() {
        assert_eq!(eval("1 / 0"), Err(CalcError::DivByZero));
        assert!(matches!(eval("1 +"), Err(CalcError::Syntax(_))));
        assert!(matches!(eval("nonsense(2)"), Err(CalcError::UnknownIdent(_))));
        assert!(matches!(eval("foo"), Err(CalcError::UnknownIdent(_))));
    }

    #[test]
    fn unit_conversion() {
        assert!(close(convert(1.0, "km", "m").unwrap(), 1000.0));
        assert!(close(convert(100.0, "cm", "m").unwrap(), 1.0));
        assert!(close(convert(1.0, "mi", "km").unwrap(), 1.609344));
        assert!(close(convert(1.0, "kg", "g").unwrap(), 1000.0));
        assert!(close(convert(1.0, "lb", "kg").unwrap(), 0.45359237));
        assert!(close(convert(1.0, "gb", "mb").unwrap(), 1024.0));
        // temperatuur
        assert!(close(convert(0.0, "c", "k").unwrap(), 273.15));
        assert!(close(convert(100.0, "c", "f").unwrap(), 212.0));
        assert!(close(convert(32.0, "f", "c").unwrap(), 0.0));
        // onverenigbaar
        assert!(matches!(convert(1.0, "kg", "m"), Err(CalcError::Syntax(_))));
    }
}
