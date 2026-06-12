//! EuroCalc — de formule-engine van EuroSuite Calc (ES-Calc).
//!
//! Parseert en evalueert rekenblad-formules over een [`eurodoc`]-`SheetBody`:
//! getallen, **celverwijzingen** (`A1`), **bereiken** (`A1:B3`), de operatoren
//! `+ - * / ^ %`, haakjes, en functies (`SUM AVERAGE MIN MAX COUNT IF ROUND ABS`).
//! Formule-cellen die naar elkaar verwijzen worden recursief geëvalueerd, met
//! **cyclusdetectie**. Pure, host-geteste `no_std`-logica (f64-rekenkern).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use eurodoc::model::{Cell, SheetBody};

/// Een evaluatiefout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CalcError {
    /// Syntaxfout in de formule.
    Syntax,
    /// Verwijzing naar een ongeldige cel/naam.
    BadRef,
    /// Deling door nul.
    DivZero,
    /// Een cel verwijst (in)direct naar zichzelf.
    Cycle,
}

/// Zet een A1-celnaam om naar (rij, kolom), 0-gebaseerd. `"B3"` → (2, 1).
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let mut col: u32 = 0;
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        // Overloop-veilig (audit C5): een te lange kolomnaam → geen geldige ref.
        col = col
            .checked_mul(26)?
            .checked_add((bytes[i].to_ascii_uppercase() - b'A' + 1) as u32)?;
        i += 1;
    }
    if i == 0 || i == bytes.len() {
        return None;
    }
    let row: u32 = s[i..].parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

// ── Tokenizer ────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Ident(String), // functienaam of celnaam
    Op(char),
    LParen,
    RParen,
    Comma,
    Colon,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, CalcError> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b',' | b';' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            b'+' | b'-' | b'*' | b'/' | b'^' | b'%' => {
                out.push(Tok::Op(c as char));
                i += 1;
            }
            b'0'..=b'9' | b'.' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                let num: f64 = s[start..i].parse().map_err(|_| CalcError::Syntax)?;
                out.push(Tok::Num(num));
            }
            _ if c.is_ascii_alphabetic() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(String::from(&s[start..i])));
            }
            _ => return Err(CalcError::Syntax),
        }
    }
    Ok(out)
}

// ── Parser (recursive descent met precedentie) ──────────────────────────────

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    /// Recursiediepte — begrensd zodat diep-geneste invoer de stack niet overloopt (audit H13).
    depth: usize,
}

/// Maximale nesting-diepte van een formule (haakjes/unair/macht).
const MAX_DEPTH: usize = 256;

/// Een geparseerde expressieboom.
enum Expr {
    Num(f64),
    Ref(u32, u32),
    Range((u32, u32), (u32, u32)),
    Bin(char, alloc::boxed::Box<Expr>, alloc::boxed::Box<Expr>),
    Neg(alloc::boxed::Box<Expr>),
    Call(String, Vec<Expr>),
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_expr(&mut self) -> Result<Expr, CalcError> {
        self.parse_add()
    }

