//! EuroCoreutils — GNU-compatible coreutils core for EuroOS.
//!
//! Each command is a pure function `fn(args, input) -> Vec<u8>` (or `-> (Vec<u8>,
//! i32)` where the exit code matters): option flags + the stdin bytes go in, the output
//! comes out. The shell ([`kernel::shell`]) optionally reads a file from EuroFS as
//! `input` and prints the output. This way all text logic is `no_std` + fully host-tested
//! against the expected GNU output, without QEMU.
//!
//! Batches: CU-1 (trivial) · CU-3 (text I/O) · CU-4 (transform) · CU-6 (checksums &
//! encoding). FS mutations (cp/mv/touch/…) live on the shell side on the EuroFS primitives.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::ToString as _; // trait in scope for i64/&str .to_string()
use alloc::vec::Vec;

pub mod args;
pub mod checksum;
pub mod compute;
pub mod encoding;
pub mod find;
pub mod text;

pub use args::Args;

/// Splits `input` into lines (without the trailing `\n`), keeps empty lines.
pub(crate) fn lines(input: &[u8]) -> Vec<&[u8]> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&[u8]> = input.split(|&b| b == b'\n').collect();
    // A trailing newline yields an empty last split → drop it.
    if input.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

/// Join lines together with a trailing newline per line.
pub(crate) fn join_lines(rows: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in rows {
        out.extend_from_slice(r);
        out.push(b'\n');
    }
    out
}

// ── CU-1: trivial commands ───────────────────────────────────────────────────

/// `echo [-n] [-e] ARGS...` — prints the arguments separated by spaces.
pub fn echo(args: &[&str]) -> Vec<u8> {
    let mut no_newline = false;
    let mut escapes = false;
    let mut start = 0;
    for a in args {
        match *a {
            "-n" => no_newline = true,
            "-e" => escapes = true,
            "-ne" | "-en" => {
                no_newline = true;
                escapes = true;
            }
            _ => break,
        }
        start += 1;
    }
    let joined = args[start..].join(" ");
    let mut out: Vec<u8> = if escapes { unescape(&joined) } else { joined.into_bytes() };
    if !no_newline {
        out.push(b'\n');
    }
    out
}

fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'\\' => out.push(b'\\'),
                b'0' => out.push(0),
                other => {
                    out.push(b'\\');
                    out.push(other);
                }
            }
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    out
}

/// `seq [FIRST [STEP]] LAST` — a range of numbers, one per line.
pub fn seq(args: &[&str]) -> Vec<u8> {
    let nums: Vec<i64> = args.iter().filter_map(|a| a.parse::<i64>().ok()).collect();
    let (first, step, last) = match nums.len() {
        1 => (1, 1, nums[0]),
        2 => (nums[0], 1, nums[1]),
        3 => (nums[0], nums[1], nums[2]),
        _ => return b"seq: usage: seq [FIRST [STEP]] LAST\n".to_vec(),
    };
    let mut out = Vec::new();
    if step == 0 {
        return out;
    }
    let mut v = first;
    while (step > 0 && v <= last) || (step < 0 && v >= last) {
        out.extend_from_slice(v.to_string().as_bytes());
        out.push(b'\n');
        v += step;
    }
    out
}

/// `basename PATH [SUFFIX]` — the last path component (without suffix).
pub fn basename(args: &[&str]) -> Vec<u8> {
    let path = args.first().copied().unwrap_or("");
    let trimmed = path.trim_end_matches('/');
    let mut base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if base.is_empty() {
        base = "/";
    }
    let mut s = base.to_string();
    if let Some(suf) = args.get(1) {
        if s.ends_with(suf) && s.len() > suf.len() {
            s.truncate(s.len() - suf.len());
        }
    }
    s.push('\n');
    s.into_bytes()
}

/// `dirname PATH` — the path without the last component.
pub fn dirname(args: &[&str]) -> Vec<u8> {
    let path = args.first().copied().unwrap_or("");
    let trimmed = path.trim_end_matches('/');
    let dir = match trimmed.rfind('/') {
        Some(0) => "/",
        Some(i) => &trimmed[..i],
        None => ".",
    };
    let mut s = dir.to_string();
    s.push('\n');
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn echo_basic() {
        assert_eq!(s(echo(&["hallo", "wereld"])), "hallo wereld\n");
        assert_eq!(s(echo(&["-n", "x"])), "x");
        assert_eq!(s(echo(&["-e", "a\\nb"])), "a\nb\n");
    }

    #[test]
    fn seq_forms() {
        assert_eq!(s(seq(&["3"])), "1\n2\n3\n");
        assert_eq!(s(seq(&["2", "4"])), "2\n3\n4\n");
        assert_eq!(s(seq(&["1", "2", "5"])), "1\n3\n5\n");
        assert_eq!(s(seq(&["5", "-2", "1"])), "5\n3\n1\n");
    }

    #[test]
    fn basename_dirname() {
        assert_eq!(s(basename(&["/usr/bin/euro"])), "euro\n");
        assert_eq!(s(basename(&["/usr/bin/euro.efi", ".efi"])), "euro\n");
        assert_eq!(s(basename(&["/"])), "/\n");
        assert_eq!(s(dirname(&["/usr/bin/euro"])), "/usr/bin\n");
        assert_eq!(s(dirname(&["/usr"])), "/\n");
        assert_eq!(s(dirname(&["euro"])), ".\n");
    }
}
