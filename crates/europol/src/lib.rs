//! EuroPol — a **declarative policy engine** (plan X).
//!
//! EuroGuard capabilities are a technically correct enforcement layer, but
//! administrators do not think in bits — they think in policy: *"this application may not
//! access the internet."* EuroPol translates a readable policy (TOML-like) into an
//! **EuroGuard capability mask** + path/network rules, with `[allow]`/`[deny]`
//! sections where **deny always wins**. The kernel enforces the result in the
//! syscall path (the existing capability check). Pure `no_std` logic → host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ── EuroGuard capability bits (mirrored from `kernel::ring3`) ────────────────
pub const CAP_CONSOLE: u64 = 1 << 0;
pub const CAP_PROC_INFO: u64 = 1 << 1;
pub const CAP_FILE: u64 = 1 << 2;
pub const CAP_NET: u64 = 1 << 3;
pub const CAP_IMMUTABLE_ADMIN: u64 = 1 << 4;

/// Map a capability name to its bit.
pub fn cap_bit(name: &str) -> Option<u64> {
    Some(match name.trim().trim_matches('"') {
        "CAP_CONSOLE" => CAP_CONSOLE,
        "CAP_PROC_INFO" => CAP_PROC_INFO,
        "CAP_FILE" => CAP_FILE,
        "CAP_NET" => CAP_NET,
        "CAP_IMMUTABLE_ADMIN" => CAP_IMMUTABLE_ADMIN,
        _ => return None,
    })
}

/// The name of a (single) capability bit — for `explain`/listings.
pub fn cap_name(bit: u64) -> &'static str {
    match bit {
        CAP_CONSOLE => "CAP_CONSOLE",
        CAP_PROC_INFO => "CAP_PROC_INFO",
        CAP_FILE => "CAP_FILE",
        CAP_NET => "CAP_NET",
        CAP_IMMUTABLE_ADMIN => "CAP_IMMUTABLE_ADMIN",
        _ => "CAP_?",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// A compiled policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub name: String,
    pub allow_caps: u64,
    pub deny_caps: u64,
    pub allow_paths: Vec<String>,
    pub deny_paths: Vec<String>,
    pub log_denied: bool,
}

impl Policy {
    /// The effective capability mask: add `allow` to `base`, then subtract `deny`.
    /// **Deny always wins** (even over a granted base capability).
    pub fn effective_caps(&self, base: u64) -> u64 {
        (base | self.allow_caps) & !self.deny_caps
    }

    /// Is this capability allowed? (deny bit wins; otherwise allow bit; otherwise default-deny).
    pub fn check_cap(&self, cap: u64) -> Decision {
        if self.deny_caps & cap != 0 {
            Decision::Deny
        } else if self.allow_caps & cap != 0 {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }

    /// Is this path allowed? A `deny` prefix wins; otherwise an `allow` prefix; otherwise deny.
    pub fn check_path(&self, path: &str) -> Decision {
        if self.deny_paths.iter().any(|p| path.starts_with(p.as_str())) {
            return Decision::Deny;
        }
        if self.allow_paths.iter().any(|p| path.starts_with(p.as_str())) {
            return Decision::Allow;
        }
        Decision::Deny
    }

    /// Explain WHY a capability is allowed/denied (for `europol explain`).
    pub fn explain_cap(&self, cap: u64) -> String {
        let n = cap_name(cap);
        if self.deny_caps & cap != 0 {
            alloc::format!("{n}: DENIED by [deny].capabilities in policy '{}'", self.name)
        } else if self.allow_caps & cap != 0 {
            alloc::format!("{n}: allowed by [allow].capabilities in policy '{}'", self.name)
        } else {
            alloc::format!("{n}: DENIED (not in [allow] of policy '{}' — default-deny)", self.name)
        }
    }
}

/// Parse a policy text (TOML-like): `name = "..."`, sections `[allow]`/`[deny]`,
/// and keys `capabilities = ["CAP_X", ...]` / `paths = ["/a", "/b"]` /
/// `log_denied = true`.
pub fn parse(text: &str) -> Policy {
    let mut p = Policy::default();
    let mut section = ""; // "allow" | "deny" | ""
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = match line.trim_matches(['[', ']']) {
                "allow" => "allow",
                "deny" => "deny",
                _ => "",
            };
            continue;
        }
        let (key, val) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        match key {
            "name" => p.name = String::from(val.trim_matches('"')),
            "log_denied" => p.log_denied = val == "true",
            "capabilities" => {
                let mask = parse_array(val).iter().filter_map(|s| cap_bit(s)).fold(0u64, |a, b| a | b);
                if section == "deny" {
                    p.deny_caps |= mask;
                } else {
                    p.allow_caps |= mask;
                }
            }
            "paths" => {
                let list: Vec<String> = parse_array(val).iter().map(|s| String::from(s.trim_matches('"'))).collect();
                if section == "deny" {
                    p.deny_paths.extend(list);
                } else {
                    p.allow_paths.extend(list);
                }
            }
            _ => {}
        }
    }
    p
}

