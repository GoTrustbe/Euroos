//! EuroPkg — de afhankelijkheids-resolver van de pakketbeheerder (plan M2).
//!
//! `eupkg` bouwt + verifieert al getekende `.eupkg`-pakketten (ZIP + manifest +
//! SHA-256 + Ed25519). M2 voegt de ontbrekende kern toe: **semver-versies +
//! constraints** en een **dependency-resolver** die uit een repository-index een
//! geldige, topologisch geordende installatievolgorde berekent — met detectie van
//! ontbrekende pakketten, onvervulbare versie-eisen, conflicten en cycli.
//!
//! Pure, host-geteste `no_std`-logica; de echte download/verify/unpack koppelt de
//! kernel/`eupkg` eraan.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Een semantische versie `major.minor.patch`.
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
    /// Parse `"1.2.3"` (ontbrekende delen = 0, dus `"1.2"` = 1.2.0).
    pub fn parse(s: &str) -> Option<Version> {
        let mut it = s.trim().split('.');
        let major = it.next()?.parse().ok()?;
        // Ontbrekende delen = 0, maar een aanwezig-maar-ongeldig deel ("1.x") faalt.
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

/// Een versie-eis op een afhankelijkheid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Constraint {
    /// Elke versie.
    Any,
    /// Exact deze versie.
    Exact(Version),
    /// Deze versie of hoger.
    AtLeast(Version),
    /// Caret: `^1.2.0` = `>=1.2.0` én zelfde major (`<2.0.0`). Major 0 → zelfde minor.
    Caret(Version),
}

impl Constraint {
    /// Voldoet `v` aan deze eis?
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

/// Een afhankelijkheid: een pakketnaam + een versie-eis.
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

/// Een (beschikbaar) pakket in de repository-index.
#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub version: Version,
    pub deps: Vec<Dep>,
}

/// De repository-index: alle beschikbare pakketten (mogelijk meerdere versies).
#[derive(Default)]
pub struct Repo {
    pub packages: Vec<Package>,
}

/// Waarom de resolutie faalt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// Geen pakket met deze naam in de index.
    NotFound(String),
    /// Wel het pakket, maar geen versie die aan de eis voldoet.
    NoMatchingVersion(String),
    /// Twee eisen op hetzelfde pakket zijn onverenigbaar (gekozen versies botsen).
    Conflict(String),
    /// Een afhankelijkheidscyclus.
    Cycle(String),
}

impl Repo {
    pub fn new() -> Repo {
        Repo { packages: Vec::new() }
    }

    /// Voeg een pakket(versie) toe aan de index.
    pub fn add(&mut self, name: &str, version: Version, deps: Vec<Dep>) {
        self.packages.push(Package { name: name.to_string(), version, deps });
    }

    /// De hoogste versie van `name` die aan `c` voldoet.
    fn best(&self, name: &str, c: Constraint) -> Option<&Package> {
        self.packages
            .iter()
            .filter(|p| p.name == name && c.matches(p.version))
            .max_by_key(|p| p.version)
    }

    /// Bestaat dit pakket (ongeacht versie)?
    fn exists(&self, name: &str) -> bool {
        self.packages.iter().any(|p| p.name == name)
    }

    /// Los de afhankelijkheden van `root` op tot een topologisch geordende
    /// installatievolgorde (afhankelijkheden vóór wie ze gebruikt). De wortel staat
    /// als laatste. Elk pakket komt één keer voor; een herhaalde eis moet
    /// verenigbaar zijn met de reeds gekozen versie.
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
        // Cyclusdetectie eerst: zit `name` nog in de actieve keten, dan is het een cyclus
        // (ook al staat hij al in `chosen` — hij is nog niet volledig opgelost).
        if on_stack.iter().any(|n| n == name) {
            return Err(ResolveError::Cycle(name.to_string()));
        }
        // Al volledig gekozen? Dan moet de bestaande keuze aan deze eis voldoen.
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
        // Afhankelijkheden eerst (diepte-eerst → topologische volgorde).
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
        // libc vóór libnet/libssl; alles vóór app (laatste). libc maar één keer.
        assert_eq!(names.iter().filter(|n| **n == "libc").count(), 1);
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("libc") < pos("libnet"));
        assert!(pos("libc") < pos("libssl"));
        assert!(pos("libnet") < pos("app"));
        assert_eq!(names.last(), Some(&"app"));
        // Hoogste passende libc gekozen.
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
        // app eist lib =1.0 én (via mid) lib =2.0 → conflict.
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
