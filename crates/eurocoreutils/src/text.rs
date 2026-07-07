//! Text-processing commands (CU-3 + CU-4): head/tail/wc/tac/rev/nl/fold/cat +
//! sort/uniq/cut/tr. All purely `fn(args, input) -> Vec<u8>`.

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::args::Args;
use crate::{join_lines, lines};

/// `head [-n N]` — first N lines (default 10).
/// GNU short form `-N` (e.g. `head -2`, `tail -5`): take the number from a `-<digits>` token.
fn numeric_shorthand(args: &[&str]) -> Option<usize> {
    args.iter().find_map(|a| {
        let s = a.strip_prefix('-')?;
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    })
}

pub fn head(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &['n', 'c']);
    let rows = lines(input);
    if let Some(c) = a.opt("c") {
        let n = c.parse::<usize>().unwrap_or(0).min(input.len());
        return input[..n].to_vec();
    }
    let default = numeric_shorthand(args).unwrap_or(10); // `head -2` short form
    let n = a.num("n", default).min(rows.len());
    join_lines(&rows[..n].iter().map(|r| r.to_vec()).collect::<Vec<_>>())
}

/// `tail [-n N]` — last N lines (default 10).
pub fn tail(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &['n', 'c']);
    let rows = lines(input);
    if let Some(c) = a.opt("c") {
        let n = c.parse::<usize>().unwrap_or(0).min(input.len());
        return input[input.len() - n..].to_vec();
    }
    let default = numeric_shorthand(args).unwrap_or(10); // `tail -2` short form
    let n = a.num("n", default).min(rows.len());
    let start = rows.len() - n;
    join_lines(&rows[start..].iter().map(|r| r.to_vec()).collect::<Vec<_>>())
}

/// `wc [-l|-w|-c]` — count lines/words/bytes. Without flags: all three + total.
pub fn wc(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    let l = lines(input).len();
    let w = input.split(|b| b.is_ascii_whitespace()).filter(|s| !s.is_empty()).count();
    let c = input.len();
    let mut out = alloc::string::String::new();
    let any = a.flag('l') || a.flag('w') || a.flag('c');
    if a.flag('l') || !any {
        out.push_str(&alloc::format!("{l:>8}"));
    }
    if a.flag('w') || !any {
        out.push_str(&alloc::format!("{w:>8}"));
    }
    if a.flag('c') || !any {
        out.push_str(&alloc::format!("{c:>8}"));
    }
    out.push('\n');
    out.into_bytes()
}

/// `tac` — lines in reverse order.
pub fn tac(_args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut rows: Vec<Vec<u8>> = lines(input).iter().map(|r| r.to_vec()).collect();
    rows.reverse();
    join_lines(&rows)
}

/// `rev` — reverse the characters of each line.
pub fn rev(_args: &[&str], input: &[u8]) -> Vec<u8> {
    let rows: Vec<Vec<u8>> = lines(input)
        .iter()
        .map(|r| {
            let mut v = r.to_vec();
            v.reverse();
            v
        })
        .collect();
    join_lines(&rows)
}

/// `nl` — number non-empty lines (GNU default: right-aligned, tab-separated).
pub fn nl(_args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut n = 0;
    for row in lines(input) {
        if row.is_empty() {
            out.push(b'\n');
        } else {
            n += 1;
            out.extend_from_slice(alloc::format!("{n:>6}\t").as_bytes());
            out.extend_from_slice(row);
            out.push(b'\n');
        }
    }
    out
}

/// `fold [-w N]` — wrap lines at a given width (default 80).
pub fn fold(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &['w']);
    let w = a.num("w", 80).max(1);
    let mut out = Vec::new();
    for row in lines(input) {
        let mut i = 0;
        while i < row.len() {
            let end = (i + w).min(row.len());
            out.extend_from_slice(&row[i..end]);
            out.push(b'\n');
            i = end;
        }
        if row.is_empty() {
            out.push(b'\n');
        }
    }
    out
}

