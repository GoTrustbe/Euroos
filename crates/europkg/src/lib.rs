//! EuroPkg — the dependency resolver of the package manager (plan M2).
//!
//! `eupkg` builds + verifies already-signed `.eupkg` packages (ZIP + manifest +
//! SHA-256 + Ed25519). M2 adds the missing core: **semver versions +
//! constraints** and a **dependency resolver** that computes from a repository index a
//! valid, topologically ordered install order — with detection of
//! missing packages, unsatisfiable version requirements, conflicts and cycles.
//!
//! Pure, host-tested `no_std` logic. **3E-6 adds the EXECUTION**: [`store`]
//! (install/remove/upgrade on a content-addressed store, signed index) and
//! [`zipread`] (a minimal STORED-only ZIP reader, so ring 0 needs no inflate).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod store;
pub mod zipread;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// One row of the (Ed25519-signed) repository index — 3E-6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgIndexEntry {
    pub name: String,
    pub version: String,
    /// Repository path of the `.eupkg` file (fetched via FS/HTTP).
    pub file: String,
    pub deps: Vec<String>,
}

/// A semantic version `major.minor.patch`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Version { major, minor, patch }
    }
    /// `"1.2.3"` — the canonical form `parse` accepts.
    pub fn to_string_dotted(&self) -> String {
        alloc::format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
    /// Parse `"1.2.3"` (missing parts = 0, so `"1.2"` = 1.2.0).
    pub fn parse(s: &str) -> Option<Version> {
        let mut it = s.trim().split('.');
        let major = it.next()?.parse().ok()?;
        // Missing parts = 0, but a present-but-invalid part ("1.x") fails.
        let minor = match it.next() {
            Some(x) => x.parse().ok()?,
            None => 0,
        };
        let patch = match it.next() {
            Some(x) => x.parse().ok()?,
            None => 0,
        };
        if it.next().is_some() {
            return None;
        }
        Some(Version { major, minor, patch })
    }
}

/// A version requirement on a dependency.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Constraint {
    /// Any version.
    Any,
    /// Exactly this version.
    Exact(Version),
    /// This version or higher.
    AtLeast(Version),
    /// Caret: `^1.2.0` = `>=1.2.0` and same major (`<2.0.0`). Major 0 → same minor.
    Caret(Version),
}

impl Constraint {
    /// Does `v` satisfy this requirement?
    pub fn matches(&self, v: Version) -> bool {
        match self {
            Constraint::Any => true,
            Constraint::Exact(e) => v == *e,
            Constraint::AtLeast(m) => v >= *m,
            Constraint::Caret(b) => {
                if v < *b {
                    return false;
                }
                if b.major > 0 {
                    v.major == b.major
                } else if b.minor > 0 {
                    v.major == 0 && v.minor == b.minor
                } else {
                    v.major == 0 && v.minor == 0
                }
            }
        }
    }
}

/// A dependency: a package name + a version requirement.
#[derive(Clone, Debug)]
pub struct Dep {
    pub name: String,
    pub constraint: Constraint,
}

impl Dep {
    pub fn new(name: &str, constraint: Constraint) -> Dep {
        Dep { name: name.to_string(), constraint }
    }
}

/// An (available) package in the repository index.
#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub version: Version,
    pub deps: Vec<Dep>,
}

/// The repository index: all available packages (possibly multiple versions).
#[derive(Default)]
pub struct Repo {
    pub packages: Vec<Package>,
}

/// Why resolution fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// No package with this name in the index.
    NotFound(String),
    /// The package exists, but no version satisfies the requirement.
    NoMatchingVersion(String),
    /// Two requirements on the same package are incompatible (chosen versions clash).
    Conflict(String),
    /// A dependency cycle.
    Cycle(String),
}

impl Repo {
    pub fn new() -> Repo {
        Repo { packages: Vec::new() }
    }

    /// Add a package(version) to the index.
    pub fn add(&mut self, name: &str, version: Version, deps: Vec<Dep>) {
        self.packages.push(Package { name: name.to_string(), version, deps });
    }

    /// The highest version of `name` that satisfies `c`.
    fn best(&self, name: &str, c: Constraint) -> Option<&Package> {
        self.packages
            .iter()
            .filter(|p| p.name == name && c.matches(p.version))
            .max_by_key(|p| p.version)
    }

    /// Does this package exist (regardless of version)?
    fn exists(&self, name: &str) -> bool {
        self.packages.iter().any(|p| p.name == name)
    }

    /// Resolve the dependencies of `root` into a topologically ordered
    /// install order (dependencies before whoever uses them). The root comes
    /// last. Each package appears once; a repeated requirement must be
    /// compatible with the already-chosen version.
    pub fn resolve(&self, root: &str) -> Result<Vec<(String, Version)>, ResolveError> {
        let mut chosen: Vec<(String, Version)> = Vec::new();
        let mut order: Vec<(String, Version)> = Vec::new();
        let mut on_stack: Vec<String> = Vec::new();
        self.visit(root, Constraint::Any, &mut chosen, &mut order, &mut on_stack)?;
        Ok(order)
    }

