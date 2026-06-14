//! Path utilities for a no_std environment (no `std::path`).
//!
//! All paths are absolute and `/`-separated. There is no support for
//! `.` / `..` in this layer — that belongs in a higher VFS layer so the
//! semantics (symlink resolution, chroot boundaries) stay explicit.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Split a path into non-empty components.
/// `"/foo//bar/"` → `["foo", "bar"]`.
pub fn split_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Parent directory of a path. `"/a/b/c"` → `"/a/b"`, `"/a"` → `"/"`.
pub fn parent(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/",
        Some(idx) => &trimmed[..idx],
    }
}

/// Last component (file name). `"/a/b/c.txt"` → `"c.txt"`.
pub fn filename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    }
}

/// Normalize: force absolute, remove duplicate/trailing slashes.
/// `""`/`"/"` → `"/"`.
pub fn normalize(path: &str) -> String {
    let components = split_path(path);
    if components.is_empty() {
        return "/".to_string();
    }
    let mut result = String::with_capacity(path.len());
    for c in components {
        result.push('/');
        result.push_str(c);
    }
    result
}

/// Combine a directory path with a name.
pub fn join(base: &str, name: &str) -> String {
    let base = base.trim_end_matches('/');
    let mut s = String::with_capacity(base.len() + 1 + name.len());
    s.push_str(base);
    s.push('/');
    s.push_str(name);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn split_negeert_lege_componenten() {
        assert_eq!(split_path("/foo//bar/"), vec!["foo", "bar"]);
        assert_eq!(split_path("/"), Vec::<&str>::new());
        assert_eq!(split_path(""), Vec::<&str>::new());
    }

    #[test]
    fn parent_van_diverse_paden() {
        assert_eq!(parent("/a/b/c"), "/a/b");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(parent("/a/b/"), "/a"); // trailing slash ignored
    }

    #[test]
    fn filename_van_diverse_paden() {
        assert_eq!(filename("/a/b/c.txt"), "c.txt");
        assert_eq!(filename("/a"), "a");
        assert_eq!(filename("/a/b/"), "b");
    }

    #[test]
    fn normalize_idempotent_en_absoluut() {
        assert_eq!(normalize("/foo//bar/"), "/foo/bar");
        assert_eq!(normalize("foo/bar"), "/foo/bar");
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
        // Idempotent
        let once = normalize("/x//y/");
        assert_eq!(normalize(&once), once);
    }

    #[test]
    fn join_voegt_correct_samen() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/etc", "hosts"), "/etc/hosts");
        assert_eq!(join("/etc/", "hosts"), "/etc/hosts");
    }
}