/// `cat [-n]` — passthrough; with `-n` number the lines.
pub fn cat(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    if !a.flag('n') {
        return input.to_vec();
    }
    let mut out = Vec::new();
    for (i, row) in lines(input).iter().enumerate() {
        out.extend_from_slice(alloc::format!("{:>6}\t", i + 1).as_bytes());
        out.extend_from_slice(row);
        out.push(b'\n');
    }
    out
}

/// `sort [-r] [-n] [-u]` — sort lines (reverse / numeric / unique).
pub fn sort(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    let mut rows: Vec<Vec<u8>> = lines(input).iter().map(|r| r.to_vec()).collect();
    if a.flag('n') {
        rows.sort_by_key(|r| core::str::from_utf8(r).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(i64::MIN));
    } else {
        rows.sort();
    }
    if a.flag('r') {
        rows.reverse();
    }
    if a.flag('u') {
        rows.dedup();
    }
    join_lines(&rows)
}

/// `uniq [-c] [-d]` — remove/mark consecutive duplicate lines.
pub fn uniq(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    let rows = lines(input);
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let mut count = 1;
        while i + count < rows.len() && rows[i + count] == rows[i] {
            count += 1;
        }
        let is_dup = count > 1;
        if !(a.flag('d') && !is_dup) {
            if a.flag('c') {
                out.extend_from_slice(alloc::format!("{count:>7} ").as_bytes());
            }
            out.extend_from_slice(rows[i]);
            out.push(b'\n');
        }
        i += count;
    }
    out
}

/// `cut -d DELIM -f N` (fields) or `cut -c N-M` (characters, 1-based).
pub fn cut(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &['d', 'f', 'c']);
    let mut out = Vec::new();
    if let Some(cspec) = a.opt("c") {
        let (lo, hi) = parse_range(cspec);
        for row in lines(input) {
            let end = hi.min(row.len());
            let start = lo.saturating_sub(1).min(row.len());
            if start < end {
                out.extend_from_slice(&row[start..end]);
            }
            out.push(b'\n');
        }
        return out;
    }
    let delim = a.opt("d").and_then(|d| d.bytes().next()).unwrap_or(b'\t');
    let field = a.num("f", 1).max(1);
    for row in lines(input) {
        let parts: Vec<&[u8]> = row.split(|&b| b == delim).collect();
        if let Some(p) = parts.get(field - 1) {
            out.extend_from_slice(p);
        }
        out.push(b'\n');
    }
    out
}

fn parse_range(spec: &str) -> (usize, usize) {
    if let Some((a, b)) = spec.split_once('-') {
        let lo = a.parse().unwrap_or(1);
        let hi = b.parse().unwrap_or(usize::MAX);
        (lo, hi)
    } else {
        let n = spec.parse().unwrap_or(1);
        (n, n)
    }
}

/// `tr SET1 [SET2]` (translate) or `tr -d SET1` (delete).
pub fn tr(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &[]);
    let pos = &a.positional;
    if a.flag('d') {
        let del = pos.first().map(|s| s.as_bytes().to_vec()).unwrap_or_default();
        return input.iter().copied().filter(|b| !del.contains(b)).collect();
    }
    let set1 = pos.first().map(|s| s.as_bytes().to_vec()).unwrap_or_default();
    let set2 = pos.get(1).map(|s| s.as_bytes().to_vec()).unwrap_or_default();
    input
        .iter()
        .map(|&b| match set1.iter().position(|&c| c == b) {
            Some(i) if !set2.is_empty() => set2[i.min(set2.len() - 1)],
            _ => b,
        })
        .collect()
}

