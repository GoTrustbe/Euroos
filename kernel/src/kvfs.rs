//! kvfs: the kernel's root VFS as a shared, per-mount-locked handle (J1).
//!
//! The boot task builds the root VFS and keeps using it as its
//! `&mut dyn FileSystem`; a clone of the same handle is published here so code
//! on other cores (kwork jobs on the APs) can reach the filesystem too. Every
//! access goes through `SharedVfs`: the mount table under a reader-writer lock,
//! one lock per mounted filesystem.

use eurofs::{FileSystem, SharedVfs};

pub type KernelVfs = SharedVfs<dyn FileSystem>;

struct Published(KernelVfs);
// SAFETY: the kernel's filesystem objects are plain heap data plus block
// devices that do their own (port/MMIO) locking; none is tied to a core or a
// thread-local. `SharedVfs` only hands them out under their per-mount lock, so
// moving the handle between cores and sharing it is sound. (The type system
// cannot see this: `dyn FileSystem` carries no `Send` bound.)
unsafe impl Send for Published {}
unsafe impl Sync for Published {}

static ROOT: spin::Once<Published> = spin::Once::new();

/// Publish the root VFS (once, by the boot task).
pub fn publish(v: &KernelVfs) {
    ROOT.call_once(|| Published(v.clone()));
}

/// The root VFS from any core (None before the boot task published it).
pub fn root() -> Option<KernelVfs> {
    ROOT.get().map(|p| p.0.clone())
}
