//! EuroSandbox — containers via het EuroGuard-capability-model (plan F2), GEEN
//! Linux-namespaces. Een container = een proces met:
//!   - een ge-chroot'e bestandswortel (alle paden blijven binnen `/containers/<naam>`),
//!   - een ingeperkt capability-masker (alleen wat de container mag),
//!   - een ingeperkte netwerk-scope (alleen toegestane host:poort).
//!
//! Pure `no_std`-logica zodat de veiligheidskritische padresolutie (geen `..`-
//! ontsnapping uit de container) host-getest is.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Netwerk-scope van een container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetScope {
    /// Geen netwerk.
    None,
    /// Alleen deze (ip, poort)-paren.
    Allow(Vec<([u8; 4], u16)>),
    /// Onbeperkt (binnen de grenzen van CAP_NET).
    Any,
}

/// Het beleid van één container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    /// De host-padwortel (`/containers/<naam>`); alle container-paden vallen hieronder.
    pub root: String,
    /// Toegestane capabilities (masker dat over het basismasker wordt gelegd).
    pub caps: u64,
    pub net: NetScope,
}

impl Container {
    pub fn new(name: &str, caps: u64, net: NetScope) -> Self {
        let root = alloc::format!("/containers/{name}");
        Container { name: String::from(name), root, caps, net }
    }

    /// Effectieve capabilities = intersectie van het proces-basismasker en wat de
    /// container toestaat (een container kan rechten alleen INPERKEN, nooit uitbreiden).
    pub fn effective_caps(&self, base: u64) -> u64 {
        base & self.caps
    }

    /// Mag de container naar (ip, poort) verbinden?
    pub fn allow_connect(&self, ip: [u8; 4], port: u16) -> bool {
        match &self.net {
            NetScope::None => false,
            NetScope::Any => true,
            NetScope::Allow(list) => list.iter().any(|&(a, p)| a == ip && p == port),
        }
    }

    /// Vertaal een container-pad naar het echte host-pad ONDER de wortel. `..`-
    /// componenten kunnen nooit boven de wortel uitkomen (chroot-semantiek), dus
    /// een container kan z'n bestandssysteem niet ontsnappen. Geeft altijd een pad
    /// dat met `self.root` begint.
    pub fn resolve(&self, requested: &str) -> String {
        let mut stack: Vec<&str> = Vec::new();
        for comp in requested.split('/') {
            match comp {
                "" | "." => {}            // lege/`.`-component overslaan
                ".." => {
                    stack.pop(); // nooit voorbij de (virtuele) wortel
                }
                other => stack.push(other),
            }
        }
        let mut out = self.root.clone();
        for c in stack {
            out.push('/');
            out.push_str(c);
        }
        out
    }

    /// True als `host_path` daadwerkelijk binnen de container-wortel ligt. Een
    /// extra verdedigingslaag bovenop [`resolve`].
    pub fn contains(&self, host_path: &str) -> bool {
        if host_path == self.root {
            return true;
        }
        // Moet beginnen met "<root>/" — niet enkel met de root als prefix van een
        // langere naam (bv. "/containers/foobar" mag niet matchen op "/containers/foo").
        host_path.len() > self.root.len()
            && host_path.starts_with(&self.root)
            && host_path.as_bytes()[self.root.len()] == b'/'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> Container {
        Container::new("web", 0b1111, NetScope::Allow(alloc::vec![([10, 0, 2, 2], 443)]))
    }

    #[test]
    fn caps_can_only_shrink() {
        let con = Container::new("x", 0b0101, NetScope::None);
        // Basis heeft alle 4 rechten; container staat alleen bit 0 en 2 toe.
        assert_eq!(con.effective_caps(0b1111), 0b0101);
        // Een container kan geen recht toevoegen dat het proces niet heeft.
        assert_eq!(con.effective_caps(0b0001), 0b0001);
    }

    #[test]
    fn path_cannot_escape_root() {
        let con = c();
        assert_eq!(con.resolve("data/x.txt"), "/containers/web/data/x.txt");
        assert_eq!(con.resolve("/data/x.txt"), "/containers/web/data/x.txt");
        // Klassieke ontsnappingspogingen blijven binnen de wortel.
        assert_eq!(con.resolve("../../etc/passwd"), "/containers/web/etc/passwd");
        assert_eq!(con.resolve("../../../"), "/containers/web");
        assert_eq!(con.resolve("a/../../../../b"), "/containers/web/b");
        assert_eq!(con.resolve("./a/./b/../c"), "/containers/web/a/c");
        // Elk resultaat ligt binnen de container.
        for p in ["../../x", "a/../../b", "/", "..", "foo/../../../bar"] {
            assert!(con.contains(&con.resolve(p)) || con.resolve(p) == con.root);
        }
    }

    #[test]
    fn contains_rejects_prefix_siblings() {
        let con = c();
        assert!(con.contains("/containers/web/file"));
        assert!(con.contains("/containers/web")); // de wortel zelf
        // Een broer-map met de wortel als naam-prefix mag NIET binnen vallen.
        assert!(!con.contains("/containers/webroot/secret"));
        assert!(!con.contains("/containers/web2"));
        assert!(!con.contains("/etc/passwd"));
    }

    #[test]
    fn net_scope_enforced() {
        let con = c();
        assert!(con.allow_connect([10, 0, 2, 2], 443));
        assert!(!con.allow_connect([10, 0, 2, 2], 80)); // verkeerde poort
        assert!(!con.allow_connect([1, 1, 1, 1], 443)); // verkeerd ip
        let none = Container::new("n", 0, NetScope::None);
        assert!(!none.allow_connect([10, 0, 2, 2], 443));
        let any = Container::new("a", 0, NetScope::Any);
        assert!(any.allow_connect([8, 8, 8, 8], 53));
    }
}
