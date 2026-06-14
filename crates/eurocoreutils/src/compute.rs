//! Compute & control commands (CU-7): `printf · expr · test`/`[` · `numfmt · factor`.
//! All pure functions: `fn(args[, input]) -> Vec<u8>` or `-> (Vec<u8>, i32)`
//! where the exit code matters (test/`[`). Host-tested against the expected GNU output.

use crate::args::Args;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// `printf FORMAT [ARGS...]` — C-like formatting. Supports `%s %d %i %x %X %o
/// %c %%` + width (`%5s`, `%-5s`, `%05d`) and backslash escapes (`\n \t \\` …).
/// Arguments are reused cyclically as long as the format consumes them (GNU).
pub fn printf(args: &[&str]) -> Vec<u8> {
    if args.is_empty() {
        return Vec::new();
    }
    let format = args[0];
    let rest = &args[1..];
    let mut out = Vec::new();
    let mut ai = 0usize;
    // With ≥1 conversion GNU keeps repeating the format until the args run out.
    loop {
        let consumed_before = ai;
        apply_format(format, rest, &mut ai, &mut out);
        // Stop when no more args were consumed (no conversions or done).
        if ai >= rest.len() || ai == consumed_before {
            break;
        }
    }
    out
}

fn apply_format(format: &str, args: &[&str], ai: &mut usize, out: &mut Vec<u8>) {
    let b = format.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                i += 1;
                match b[i] {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'\\' => out.push(b'\\'),
                    b'a' => out.push(0x07),
                    b'0' => out.push(0),
                    other => {
                        out.push(b'\\');
                        out.push(other);
                    }
                }
                i += 1;
            }
            b'%' if i + 1 < b.len() => {
                // Read the conversion specification: %[-][0][width]<conv>
                let start = i;
                i += 1;
                if b[i] == b'%' {
                    out.push(b'%');
                    i += 1;
                    continue;
                }
                let mut left = false;
                let mut zero = false;
                while i < b.len() && (b[i] == b'-' || b[i] == b'0') {
                    if b[i] == b'-' {
                        left = true;
                    } else {
                        zero = true;
                    }
                    i += 1;
                }
                let mut width = 0usize;
                while i < b.len() && b[i].is_ascii_digit() {
                    width = width * 10 + (b[i] - b'0') as usize;
                    i += 1;
                }
                if i >= b.len() {
                    // Incomplete spec → literal.
                    out.extend_from_slice(&b[start..]);
                    return;
                }
                let conv = b[i];
                i += 1;
                let arg = args.get(*ai).copied().unwrap_or("");
                let rendered = render_conv(conv, arg, ai);
                pad(&rendered, width, left, zero && !left, out);
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
}

