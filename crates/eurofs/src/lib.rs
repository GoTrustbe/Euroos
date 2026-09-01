//! EuroFS — European sovereign filesystem (Track 2 of EuroKernel).
//!
//! This crate is `no_std` + `alloc` so it can run in the kernel, but under
//! `cargo test` it compiles with `std` (via the `cfg_attr` below) so the
//! full logic can be tested on the host — no QEMU, no hardware.
//!
//! Layers:
//! - [`block`]      — `BlockDevice` abstraction + in-memory test device
//! - [`path`]       — path-parsing utilities (no_std)
//! - [`fs`]         — `FileSystem` trait, shared by ramdisk and EuroFS
//! - [`ramdisk`]    — in-memory FS (Phase 1, bootstrap for the kernel)
//! - [`superblock`] — on-disk EuroFS superblock (Phase 2, CoW filesystem)
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod badblocks;
pub mod block;
pub mod cache;
pub mod checksum;
pub mod disk;
pub mod faulty;
pub mod fs;
pub mod path;
pub mod ramdisk;
pub mod superblock;
pub mod vfs;

pub use block::{BlockDevice, BlockError, BlockResult, MemoryBlockDevice};
pub use disk::EuroFs;
pub use fs::{
    DirEntry, EntryKind, FileSystem, FsError, FsResult, ScrubReport, SnapshotInfo, FLAG_APPEND_ONLY, FLAG_VERSIONED,
    FLAG_IMMUTABLE, SNAP_AUTO_ROLLBACK, SNAP_READONLY,
};
pub use ramdisk::RamDisk;
pub use vfs::Vfs;
pub use superblock::EuroFsSuperblock;