/// Parse a TOML array `["a", "b", "c"]` → the individual elements (without quotes).
fn parse_array(val: &str) -> Vec<String> {
    val.trim_matches(['[', ']'])
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str = r#"
        name = "firefox"
        [allow]
        capabilities = ["CAP_NET", "CAP_FILE", "CAP_CONSOLE"]
        paths = ["/home/user", "/tmp"]
        [deny]
        capabilities = ["CAP_IMMUTABLE_ADMIN"]
        paths = ["/etc", "/boot"]
        log_denied = true
    "#;

    #[test]
    fn parse_and_effective_caps() {
        let p = parse(FIREFOX);
        assert_eq!(p.name, "firefox");
        assert!(p.log_denied);
        assert_eq!(p.allow_caps, CAP_NET | CAP_FILE | CAP_CONSOLE);
        assert_eq!(p.deny_caps, CAP_IMMUTABLE_ADMIN);
        // Effective: a process with ALL rights loses CAP_IMMUTABLE_ADMIN.
        let all = CAP_CONSOLE | CAP_PROC_INFO | CAP_FILE | CAP_NET | CAP_IMMUTABLE_ADMIN;
        let eff = p.effective_caps(all);
        assert_eq!(eff & CAP_IMMUTABLE_ADMIN, 0); // deny wins
        assert_ne!(eff & CAP_NET, 0);
    }

    #[test]
    fn deny_wins_over_allow() {
        // Same cap in both allow and deny → denied.
        let p = parse("name=\"x\"\n[allow]\ncapabilities=[\"CAP_NET\"]\n[deny]\ncapabilities=[\"CAP_NET\"]");
        assert_eq!(p.check_cap(CAP_NET), Decision::Deny);
        assert_eq!(p.effective_caps(CAP_NET) & CAP_NET, 0);
    }

    #[test]
    fn cap_checks() {
        let p = parse(FIREFOX);
        assert_eq!(p.check_cap(CAP_NET), Decision::Allow);
        assert_eq!(p.check_cap(CAP_IMMUTABLE_ADMIN), Decision::Deny);
        assert_eq!(p.check_cap(CAP_PROC_INFO), Decision::Deny); // not in allow → default-deny
    }

    #[test]
    fn path_checks() {
        let p = parse(FIREFOX);
        assert_eq!(p.check_path("/home/user/.firefox/prefs.js"), Decision::Allow);
        assert_eq!(p.check_path("/etc/shadow"), Decision::Deny);
        assert_eq!(p.check_path("/boot/loader"), Decision::Deny);
        assert_eq!(p.check_path("/var/random"), Decision::Deny); // not in allow
    }

    #[test]
    fn explain_is_human_readable() {
        let p = parse(FIREFOX);
        assert!(p.explain_cap(CAP_NET).contains("allowed"));
        assert!(p.explain_cap(CAP_IMMUTABLE_ADMIN).contains("DENIED by [deny]"));
        assert!(p.explain_cap(CAP_PROC_INFO).contains("default-deny"));
    }
}
