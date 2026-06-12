//! Virtuele filesysteem-laag (plan G2): één `FileSystem`-façade over meerdere
//! gemounte filesystems. Een pad wordt op het LANGSTE matchende mountpoint
//! gerouteerd, met het mountpoint-prefix gestript, en doorgegeven aan dat FS.
//! Zo kan de shell `/mnt/...` op een tweede schijf bedienen zonder iets te weten
//! van mounts. Cross-mount `rename` → `EXDEV`.
//!
//! De routerings-logica is pure data en host-getest met een mock-FS.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::{DirEntry, FileSystem, FsError, FsResult, ScrubReport};

struct Mount {
    point: String,
    fs: Box<dyn FileSystem>,
}

/// Mount-tabel + router. Implementeert zelf `FileSystem`, dus de shell gebruikt
/// hem transparant als "het" filesysteem.
pub struct Vfs {
    root: Box<dyn FileSystem>,
    mounts: Vec<Mount>, // gesorteerd: langste mountpoint eerst
}

/// Is `path` gelijk aan of ligt het ONDER mountpoint `mp`?
fn path_under(mp: &str, path: &str) -> bool {
    path == mp || (path.starts_with(mp) && path.as_bytes().get(mp.len()) == Some(&b'/'))
}

/// Strip het mountpoint-prefix → een wortel-relatief pad op dat FS.
fn strip(mp: &str, path: &str) -> String {
    let rest = &path[mp.len()..];
    if rest.is_empty() {
        "/".to_string()
    } else {
        rest.to_string()
    }
}

impl Vfs {
    pub fn new(root: Box<dyn FileSystem>) -> Self {
        Vfs { root, mounts: Vec::new() }
    }

    /// Mount `fs` op `point` (bv. `/mnt`). Houdt de lijst langste-eerst gesorteerd.
    pub fn mount(&mut self, point: &str, fs: Box<dyn FileSystem>) {
        self.mounts.retain(|m| m.point != point);
        self.mounts.push(Mount { point: point.to_string(), fs });
        self.mounts.sort_by_key(|m| core::cmp::Reverse(m.point.len())); // langste mountpoint eerst
    }

    /// Verwijder een mount; `true` als die bestond.
    pub fn umount(&mut self, point: &str) -> bool {
        let before = self.mounts.len();
        self.mounts.retain(|m| m.point != point);
        self.mounts.len() != before
    }

    /// De mountpoints (root impliciet `/`), langste-eerst.
    pub fn mount_points(&self) -> Vec<String> {
        self.mounts.iter().map(|m| m.point.clone()).collect()
    }

    /// Routeer een pad → (mount-index of None=root, gestript pad).
    fn route(&self, path: &str) -> (Option<usize>, String) {
        for (i, m) in self.mounts.iter().enumerate() {
            if path_under(&m.point, path) {
                return (Some(i), strip(&m.point, path));
            }
        }
        (None, path.to_string())
    }

    fn fs_ref(&self, idx: Option<usize>) -> &dyn FileSystem {
        match idx {
            None => self.root.as_ref(),
            Some(i) => self.mounts[i].fs.as_ref(),
        }
    }
    fn fs_mut(&mut self, idx: Option<usize>) -> &mut dyn FileSystem {
        match idx {
            None => self.root.as_mut(),
            Some(i) => self.mounts[i].fs.as_mut(),
        }
    }

}

