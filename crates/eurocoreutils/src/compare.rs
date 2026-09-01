//! Two-input comparison / relational commands (CU-3 family): `comm` and `join`
//! over two SORTED inputs, plus `split` which carves one input into named pieces.
//!
//! Because a pure function cannot read or write files, `comm`/`join` take both
//! file contents as arguments (the shell reads both files), and `split` returns
//! `(name, chunk)` pairs for the shell to write.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::args::Args;
use crate::lines;

/// `comm [-1] [-2] [-3] FILE1 FILE2` — compare two SORTED inputs line by line.
///
/// Three columns: lines only in `a`, lines only in `b`, lines in both. `-1`/`-2`/`-3`
/// suppress the respective column. GNU indents column 2 by one tab (after the
/// columns to its left that are shown) and column 3 by two; this matches that
/// layout for the default (no suppression) and the common suppressed forms.
pub fn comm(args: &[&str], a: &[u8], b: &[u8]) -> Vec<u8> {
    let parsed = Args::parse(args, &[]);
    let s1 = !parsed.flag('1'); // show column 1?
    let s2 = !parsed.flag('2');
    let s3 = !parsed.flag('3');

    // The indent of a column = number of *shown* columns before it.
    let mut out = Vec::new();
    let emit = |col: u8, line: &[u8], out: &mut Vec<u8>| {
        let (show, indent) = match col {
            1 => (s1, 0),
            2 => (s2, s1 as usize),
            _ => (s3, s1 as usize + s2 as usize),
        };
        if !show {
            return;
        }
        for _ in 0..indent {
            out.push(b'\t');
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    };

    let la = lines(a);
    let lb = lines(b);
    let (mut i, mut j) = (0usize, 0usize);
    while i < la.len() && j < lb.len() {
        match la[i].cmp(lb[j]) {
            core::cmp::Ordering::Less => {
                emit(1, la[i], &mut out);
                i += 1;
            }
            core::cmp::Ordering::Greater => {
                emit(2, lb[j], &mut out);
                j += 1;
            }
            core::cmp::Ordering::Equal => {
                emit(3, la[i], &mut out);
                i += 1;
                j += 1;
            }
        }
    }
    while i < la.len() {
        emit(1, la[i], &mut out);
        i += 1;
    }
    while j < lb.len() {
        emit(2, lb[j], &mut out);
        j += 1;
    }
    out
}

/// `join [-1 F] [-2 F] [-t CHAR] FILE1 FILE2` — relational join of two SORTED
/// inputs on a common field (default field 1). Output: the join field, then the
/// remaining fields of `a`, then the remaining fields of `b`, separated by the
/// delimiter (default a single space; `-t` sets it). Inputs must be sorted on
/// the join field. Matches GNU for the common one-line-per-key case (and emits
/// the cartesian product for repeated keys, like GNU).
pub fn join(args: &[&str], a: &[u8], b: &[u8]) -> Vec<u8> {
    let parsed = Args::parse(args, &['1', '2', 't']);
    let f1 = parsed.num("1", 1).max(1) - 1;
    let f2 = parsed.num("2", 1).max(1) - 1;
    let delim: Option<u8> = parsed.opt("t").and_then(|d| d.bytes().next());

    let split = |row: &[u8]| -> Vec<Vec<u8>> {
        match delim {
            Some(d) => row.split(|&b| b == d).map(|s| s.to_vec()).collect(),
            // default: split on runs of blanks (GNU default field splitting)
            None => row
                .split(|&b| b == b' ' || b == b'\t')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_vec())
                .collect(),
        }
    };
    let sep: u8 = delim.unwrap_or(b' ');

    let la: Vec<Vec<Vec<u8>>> = lines(a).iter().map(|r| split(r)).collect();
    let lb: Vec<Vec<Vec<u8>>> = lines(b).iter().map(|r| split(r)).collect();

    let key = |fields: &[Vec<u8>], idx: usize| -> Vec<u8> {
        fields.get(idx).cloned().unwrap_or_default()
    };

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < la.len() && j < lb.len() {
        let ka = key(&la[i], f1);
        let kb = key(&lb[j], f2);
        match ka.cmp(&kb) {
            core::cmp::Ordering::Less => i += 1,
            core::cmp::Ordering::Greater => j += 1,
            core::cmp::Ordering::Equal => {
                // Gather the equal-key runs on both sides, emit the product.
                let mut ie = i;
                while ie < la.len() && key(&la[ie], f1) == ka {
                    ie += 1;
                }
                let mut je = j;
                while je < lb.len() && key(&lb[je], f2) == kb {
                    je += 1;
                }
                for ra in &la[i..ie] {
                    for rb in &lb[j..je] {
                        emit_join(&mut out, &ka, ra, f1, rb, f2, sep);
                    }
                }
                i = ie;
                j = je;
            }
        }
    }
    out
}

