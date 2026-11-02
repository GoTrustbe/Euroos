//! `SharedVfs`: the VFS with **per-mount locks** (J1 fine-grained locking).
//!
//! [`crate::Vfs`] is a plain owned value: whoever holds `&mut Vfs` holds every
//! mounted filesystem at once. In the kernel that meant a single owner (the boot
//! task) and no filesystem access from any other core. `SharedVfs` is the same
//! router, built for concurrency:
//!
//! - the **mount table** is a per-CPU replicated read-mostly lock
//!   (`eurolock::sync::ReadMostly`, class `FsMountTable`): routing a path reads
//!   only this core's replica (longest-prefix match + clone of an `Arc` to the
//!   target mount), never across I/O. Mount/umount are the only writers.
//! - **each mount has its own lock** (class `Fs`), held for exactly one
//!   filesystem operation. Two cores working on two different mounts never wait
//!   for each other; two cores on the same mount serialise on that mount only.
//! - `umount` while an operation is in flight is safe: the operation holds an
//!   `Arc` to the mount, so the filesystem stays alive until it finishes and is
//!   dropped afterwards.
//!
//! Both locks are `Mode::Task`: a filesystem operation can take milliseconds of
//! disk I/O, which must not run with interrupts disabled, and therefore these
//! locks must never be taken from an `IF=0` context (the kernel hook counts it if
//! they are). Lock order: `FsMountTable` → (released) → `Fs` → `BlockCache` →
//! `Heap`; no path holds two mounts at once (cross-mount rename is `EXDEV`).
//!
//! `SharedVfs` is a cheap handle (`Clone` = another reference to the same table),
//! so the kernel can keep one in a global for other cores while the boot task
//! keeps using it as its `&mut dyn FileSystem`.
//!
//! Generic over the stored filesystem type `F`: the kernel uses
//! `dyn FileSystem` (its filesystems are not `Send` in the type system); the host
//! tests use `dyn FileSystem + Send`, which makes the compiler prove the locking
//! is thread-safe and lets real threads hammer it.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use eurolock::sync::{Mutex, ReadMostly, Replica};
use eurolock::{LockClass, LockSite, Mode};

use crate::fs::{DirEntry, FileSystem, FsError, FsResult, ScrubReport, SnapshotInfo};

/// Lock-statistics site for the mount table.
pub static SITE_MOUNT_TABLE: LockSite = LockSite::new("vfs.mount_table", LockClass::FsMountTable);
/// Lock-statistics site for all per-mount locks together.
pub static SITE_MOUNT: LockSite = LockSite::new("vfs.mount (per-mount)", LockClass::Fs);

/// A filesystem type that can live in a `SharedVfs`. The only extra ability
/// needed is accepting a `Box<dyn FileSystem>` handed to `mount_fs` (the
/// `FileSystem` trait's dynamic-mount entry point).
pub trait VfsMember: FileSystem {
    fn from_dyn(fs: Box<dyn FileSystem>) -> Option<Box<Self>>;
}

impl VfsMember for dyn FileSystem {
    fn from_dyn(fs: Box<dyn FileSystem>) -> Option<Box<Self>> {
        Some(fs)
    }
}

impl VfsMember for dyn FileSystem + Send {
    /// A non-`Send` box cannot join a thread-safe table.
    fn from_dyn(_fs: Box<dyn FileSystem>) -> Option<Box<Self>> {
        None
    }
}

struct MountCell<F: ?Sized> {
    fs: Mutex<Box<F>>,
}

impl<F: ?Sized> MountCell<F> {
    fn new(fs: Box<F>) -> Arc<Self> {
        Arc::new(MountCell { fs: Mutex::new_site(&SITE_MOUNT, Mode::Task, fs) })
    }
}

struct MountEntry<F: ?Sized> {
    point: String,
    cell: Arc<MountCell<F>>,
}

struct Inner<F: ?Sized> {
    root: Arc<MountCell<F>>,
    /// Sorted longest mountpoint first (longest-prefix routing).
    mounts: ReadMostly<Vec<MountEntry<F>>>,
}