    fn visit(
        &self,
        name: &str,
        c: Constraint,
        chosen: &mut Vec<(String, Version)>,
        order: &mut Vec<(String, Version)>,
        on_stack: &mut Vec<String>,
    ) -> Result<(), ResolveError> {
        // Cycle detection first: if `name` is still in the active chain, it is a cycle
        // (even if it is already in `chosen` — it is not yet fully resolved).
        if on_stack.iter().any(|n| n == name) {
            return Err(ResolveError::Cycle(name.to_string()));
        }
        // Already fully chosen? Then the existing choice must satisfy this requirement.
        if let Some((_, v)) = chosen.iter().find(|(n, _)| n == name) {
            if c.matches(*v) {
                return Ok(());
            }
            return Err(ResolveError::Conflict(name.to_string()));
        }
        if !self.exists(name) {
            return Err(ResolveError::NotFound(name.to_string()));
        }
        let pkg = self.best(name, c).ok_or_else(|| ResolveError::NoMatchingVersion(name.to_string()))?;
        let version = pkg.version;
        chosen.push((name.to_string(), version));
        on_stack.push(name.to_string());
        // Dependencies first (depth-first → topological order).
        let deps = pkg.deps.clone();
        for d in &deps {
            self.visit(&d.name, d.constraint, chosen, order, on_stack)?;
        }
        on_stack.pop();
        order.push((name.to_string(), version));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: u32, b: u32, c: u32) -> Version {
        Version::new(a, b, c)
    }

    #[test]
    fn version_parse_and_order() {
        assert_eq!(Version::parse("1.2.3"), Some(v(1, 2, 3)));
        assert_eq!(Version::parse("2.0"), Some(v(2, 0, 0)));
        assert!(v(1, 2, 0) < v(1, 10, 0));
        assert!(Version::parse("1.x").is_none());
    }

    #[test]
    fn constraints() {
        assert!(Constraint::Caret(v(1, 2, 0)).matches(v(1, 9, 9)));
        assert!(!Constraint::Caret(v(1, 2, 0)).matches(v(2, 0, 0)));
        assert!(!Constraint::Caret(v(1, 2, 0)).matches(v(1, 1, 0)));
        assert!(Constraint::AtLeast(v(1, 0, 0)).matches(v(3, 0, 0)));
        assert!(Constraint::Exact(v(1, 2, 3)).matches(v(1, 2, 3)));
    }

    fn repo() -> Repo {
        let mut r = Repo::new();
        // app → libnet ^1.0, libssl ^2.0 ; libnet → libc >=1 ; libssl → libc >=1
        r.add("app", v(1, 0, 0), alloc::vec![Dep::new("libnet", Constraint::Caret(v(1, 0, 0))), Dep::new("libssl", Constraint::Caret(v(2, 0, 0)))]);
        r.add("libnet", v(1, 4, 0), alloc::vec![Dep::new("libc", Constraint::AtLeast(v(1, 0, 0)))]);
        r.add("libssl", v(2, 1, 0), alloc::vec![Dep::new("libc", Constraint::AtLeast(v(1, 0, 0)))]);
        r.add("libc", v(1, 2, 0), alloc::vec![]);
        r.add("libc", v(1, 5, 0), alloc::vec![]);
        r
    }

    #[test]
    fn resolves_in_topological_order() {
        let order = repo().resolve("app").unwrap();
        let names: Vec<&str> = order.iter().map(|(n, _)| n.as_str()).collect();
        // libc before libnet/libssl; everything before app (last). libc only once.
        assert_eq!(names.iter().filter(|n| **n == "libc").count(), 1);
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("libc") < pos("libnet"));
        assert!(pos("libc") < pos("libssl"));
        assert!(pos("libnet") < pos("app"));
        assert_eq!(names.last(), Some(&"app"));
        // Highest matching libc chosen.
        assert_eq!(order.iter().find(|(n, _)| n == "libc").unwrap().1, v(1, 5, 0));
    }

    #[test]
    fn missing_package() {
        let mut r = Repo::new();
        r.add("app", v(1, 0, 0), alloc::vec![Dep::new("ghost", Constraint::Any)]);
        assert_eq!(r.resolve("app"), Err(ResolveError::NotFound("ghost".to_string())));
    }

    #[test]
    fn unsatisfiable_version() {
        let mut r = Repo::new();
        r.add("app", v(1, 0, 0), alloc::vec![Dep::new("lib", Constraint::AtLeast(v(5, 0, 0)))]);
        r.add("lib", v(1, 0, 0), alloc::vec![]);
        assert_eq!(r.resolve("app"), Err(ResolveError::NoMatchingVersion("lib".to_string())));
    }

    #[test]
    fn version_conflict() {
        let mut r = Repo::new();
        // app requires lib =1.0 and (via mid) lib =2.0 → conflict.
        r.add("app", v(1, 0, 0), alloc::vec![Dep::new("lib", Constraint::Exact(v(1, 0, 0))), Dep::new("mid", Constraint::Any)]);
        r.add("mid", v(1, 0, 0), alloc::vec![Dep::new("lib", Constraint::Exact(v(2, 0, 0)))]);
        r.add("lib", v(1, 0, 0), alloc::vec![]);
        r.add("lib", v(2, 0, 0), alloc::vec![]);
        assert_eq!(r.resolve("app"), Err(ResolveError::Conflict("lib".to_string())));
    }

    #[test]
    fn cycle_detected() {
        let mut r = Repo::new();
        r.add("a", v(1, 0, 0), alloc::vec![Dep::new("b", Constraint::Any)]);
        r.add("b", v(1, 0, 0), alloc::vec![Dep::new("a", Constraint::Any)]);
        assert_eq!(r.resolve("a"), Err(ResolveError::Cycle("a".to_string())));
    }
}