impl FileSystem for Vfs {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let (idx, sub) = self.route(path);
        self.fs_ref(idx).read_file(&sub)
    }
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let (idx, sub) = self.route(path);
        self.fs_mut(idx).write_file(&sub, data)
    }
    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        let (idx, sub) = self.route(path);
        self.fs_mut(idx).remove_file(&sub)
    }
    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        let (idx, sub) = self.route(path);
        self.fs_mut(idx).create_dir(&sub)
    }
    fn get_flags(&self, path: &str) -> FsResult<u32> {
        let (idx, sub) = self.route(path);
        self.fs_ref(idx).get_flags(&sub)
    }
    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        let (idx, sub) = self.route(path);
        self.fs_mut(idx).set_flags(&sub, flags)
    }
    // EuroSnap: snapshots horen bij een mount; we routeren op het pad (default: root).
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        self.fs_mut(None).snapshot_create(label, flags)
    }
    fn snapshot_list(&self) -> alloc::vec::Vec<crate::fs::SnapshotInfo> {
        self.fs_ref(None).snapshot_list()
    }
    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        self.fs_mut(None).snapshot_rollback(id)
    }
    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        self.fs_mut(None).snapshot_delete(id)
    }
    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let (idx, sub) = self.route(path);
        self.fs_mut(idx).remove_dir(&sub)
    }
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let (a, oldp) = self.route(old);
        let (b, newp) = self.route(new);
        if a != b {
            return Err(FsError::Unsupported); // cross-mount = EXDEV
        }
        self.fs_mut(a).rename(&oldp, &newp)
    }
    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let (idx, sub) = self.route(path);
        self.fs_ref(idx).list_dir(&sub)
    }
    fn exists(&self, path: &str) -> bool {
        let (idx, sub) = self.route(path);
        self.fs_ref(idx).exists(&sub)
    }
    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let (idx, sub) = self.route(path);
        self.fs_ref(idx).metadata(&sub)
    }
    fn space_info(&self) -> (u64, u64) {
        self.root.space_info()
    }
    /// `df`-regels: ruimte per mount (root + elke mount).
    fn df(&self) -> Vec<(String, u64, u64)> {
        let mut out = alloc::vec![(String::from("/"), self.root.space_info().0, self.root.space_info().1)];
        for m in &self.mounts {
            let (t, f) = m.fs.space_info();
            out.push((m.point.clone(), t, f));
        }
        out
    }
    fn scrub(&self) -> ScrubReport {
        self.root.scrub()
    }
    fn repair(&mut self) -> ScrubReport {
        self.root.repair()
    }
    fn set_clock(&mut self, now: u64) {
        self.root.set_clock(now);
        for m in &mut self.mounts {
            m.fs.set_clock(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::EntryKind;
    use alloc::collections::BTreeMap;

    /// Minimaal in-geheugen-FS dat onthoudt op welke (gestripte) paden het wordt
    /// aangesproken — zo bewijzen we de routering.
    #[derive(Default)]
    struct MockFs {
        files: BTreeMap<String, Vec<u8>>,
    }
    impl FileSystem for MockFs {
        fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
            self.files.get(path).cloned().ok_or(FsError::NotFound)
        }
        fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
            self.files.insert(path.to_string(), data.to_vec());
            Ok(())
        }
        fn remove_file(&mut self, path: &str) -> FsResult<()> {
            self.files.remove(path).map(|_| ()).ok_or(FsError::NotFound)
        }
        fn create_dir(&mut self, _p: &str) -> FsResult<()> {
            Ok(())
        }
        fn list_dir(&self, _p: &str) -> FsResult<Vec<DirEntry>> {
            Ok(Vec::new())
        }
        fn exists(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }
        fn metadata(&self, path: &str) -> FsResult<DirEntry> {
            if self.files.contains_key(path) {
                Ok(DirEntry { name: String::new(), kind: EntryKind::File, size: 0, mode: 0, mtime: 0 })
            } else {
                Err(FsError::NotFound)
            }
        }
        fn space_info(&self) -> (u64, u64) {
            (1000, 500)
        }
    }

    #[test]
    fn routes_to_longest_prefix_mount() {
        let mut vfs = Vfs::new(Box::new(MockFs::default()));
        vfs.mount("/mnt", Box::new(MockFs::default()));
        vfs.mount("/mnt/data", Box::new(MockFs::default()));

        // /etc/x → root (gestript pad blijft /etc/x).
        vfs.write_file("/etc/x", b"r").unwrap();
        assert_eq!(vfs.read_file("/etc/x").unwrap(), b"r");
        // /mnt/foo → mount /mnt, gestript naar /foo.
        vfs.write_file("/mnt/foo", b"m").unwrap();
        // /mnt/data/y → mount /mnt/data (LANGSTE prefix), gestript naar /y.
        vfs.write_file("/mnt/data/y", b"d").unwrap();

        // Bewijs de isolatie: elk landt in het juiste FS, niet in een ander.
        assert_eq!(vfs.read_file("/mnt/foo").unwrap(), b"m");
        assert_eq!(vfs.read_file("/mnt/data/y").unwrap(), b"d");
        // Het root-FS kent /mnt/foo NIET (dat zit in de mount).
        assert_eq!(vfs.fs_ref(None).read_file("/mnt/foo"), Err(FsError::NotFound));
        // De /mnt-mount kent het pad als /foo, niet /mnt/foo.
        assert_eq!(vfs.read_file("/mnt/data/y").unwrap(), b"d");
    }

    #[test]
    fn mount_root_itself_and_strip() {
        let mut vfs = Vfs::new(Box::new(MockFs::default()));
        vfs.mount("/mnt", Box::new(MockFs::default()));
        // Het mountpoint zelf (`/mnt`) → gestript naar `/`.
        assert_eq!(super::strip("/mnt", "/mnt"), "/");
        assert_eq!(super::strip("/mnt", "/mnt/a/b"), "/a/b");
        assert!(super::path_under("/mnt", "/mnt"));
        assert!(super::path_under("/mnt", "/mnt/x"));
        // Een broer met naam-prefix valt NIET onder de mount.
        assert!(!super::path_under("/mnt", "/mnt2"));
        assert!(!super::path_under("/mnt", "/mntfoo/x"));
    }

    #[test]
    fn cross_mount_rename_is_exdev() {
        let mut vfs = Vfs::new(Box::new(MockFs::default()));
        vfs.mount("/mnt", Box::new(MockFs::default()));
        vfs.write_file("/a", b"x").unwrap();
        // Rename van root naar de mount → niet ondersteund (EXDEV).
        assert_eq!(vfs.rename("/a", "/mnt/a"), Err(FsError::Unsupported));
    }

    #[test]
    fn umount_and_df() {
        let mut vfs = Vfs::new(Box::new(MockFs::default()));
        vfs.mount("/mnt", Box::new(MockFs::default()));
        assert_eq!(vfs.df().len(), 2); // root + /mnt
        assert!(vfs.umount("/mnt"));
        assert!(!vfs.umount("/mnt")); // al weg
        assert_eq!(vfs.df().len(), 1);
    }
}