/// `grep [-i] [-v] [-n] [-c] PATTERN` — print lines that contain `PATTERN`
/// (substring). `-i` case-insensitive, `-v` invert, `-n` line numbers, `-c` count only.
pub fn grep(args: &[&str], input: &[u8]) -> Vec<u8> {
    use alloc::string::String;
    let a = Args::parse(args, &[]);
    let pattern = a.positional.first().cloned().unwrap_or_default();
    let ignore = a.flag('i');
    let pat = if ignore { pattern.to_lowercase() } else { pattern.clone() };
    let mut out = Vec::new();
    let mut matches = 0usize;
    for (i, row) in lines(input).iter().enumerate() {
        let hay = String::from_utf8_lossy(row);
        let hay_cmp = if ignore { hay.to_lowercase() } else { hay.to_string() };
        let found = !pat.is_empty() && hay_cmp.contains(pat.as_str());
        if found != a.flag('v') {
            matches += 1;
            if !a.flag('c') {
                if a.flag('n') {
                    out.extend_from_slice(alloc::format!("{}:", i + 1).as_bytes());
                }
                out.extend_from_slice(row);
                out.push(b'\n');
            }
        }
    }
    if a.flag('c') {
        out.extend_from_slice(alloc::format!("{matches}\n").as_bytes());
    }
    out
}