fn emit_join(
    out: &mut Vec<u8>,
    key: &[u8],
    ra: &[Vec<u8>],
    f1: usize,
    rb: &[Vec<u8>],
    f2: usize,
    sep: u8,
) {
    out.extend_from_slice(key);
    for (idx, f) in ra.iter().enumerate() {
        if idx != f1 {
            out.push(sep);
            out.extend_from_slice(f);
        }
    }
    for (idx, f) in rb.iter().enumerate() {
        if idx != f2 {
            out.push(sep);
            out.extend_from_slice(f);
        }
    }
    out.push(b'\n');
}

/// `split [-l N] [-b N] [INPUT] [PREFIX]` — carve `input` into pieces named
/// `xaa`, `xab`, … (prefix overridable by the trailing positional). Returns
/// `(name, chunk)` pairs for the shell to write. Defaults to `-l 1000` like GNU.
///
/// Note: the first positional is treated as INPUT (ignored here — the shell
/// already supplied the bytes), and only a *second* positional is used as the
/// prefix, matching the GNU `split [INPUT [PREFIX]]` argument order.
pub fn split(args: &[&str], input: &[u8]) -> Vec<(String, Vec<u8>)> {
    let parsed = Args::parse(args, &['l', 'b']);
    // GNU: prefix is the 2nd positional; default "x".
    let prefix = parsed.positional.get(1).cloned().unwrap_or_else(|| "x".to_string());

    let chunks: Vec<Vec<u8>> = if let Some(bspec) = parsed.opt("b") {
        let n = parse_size(bspec).max(1);
        input.chunks(n).map(|c| c.to_vec()).collect()
    } else {
        let n = parsed.opt("l").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1000).max(1);
        // line-mode: keep the newline with each line.
        let mut chunks = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut count = 0usize;
        for &byte in input {
            cur.push(byte);
            if byte == b'\n' {
                count += 1;
                if count == n {
                    chunks.push(core::mem::take(&mut cur));
                    count = 0;
                }
            }
        }
        if !cur.is_empty() {
            chunks.push(cur);
        }
        chunks
    };

    chunks
        .into_iter()
        .enumerate()
        .map(|(idx, data)| (alloc::format!("{prefix}{}", suffix(idx)), data))
        .collect()
}

/// GNU default 2-letter suffix: aa, ab, …, az, ba, … (plain base-26, min two
/// letters). Past `zz` (index 675) it naturally grows to three letters.
fn suffix(mut idx: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push((b'a' + (idx % 26) as u8) as char);
        idx /= 26;
        if idx == 0 {
            break;
        }
    }
    while letters.len() < 2 {
        letters.push('a');
    }
    letters.iter().rev().collect()
}