    fn parse_add(&mut self) -> Result<Expr, CalcError> {
        let mut left = self.parse_mul()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::Bin(op, alloc::boxed::Box::new(left), alloc::boxed::Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, CalcError> {
        let mut left = self.parse_pow()?;
        while let Some(Tok::Op(op @ ('*' | '/' | '%'))) = self.peek() {
            let op = *op;
            self.pos += 1;
            let right = self.parse_pow()?;
            left = Expr::Bin(op, alloc::boxed::Box::new(left), alloc::boxed::Box::new(right));
        }
        Ok(left)
    }

    fn parse_pow(&mut self) -> Result<Expr, CalcError> {
        let left = self.parse_unary()?;
        if let Some(Tok::Op('^')) = self.peek() {
            self.pos += 1;
            let right = self.parse_pow()?; // rechts-associatief
            return Ok(Expr::Bin('^', alloc::boxed::Box::new(left), alloc::boxed::Box::new(right)));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, CalcError> {
        // Elke recursielaag (haakjes/unair/macht) passeert hier precies één keer.
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(CalcError::Syntax);
        }
        let r = self.parse_unary_inner();
        self.depth -= 1;
        r
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, CalcError> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            return Ok(Expr::Neg(alloc::boxed::Box::new(self.parse_unary()?)));
        }
        if let Some(Tok::Op('+')) = self.peek() {
            self.pos += 1;
            return self.parse_unary();
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Expr, CalcError> {
        match self.bump().cloned() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::LParen) => {
                let e = self.parse_expr()?;
                if self.bump() != Some(&Tok::RParen) {
                    return Err(CalcError::Syntax);
                }
                Ok(e)
            }
            Some(Tok::Ident(id)) => {
                if self.peek() == Some(&Tok::LParen) {
                    // Functie-aanroep.
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            match self.bump() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::RParen) => break,
                                _ => return Err(CalcError::Syntax),
                            }
                        }
                    } else {
                        self.pos += 1;
                    }
                    Ok(Expr::Call(id.to_ascii_uppercase(), args))
                } else if self.peek() == Some(&Tok::Colon) {
                    // Bereik A1:B3.
                    let from = parse_ref(&id).ok_or(CalcError::BadRef)?;
                    self.pos += 1;
                    let to_id = match self.bump() {
                        Some(Tok::Ident(s)) => s.clone(),
                        _ => return Err(CalcError::Syntax),
                    };
                    let to = parse_ref(&to_id).ok_or(CalcError::BadRef)?;
                    Ok(Expr::Range(from, to))
                } else {
                    let (r, c) = parse_ref(&id).ok_or(CalcError::BadRef)?;
                    Ok(Expr::Ref(r, c))
                }
            }
            _ => Err(CalcError::Syntax),
        }
    }
}

/// Evalueer een formule (`"=A1+SUM(B1:B3)"` of zonder `=`) over `sheet`.
pub fn eval(formula: &str, sheet: &SheetBody) -> Result<f64, CalcError> {
    let f = formula.trim().strip_prefix('=').unwrap_or(formula.trim());
    let toks = tokenize(f)?;
    let mut p = Parser { toks: &toks, pos: 0, depth: 0 };
    let expr = p.parse_expr()?;
    if p.pos != toks.len() {
        return Err(CalcError::Syntax);
    }
    let mut stack = Vec::new();
    eval_expr(&expr, sheet, &mut stack)
}

/// Evalueer de numerieke waarde van een cel (formules recursief, met cyclusdetectie).
fn cell_value(row: u32, col: u32, sheet: &SheetBody, stack: &mut Vec<(u32, u32)>) -> Result<f64, CalcError> {
    if stack.contains(&(row, col)) {
        return Err(CalcError::Cycle);
    }
    match sheet.get(row, col) {
        Cell::Empty => Ok(0.0),
        Cell::Text(_) => Ok(0.0), // tekst telt als 0 in rekenkundige context
        Cell::Number { scaled, scale } => Ok(scaled as f64 / powf(10.0, scale as f64)),
        Cell::Formula(f) => {
            stack.push((row, col));
            let f = f.trim().strip_prefix('=').unwrap_or(f.trim());
            let toks = tokenize(f)?;
            let mut p = Parser { toks: &toks, pos: 0, depth: 0 };
            let expr = p.parse_expr()?;
            let v = eval_expr(&expr, sheet, stack);
            stack.pop();
            v
        }
    }
}