/// The concurrent VFS handle (see module docs).
pub struct SharedVfs<F: ?Sized + VfsMember> {
    inner: Arc<Inner<F>>,
}

impl<F: ?Sized + VfsMember> Clone for SharedVfs<F> {
    fn clone(&self) -> Self {
        SharedVfs { inner: self.inner.clone() }
    }
}

fn path_under(mp: &str, path: &str) -> bool {
    path == mp || (path.starts_with(mp) && path.as_bytes().get(mp.len()) == Some(&b'/'))
}

fn strip(mp: &str, path: &str) -> String {
    let rest = &path[mp.len()..];
    if rest.is_empty() {
        "/".to_string()
    } else {
        rest.to_string()
    }
}

impl<F: ?Sized + VfsMember> SharedVfs<F> {
    pub fn new(root: Box<F>) -> Self {
        SharedVfs {
            inner: Arc::new(Inner {
                root: MountCell::new(root),
                mounts: ReadMostly::new_site(&SITE_MOUNT_TABLE, Mode::Task, core::array::from_fn(|_| Replica::new(Vec::new()))),
            }),
        }
    }

    /// Mount `fs` at `point` (replacing an existing mount there).
    pub fn mount(&self, point: &str, fs: Box<F>) {
        let cell = MountCell::new(fs);
        let old = self.inner.mounts.write(|t| {
            let old = t.iter().position(|m| m.point == point).map(|i| t.remove(i));
            t.push(MountEntry { point: point.to_string(), cell: cell.clone() });
            t.sort_by_key(|m| core::cmp::Reverse(m.point.len()));
            old
        });
        // The replaced filesystem's last reference is dropped OUTSIDE the table
        // lock (its drop may flush to disk).
        drop(old);
    }

    /// Remove a mount; `true` if it existed. In-flight operations on it finish
    /// first (they hold their own reference).
    pub fn umount(&self, point: &str) -> bool {
        let old = self.inner.mounts.write(|t| t.iter().position(|m| m.point == point).map(|i| t.remove(i)));
        old.is_some()
    }

    /// The mountpoints (root implicitly `/`), longest first.
    pub fn mount_points(&self) -> Vec<String> {
        self.inner.mounts.read(|t| t.iter().map(|m| m.point.clone()).collect())
    }

    /// Route `path` to its mount: (mount, path inside it, mountpoint). The table
    /// lock is released before this returns.
    fn route(&self, path: &str) -> (Arc<MountCell<F>>, String) {
        let hit = self.inner.mounts.read(|t| {
            t.iter().find(|m| path_under(&m.point, path)).map(|m| (m.cell.clone(), strip(&m.point, path)))
        });
        hit.unwrap_or_else(|| (self.inner.root.clone(), path.to_string()))
    }

    /// Run `f` on the filesystem that owns `path`, holding only that mount's lock.
    pub fn with_fs<R>(&self, path: &str, f: impl FnOnce(&mut F, &str) -> R) -> R {
        let (cell, sub) = self.route(path);
        let mut g = cell.fs.lock();
        f(&mut **g, &sub)
    }

    fn with_root<R>(&self, f: impl FnOnce(&mut F) -> R) -> R {
        let mut g = self.inner.root.fs.lock();
        f(&mut **g)
    }

    /// Every mount (root first), cloned out of the table so each can be locked on
    /// its own without holding the table.
    fn all_cells(&self) -> Vec<(String, Arc<MountCell<F>>)> {
        let mut v = alloc::vec![(String::from("/"), self.inner.root.clone())];
        self.inner.mounts.read(|t| {
            for m in t.iter() {
                v.push((m.point.clone(), m.cell.clone()));
            }
        });
        v
    }

    /// Are two paths on the same mount? (cross-mount rename = EXDEV)
    fn same_mount(&self, a: &str, b: &str) -> Option<(Arc<MountCell<F>>, String, String)> {
        let (ca, pa) = self.route(a);
        let (cb, pb) = self.route(b);
        if Arc::ptr_eq(&ca, &cb) {
            Some((ca, pa, pb))
        } else {
            None
        }
    }
}