/// Parse a size like `512`, `1k`, `1K`, `2m`, `1g` (powers of 1024).
fn parse_size(s: &str) -> usize {
    let (num, mult) = match s.chars().last() {
        Some('k') | Some('K') => (&s[..s.len() - 1], 1024),
        Some('m') | Some('M') => (&s[..s.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    // checked_mul: a huge `-b 999999999999g` must not overflow (panic in debug / wrap in
    // release) — saturate to usize::MAX instead (one giant chunk, no UB).
    num.parse::<usize>().unwrap_or(0).saturating_mul(mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn comm_default() {
        // a: apple banana cherry ; b: banana cherry date
        let a = b"apple\nbanana\ncherry\n";
        let b = b"banana\ncherry\ndate\n";
        // col1 only-a (apple), col2 only-b (date, indent 1 tab), col3 both (indent 2 tabs)
        assert_eq!(
            s(comm(&[], a, b)),
            "apple\n\t\tbanana\n\t\tcherry\n\tdate\n"
        );
    }

    #[test]
    fn comm_suppress() {
        let a = b"apple\nbanana\ncherry\n";
        let b = b"banana\ncherry\ndate\n";
        // -12 -> only column 3 shown, no indent (no columns shown before it)
        assert_eq!(s(comm(&["-1", "-2"], a, b)), "banana\ncherry\n");
        // -3 -> drop the common column; col1 shown, col2 indented by 1
        assert_eq!(s(comm(&["-3"], a, b)), "apple\n\tdate\n");
        // -23 -> only column 1, no indent
        assert_eq!(s(comm(&["-2", "-3"], a, b)), "apple\n");
    }

    #[test]
    fn join_default() {
        let a = b"1 apple\n2 banana\n3 cherry\n";
        let b = b"1 red\n2 yellow\n4 blue\n";
        assert_eq!(s(join(&[], a, b)), "1 apple red\n2 banana yellow\n");
    }

    #[test]
    fn join_fields_and_delim() {
        // join on field 2 of a, field 1 of b
        let a = b"apple 1\nbanana 2\n";
        let b = b"1 red\n2 yellow\n";
        assert_eq!(s(join(&["-1", "2", "-2", "1"], a, b)), "1 apple red\n2 banana yellow\n");
        // tab delimiter via -t
        let a2 = b"1:apple\n2:banana\n";
        let b2 = b"1:red\n2:yellow\n";
        assert_eq!(s(join(&["-t", ":"], a2, b2)), "1:apple:red\n2:banana:yellow\n");
    }

    #[test]
    fn join_cartesian() {
        let a = b"1 a\n1 b\n";
        let b = b"1 x\n1 y\n";
        assert_eq!(s(join(&[], a, b)), "1 a x\n1 a y\n1 b x\n1 b y\n");
    }

    #[test]
    fn split_lines() {
        let input = b"l1\nl2\nl3\nl4\nl5\n";
        let pieces = split(&["-l", "2"], input);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].0, "xaa");
        assert_eq!(pieces[1].0, "xab");
        assert_eq!(pieces[2].0, "xac");
        assert_eq!(s(pieces[0].1.clone()), "l1\nl2\n");
        assert_eq!(s(pieces[1].1.clone()), "l3\nl4\n");
        assert_eq!(s(pieces[2].1.clone()), "l5\n");
    }

    #[test]
    fn split_bytes_and_prefix() {
        let input = b"abcdefg";
        // first positional is INPUT (ignored), second is the prefix
        let pieces = split(&["-b", "3", "in.txt", "part_"], input);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].0, "part_aa");
        assert_eq!(s(pieces[0].1.clone()), "abc");
        assert_eq!(s(pieces[1].1.clone()), "def");
        assert_eq!(s(pieces[2].1.clone()), "g");
    }

    #[test]
    fn suffix_sequence() {
        assert_eq!(suffix(0), "aa");
        assert_eq!(suffix(1), "ab");
        assert_eq!(suffix(25), "az");
        assert_eq!(suffix(26), "ba");
        assert_eq!(suffix(27), "bb");
    }
}