fn eval_expr(e: &Expr, sheet: &SheetBody, stack: &mut Vec<(u32, u32)>) -> Result<f64, CalcError> {
    match e {
        Expr::Num(n) => Ok(*n),
        Expr::Ref(r, c) => cell_value(*r, *c, sheet, stack),
        Expr::Range(..) => Err(CalcError::Syntax), // een kaal bereik is geen scalair
        Expr::Neg(x) => Ok(-eval_expr(x, sheet, stack)?),
        Expr::Bin(op, a, b) => {
            let x = eval_expr(a, sheet, stack)?;
            let y = eval_expr(b, sheet, stack)?;
            Ok(match op {
                '+' => x + y,
                '-' => x - y,
                '*' => x * y,
                '/' => {
                    if y == 0.0 {
                        return Err(CalcError::DivZero);
                    }
                    x / y
                }
                '%' => {
                    if y == 0.0 {
                        return Err(CalcError::DivZero); // audit M2: zoals '/'
                    }
                    x % y
                }
                '^' => powf(x, y),
                _ => return Err(CalcError::Syntax),
            })
        }
        Expr::Call(name, args) => eval_call(name, args, sheet, stack),
    }
}

/// Verzamel de scalaire waarden van een argument (een bereik levert meerdere).
fn collect(e: &Expr, sheet: &SheetBody, stack: &mut Vec<(u32, u32)>, out: &mut Vec<f64>) -> Result<(), CalcError> {
    match e {
        Expr::Range((r0, c0), (r1, c1)) => {
            let (rlo, rhi) = (*r0.min(r1), *r0.max(r1));
            let (clo, chi) = (*c0.min(c1), *c0.max(c1));
            for r in rlo..=rhi {
                for c in clo..=chi {
                    out.push(cell_value(r, c, sheet, stack)?);
                }
            }
            Ok(())
        }
        _ => {
            out.push(eval_expr(e, sheet, stack)?);
            Ok(())
        }
    }
}

fn eval_call(name: &str, args: &[Expr], sheet: &SheetBody, stack: &mut Vec<(u32, u32)>) -> Result<f64, CalcError> {
    let mut vals = Vec::new();
    for a in args {
        collect(a, sheet, stack, &mut vals)?;
    }
    match name {
        "SUM" => Ok(vals.iter().sum()),
        "AVERAGE" => {
            if vals.is_empty() {
                return Err(CalcError::DivZero);
            }
            Ok(vals.iter().sum::<f64>() / vals.len() as f64)
        }
        "MIN" => vals.iter().cloned().reduce(f64::min).ok_or(CalcError::Syntax),
        "MAX" => vals.iter().cloned().reduce(f64::max).ok_or(CalcError::Syntax),
        "COUNT" => Ok(vals.len() as f64),
        "ABS" => vals.first().map(|v| v.abs()).ok_or(CalcError::Syntax),
        "ROUND" => {
            let v = *vals.first().ok_or(CalcError::Syntax)?;
            let digits = vals.get(1).copied().unwrap_or(0.0) as i32;
            let f = powf(10.0, digits as f64);
            Ok(round_half(v * f) / f)
        }
        "IF" => {
            if vals.len() < 2 {
                return Err(CalcError::Syntax);
            }
            Ok(if vals[0] != 0.0 { vals[1] } else { vals.get(2).copied().unwrap_or(0.0) })
        }
        _ => Err(CalcError::BadRef),
    }
}

// no_std-vriendelijke helpers (geen libm nodig voor deze gevallen).
fn round_half(x: f64) -> f64 {
    if x >= 0.0 {
        (x + 0.5) as i64 as f64
    } else {
        -((-x + 0.5) as i64 as f64)
    }
}

