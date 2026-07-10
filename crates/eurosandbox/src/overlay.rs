//! **EuroFS overlay** for a container (3F-1) — a copy-on-write union of a
//! read-only **lower** layer (the shared, verified image) and a writable
//! **upper** layer (the container's private diff). Reads resolve upper-first
//! then fall through to lower; writes go to upper (copy-up); deletes leave a
//! whiteout so the lower file is hidden without being mutated.
//!
//! Pure path/bookkeeping logic (which layer a path resolves to) so it is
//! host-tested; the kernel maps the two layers onto real EuroFS directories.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Which layer a read resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Served from the container's private writable diff.
    Upper,
    /// Served from the shared read-only image.
    Lower,
    /// Hidden by a whiteout (deleted in this container) — a read must fail.
    Whiteout,
    /// Not present in either layer.
    Absent,
}

/// A CoW overlay over two layers, tracking which paths have been copied up or
/// whited out. The actual bytes live in EuroFS; this tracks the union view.
#[derive(Debug, Default)]
pub struct Overlay {
    /// Paths written in the upper layer (copy-up set).
    upper: BTreeSet<String>,
    /// Deleted paths (whiteouts).
    whiteouts: BTreeSet<String>,
}

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    fn norm(path: &str) -> String {
        // Normalise to a leading-slash canonical form (no trailing slash).
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_string()
        } else if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            alloc::format!("/{trimmed}")
        }
    }

    /// Resolve a read for `path`, given whether the lower image contains it.
    pub fn resolve_read(&self, path: &str, in_lower: bool) -> Layer {
        let p = Self::norm(path);
        if self.whiteouts.contains(&p) {
            return Layer::Whiteout;
        }
        if self.upper.contains(&p) {
            return Layer::Upper;
        }
        if in_lower {
            Layer::Lower
        } else {
            Layer::Absent
        }
    }

    /// Record a write (copy-up): the path now lives in the upper layer, and any
    /// prior whiteout is cleared (the file was recreated).
    pub fn on_write(&mut self, path: &str) {
        let p = Self::norm(path);
        self.whiteouts.remove(&p);
        self.upper.insert(p);
    }

    /// Record a delete: whiteout the path so the lower copy is hidden, and drop
    /// any upper copy.
    pub fn on_delete(&mut self, path: &str) {
        let p = Self::norm(path);
        self.upper.remove(&p);
        self.whiteouts.insert(p);
    }

    /// Is the path deleted in this container?
    pub fn is_deleted(&self, path: &str) -> bool {
        self.whiteouts.contains(&Self::norm(path))
    }

    /// The set of paths that have been modified (copy-up) — the container diff.
    pub fn diff(&self) -> Vec<String> {
        self.upper.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_resolves_upper_first_then_lower() {
        let mut o = Overlay::new();
        // A file only in the lower image reads from Lower.
        assert_eq!(o.resolve_read("/etc/os-release", true), Layer::Lower);
        // Writing it copies up → now Upper, lower untouched.
        o.on_write("/etc/os-release");
        assert_eq!(o.resolve_read("/etc/os-release", true), Layer::Upper);
    }

    #[test]
    fn delete_leaves_a_whiteout_hiding_lower() {
        let mut o = Overlay::new();
        o.on_delete("/etc/hosts");
        assert!(o.is_deleted("/etc/hosts"));
        // Even though the lower image still has it, the container sees it gone.
        assert_eq!(o.resolve_read("/etc/hosts", true), Layer::Whiteout);
    }

    #[test]
    fn recreating_a_deleted_file_clears_the_whiteout() {
        let mut o = Overlay::new();
        o.on_delete("/tmp/x");
        o.on_write("/tmp/x");
        assert!(!o.is_deleted("/tmp/x"));
        assert_eq!(o.resolve_read("/tmp/x", true), Layer::Upper);
    }

    #[test]
    fn absent_when_in_neither_layer() {
        let o = Overlay::new();
        assert_eq!(o.resolve_read("/nope", false), Layer::Absent);
    }

    #[test]
    fn diff_lists_copy_ups_only() {
        let mut o = Overlay::new();
        o.on_write("/a");
        o.on_write("/b");
        o.on_delete("/c");
        let d = o.diff();
        assert!(d.contains(&"/a".to_string()) && d.contains(&"/b".to_string()));
        assert!(!d.contains(&"/c".to_string()));
    }
}
