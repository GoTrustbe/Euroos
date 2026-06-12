//! EuroFS — Europees soeverein filesysteem (Track 2 van EuroKernel).
//!
//! Deze crate is `no_std` + `alloc` zodat ze in de kernel draait, maar onder
//! `cargo test` compileert ze met `std` (via `cfg_attr` hieronder) zodat de
//! volledige logica op de host getest kan worden — geen QEMU, geen hardware.
//!
//! Lagen:
//! - [`block`]      — `BlockDevice` abstractie + in-memory testdevice
//! - [`path`]       — pad-parsing utilities (no_std)
//! - [`fs`]         — `FileSystem` trait, gedeeld door ramdisk en EuroFS
//! - [`ramdisk`]    — in-memory FS (Fase 1, bootstrap voor de kernel)
//! - [`superblock`] — on-disk EuroFS superblok (Fase 2, CoW filesysteem)
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod badblocks;
pub mod block;
pub mod cache;
pub mod checksum;
pub mod disk;
pub mod fs;
pub mod path;
pub mod ramdisk;
pub mod superblock;
pub mod vfs;

pub use block::{BlockDevice, BlockError, BlockResult, MemoryBlockDevice};
pub use disk::EuroFs;
pub use fs::{
    DirEntry, EntryKind, FileSystem, FsError, FsResult, ScrubReport, SnapshotInfo, FLAG_APPEND_ONLY,
    FLAG_IMMUTABLE, SNAP_AUTO_ROLLBACK, SNAP_READONLY,
};
pub use ramdisk::RamDisk;
pub use vfs::Vfs;
pub use superblock::EuroFsSuperblock;