/// `shuf [-n N] [-e ARGS...] [-i LO-HI]` — randomly permute the input lines.
///
/// DETERMINISM (by design): there is no entropy source in the kernel, so this is
/// a deterministic pseudo-shuffle. A 64-bit xorshift PRNG is seeded from a fixed
/// constant XORed with the input length, so the same input always yields the same
/// permutation. This is intentional (host-testable, reproducible) and is NOT a
/// cryptographic or security-grade shuffle.
///
/// Modes:
/// - default: shuffle the lines of `input`.
/// - `-e ARG...`: treat the positional args as the lines to shuffle (ignore input).
/// - `-i LO-HI`: shuffle the integer range `LO..=HI`.
/// - `-n N`: output at most N lines (head of the shuffled result).
pub fn shuf(args: &[&str], input: &[u8]) -> Vec<u8> {
    let a = Args::parse(args, &['n', 'i']);

    // Build the candidate item set.
    let mut items: Vec<Vec<u8>> = if let Some(range) = a.opt("i") {
        // `-i LO-HI`: bound the materialised range. An open-ended `-i 5-` (HI = usize::MAX)
        // or a huge range would otherwise try to allocate billions of strings (OOM/hang).
        const SHUF_RANGE_CAP: usize = 1_000_000;
        let (lo, hi) = parse_range(range);
        if hi < lo {
            Vec::new()
        } else {
            let span = hi - lo; // inclusive count = span + 1
            let capped_hi = if span >= SHUF_RANGE_CAP { lo + SHUF_RANGE_CAP - 1 } else { hi };
            (lo..=capped_hi).map(|n| n.to_string().into_bytes()).collect()
        }
    } else if a.flag('e') {
        a.positional.iter().map(|p| p.clone().into_bytes()).collect()
    } else {
        lines(input).iter().map(|r| r.to_vec()).collect()
    };

    // Deterministic seed: fixed constant XOR item count XOR total byte length.
    let total_len: usize = items.iter().map(|i| i.len()).sum();
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15 ^ (items.len() as u64) ^ ((total_len as u64) << 17);
    if state == 0 {
        state = 0xDEAD_BEEF_CAFE_F00D; // xorshift must not be seeded with 0
    }
    let mut next = || {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    // Fisher–Yates with the deterministic PRNG.
    let n = items.len();
    for i in (1..n).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }

    let limit = a.opt("n").and_then(|v| v.parse::<usize>().ok()).unwrap_or(items.len()).min(items.len());
    join_lines(&items[..limit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn head_tail() {
        let inp = b"1\n2\n3\n4\n5\n";
        assert_eq!(s(head(&["-n", "2"], inp)), "1\n2\n");
        assert_eq!(s(tail(&["-n", "2"], inp)), "4\n5\n");
        // GNU short form `-N`.
        assert_eq!(s(head(&["-2"], inp)), "1\n2\n");
        assert_eq!(s(tail(&["-2"], inp)), "4\n5\n");
        assert_eq!(s(head(&[], b"a\nb\n")), "a\nb\n"); // <10 lines -> everything
    }

    #[test]
    fn wc_counts() {
        let out = s(wc(&["-l"], b"a\nb\nc\n"));
        assert_eq!(out.trim(), "3");
        assert_eq!(s(wc(&["-w"], b"een twee drie")).trim(), "3");
        assert_eq!(s(wc(&["-c"], b"abcd")).trim(), "4");
    }

    #[test]
    fn tac_rev_nl() {
        assert_eq!(s(tac(&[], b"a\nb\nc\n")), "c\nb\na\n");
        assert_eq!(s(rev(&[], b"abc\n")), "cba\n");
        assert_eq!(s(nl(&[], b"x\ny\n")), "     1\tx\n     2\ty\n");
    }

    #[test]
    fn sort_uniq() {
        assert_eq!(s(sort(&[], b"kat\naap\nbaar\n")), "aap\nbaar\nkat\n");
        assert_eq!(s(sort(&["-r"], b"a\nb\nc\n")), "c\nb\na\n");
        assert_eq!(s(sort(&["-n"], b"10\n2\n1\n")), "1\n2\n10\n");
        assert_eq!(s(uniq(&[], b"a\na\nb\n")), "a\nb\n");
        assert_eq!(s(uniq(&["-c"], b"a\na\nb\n")).replace("  ", ""), "2 a\n1 b\n".replace("  ", ""));
    }

    #[test]
    fn cut_tr() {
        assert_eq!(s(cut(&["-d", ":", "-f", "2"], b"a:b:c\nx:y:z\n")), "b\ny\n");
        assert_eq!(s(cut(&["-c", "1-3"], b"abcdef\n")), "abc\n");
        assert_eq!(s(tr(&["abc", "ABC"], b"abc")), "ABC"); // literal set mapping
        assert_eq!(s(tr(&["abc", "xyz"], b"aabbcc")), "xxyyzz");
        assert_eq!(s(tr(&["-d", "b"], b"aabbcc")), "aacc");
    }

    #[test]
    fn fold_w() {
        assert_eq!(s(fold(&["-w", "3"], b"abcdefg\n")), "abc\ndef\ng\n");
    }

    #[test]
    fn shuf_is_deterministic_permutation() {
        let inp = b"a\nb\nc\nd\ne\n";
        let out1 = s(shuf(&[], inp));
        let out2 = s(shuf(&[], inp));
        // Deterministic: identical across calls.
        assert_eq!(out1, out2);
        // A permutation: same multiset of lines.
        let mut got: Vec<&str> = out1.lines().collect();
        let mut want = alloc::vec!["a", "b", "c", "d", "e"];
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn shuf_n_limit() {
        let inp = b"a\nb\nc\nd\ne\n";
        let out = s(shuf(&["-n", "3"], inp));
        assert_eq!(out.lines().count(), 3);
        // The head-of-shuffled is a prefix of the full shuffle.
        let full = s(shuf(&[], inp));
        let full3: String = full.lines().take(3).map(|l| alloc::format!("{l}\n")).collect();
        assert_eq!(out, full3);
    }

    #[test]
    fn shuf_e_and_i() {
        // -e: args are the items.
        let out = s(shuf(&["-e", "x", "y", "z"], b""));
        let mut got: Vec<&str> = out.lines().collect();
        got.sort();
        assert_eq!(got, alloc::vec!["x", "y", "z"]);
        // -i LO-HI: integer range.
        let out = s(shuf(&["-i", "1-5"], b""));
        let mut got: Vec<i32> = out.lines().map(|l| l.parse().unwrap()).collect();
        got.sort();
        assert_eq!(got, alloc::vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn grep_match() {
        let inp = b"appel\nbanaan\nAppelmoes\nperen\n";
        assert_eq!(s(grep(&["appel"], inp)), "appel\n");
        assert_eq!(s(grep(&["-i", "appel"], inp)), "appel\nAppelmoes\n");
        assert_eq!(s(grep(&["-v", "appel"], inp)), "banaan\nAppelmoes\nperen\n");
        assert_eq!(s(grep(&["-c", "-i", "appel"], inp)).trim(), "2");
        assert_eq!(s(grep(&["-n", "peren"], inp)), "4:peren\n");
    }
}