impl<F: ?Sized + VfsMember> FileSystem for SharedVfs<F> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        self.with_fs(path, |fs, p| fs.read_file(p))
    }
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.write_file(p, data))
    }
    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.remove_file(p))
    }
    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.create_dir(p))
    }
    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.remove_dir(p))
    }
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        match self.same_mount(old, new) {
            Some((cell, a, b)) => cell.fs.lock().rename(&a, &b),
            None => Err(FsError::Unsupported), // cross-mount = EXDEV
        }
    }
    fn get_flags(&self, path: &str) -> FsResult<u32> {
        self.with_fs(path, |fs, p| fs.get_flags(p))
    }
    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.set_flags(p, flags))
    }
    fn set_uid_context(&mut self, uid: u32) {
        for (_, c) in self.all_cells() {
            c.fs.lock().set_uid_context(uid);
        }
    }
    fn uid_context(&self) -> u32 {
        self.with_root(|fs| fs.uid_context())
    }
    fn owner(&self, path: &str) -> FsResult<u32> {
        self.with_fs(path, |fs, p| fs.owner(p))
    }
    fn chmod(&mut self, path: &str, mode: u16) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.chmod(p, mode))
    }
    fn versions(&self, path: &str) -> FsResult<Vec<(u32, u64, u64)>> {
        self.with_fs(path, |fs, p| fs.versions(p))
    }
    fn restore_version(&mut self, path: &str, n: u32) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.restore_version(p, n))
    }
    fn chown(&mut self, path: &str, uid: u32) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.chown(p, uid))
    }
    fn quota_set(&mut self, uid: u32, limit_blocks: u64) -> FsResult<()> {
        self.with_root(|fs| fs.quota_set(uid, limit_blocks))
    }
    fn quota_info(&self, uid: u32) -> FsResult<(u64, u64)> {
        self.with_root(|fs| fs.quota_info(uid))
    }
    fn quota_list(&self) -> Vec<(u32, u64, u64)> {
        self.with_root(|fs| fs.quota_list())
    }
    fn create_symlink(&mut self, path: &str, target: &str) -> FsResult<()> {
        self.with_fs(path, |fs, p| fs.create_symlink(p, target))
    }
    fn read_link(&self, path: &str) -> FsResult<String> {
        self.with_fs(path, |fs, p| fs.read_link(p))
    }
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        self.with_root(|fs| fs.snapshot_create(label, flags))
    }
    fn snapshot_list(&self) -> Vec<SnapshotInfo> {
        self.with_root(|fs| fs.snapshot_list())
    }
    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        self.with_root(|fs| fs.snapshot_rollback(id))
    }
    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        self.with_root(|fs| fs.snapshot_delete(id))
    }
    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        self.with_fs(path, |fs, p| fs.list_dir(p))
    }
    fn exists(&self, path: &str) -> bool {
        self.with_fs(path, |fs, p| fs.exists(p))
    }
    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        self.with_fs(path, |fs, p| fs.metadata(p))
    }
    fn space_info(&self) -> (u64, u64) {
        self.with_root(|fs| fs.space_info())
    }
    fn df(&self) -> Vec<(String, u64, u64)> {
        self.all_cells()
            .into_iter()
            .map(|(p, c)| {
                let (t, f) = c.fs.lock().space_info();
                (p, t, f)
            })
            .collect()
    }
    fn alloc_debug(&self, path: &str) -> Option<String> {
        self.with_fs(path, |fs, p| fs.alloc_debug(p))
    }
    fn scrub(&self) -> ScrubReport {
        self.with_root(|fs| fs.scrub())
    }
    fn repair(&mut self) -> ScrubReport {
        self.with_root(|fs| fs.repair())
    }
    fn set_clock(&mut self, now: u64) {
        for (_, c) in self.all_cells() {
            c.fs.lock().set_clock(now);
        }
    }
    fn mount_fs(&mut self, point: &str, fs: Box<dyn FileSystem>) -> FsResult<()> {
        match F::from_dyn(fs) {
            Some(b) => {
                self.mount(point, b);
                Ok(())
            }
            None => Err(FsError::Unsupported),
        }
    }
    fn umount_fs(&mut self, point: &str) -> FsResult<()> {
        if self.umount(point) {
            Ok(())
        } else {
            Err(FsError::NotFound)
        }
    }
    fn list_mounts(&self) -> Vec<String> {
        self.mount_points()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ramdisk::RamDisk;
    use crate::{EuroFs, MemoryBlockDevice};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    type TVfs = SharedVfs<dyn FileSystem + Send>;

    fn ram() -> Box<dyn FileSystem + Send> {
        Box::new(RamDisk::new(1 << 24))
    }

    fn eurofs(blocks: u64) -> Box<dyn FileSystem + Send> {
        Box::new(EuroFs::format(MemoryBlockDevice::new(blocks, 4096), [7u8; 16], 1).unwrap())
    }

    #[test]
    fn routes_like_the_plain_vfs() {
        let mut v = TVfs::new(ram());
        v.mount("/mnt", ram());
        v.mount("/mnt/data", ram());
        v.write_file("/x", b"r").unwrap();
        v.write_file("/mnt/foo", b"m").unwrap();
        v.write_file("/mnt/data/y", b"d").unwrap();
        assert_eq!(v.read_file("/x").unwrap(), b"r");
        assert_eq!(v.read_file("/mnt/foo").unwrap(), b"m");
        assert_eq!(v.read_file("/mnt/data/y").unwrap(), b"d");
        // Isolation: the root does not know the mount's file, the mount knows it
        // under its stripped path.
        assert!(v.with_root(|fs| fs.read_file("/mnt/foo")).is_err());
        assert_eq!(v.with_fs("/mnt/foo", |fs, p| fs.read_file(p).map(|_| p.to_string())).unwrap(), "/foo");
        assert!(!path_under("/mnt", "/mnt2"));
        assert_eq!(v.mount_points(), std::vec!["/mnt/data".to_string(), "/mnt".to_string()]);
    }

    #[test]
    fn cross_mount_rename_is_exdev_same_mount_works() {
        let mut v = TVfs::new(ram());
        v.mount("/mnt", eurofs(512));
        v.write_file("/a", b"x").unwrap();
        assert_eq!(v.rename("/a", "/mnt/a"), Err(FsError::Unsupported));
        v.write_file("/mnt/b", b"y").unwrap();
        v.rename("/mnt/b", "/mnt/c").unwrap();
        assert_eq!(v.read_file("/mnt/c").unwrap(), b"y");
    }

    #[test]
    fn umount_df_and_dynamic_mount() {
        let mut v = TVfs::new(ram());
        v.mount("/mnt", ram());
        assert_eq!(v.df().len(), 2);
        assert!(v.umount("/mnt"));
        assert!(!v.umount("/mnt"));
        assert_eq!(v.df().len(), 1);
        // A non-Send box cannot join a thread-safe table: refused, not unsound.
        assert_eq!(v.mount_fs("/x", Box::new(RamDisk::new(1 << 24))), Err(FsError::Unsupported));
        // The kernel flavour accepts it.
        let mut k: SharedVfs<dyn FileSystem> = SharedVfs::new(Box::new(RamDisk::new(1 << 24)));
        assert_eq!(k.mount_fs("/x", Box::new(RamDisk::new(1 << 24))), Ok(()));
        assert_eq!(k.list_mounts(), std::vec!["/x".to_string()]);
    }

    #[test]
    fn clones_share_one_table() {
        let mut a = TVfs::new(ram());
        let b = a.clone();
        b.mount("/m", ram());
        a.write_file("/m/f", b"1").unwrap();
        assert_eq!(b.read_file("/m/f").unwrap(), b"1");
    }

    /// Four threads, four mounts, real EuroFS on each: every write must read back
    /// byte-exact, from its own mount, with no interference.
    #[test]
    fn parallel_io_on_separate_mounts_is_isolated_and_exact() {
        let v = TVfs::new(ram());
        for i in 0..4 {
            v.mount(&std::format!("/d{i}"), eurofs(2048));
        }
        let hs: std::vec::Vec<_> = (0..4)
            .map(|t| {
                let mut v = v.clone();
                std::thread::spawn(move || {
                    for n in 0..150u32 {
                        let p = std::format!("/d{t}/f{}", n % 8);
                        let data: std::vec::Vec<u8> = (0..(64 + n as usize)).map(|k| (k as u32 ^ t ^ n) as u8).collect();
                        v.write_file(&p, &data).unwrap();
                        assert_eq!(v.read_file(&p).unwrap(), data, "thread {t} round {n}");
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        for t in 0..4 {
            assert_eq!(v.list_dir(&std::format!("/d{t}")).unwrap().len(), 8);
        }
    }

    /// Four threads on the SAME mount: the per-mount lock must serialise them
    /// (no lost or torn file).
    #[test]
    fn parallel_io_on_one_mount_is_serialised() {
        let v = TVfs::new(ram());
        v.mount("/shared", eurofs(4096));
        let hs: std::vec::Vec<_> = (0..4)
            .map(|t| {
                let mut v = v.clone();
                std::thread::spawn(move || {
                    for n in 0..100u32 {
                        let p = std::format!("/shared/t{t}-{n}");
                        v.write_file(&p, &[t as u8; 100]).unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let names = v.list_dir("/shared").unwrap();
        assert_eq!(names.len(), 400);
        for t in 0..4u8 {
            assert_eq!(v.read_file(&std::format!("/shared/t{t}-99")).unwrap(), std::vec![t; 100]);
        }
    }

    /// Mount and umount churn on one thread while others do I/O on other mounts
    /// and on the churned one: no panic, no use-after-free, I/O on stable mounts
    /// stays correct, I/O on the churned mount either succeeds or sees NotFound.
    #[test]
    fn mount_churn_during_io_is_safe() {
        let v = TVfs::new(ram());
        v.mount("/stable", eurofs(2048));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let churn = {
            let v = v.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    v.mount("/flip", ram());
                    v.umount("/flip");
                    n += 1;
                }
                n
            })
        };
        let workers: std::vec::Vec<_> = (0..3)
            .map(|t| {
                let mut v = v.clone();
                std::thread::spawn(move || {
                    for n in 0..300u32 {
                        let p = std::format!("/stable/w{t}");
                        v.write_file(&p, &n.to_le_bytes()).unwrap();
                        assert_eq!(v.read_file(&p).unwrap(), n.to_le_bytes());
                        // The churned mount may or may not be there; whatever answers
                        // must be a filesystem, never garbage.
                        let _ = v.write_file("/flip/x", b"x");
                        let _ = v.read_file("/flip/x");
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(churn.join().unwrap() > 0);
    }

    /// The point of per-mount locks, demonstrated on the host: while one thread
    /// holds mount A busy, a thread on mount B still makes progress. With a single
    /// global lock B would be stalled for the whole hold.
    #[test]
    fn a_busy_mount_does_not_block_another_mount() {
        let v = TVfs::new(ram());
        v.mount("/a", ram());
        v.mount("/b", ram());
        let held = std::sync::Arc::new(AtomicBool::new(false));
        let release = std::sync::Arc::new(AtomicBool::new(false));
        let holder = {
            let v = v.clone();
            let held = held.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                v.with_fs("/a/x", |_fs, _p| {
                    held.store(true, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                });
            })
        };
        while !held.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        let done = AtomicU64::new(0);
        let mut w = v.clone();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(50) {
            w.write_file("/b/y", b"progress").unwrap();
            done.fetch_add(1, Ordering::Relaxed);
        }
        release.store(true, Ordering::SeqCst);
        holder.join().unwrap();
        assert!(done.load(Ordering::Relaxed) > 10, "mount /b must progress while /a is held");
    }
}
