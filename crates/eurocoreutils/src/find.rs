//! `find` (CU-5) — the match logic, pure and host-tested. *Walking* the VFS tree
//! happens on the shell side (which has the `FileSystem`); this module decides per
//! found item whether it matches: `-name <glob>`, `-type f|d`, `-maxdepth N`.

use alloc::string::String;

/// Glob match: `*` (zero or more characters), `?` (one character), the rest literal.
/// A classic recursive matcher — enough for `-name` patterns.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    glob_bytes(pattern.as_bytes(), name.as_bytes())
}

fn glob_bytes(p: &[u8], n: &[u8]) -> bool {
    // Iterative with a single backtrack position for `*` — O(len) instead of the
    // exponential double recursion (audit H12: `*a*a*…` must not hang find).
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star_p, mut star_n): (Option<usize>, usize) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = Some(pi); // remember the `*` and backtrack to it on a mismatch
            star_n = ni;
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1; // let the `*` swallow one more character
            star_n += 1;
            ni = star_n;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// The requested filters of a `find` invocation.
pub struct FindOpts {
    pub name: Option<String>,
    /// `Some(true)` = directories only (`-type d`), `Some(false)` = files only (`-type f`).
    pub want_dir: Option<bool>,
    pub maxdepth: Option<usize>,
}

impl FindOpts {
    /// Parse `find` arguments. `-name <glob>`, `-type f|d`, `-maxdepth N`.
    pub fn parse(args: &[&str]) -> FindOpts {
        let mut o = FindOpts { name: None, want_dir: None, maxdepth: None };
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-name" => {
                    if let Some(v) = args.get(i + 1) {
                        o.name = Some(String::from(*v));
                        i += 1;
                    }
                }
                "-type" => {
                    match args.get(i + 1).copied() {
                        Some("d") => o.want_dir = Some(true),
                        Some("f") => o.want_dir = Some(false),
                        _ => {}
                    }
                    i += 1;
                }
                "-maxdepth" => {
                    if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                        o.maxdepth = Some(v);
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        o
    }

    /// The first positional arg = start path, or `.`. Skips the values of
    /// `-name`/`-type`/`-maxdepth` so they are not read as a path.
    pub fn start_path(args: &[&str]) -> String {
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-name" | "-type" | "-maxdepth" => i += 2, // skip option + value
                t if t.starts_with('-') => i += 1,
                t => return String::from(t),
            }
        }
        String::from(".")
    }

    /// Does an item at `depth` (0 = the start path itself) match these filters?
    pub fn matches(&self, name: &str, is_dir: bool, depth: usize) -> bool {
        if let Some(md) = self.maxdepth {
            if depth > md {
                return false;
            }
        }
        if let Some(want) = self.want_dir {
            if want != is_dir {
                return false;
            }
        }
        if let Some(ref pat) = self.name {
            if !glob_match(pat, name) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("*.txt", "notes.txt"));
        assert!(glob_match("*.txt", ".txt"));
        assert!(!glob_match("*.txt", "notes.md"));
        assert!(glob_match("file?", "file1"));
        assert!(!glob_match("file?", "file12"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(glob_match("euro", "euro"));
        assert!(!glob_match("euro", "eurokernel"));
    }

    #[test]
    fn opt_parsing() {
        let o = FindOpts::parse(&["/etc", "-name", "*.conf", "-type", "f", "-maxdepth", "2"]);
        assert_eq!(o.name.as_deref(), Some("*.conf"));
        assert_eq!(o.want_dir, Some(false));
        assert_eq!(o.maxdepth, Some(2));
        assert_eq!(FindOpts::start_path(&["/etc", "-name", "*.conf"]), "/etc");
        assert_eq!(FindOpts::start_path(&["-name", "x"]), ".");
    }

    #[test]
    fn matching() {
        let o = FindOpts::parse(&["-name", "*.txt", "-type", "f", "-maxdepth", "2"]);
        assert!(o.matches("a.txt", false, 1));
        assert!(!o.matches("a.txt", true, 1)); // -type f, but this is a directory
        assert!(!o.matches("a.md", false, 1)); // name does not match
        assert!(!o.matches("a.txt", false, 3)); // too deep
        let any = FindOpts::parse(&[]);
        assert!(any.matches("wat-dan-ook", true, 5));
    }
}
