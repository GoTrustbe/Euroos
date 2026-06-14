//! Shared argument parser (CU-0): short flags (`-r`), combined flags
//! (`-rf`), long options (`--max-depth=3` / `-n 5`) and positional arguments.

use alloc::string::String;
use alloc::vec::Vec;

pub struct Args {
    pub flags: Vec<char>,
    pub options: Vec<(String, String)>,
    pub positional: Vec<String>,
}

impl Args {
    /// Parse the arguments. Options that expect a value (`value_opts`, e.g. `n`,
    /// `c`, `d`, `f`, `w`) take the NEXT token as their value (`-n 5`).
    pub fn parse(input: &[&str], value_opts: &[char]) -> Args {
        let mut a = Args { flags: Vec::new(), options: Vec::new(), positional: Vec::new() };
        let mut i = 0;
        while i < input.len() {
            let tok = input[i];
            if let Some(long) = tok.strip_prefix("--") {
                if let Some((k, v)) = long.split_once('=') {
                    a.options.push((k.into(), v.into()));
                } else {
                    a.options.push((long.into(), String::new()));
                }
            } else if tok.len() >= 2 && tok.starts_with('-') && tok != "-" {
                let chars: Vec<char> = tok[1..].chars().collect();
                let mut j = 0;
                while j < chars.len() {
                    let c = chars[j];
                    if value_opts.contains(&c) {
                        // Value: the rest of this token, or the next token.
                        let rest: String = chars[j + 1..].iter().collect();
                        if !rest.is_empty() {
                            a.options.push((c.to_string(), rest));
                        } else if i + 1 < input.len() {
                            i += 1;
                            a.options.push((c.to_string(), input[i].into()));
                        }
                        break; // the rest of this token is consumed
                    } else {
                        a.flags.push(c);
                    }
                    j += 1;
                }
            } else {
                a.positional.push(tok.into());
            }
            i += 1;
        }
        a
    }

    pub fn flag(&self, c: char) -> bool {
        self.flags.contains(&c)
    }

    pub fn opt(&self, name: &str) -> Option<&str> {
        self.options.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Numeric option (e.g. `-n 5`); falls back to `default`.
    pub fn num(&self, name: &str, default: usize) -> usize {
        self.opt(name).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

use alloc::string::ToString;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags_options_positional() {
        let a = Args::parse(&["-rf", "-n", "5", "--max-depth=3", "file.txt"], &['n']);
        assert!(a.flag('r') && a.flag('f'));
        assert_eq!(a.num("n", 10), 5);
        assert_eq!(a.opt("max-depth"), Some("3"));
        assert_eq!(a.positional, alloc::vec!["file.txt".to_string()]);
    }

    #[test]
    fn attached_value() {
        let a = Args::parse(&["-n3"], &['n']);
        assert_eq!(a.num("n", 10), 3);
    }
}
