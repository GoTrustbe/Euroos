//! EuroSandbox — containers via the EuroGuard capability model (plan F2), NOT
//! Linux namespaces. A container = a process with:
//!   - a chrooted file root (all paths stay within `/containers/<name>`),
//!   - a restricted capability mask (only what the container is allowed),
//!   - a restricted network scope (only permitted host:port).
//!
//! Pure `no_std` logic so that the security-critical path resolution (no `..`
//! escape out of the container) is host-tested.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod image;
pub mod limits;
pub mod overlay;

pub use image::{verify_image, ImageError, ImageManifest, SignedImage};
pub use limits::{LimitBreach, ResourceLimits, Usage};
pub use overlay::{Layer, Overlay};

use alloc::string::String;
use alloc::vec::Vec;

/// Network scope of a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetScope {
    /// No network.
    None,
    /// Only these (ip, port) pairs.
    Allow(Vec<([u8; 4], u16)>),
    /// Unrestricted (within the bounds of CAP_NET).
    Any,
}

/// The policy of a single container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    /// The host path root (`/containers/<name>`); all container paths fall under it.
    pub root: String,
    /// Permitted capabilities (mask laid over the base mask).
    pub caps: u64,
    pub net: NetScope,
}

impl Container {
    pub fn new(name: &str, caps: u64, net: NetScope) -> Self {
        let root = alloc::format!("/containers/{name}");
        Container { name: String::from(name), root, caps, net }
    }

    /// Effective capabilities = intersection of the process base mask and what the
    /// container permits (a container can only RESTRICT rights, never expand them).
    pub fn effective_caps(&self, base: u64) -> u64 {
        base & self.caps
    }

    /// May the container connect to (ip, port)?
    pub fn allow_connect(&self, ip: [u8; 4], port: u16) -> bool {
        match &self.net {
            NetScope::None => false,
            NetScope::Any => true,
            NetScope::Allow(list) => list.iter().any(|&(a, p)| a == ip && p == port),
        }
    }

    /// Translate a container path to the real host path UNDER the root. `..`
    /// components can never climb above the root (chroot semantics), so
    /// a container cannot escape its filesystem. Always returns a path
    /// that begins with `self.root`.
    pub fn resolve(&self, requested: &str) -> String {
        let mut stack: Vec<&str> = Vec::new();
        for comp in requested.split('/') {
            match comp {
                "" | "." => {}            // skip empty/`.` component
                ".." => {
                    stack.pop(); // never past the (virtual) root
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

    /// True if `host_path` actually lies within the container root. An
    /// extra defense layer on top of [`resolve`].
    pub fn contains(&self, host_path: &str) -> bool {
        if host_path == self.root {
            return true;
        }
        // Must begin with "<root>/" — not just with the root as a prefix of a
        // longer name (e.g. "/containers/foobar" must not match "/containers/foo").
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
        // Base has all 4 rights; container permits only bit 0 and 2.
        assert_eq!(con.effective_caps(0b1111), 0b0101);
        // A container cannot add a right that the process does not have.
        assert_eq!(con.effective_caps(0b0001), 0b0001);
    }

    #[test]
    fn path_cannot_escape_root() {
        let con = c();
        assert_eq!(con.resolve("data/x.txt"), "/containers/web/data/x.txt");
        assert_eq!(con.resolve("/data/x.txt"), "/containers/web/data/x.txt");
        // Classic escape attempts stay within the root.
        assert_eq!(con.resolve("../../etc/passwd"), "/containers/web/etc/passwd");
        assert_eq!(con.resolve("../../../"), "/containers/web");
        assert_eq!(con.resolve("a/../../../../b"), "/containers/web/b");
        assert_eq!(con.resolve("./a/./b/../c"), "/containers/web/a/c");
        // Every result lies within the container.
        for p in ["../../x", "a/../../b", "/", "..", "foo/../../../bar"] {
            assert!(con.contains(&con.resolve(p)) || con.resolve(p) == con.root);
        }
    }

    #[test]
    fn contains_rejects_prefix_siblings() {
        let con = c();
        assert!(con.contains("/containers/web/file"));
        assert!(con.contains("/containers/web")); // the root itself
        // A sibling directory with the root as a name prefix must NOT fall within.
        assert!(!con.contains("/containers/webroot/secret"));
        assert!(!con.contains("/containers/web2"));
        assert!(!con.contains("/etc/passwd"));
    }

    #[test]
    fn net_scope_enforced() {
        let con = c();
        assert!(con.allow_connect([10, 0, 2, 2], 443));
        assert!(!con.allow_connect([10, 0, 2, 2], 80)); // wrong port
        assert!(!con.allow_connect([1, 1, 1, 1], 443)); // wrong ip
        let none = Container::new("n", 0, NetScope::None);
        assert!(!none.allow_connect([10, 0, 2, 2], 443));
        let any = Container::new("a", 0, NetScope::Any);
        assert!(any.allow_connect([8, 8, 8, 8], 53));
    }
}