fn render_conv(conv: u8, arg: &str, ai: &mut usize) -> String {
    let consumes = matches!(conv, b's' | b'd' | b'i' | b'x' | b'X' | b'o' | b'c');
    if consumes {
        *ai += 1;
    }
    match conv {
        b's' => arg.to_string(),
        b'd' | b'i' => arg.trim().parse::<i64>().unwrap_or(0).to_string(),
        b'x' => alloc::format!("{:x}", arg.trim().parse::<i64>().unwrap_or(0)),
        b'X' => alloc::format!("{:X}", arg.trim().parse::<i64>().unwrap_or(0)),
        b'o' => alloc::format!("{:o}", arg.trim().parse::<i64>().unwrap_or(0)),
        b'c' => arg.chars().next().map(|c| c.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn pad(s: &str, width: usize, left: bool, zero: bool, out: &mut Vec<u8>) {
    let len = s.chars().count();
    if len >= width {
        out.extend_from_slice(s.as_bytes());
        return;
    }
    let fill = if zero { b'0' } else { b' ' };
    if left {
        out.extend_from_slice(s.as_bytes());
        out.extend(core::iter::repeat(b' ').take(width - len));
    } else {
        out.extend(core::iter::repeat(fill).take(width - len));
        out.extend_from_slice(s.as_bytes());
    }
}

/// `expr` — evaluate a simple arithmetic/comparison expression.
/// Supports `+ - * / %` with `*`/`/`/`%` precedence and parentheses; comparisons
/// `= != < <= > >=` (1/0); `length STR`. Returns `(output, exit-code)`: exit 0 if
/// the result is non-zero/non-empty, otherwise 1 (GNU semantics).
pub fn expr(args: &[&str]) -> (Vec<u8>, i32) {
    if args.len() == 2 && args[0] == "length" {
        let n = args[1].chars().count() as i64;
        return num_result(n);
    }
    let mut p = ExprParser { toks: args, pos: 0 };
    match p.parse_cmp() {
        Some(v) if p.pos == args.len() => num_result(v),
        _ => (b"expr: syntax error\n".to_vec(), 2),
    }
}

fn num_result(v: i64) -> (Vec<u8>, i32) {
    let mut s = v.to_string();
    s.push('\n');
    let code = if v != 0 { 0 } else { 1 };
    (s.into_bytes(), code)
}

struct ExprParser<'a> {
    toks: &'a [&'a str],
    pos: usize,
}

impl<'a> ExprParser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.toks.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<&'a str> {
        let t = self.toks.get(self.pos).copied();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn parse_cmp(&mut self) -> Option<i64> {
        let mut left = self.parse_add()?;
        while let Some(op) = self.peek() {
            if !matches!(op, "=" | "!=" | "<" | "<=" | ">" | ">=") {
                break;
            }
            self.pos += 1;
            let right = self.parse_add()?;
            left = match op {
                "=" => (left == right) as i64,
                "!=" => (left != right) as i64,
                "<" => (left < right) as i64,
                "<=" => (left <= right) as i64,
                ">" => (left > right) as i64,
                ">=" => (left >= right) as i64,
                _ => return None,
            };
        }
        Some(left)
    }
    fn parse_add(&mut self) -> Option<i64> {
        let mut left = self.parse_mul()?;
        while let Some(op) = self.peek() {
            if op != "+" && op != "-" {
                break;
            }
            self.pos += 1;
            let right = self.parse_mul()?;
            // Saturating instead of overflowing (audit M3): no panic on large args.
            left = if op == "+" { left.saturating_add(right) } else { left.saturating_sub(right) };
        }
        Some(left)
    }
    fn parse_mul(&mut self) -> Option<i64> {
        let mut left = self.parse_atom()?;
        while let Some(op) = self.peek() {
            if !matches!(op, "*" | "/" | "%") {
                break;
            }
            self.pos += 1;
            let right = self.parse_atom()?;
            left = match op {
                "*" => left.saturating_mul(right),
                "/" if right != 0 => left / right,
                "%" if right != 0 => left % right,
                _ => return None,
            };
        }
        Some(left)
    }
    fn parse_atom(&mut self) -> Option<i64> {
        match self.bump()? {
            "(" => {
                let v = self.parse_cmp()?;
                if self.bump() != Some(")") {
                    return None;
                }
                Some(v)
            }
            t => t.parse::<i64>().ok(),
        }
    }
}

/// `test EXPR` / `[ EXPR ]` — POSIX condition. Returns only an exit code (0=true).
/// Supports string (`-z -n = !=`), integer (`-eq -ne -lt -le -gt -ge`), and the
/// unary `!` negation. (File tests like `-f`/`-d` are handled by the shell itself.)
pub fn test(args: &[&str]) -> i32 {
    // `[ ... ]`: strip the trailing `]`.
    let mut a = args;
    if a.last() == Some(&"]") {
        a = &a[..a.len() - 1];
    }
    // General `! EXPR` negation (applies to any length ≥ 2).
    if a.len() >= 2 && a[0] == "!" {
        return (test(&a[1..]) == 0) as i32;
    }
    match a.len() {
        0 => 1,
        1 => (!a[0].is_empty()) as i32 ^ 1, // non-empty string = true(0)
        2 => {
            let r = match a[0] {
                "-z" => a[1].is_empty(),
                "-n" => !a[1].is_empty(),
                _ => return 2,
            };
            (!r) as i32
        }
        3 => {
            let l = a[0];
            let r = a[2];
            let res = match a[1] {
                "=" | "==" => l == r,
                "!=" => l != r,
                "-eq" => int_cmp(l, r, |x, y| x == y),
                "-ne" => int_cmp(l, r, |x, y| x != y),
                "-lt" => int_cmp(l, r, |x, y| x < y),
                "-le" => int_cmp(l, r, |x, y| x <= y),
                "-gt" => int_cmp(l, r, |x, y| x > y),
                "-ge" => int_cmp(l, r, |x, y| x >= y),
                _ => return 2,
            };
            (!res) as i32
        }
        _ => 2,
    }
}

fn int_cmp(l: &str, r: &str, f: impl Fn(i64, i64) -> bool) -> bool {
    match (l.trim().parse::<i64>(), r.trim().parse::<i64>()) {
        (Ok(x), Ok(y)) => f(x, y),
        _ => false,
    }
}

/// `numfmt --to=iec N` — make a number human-friendly (IEC: K/M/G/T on 1024).
/// With `--to=si` on 1000. Without `--to` it echoes the input.
pub fn numfmt(args: &[&str]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    let to = a
        .options
        .iter()
        .find(|(k, _)| k == "to")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let base: f64 = match to.as_str() {
        "iec" | "iec-i" => 1024.0,
        "si" => 1000.0,
        _ => 0.0,
    };
    let mut out = Vec::new();
    for n in &a.positional {
        let v: f64 = n.parse().unwrap_or(0.0);
        let s = if base == 0.0 {
            n.clone()
        } else {
            human(v, base)
        };
        out.extend_from_slice(s.as_bytes());
        out.push(b'\n');
    }
    out
}

fn human(mut v: f64, base: f64) -> String {
    const UNITS: [&str; 5] = ["", "K", "M", "G", "T"];
    let mut u = 0;
    while v >= base && u < UNITS.len() - 1 {
        v /= base;
        u += 1;
    }
    // GNU rounds up to 1 decimal for <10, otherwise a whole number.
    if u == 0 {
        return alloc::format!("{}", v as i64);
    }
    let scaled = (v * 10.0 + 0.5) as i64; // 1 decimal, rounded
    let whole = scaled / 10;
    let frac = scaled % 10;
    if whole < 10 {
        alloc::format!("{whole}.{frac}{}", UNITS[u]) // GNU always shows 1 decimal < 10
    } else {
        alloc::format!("{}{}", scaled / 10, UNITS[u])
    }
}

/// `factor N...` — prime factorization, GNU format `N: p p q`.
pub fn factor(args: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in args {
        let n: u64 = match tok.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        out.extend_from_slice(tok.trim().as_bytes());
        out.push(b':');
        let mut m = n;
        let mut d = 2u64;
        while d * d <= m {
            while m % d == 0 {
                out.push(b' ');
                out.extend_from_slice(d.to_string().as_bytes());
                m /= d;
            }
            d += if d == 2 { 1 } else { 2 };
        }
        if m > 1 {
            out.push(b' ');
            out.extend_from_slice(m.to_string().as_bytes());
        }
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn printf_basic() {
        assert_eq!(s(printf(&["%s=%d\\n", "x", "42"])), "x=42\n");
        assert_eq!(s(printf(&["%5s|", "hi"])), "   hi|");
        assert_eq!(s(printf(&["%-5s|", "hi"])), "hi   |");
        assert_eq!(s(printf(&["%05d", "42"])), "00042");
        assert_eq!(s(printf(&["%x", "255"])), "ff");
        assert_eq!(s(printf(&["%d%%\\n", "50"])), "50%\n");
    }

    #[test]
    fn printf_recycles_args() {
        // GNU repeats the format until the args run out.
        assert_eq!(s(printf(&["[%s]", "a", "b", "c"])), "[a][b][c]");
    }

    #[test]
    fn expr_arith() {
        assert_eq!(s(expr(&["2", "+", "3", "*", "4"]).0), "14\n");
        assert_eq!(s(expr(&["(", "2", "+", "3", ")", "*", "4"]).0), "20\n");
        assert_eq!(expr(&["5", "-", "5"]).1, 1); // result 0 → exit 1
        assert_eq!(s(expr(&["10", "%", "3"]).0), "1\n");
        assert_eq!(s(expr(&["3", "<", "5"]).0), "1\n");
        assert_eq!(s(expr(&["length", "hallo"]).0), "5\n");
    }

    #[test]
    fn test_strings_and_ints() {
        assert_eq!(test(&["-z", ""]), 0);
        assert_eq!(test(&["-n", "x"]), 0);
        assert_eq!(test(&["abc", "=", "abc"]), 0);
        assert_eq!(test(&["abc", "=", "xyz"]), 1);
        assert_eq!(test(&["5", "-gt", "3"]), 0);
        assert_eq!(test(&["5", "-lt", "3"]), 1);
        assert_eq!(test(&["5", "-eq", "5", "]"]), 0); // `[ ... ]` form
        assert_eq!(test(&["!", "x", "=", "y"]), 0);
    }

    #[test]
    fn numfmt_iec() {
        assert_eq!(s(numfmt(&["--to=iec", "1024"])), "1.0K\n");
        assert_eq!(s(numfmt(&["--to=iec", "1048576"])), "1.0M\n");
        assert_eq!(s(numfmt(&["--to=si", "1500"])), "1.5K\n");
        assert_eq!(s(numfmt(&["500"])), "500\n");
    }

    #[test]
    fn factor_primes() {
        assert_eq!(s(factor(&["12"])), "12: 2 2 3\n");
        assert_eq!(s(factor(&["17"])), "17: 17\n");
        assert_eq!(s(factor(&["1"])), "1:\n");
        assert_eq!(s(factor(&["100"])), "100: 2 2 5 5\n");
    }
}