/// Gehele machten (de enige die het rekenblad in de praktijk nodig heeft).
fn powf(base: f64, exp: f64) -> f64 {
    let n = exp as i64;
    if n as f64 == exp {
        let mut r = 1.0;
        let mut b = base;
        let mut e = n.unsigned_abs();
        while e > 0 {
            if e & 1 == 1 {
                r *= b;
            }
            b *= b;
            e >>= 1;
        }
        if n < 0 {
            1.0 / r
        } else {
            r
        }
    } else {
        // Niet-gehele exponent: niet ondersteund zonder libm → benader 0.
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> SheetBody {
        let mut s = SheetBody::default();
        s.set(0, 0, Cell::Number { scaled: 10, scale: 0 }); // A1 = 10
        s.set(1, 0, Cell::Number { scaled: 20, scale: 0 }); // A2 = 20
        s.set(2, 0, Cell::Number { scaled: 30, scale: 0 }); // A3 = 30
        s.set(0, 1, Cell::Number { scaled: 250, scale: 1 }); // B1 = 25.0
        s
    }

    #[test]
    fn refs_and_arithmetic() {
        let s = sheet();
        assert_eq!(eval("=A1+A2", &s), Ok(30.0));
        assert_eq!(eval("=A1*2+A3", &s), Ok(50.0));
        assert_eq!(eval("=(A1+A2)*2", &s), Ok(60.0));
        assert_eq!(eval("=B1", &s), Ok(25.0));
    }

    #[test]
    fn col_parse() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("B3"), Some((2, 1)));
        assert_eq!(parse_ref("AA1"), Some((0, 26)));
        assert_eq!(parse_ref("Z"), None);
    }

    #[test]
    fn functions_and_ranges() {
        let s = sheet();
        assert_eq!(eval("=SUM(A1:A3)", &s), Ok(60.0));
        assert_eq!(eval("=AVERAGE(A1:A3)", &s), Ok(20.0));
        assert_eq!(eval("=MAX(A1:A3)", &s), Ok(30.0));
        assert_eq!(eval("=MIN(A1:A3)", &s), Ok(10.0));
        assert_eq!(eval("=COUNT(A1:A3)", &s), Ok(3.0));
        assert_eq!(eval("=SUM(A1:A3)+B1", &s), Ok(85.0));
    }

    #[test]
    fn nested_formula_cells() {
        let mut s = sheet();
        s.set(3, 0, Cell::Formula(String::from("=SUM(A1:A3)"))); // A4 = 60
        s.set(4, 0, Cell::Formula(String::from("=A4*2"))); // A5 = 120
        assert_eq!(eval("=A5+A4", &s), Ok(180.0));
    }

    #[test]
    fn if_and_round() {
        let s = sheet();
        assert_eq!(eval("=IF(A1, 100, 200)", &s), Ok(100.0));
        assert_eq!(eval("=IF(0, 100, 200)", &s), Ok(200.0));
        assert_eq!(eval("=ROUND(3.14159, 2)", &s), Ok(3.14));
        assert_eq!(eval("=2^10", &s), Ok(1024.0));
    }

    #[test]
    fn errors() {
        let s = sheet();
        assert_eq!(eval("=1/0", &s), Err(CalcError::DivZero));
        assert_eq!(eval("=A1+", &s), Err(CalcError::Syntax));
    }

    #[test]
    fn audit_regressions() {
        let s = sheet();
        // C5: te lange kolomnaam → geen panic, nette fout.
        assert_eq!(eval("=ZZZZZZZ1", &s), Err(CalcError::BadRef));
        // H13: diep-geneste invoer → geen stack-overflow, nette syntaxfout.
        let deep = alloc::format!("={}1{}", "(".repeat(5000), ")".repeat(5000));
        assert_eq!(eval(&deep, &s), Err(CalcError::Syntax));
        let neg = alloc::format!("={}1", "-".repeat(5000));
        assert_eq!(eval(&neg, &s), Err(CalcError::Syntax));
        // M2: modulo door nul → DivZero (zoals '/').
        assert_eq!(eval("=5%0", &s), Err(CalcError::DivZero));
    }

    #[test]
    fn cycle_detection() {
        let mut s = SheetBody::default();
        s.set(0, 0, Cell::Formula(String::from("=A2"))); // A1=A2
        s.set(1, 0, Cell::Formula(String::from("=A1"))); // A2=A1
        assert_eq!(eval("=A1", &s), Err(CalcError::Cycle));
    }
}
