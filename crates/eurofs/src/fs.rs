//! The `FileSystem` abstraction shared by ramdisk and (later) EuroFS.

use alloc::string::String;
use alloc::vec::Vec;

/// Error categories, POSIX-like but deliberately our own set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    NotADirectory,
    NotAFile,
    AlreadyExists,
    PermissionDenied,
    NoSpace,
    InvalidPath,
    IoError,
    Corruption,
    /// Operation not supported by this filesystem.
    Unsupported,
    /// Directory is not empty (e.g. with `remove_dir`).
    NotEmpty,
}

pub type FsResult<T> = Result<T, FsError>;

// ── L1: immutability flags ────────────────────────────────────────────────
/// IMMUTABLE: the file can NOT be modified, removed or renamed — not even
/// by root — until the flag is cleared (and that requires `CAP_IMMUTABLE_ADMIN`).
pub const FLAG_IMMUTABLE: u32 = 1 << 0;
/// APPEND_ONLY: writes may only EXTEND the existing content (the new data
/// must begin with the old); no overwriting, truncating or removing. Basis
/// for the tamper-evident audit log (P3).
pub const FLAG_APPEND_ONLY: u32 = 1 << 1;

// ── EuroSnap (Sprint S): CoW snapshots ──────────────────────────────────────
/// Snapshot flags.
pub const SNAP_READONLY: u32 = 0; // frozen state (default)
pub const SNAP_AUTO_ROLLBACK: u32 = 1 << 0; // auto-remove after a successful boot (G4 update)

/// Public description of a snapshot (for `snapshot_list`/the shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub id: u64,
    pub parent: u64,
    pub timestamp: u64,
    pub checkpoint_id: u64,
    pub flags: u32,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// POSIX permissions (e.g. 0o644 for files, 0o755 for directories).
    pub mode: u16,
    /// Last-modified time (kernel clock, seconds since boot/epoch; 0 = unknown).
    pub mtime: u64,
}

/// Outcome of an integrity check (scrub/fsck) — S7 storage reliability.
#[derive(Debug, Clone, Default)]
pub struct ScrubReport {
    /// Number of checked objects (inodes).
    pub objects: usize,
    /// Number of errors found (checksum/magic/struct/cross-link).
    pub errors: usize,
    /// Number of data blocks referenced by inodes.
    pub blocks_referenced: u64,
    /// Superblock (magic + checksum) intact?
    pub superblock_ok: bool,
    /// Do the referenced blocks match the free-space bitmap?
    pub bitmap_ok: bool,
    /// Number of things actually repaired by `repair()` (e.g. superblock slots).
    pub repaired: usize,
    /// Number of objects whose data-path checksum (XXH3 over the content) was verified.
    pub data_verified: usize,
    /// Number of objects with data corruption that could NOT be repaired (single disk,
    /// no redundancy — only with a mirror/RAID (B3) is reconstruction possible).
    pub data_unrecoverable: usize,
    /// Detail messages (bounded).
    pub messages: Vec<String>,
}

/// Core interface for every EuroKernel filesystem.
///
/// Deliberately minimal and synchronous: in a microkernel, FS I/O runs via IPC and
/// polling, not via `async` (no async runtime in kernel space).
pub trait FileSystem {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()>;
    fn remove_file(&mut self, path: &str) -> FsResult<()>;
    fn create_dir(&mut self, path: &str) -> FsResult<()>;

    /// Remove an EMPTY directory (like `rmdir`). A non-empty directory → `NotEmpty`; a
    /// file → `NotADirectory`. Default: not supported.
    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let _ = path;
        Err(FsError::Unsupported)
    }

    /// Rename/move `old` to `new` (like `mv`/`rename(2)`). An existing
    /// FILE at `new` is replaced; an existing DIRECTORY at `new` is an error.
    /// Default: not supported.
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let _ = (old, new);
        Err(FsError::Unsupported)
    }
    /// L1: read the immutability flags of a file (`FLAG_IMMUTABLE` | `FLAG_APPEND_ONLY`).
    /// Default 0 (mutable / not supported).
    fn get_flags(&self, path: &str) -> FsResult<u32> {
        let _ = path;
        Ok(0)
    }

    /// L1: set the immutability flags of a file. The CAPABILITY check
    /// (`CAP_IMMUTABLE_ADMIN`, L2) happens in the kernel layer ABOVE this call.
    /// Default: not supported.
    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        let _ = (path, flags);
        Err(FsError::Unsupported)
    }

    // ── EuroSnap (Sprint S): CoW snapshots — not supported by default ──
    /// Make a snapshot of the current FS state (cheap thanks to CoW: just a
    /// frozen root pointer). Returns the snapshot id.
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        let _ = (label, flags);
        Err(FsError::Unsupported)
    }
    /// List all snapshots (oldest → newest).
    fn snapshot_list(&self) -> Vec<SnapshotInfo> {
        Vec::new()
    }
    /// Restore the FS to the state of snapshot `id` (afterwards usually requires a
    /// remount/reboot for in-flight state).
    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        let _ = id;
        Err(FsError::Unsupported)
    }
    /// Remove a snapshot + reclaim its exclusive blocks (GC).
    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        let _ = id;
        Err(FsError::Unsupported)
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>>;
    fn exists(&self, path: &str) -> bool;
    fn metadata(&self, path: &str) -> FsResult<DirEntry>;
    /// `(total, free)` in bytes.
    fn space_info(&self) -> (u64, u64);

    /// Space per mountpoint for `df`: `(mountpoint, total, free)`. Default:
    /// only the root (`/`). The VFS overrides this with one line per mount.
    fn df(&self) -> Vec<(String, u64, u64)> {
        let (t, f) = self.space_info();
        alloc::vec![(String::from("/"), t, f)]
    }

    /// Integrity check (scrub/fsck): verify superblock + all inode checksums
    /// + structural consistency. Default: not supported (everything "ok", 0 objects).
    fn scrub(&self) -> ScrubReport {
        ScrubReport { superblock_ok: true, bitmap_ok: true, ..Default::default() }
    }

    /// Repair what is safely repairable (e.g. a degraded superblock slot from the
    /// valid A/B copy) and then return a fresh scrub report. Default:
    /// nothing to repair, equal to [`scrub`].
    fn repair(&mut self) -> ScrubReport {
        self.scrub()
    }

    /// Repair interface for future redundancy (mirror/RAID — B3): rewrite
    /// the logical block `lba` with a verified good copy. Default not
    /// supported (a single disk without redundancy cannot reconstruct).
    fn repair_block(&mut self, _lba: u64, _good: &[u8]) -> FsResult<()> {
        Err(FsError::Unsupported)
    }

    /// Set the kernel clock (Unix seconds) that the filesystem uses for `mtime`
    /// on create/write. Default: ignored (FS without time awareness).
    fn set_clock(&mut self, now: u64) {
        let _ = now;
    }
}
