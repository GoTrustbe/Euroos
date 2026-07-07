//! EuroFS on-disk filesystem (Track 2, Phase 2): copy-on-write, per-block
//! integrity, crash-consistent.
//!
//! Design (deliberate choices, explicitly documented):
//! - **Copy-on-write**: a mutation never overwrites live data. New data
//!   and a new inode are written to FREE blocks; only the atomic
//!   superblock update (checkpoint bump + flush) makes them "live". A crash before
//!   that update → the old state remains fully intact.
//! - **Allocator without on-disk bitmap**: the free space is reconstructed at `mount`
//!   by scanning all reachable blocks starting from the committed superblock.
//!   This way uncommitted (leaked) blocks are automatically free again
//!   — exactly the "space scan" from the spec. No bitmap that can de-synchronize.
//! - **Object map** (OID → inode block): currently a flat, CoW-rewritten table.
//!   Correct and simple; a B+tree (O(log n) at scale) is the Phase-3
//!   replacement with the same semantics. DELIBERATELY no B+tree now — see roadmap.
//! - **Inode** fills one 4 KiB block: header + up to 8 extents + inline data
//!   (≤ 3896 B) + XXH3 checksum. Directories are "files" whose data is a
//!   sequence of 64-byte dir entries — one data path for files and directories.
//!
//! Block size is fixed at 4096 for Phase 2 (DEFAULT_BLOCK_SIZE).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::block::BlockDevice;
use crate::checksum::xxh3_64;
use crate::fs::{DirEntry, EntryKind, FileSystem, FsError, FsResult, SnapshotInfo, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};
use crate::path::{filename, parent, split_path};
use crate::superblock::{EuroFsSuperblock, RESERVED_BLOCKS};
use core::sync::atomic::{AtomicU64, Ordering};

// PERF instrumentation (temporary): exposed via alloc_debug / `fsdebug`.
static RESOLVE_HITS: AtomicU64 = AtomicU64::new(0);
static RESOLVE_MISS: AtomicU64 = AtomicU64::new(0);
static BLOCK_READS: AtomicU64 = AtomicU64::new(0);

const BS: usize = 4096;
const INODE_MAGIC: u32 = 0x4546_494E; // "EFIN"
const ROOT_OID: u64 = 1;

// Inode block layout (offsets in bytes).
const OFF_MAGIC: usize = 0;
const OFF_OID: usize = 8;
const OFF_PARENT: usize = 16;
const OFF_TYPE: usize = 24;
const OFF_FLAGS: usize = 28; // u32 immutability flags (L1) — free slot, 0 = legacy/mutable
const OFF_MODE: usize = 26;
const OFF_SIZE: usize = 32;
const OFF_INLINE_LEN: usize = 40;
const OFF_EXTENT_COUNT: usize = 44;
const OFF_MTIME: usize = 48; // u64 last-modified
const OFF_DATA_CHECKSUM: usize = 56; // u64 XXH3 over the full file data (0 = legacy/unknown)
const OFF_EXTENTS: usize = 64; // 8 × 16 bytes
const MAX_EXTENTS: usize = 8;
const OFF_INLINE: usize = OFF_EXTENTS + MAX_EXTENTS * 16; // 192
const OFF_CHECKSUM: usize = BS - 8; // 4088
const INLINE_CAP: usize = OFF_CHECKSUM - OFF_INLINE; // 3896

const TYPE_FILE: u8 = 1;
const TYPE_DIR: u8 = 2;
const TYPE_SYMLINK: u8 = 3;
/// Max symlink hops before declaring a loop (POSIX `ELOOP`).
const SYMLINK_MAX_HOPS: u32 = 40;

const DIRENT_SIZE: usize = 64;
const DIRENT_NAME_CAP: usize = 48;

/// BUG-008: reject a name that would not fit a directory entry instead of silently
/// truncating it (a truncated name is stored under a different key and is then
/// unreachable by the name the caller used — `write` would falsely report success).
fn check_name(name: &str) -> FsResult<()> {
    if name.as_bytes().len() > DIRENT_NAME_CAP {
        Err(FsError::InvalidPath)
    } else {
        Ok(())
    }
}

// ── small little-endian helpers ──────────────────────────────────────────
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}
fn wr_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

/// In-memory representation of an inode (decoded from one block).
#[derive(Clone)]
struct Inode {
    oid: u64,
    parent: u64,
    otype: u8,
    /// L1: immutability flags (FLAG_IMMUTABLE | FLAG_APPEND_ONLY). 0 = mutable.
    flags: u32,
    mode: u16,
    size: u64,
    mtime: u64,
    /// XXH3 over the full file data (data-path integrity). 0 = not set
    /// (old/legacy format) → verification is then skipped.
    data_checksum: u64,
    extents: Vec<(u64, u32)>, // (physical_block, block_count)
    inline: Vec<u8>,
}

impl Inode {
    fn new(oid: u64, parent: u64, otype: u8, now: u64) -> Self {
        Self {
            oid,
            parent,
            otype,
            flags: 0,
            mode: if otype == TYPE_DIR { 0o755 } else { 0o644 },
            size: 0,
            mtime: now,
            data_checksum: 0,
            extents: Vec::new(),
            inline: Vec::new(),
        }
    }

    fn encode(&self) -> [u8; BS] {
        let mut b = [0u8; BS];
        wr_u32(&mut b, OFF_MAGIC, INODE_MAGIC);
        wr_u64(&mut b, OFF_OID, self.oid);
        wr_u64(&mut b, OFF_PARENT, self.parent);
        b[OFF_TYPE] = self.otype;
        wr_u32(&mut b, OFF_FLAGS, self.flags);
        wr_u16(&mut b, OFF_MODE, self.mode);
        wr_u64(&mut b, OFF_SIZE, self.size);
        wr_u64(&mut b, OFF_MTIME, self.mtime);
        wr_u64(&mut b, OFF_DATA_CHECKSUM, self.data_checksum);
        wr_u32(&mut b, OFF_INLINE_LEN, self.inline.len() as u32);
        wr_u32(&mut b, OFF_EXTENT_COUNT, self.extents.len() as u32);
        for (i, &(phys, cnt)) in self.extents.iter().enumerate() {
            let o = OFF_EXTENTS + i * 16;
            wr_u64(&mut b, o, phys);
            wr_u32(&mut b, o + 8, cnt);
        }
        if !self.inline.is_empty() {
            b[OFF_INLINE..OFF_INLINE + self.inline.len()].copy_from_slice(&self.inline);
        }
        let csum = xxh3_64(&b[..OFF_CHECKSUM]);
        wr_u64(&mut b, OFF_CHECKSUM, csum);
        b
    }

    fn decode(b: &[u8]) -> FsResult<Self> {
        if rd_u32(b, OFF_MAGIC) != INODE_MAGIC {
            return Err(FsError::Corruption);
        }
        if rd_u64(b, OFF_CHECKSUM) != xxh3_64(&b[..OFF_CHECKSUM]) {
            return Err(FsError::Corruption);
        }
        let inline_len = rd_u32(b, OFF_INLINE_LEN) as usize;
        let ext_count = rd_u32(b, OFF_EXTENT_COUNT) as usize;
        // An extent count > MAX_EXTENTS is not a valid inode — rather than silently
        // truncating (lost extents → data leak/loss), treat it as corruption.
        if ext_count > MAX_EXTENTS {
            return Err(FsError::Corruption);
        }
        let mut extents = Vec::new();
        for i in 0..ext_count {
            let o = OFF_EXTENTS + i * 16;
            extents.push((rd_u64(b, o), rd_u32(b, o + 8)));
        }
        Ok(Inode {
            oid: rd_u64(b, OFF_OID),
            parent: rd_u64(b, OFF_PARENT),
            otype: b[OFF_TYPE],
            flags: rd_u32(b, OFF_FLAGS),
            mode: rd_u16(b, OFF_MODE),
            size: rd_u64(b, OFF_SIZE),
            mtime: rd_u64(b, OFF_MTIME),
            data_checksum: rd_u64(b, OFF_DATA_CHECKSUM),
            extents,
            inline: b[OFF_INLINE..OFF_INLINE + inline_len].to_vec(),
        })
    }
}

// ── EuroSnap (Sprint S): CoW snapshots ──────────────────────────────────────
/// The reserved block in which the snapshot table lives (within RESERVED_BLOCKS;
/// block 1/2 = superblock-A/B, 0 = boot, 3..15 = slack → 8 is free).
const SNAPSHOT_TABLE_BLOCK: u64 = 8;
const SNAPSHOT_MAGIC: u32 = 0x5346_4E53; // "SNFS"
const MAX_SNAPSHOTS: usize = 32;
const SNAP_ENTRY_LEN: usize = 80; // id+parent+ts+objmap_root+map_blocks+ckpt+flags+label(32)

/// A snapshot entry: a frozen root pointer to a complete FS state.
#[derive(Clone)]
struct SnapshotEntry {
    id: u64,
    parent: u64,
    timestamp: u64,
    objmap_root: u64,
    map_blocks: u64,
    checkpoint_id: u64,
    flags: u32,
    label: String,
}

/// The on-disk EuroFS volume over an arbitrary `BlockDevice`.
pub struct EuroFs<D: BlockDevice> {
    dev: D,
    sb: EuroFsSuperblock,
    /// OID → block where the inode lives.
    objmap: BTreeMap<u64, u64>,
    /// In-memory free-space bitmap (1 bit per block; true = in use).
    used: Vec<bool>,
    next_oid: u64,
    now: u64,
    /// EuroSnap: active snapshots (frozen root pointers). Their blocks are PINNED in
    /// `rebuild_allocator` so that CoW does not reclaim them.
    snapshots: Vec<SnapshotEntry>,
    next_snap_id: u64,
    /// TRIM is deferred by one commit: blocks freed at commit N are discarded at
    /// commit N+1 (if still free). This preserves the single-generation rollback
    /// window — the previous checkpoint's blocks stay physically intact right after
    /// a commit, exactly what the A/B-superblock crash recovery relies on.
    pending_trim: Vec<u64>,
    /// PERF-001: path → oid resolution cache (interior-mutable, so `resolve` can read it
    /// behind `&self`). Path resolution is otherwise O(depth) per call (walk from root),
    /// making deep-tree workloads O(N²). Only successful, symlink-free `follow_final`
    /// resolutions are cached. INVALIDATION: cleared wholesale on `remove_file`/`remove_dir`/
    /// `rename`/`snapshot_rollback` — the only ops that can change a name→oid mapping.
    /// `create`/`write` only ADD names (never change an existing path's oid) so they don't
    /// invalidate. CoW changes the block an inode lives in, not its oid, so writes are safe.
    path_cache: spin::Mutex<BTreeMap<alloc::string::String, u64>>,
    /// PERF-002: oid → data extents, kept in sync by `write_object`. `rebuild_allocator`
    /// otherwise re-reads EVERY inode from disk on EVERY commit (O(objects) disk I/O per
    /// commit → O(N²) for bulk create). With this cache the rebuild marks `used` from
    /// memory. SAFETY: it's a hint — a missing entry self-heals by reading the inode from
    /// disk; it is CLEARED whenever the on-disk tree is reloaded (rollback/recovery), and
    /// `write_object` is the only code that changes an inode's extents, so it never goes
    /// stale for an object still in `objmap`.
    extents_cache: BTreeMap<u64, Vec<(u64, u32)>>,
}

impl<D: BlockDevice> EuroFs<D> {
    /// Format a fresh volume and return a mounted filesystem.
    pub fn format(dev: D, uuid: [u8; 16], now: u64) -> FsResult<Self> {
        assert_eq!(dev.block_size() as usize, BS, "EuroFS Phase 2 requires 4096-byte blocks");
        let total = dev.block_count();
        let sb = EuroFsSuperblock::new_empty(total, uuid, now);
        let mut fs = EuroFs {
            dev,
            sb,
            objmap: BTreeMap::new(),
            used: vec![false; total as usize],
            next_oid: ROOT_OID + 1,
            now,
            snapshots: Vec::new(),
            next_snap_id: 1,
            pending_trim: Vec::new(),
            path_cache: spin::Mutex::new(BTreeMap::new()),
            extents_cache: BTreeMap::new(),
        };
        // Reserve the first blocks (boot/superblock/backup/slack).
        for i in 0..(RESERVED_BLOCKS as usize).min(fs.used.len()) {
            fs.used[i] = true;
        }
        // Create the root directory (empty directory).
        let root = Inode::new(ROOT_OID, ROOT_OID, TYPE_DIR, now);
        let blk = fs.alloc_block()?;
        fs.write_block(blk, &root.encode())?;
        fs.objmap.insert(ROOT_OID, blk);
        fs.commit()?;
        Ok(fs)
    }

    /// Mount an existing volume: read superblock + object map, rebuild allocator.
    pub fn mount(dev: D, now: u64) -> FsResult<Self> {
        assert_eq!(dev.block_size() as usize, BS);
        let sb = EuroFsSuperblock::read_from(&dev)?;
        let total = dev.block_count();
        let mut fs = EuroFs {
            dev,
            sb,
            objmap: BTreeMap::new(),
            used: vec![false; total as usize],
            next_oid: ROOT_OID + 1,
            now,
            snapshots: Vec::new(),
            next_snap_id: 1,
            pending_trim: Vec::new(),
            path_cache: spin::Mutex::new(BTreeMap::new()),
            extents_cache: BTreeMap::new(),
        };
        fs.load_objmap()?;
        fs.load_snapshots()?;
        fs.rebuild_allocator()?;
        fs.next_oid = fs.objmap.keys().copied().max().unwrap_or(ROOT_OID) + 1;
        // Self-healing: read_from() succeeded so at least one A/B slot is valid. If the
        // OTHER slot is corrupt (due to an earlier torn write / bitrot), repair it
        // silently from the valid copy — so the superblock redundancy is complete again
        // right after the mount, without a manual `fsck repair`.
        if EuroFsSuperblock::degraded_slots(&fs.dev) == 1 {
            let _ = EuroFsSuperblock::heal_slots(&mut fs.dev);
        }
        Ok(fs)
    }

    pub fn superblock(&self) -> &EuroFsSuperblock {
        &self.sb
    }

    // ── block I/O ──────────────────────────────────────────────────────────
    fn read_block(&self, blk: u64, buf: &mut [u8; BS]) -> FsResult<()> {
        BLOCK_READS.fetch_add(1, Ordering::Relaxed);
        self.dev.read_blocks(blk, 1, buf).map_err(|_| FsError::IoError)
    }
    fn write_block(&mut self, blk: u64, data: &[u8; BS]) -> FsResult<()> {
        self.dev.write_blocks(blk, 1, data).map_err(|_| FsError::IoError)
    }

    // ── allocator ─────────────────────────────────────────────────────────
    fn alloc_block(&mut self) -> FsResult<u64> {
        for (i, slot) in self.used.iter_mut().enumerate() {
            if !*slot {
                *slot = true;
                return Ok(i as u64);
            }
        }
        Err(FsError::NoSpace)
    }

    fn alloc_contiguous(&mut self, count: usize) -> FsResult<u64> {
        if count == 0 {
            return Err(FsError::IoError);
        }
        let n = self.used.len();
        let mut i = 0;
        'outer: while i + count <= n {
            for j in 0..count {
                if self.used[i + j] {
                    i += j + 1;
                    continue 'outer;
                }
            }
            for j in 0..count {
                self.used[i + j] = true;
            }
            return Ok(i as u64);
        }
        Err(FsError::NoSpace)
    }

    fn free_count(&self) -> u64 {
        self.used.iter().filter(|u| !**u).count() as u64
    }

    /// Rebuild the allocator by marking all blocks reachable from the committed
    /// superblock. Automatically reclaims all leaked/old blocks.
    fn rebuild_allocator(&mut self) -> FsResult<()> {
        self.rebuild_allocator_trim(false)
    }

    /// As [`Self::rebuild_allocator`], but when `emit_trim` is set, every block that
    /// transitions allocated→free in this rebuild is reported to the backing device via
    /// [`BlockDevice::discard`] (TRIM). Used on the commit path so an SSD / thin backend
    /// can reclaim the CoW-superseded blocks; advisory, so discard errors are ignored.
    fn rebuild_allocator_trim(&mut self, emit_trim: bool) -> FsResult<()> {
        let n = self.used.len();
        let prev: Vec<bool> = if emit_trim { self.used.clone() } else { Vec::new() };
        for u in self.used.iter_mut() {
            *u = false;
        }
        for i in 0..(RESERVED_BLOCKS as usize).min(n) {
            self.used[i] = true;
        }
        // Object map blocks.
        let map_root = self.sb.object_map_root;
        let map_blocks = self.sb.extent_tree_root; // reused field: #map blocks
        for b in map_root..map_root + map_blocks {
            if (b as usize) < n {
                self.used[b as usize] = true;
            }
        }
        // Inodes + their data extents. PERF-002: take each object's extents from the
        // in-memory cache (no disk read) and fall back to reading the inode ONLY on a
        // cache miss — which also repopulates the cache. Correctness is independent of
        // cache state; only speed depends on it.
        let objs: Vec<(u64, u64)> = self.objmap.iter().map(|(&o, &b)| (o, b)).collect();
        for (oid, blk) in objs {
            if (blk as usize) < n {
                self.used[blk as usize] = true;
            }
            let exts = match self.extents_cache.get(&oid) {
                Some(e) => e.clone(),
                None => {
                    let mut buf = [0u8; BS];
                    self.read_block(blk, &mut buf)?;
                    let e = Inode::decode(&buf)?.extents;
                    self.extents_cache.insert(oid, e.clone());
                    e
                }
            };
            for (phys, cnt) in exts {
                for k in 0..cnt as u64 {
                    let bb = phys.saturating_add(k) as usize; // overflow-safe (corrupt extent)
                    if bb < n {
                        self.used[bb] = true;
                    }
                }
            }
        }
        // EuroSnap: PIN the blocks of every active snapshot so that the CoW reclaim
        // does not reuse them — this keeps every frozen state intact.
        let snaps: Vec<(u64, u64)> = self.snapshots.iter().map(|s| (s.objmap_root, s.map_blocks)).collect();
        for (root, blocks) in snaps {
            self.mark_state_blocks(root, blocks)?;
        }
        self.sb.free_blocks = self.free_count();
        // TRIM, deferred one generation: discard the blocks freed at the *previous*
        // commit that are STILL free now — so the just-committed checkpoint's freed
        // blocks stay physically intact for a one-generation rollback (the A/B
        // superblock crash-recovery guarantee). Then record this commit's newly-freed
        // blocks as the next pending set. Advisory: discard errors are ignored.
        if emit_trim {
            let pending = core::mem::take(&mut self.pending_trim);
            for &b in &pending {
                if (b as usize) < n && !self.used[b as usize] {
                    let _ = self.dev.discard(b, 1);
                }
            }
            let mut freed_now = Vec::new();
            for i in 0..n {
                if prev.get(i).copied().unwrap_or(false) && !self.used[i] {
                    freed_now.push(i as u64);
                }
            }
            self.pending_trim = freed_now;
        }
        Ok(())
    }

    /// Mark all blocks reachable from the objmap at `objmap_root`
    /// (the table blocks + the inode blocks + their data extents) as IN USE. For
    /// pinning snapshot states in [`Self::rebuild_allocator`].
    fn mark_state_blocks(&mut self, objmap_root: u64, map_blocks: u64) -> FsResult<()> {
        let n = self.used.len();
        for b in objmap_root..objmap_root.saturating_add(map_blocks) {
            if (b as usize) < n {
                self.used[b as usize] = true;
            }
        }
        let mut data = Vec::new();
        for b in 0..map_blocks {
            let mut buf = [0u8; BS];
            if self.read_block(objmap_root + b, &mut buf).is_err() {
                return Ok(()); // corrupt snapshot → safely skip
            }
            data.extend_from_slice(&buf);
        }
        if data.len() < 8 {
            return Ok(());
        }
        let count = rd_u64(&data, 0) as usize;
        for i in 0..count {
            let o = 8 + i * 16;
            if o + 16 > data.len() {
                break;
            }
            let blk = rd_u64(&data, o + 8);
            if (blk as usize) < n {
                self.used[blk as usize] = true;
            }
            let mut ib = [0u8; BS];
            if self.read_block(blk, &mut ib).is_err() {
                continue;
            }
            if let Ok(inode) = Inode::decode(&ib) {
                for (phys, cnt) in inode.extents {
                    for k in 0..cnt as u64 {
                        let bb = phys.saturating_add(k) as usize; // overflow-safe (corrupt extent)
                        if bb < n {
                            self.used[bb] = true;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Read the snapshot table from the reserved block (empty/legacy = no snapshots).
    fn load_snapshots(&mut self) -> FsResult<()> {
        let mut buf = [0u8; BS];
        if self.read_block(SNAPSHOT_TABLE_BLOCK, &mut buf).is_err() || rd_u32(&buf, 0) != SNAPSHOT_MAGIC {
            return Ok(());
        }
        let count = (rd_u32(&buf, 4) as usize).min(MAX_SNAPSHOTS);
        self.snapshots.clear();
        let mut max_id = 0u64;
        for i in 0..count {
            let o = 8 + i * SNAP_ENTRY_LEN;
            let id = rd_u64(&buf, o);
            let lab = &buf[o + 52..o + 52 + 28];
            let end = lab.iter().position(|&b| b == 0).unwrap_or(28);
            self.snapshots.push(SnapshotEntry {
                id,
                parent: rd_u64(&buf, o + 8),
                timestamp: rd_u64(&buf, o + 16),
                objmap_root: rd_u64(&buf, o + 24),
                map_blocks: rd_u64(&buf, o + 32),
                checkpoint_id: rd_u64(&buf, o + 40),
                flags: rd_u32(&buf, o + 48),
                label: String::from_utf8_lossy(&lab[..end]).into_owned(),
            });
            max_id = max_id.max(id);
        }
        self.next_snap_id = max_id + 1;
        Ok(())
    }

    /// Write the snapshot table to the reserved block (+ flush for durability).
    fn save_snapshots(&mut self) -> FsResult<()> {
        let mut buf = [0u8; BS];
        wr_u32(&mut buf, 0, SNAPSHOT_MAGIC);
        wr_u32(&mut buf, 4, self.snapshots.len().min(MAX_SNAPSHOTS) as u32);
        for (i, s) in self.snapshots.iter().take(MAX_SNAPSHOTS).enumerate() {
            let o = 8 + i * SNAP_ENTRY_LEN;
            wr_u64(&mut buf, o, s.id);
            wr_u64(&mut buf, o + 8, s.parent);
            wr_u64(&mut buf, o + 16, s.timestamp);
            wr_u64(&mut buf, o + 24, s.objmap_root);
            wr_u64(&mut buf, o + 32, s.map_blocks);
            wr_u64(&mut buf, o + 40, s.checkpoint_id);
            wr_u32(&mut buf, o + 48, s.flags);
            let lb = s.label.as_bytes();
            let ln = lb.len().min(28);
            buf[o + 52..o + 52 + ln].copy_from_slice(&lb[..ln]);
        }
        self.write_block(SNAPSHOT_TABLE_BLOCK, &buf)?;
        let _ = self.dev.flush();
        Ok(())
    }

    // ── object map (flat CoW table) ─────────────────────────────────────
    fn load_objmap(&mut self) -> FsResult<()> {
        let root = self.sb.object_map_root;
        let map_blocks = self.sb.extent_tree_root.max(1);
        let mut data = Vec::new();
        for b in 0..map_blocks {
            let mut buf = [0u8; BS];
            self.read_block(root + b, &mut buf)?;
            data.extend_from_slice(&buf);
        }
        // The objmap table is not separately checksummed; bound `count` to what is
        // actually present as data (audit H9), otherwise a corrupt/too-large
        // count indexes outside `data` and the FS panics on mount.
        let max_entries = data.len().saturating_sub(8) / 16;
        let count = (rd_u64(&data, 0) as usize).min(max_entries);
        self.objmap.clear();
        for i in 0..count {
            let o = 8 + i * 16;
            self.objmap.insert(rd_u64(&data, o), rd_u64(&data, o + 8));
        }
        Ok(())
    }

    /// Serialize the object map to fresh blocks (CoW) and return
    /// (root_block, block_count).
    fn write_objmap(&mut self) -> FsResult<(u64, u64)> {
        let entries: Vec<(u64, u64)> = self.objmap.iter().map(|(&k, &v)| (k, v)).collect();
        let bytes_needed = 8 + entries.len() * 16;
        let blocks_needed = bytes_needed.div_ceil(BS).max(1);
        let mut data = vec![0u8; blocks_needed * BS];
        wr_u64(&mut data, 0, entries.len() as u64);
        for (i, &(oid, blk)) in entries.iter().enumerate() {
            let o = 8 + i * 16;
            wr_u64(&mut data, o, oid);
            wr_u64(&mut data, o + 8, blk);
        }
        let root = self.alloc_contiguous(blocks_needed)?;
        for b in 0..blocks_needed {
            let mut buf = [0u8; BS];
            buf.copy_from_slice(&data[b * BS..(b + 1) * BS]);
            self.write_block(root + b as u64, &buf)?;
        }
        Ok((root, blocks_needed as u64))
    }

    /// The atomic commit: write object map + superblock, flush, and then rebuild
    /// the allocator (reclaims old blocks).
    fn commit(&mut self) -> FsResult<()> {
        let (root, blocks) = self.write_objmap()?;
        self.sb.object_map_root = root;
        self.sb.extent_tree_root = blocks; // #map blocks
        self.sb.checkpoint_id += 1;
        self.sb.last_written = self.now;
        self.sb.free_blocks = self.free_count();
        self.sb.checksum = self.sb.compute_checksum();
        self.sb.write_to(&mut self.dev).map_err(|_| FsError::IoError)?;
        // Reclaim + TRIM the CoW-superseded blocks now that the new checkpoint is durable.
        self.rebuild_allocator_trim(true)?;
        Ok(())
    }

    // ── reading/writing inode data ────────────────────────────────────────
    fn read_inode(&self, oid: u64) -> FsResult<Inode> {
        let blk = *self.objmap.get(&oid).ok_or(FsError::NotFound)?;
        let mut buf = [0u8; BS];
        self.read_block(blk, &mut buf)?;
        Inode::decode(&buf)
    }

    fn read_data(&self, inode: &Inode) -> FsResult<Vec<u8>> {
        let out = if inode.extents.is_empty() {
            inode.inline.clone()
        } else {
            let mut out = Vec::with_capacity(inode.size as usize);
            for &(phys, cnt) in &inode.extents {
                for k in 0..cnt as u64 {
                    let mut buf = [0u8; BS];
                    self.read_block(phys.saturating_add(k), &mut buf)?; // overflow-safe
                    out.extend_from_slice(&buf);
                }
            }
            out.truncate(inode.size as usize);
            out
        };
        // Verify the data checksum (skipped for legacy inodes with 0). This way
        // a corrupted data block yields an error instead of silently wrong bytes.
        if inode.data_checksum != 0 && xxh3_64(&out) != inode.data_checksum {
            return Err(FsError::Corruption);
        }
        Ok(out)
    }

    /// Write data for a (new) inode via CoW: allocate fresh data and
    /// inode blocks, write them, and update the object map. Commit happens separately.
    fn write_object(&mut self, mut inode: Inode, data: &[u8]) -> FsResult<()> {
        inode.size = data.len() as u64;
        // Data-path integrity: XXH3 over the full content, so that bit-rot in
        // a data block (outside the inode) can be detected on read/scrub.
        inode.data_checksum = xxh3_64(data);
        inode.extents.clear();
        inode.inline.clear();
        if data.len() <= INLINE_CAP {
            inode.inline = data.to_vec();
        } else {
            let nblocks = data.len().div_ceil(BS);
            // The extent block counter is a u32 on disk; refuse a file that does not
            // fit in one extent rather than silently truncating the counter (→ data loss).
            if nblocks > u32::MAX as usize {
                return Err(FsError::NoSpace);
            }
            let start = self.alloc_contiguous(nblocks)?;
            for b in 0..nblocks {
                let mut buf = [0u8; BS];
                let from = b * BS;
                let to = (from + BS).min(data.len());
                buf[..to - from].copy_from_slice(&data[from..to]);
                self.write_block(start + b as u64, &buf)?;
            }
            inode.extents.push((start, nblocks as u32));
        }
        let blk = self.alloc_block()?;
        let enc = inode.encode();
        self.write_block(blk, &enc)?;
        self.objmap.insert(inode.oid, blk);
        // PERF-002: keep the in-memory extents cache in sync (write_object is the ONLY
        // place an object's extents change), so rebuild_allocator needn't re-read this
        // inode from disk on the next commit.
        self.extents_cache.insert(inode.oid, inode.extents.clone());
        Ok(())
    }

    // ── directory helpers ─────────────────────────────────────────────────
    fn read_dir_entries(&self, dir_oid: u64) -> FsResult<Vec<(String, u64, u8)>> {
        let inode = self.read_inode(dir_oid)?;
        if inode.otype != TYPE_DIR {
            return Err(FsError::NotADirectory);
        }
        let data = self.read_data(&inode)?;
        let mut out = Vec::new();
        let mut o = 0;
        while o + DIRENT_SIZE <= data.len() {
            let oid = rd_u64(&data, o);
            let otype = data[o + 8];
            let name_len = data[o + 9] as usize;
            if oid != 0 && name_len <= DIRENT_NAME_CAP {
                let name = String::from_utf8_lossy(&data[o + 16..o + 16 + name_len]).into_owned();
                out.push((name, oid, otype));
            }
            o += DIRENT_SIZE;
        }
        Ok(out)
    }

    fn encode_dir_entries(entries: &[(String, u64, u8)]) -> Vec<u8> {
        let mut data = vec![0u8; entries.len() * DIRENT_SIZE];
        for (i, (name, oid, otype)) in entries.iter().enumerate() {
            let o = i * DIRENT_SIZE;
            wr_u64(&mut data, o, *oid);
            data[o + 8] = *otype;
            let nb = name.as_bytes();
            let n = nb.len().min(DIRENT_NAME_CAP);
            data[o + 9] = n as u8;
            data[o + 16..o + 16 + n].copy_from_slice(&nb[..n]);
        }
        data
    }

    fn resolve(&self, path: &str) -> FsResult<u64> {
        self.resolve_follow(path, true)
    }

    /// Resolve a path to an OID, following symlinks on every intermediate component
    /// and — when `follow_final` is true — on the final component too. An absolute
    /// symlink target restarts from root; a relative one resolves against the directory
    /// holding the link. Bounded by [`SYMLINK_MAX_HOPS`] (→ `InvalidPath` on a loop).
    fn resolve_follow(&self, path: &str, follow_final: bool) -> FsResult<u64> {
        // PERF-001 fast path: a cached symlink-free resolution (follow_final only).
        if follow_final {
            if let Some(&oid) = self.path_cache.lock().get(path) {
                RESOLVE_HITS.fetch_add(1, Ordering::Relaxed);
                return Ok(oid);
            }
            RESOLVE_MISS.fetch_add(1, Ordering::Relaxed);
        }
        let mut cur = ROOT_OID;
        let mut comps: Vec<String> = split_path(path).iter().map(|c| c.to_string()).collect();
        let mut i = 0;
        let mut hops = 0u32;
        while i < comps.len() {
            let entries = self.read_dir_entries(cur)?;
            let (oid, otype) = entries
                .iter()
                .find(|(name, _, _)| name == &comps[i])
                .map(|(_, o, t)| (*o, *t))
                .ok_or(FsError::NotFound)?;
            let is_last = i + 1 == comps.len();
            if otype == TYPE_SYMLINK && (!is_last || follow_final) {
                hops += 1;
                if hops > SYMLINK_MAX_HOPS {
                    return Err(FsError::InvalidPath); // ELOOP
                }
                let target = self.read_symlink_target(oid)?;
                let mut next: Vec<String> = split_path(&target).iter().map(|c| c.to_string()).collect();
                next.extend_from_slice(&comps[i + 1..]);
                if target.starts_with('/') {
                    cur = ROOT_OID; // absolute target → restart at root
                }
                // relative target → keep `cur` (the directory containing the link)
                comps = next;
                i = 0;
                continue;
            }
            cur = oid;
            i += 1;
        }
        // PERF-001: cache a successful, symlink-free resolution (bounded size).
        if follow_final && hops == 0 {
            let mut c = self.path_cache.lock();
            if c.len() >= 1024 {
                c.clear();
            }
            c.insert(path.to_string(), cur);
        }
        Ok(cur)
    }

    /// Read a symlink inode's stored target string (no following).
    fn read_symlink_target(&self, oid: u64) -> FsResult<String> {
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_SYMLINK {
            return Err(FsError::InvalidPath);
        }
        let bytes = self.read_data(&inode)?;
        String::from_utf8(bytes).map_err(|_| FsError::Corruption)
    }

    /// Update a directory (CoW) with a new entry list.
    fn rewrite_dir(&mut self, dir_oid: u64, entries: &[(String, u64, u8)]) -> FsResult<()> {
        let inode = self.read_inode(dir_oid)?;
        let data = Self::encode_dir_entries(entries);
        let fresh = Inode {
            extents: Vec::new(),
            inline: Vec::new(),
            size: 0,
            ..inode
        };
        self.write_object(fresh, &data)
    }

    /// Body of `write_file`, with `depth` bounding symlink-follow recursion.
    fn write_file_impl(&mut self, path: &str, data: &[u8], depth: u32) -> FsResult<()> {
        let parent_path = parent(path);
        let name = filename(path);
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        check_name(name)?; // BUG-008: don't silently truncate an over-long name
        let parent_oid = self.resolve(parent_path)?;
        let mut entries = self.read_dir_entries(parent_oid)?;

        let existing = entries.iter().find(|(n, _, _)| n == name).map(|(_, o, t)| (*o, *t));
        let oid = match existing {
            Some((_, t)) if t == TYPE_DIR => return Err(FsError::NotAFile),
            // POSIX: writing through a symlink writes its TARGET (it must NOT clobber the
            // link inode while leaving the dir-entry kind as Symlink — that desyncs the
            // entry/inode type). Redirect to the resolved target path, bounded by depth.
            Some((o, t)) if t == TYPE_SYMLINK => {
                if depth >= SYMLINK_MAX_HOPS {
                    return Err(FsError::InvalidPath); // ELOOP
                }
                let target = self.read_symlink_target(o)?;
                let real = if target.starts_with('/') {
                    target
                } else if parent_path.is_empty() || parent_path == "/" {
                    alloc::format!("/{target}")
                } else {
                    alloc::format!("{parent_path}/{target}")
                };
                return self.write_file_impl(&real, data, depth + 1);
            }
            Some((o, _)) => o,
            None => {
                let o = self.next_oid;
                self.next_oid += 1;
                entries.push((name.to_string(), o, TYPE_FILE));
                o
            }
        };

        // L1: enforce the immutability flags of an existing file + preserve them
        // across a (permitted) overwrite.
        let mut keep_flags = 0u32;
        if existing.is_some() {
            let old = self.read_inode(oid)?;
            keep_flags = old.flags;
            if old.flags & FLAG_IMMUTABLE != 0 {
                return Err(FsError::PermissionDenied); // immutable → no change
            }
            if old.flags & FLAG_APPEND_ONLY != 0 {
                // Append-only: the new data must EXTEND the old (same prefix).
                let old_data = self.read_data(&old)?;
                if data.len() < old_data.len() || data[..old_data.len()] != old_data[..] {
                    return Err(FsError::PermissionDenied);
                }
            }
        }

        let mut inode = Inode::new(oid, parent_oid, TYPE_FILE, self.now);
        inode.flags = keep_flags;
        self.write_object(inode, data)?;
        if existing.is_none() {
            self.rewrite_dir(parent_oid, &entries)?;
        }
        self.commit()
    }
}

impl<D: BlockDevice> FileSystem for EuroFs<D> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_FILE {
            return Err(FsError::NotAFile);
        }
        self.read_data(&inode)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        self.write_file_impl(path, data, 0)
    }

    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        let mut entries = self.read_dir_entries(parent_oid)?;
        let pos = entries
            .iter()
            .position(|(n, _, t)| n == name && *t == TYPE_FILE)
            .ok_or(FsError::NotFound)?;
        let oid = entries[pos].1;
        // L1: an immutable or append-only file may NOT be removed.
        let inode = self.read_inode(oid)?;
        if inode.flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0 {
            return Err(FsError::PermissionDenied);
        }
        entries.remove(pos);
        self.objmap.remove(&oid);
        self.rewrite_dir(parent_oid, &entries)?;
        self.path_cache.lock().clear(); // PERF-001: a name→oid mapping was removed
        self.commit()
    }

    fn get_flags(&self, path: &str) -> FsResult<u32> {
        let oid = self.resolve(path)?;
        Ok(self.read_inode(oid)?.flags)
    }

    // ── EuroSnap: CoW snapshots ──────────────────────────────────────────
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        if self.snapshots.len() >= MAX_SNAPSHOTS {
            return Err(FsError::NoSpace);
        }
        // Commit first → the current state is a clean, atomically-recorded root.
        self.commit()?;
        let id = self.next_snap_id;
        self.next_snap_id += 1;
        self.snapshots.push(SnapshotEntry {
            id,
            parent: self.sb.checkpoint_id, // provenance = the checkpoint it is based on
            timestamp: self.now,
            objmap_root: self.sb.object_map_root,
            map_blocks: self.sb.extent_tree_root,
            checkpoint_id: self.sb.checkpoint_id,
            flags,
            // Cut on a character boundary, not at byte 28 (audit H8: otherwise panic on
            // a multibyte UTF-8 character around the boundary).
            label: label.chars().take(28).collect::<String>(),
        });
        self.save_snapshots()?;
        Ok(id)
    }

    fn snapshot_list(&self) -> Vec<SnapshotInfo> {
        self.snapshots
            .iter()
            .map(|s| SnapshotInfo {
                id: s.id,
                parent: s.parent,
                timestamp: s.timestamp,
                checkpoint_id: s.checkpoint_id,
                flags: s.flags,
                label: s.label.clone(),
            })
            .collect()
    }

    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        let snap = self.snapshots.iter().find(|s| s.id == id).cloned().ok_or(FsError::NotFound)?;
        // Point the root pointer to the frozen state + reload that objmap.
        self.sb.object_map_root = snap.objmap_root;
        self.sb.extent_tree_root = snap.map_blocks;
        self.load_objmap()?;
        self.next_oid = self.objmap.keys().copied().max().unwrap_or(ROOT_OID) + 1;
        // Commit writes a fresh objmap + superblock; rebuild_allocator reclaims the
        // blocks of the abandoned state (unless pinned by another snapshot).
        self.path_cache.lock().clear(); // PERF-001: the whole tree changed
        // PERF-002: the on-disk tree was reloaded → drop any cached extents from the
        // abandoned state; rebuild_allocator will self-heal them from the rolled-back inodes.
        self.extents_cache.clear();
        self.commit()
    }

    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        let before = self.snapshots.len();
        self.snapshots.retain(|s| s.id != id);
        if self.snapshots.len() == before {
            return Err(FsError::NotFound);
        }
        self.save_snapshots()?;
        // GC: rebuild_allocator (via commit) now reclaims the exclusive-snapshot blocks.
        self.commit()
    }

    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_FILE {
            return Err(FsError::NotAFile);
        }
        // Rewrite the inode + the same data with the new flags (CoW). The
        // CAP_IMMUTABLE_ADMIN check (L2) lives in the kernel layer above this call.
        let data = self.read_data(&inode)?;
        let mut ni = inode;
        ni.flags = flags;
        self.write_object(ni, &data)?;
        self.commit()
    }

    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        if name.is_empty() {
            return Ok(()); // root
        }
        check_name(name)?; // BUG-008
        let mut entries = self.read_dir_entries(parent_oid)?;
        if entries.iter().any(|(n, _, _)| n == name) {
            return Err(FsError::AlreadyExists);
        }
        let oid = self.next_oid;
        self.next_oid += 1;
        let dir = Inode::new(oid, parent_oid, TYPE_DIR, self.now);
        self.write_object(dir, &[])?;
        entries.push((name.to_string(), oid, TYPE_DIR));
        self.rewrite_dir(parent_oid, &entries)?;
        self.commit()?;
        // PERF-001: cache the freshly-created dir so the NEXT-level mkdir resolves its
        // parent in O(1) instead of re-walking from root — turns deep-tree creation from
        // O(N²) into O(N). (Safe: a new name can't change an existing path's oid.)
        self.path_cache.lock().insert(path.to_string(), oid);
        Ok(())
    }

    fn create_symlink(&mut self, path: &str, target: &str) -> FsResult<()> {
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        check_name(name)?; // BUG-008
        let mut entries = self.read_dir_entries(parent_oid)?;
        if entries.iter().any(|(n, _, _)| n == name) {
            return Err(FsError::AlreadyExists);
        }
        let oid = self.next_oid;
        self.next_oid += 1;
        let link = Inode::new(oid, parent_oid, TYPE_SYMLINK, self.now);
        // The target string is the symlink's "data" (inline for the usual short paths).
        self.write_object(link, target.as_bytes())?;
        entries.push((name.to_string(), oid, TYPE_SYMLINK));
        self.rewrite_dir(parent_oid, &entries)?;
        self.commit()
    }

    fn read_link(&self, path: &str) -> FsResult<String> {
        // Resolve WITHOUT following the final component, so we read the link itself.
        let oid = self.resolve_follow(path, false)?;
        self.read_symlink_target(oid)
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let old_parent = self.resolve(parent(old))?;
        let old_name = filename(old);
        let new_parent = self.resolve(parent(new))?;
        let new_name = filename(new);
        if old_name.is_empty() || new_name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        check_name(new_name)?; // BUG-008

        // Look up the source entry.
        let src = self.read_dir_entries(old_parent)?;
        let (_, oid, otype) = src
            .iter()
            .find(|(n, _, _)| n == old_name)
            .cloned()
            .ok_or(FsError::NotFound)?;

        // L1: an immutable/append-only file may not be renamed/moved.
        if otype == TYPE_FILE {
            let inode = self.read_inode(oid)?;
            if inode.flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0 {
                return Err(FsError::PermissionDenied);
            }
        }

        // Anti-loop: a directory may not be moved INTO its own substructure.
        if otype == TYPE_DIR {
            let mut walk = new_parent;
            loop {
                if walk == oid {
                    return Err(FsError::InvalidPath);
                }
                if walk == ROOT_OID {
                    break;
                }
                walk = self.read_inode(walk)?.parent;
            }
        }

        if old_parent == new_parent {
            // Same directory: only change the name (and possibly replace an existing target file).
            let mut entries = src;
            if let Some(tp) = entries.iter().position(|(n, _, _)| n == new_name) {
                let (_, t_oid, t_type) = entries[tp].clone();
                if t_oid == oid {
                    return Ok(()); // old == new, nothing to do
                }
                if t_type == TYPE_DIR {
                    return Err(FsError::AlreadyExists); // do not overwrite directories
                }
                entries.remove(tp);
                self.objmap.remove(&t_oid);
            }
            let pos = entries
                .iter()
                .position(|(n, _, _)| n == old_name)
                .ok_or(FsError::NotFound)?;
            entries[pos].0 = new_name.to_string();
            self.rewrite_dir(old_parent, &entries)?;
        } else {
            // Moving to ANOTHER directory.
            let mut from = src;
            let mut to = self.read_dir_entries(new_parent)?;
            if let Some(tp) = to.iter().position(|(n, _, _)| n == new_name) {
                let (_, t_oid, t_type) = to[tp].clone();
                if t_type == TYPE_DIR {
                    return Err(FsError::AlreadyExists);
                }
                to.remove(tp);
                self.objmap.remove(&t_oid);
            }
            from.retain(|(n, _, _)| n != old_name);
            to.push((new_name.to_string(), oid, otype));
            // A moved DIRECTORY carries its parent reference along.
            if otype == TYPE_DIR {
                let mut inode = self.read_inode(oid)?;
                inode.parent = new_parent;
                let data = self.read_data(&inode)?;
                self.write_object(inode, &data)?;
            }
            self.rewrite_dir(old_parent, &from)?;
            self.rewrite_dir(new_parent, &to)?;
        }
        self.path_cache.lock().clear(); // PERF-001: a name→oid mapping changed
        self.commit()
    }

    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let oid = self.resolve(path)?;
        if oid == ROOT_OID {
            return Err(FsError::InvalidPath); // you do not remove the root
        }
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_DIR {
            return Err(FsError::NotADirectory);
        }
        if !self.read_dir_entries(oid)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        let mut entries = self.read_dir_entries(parent_oid)?;
        let pos = entries
            .iter()
            .position(|(n, _, t)| n == name && *t == TYPE_DIR)
            .ok_or(FsError::NotFound)?;
        entries.remove(pos);
        self.objmap.remove(&oid);
        self.rewrite_dir(parent_oid, &entries)?;
        self.path_cache.lock().clear(); // PERF-001: a name→oid mapping was removed
        self.commit()
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let oid = self.resolve(path)?;
        let entries = self.read_dir_entries(oid)?;
        let mut out = Vec::with_capacity(entries.len());
        for (name, child_oid, otype) in entries {
            let inode = self.read_inode(child_oid).ok();
            let (size, mode, mtime) = match &inode {
                Some(i) => (if otype == TYPE_FILE { i.size } else { 0 }, i.mode, i.mtime),
                None => (0, if otype == TYPE_DIR { 0o755 } else { 0o644 }, 0),
            };
            out.push(DirEntry {
                name,
                kind: match otype {
                    TYPE_DIR => EntryKind::Directory,
                    TYPE_SYMLINK => EntryKind::Symlink,
                    _ => EntryKind::File,
                },
                size,
                mode,
                mtime,
            });
        }
        Ok(out)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        Ok(DirEntry {
            name: filename(path).to_string(),
            kind: match inode.otype {
                TYPE_DIR => EntryKind::Directory,
                TYPE_SYMLINK => EntryKind::Symlink,
                _ => EntryKind::File,
            },
            size: inode.size,
            mode: inode.mode,
            mtime: inode.mtime,
        })
    }

    fn space_info(&self) -> (u64, u64) {
        let total = self.used.len() as u64 * BS as u64;
        let free = self.free_count() * BS as u64;
        (total, free)
    }

    fn alloc_debug(&self, _path: &str) -> Option<alloc::string::String> {
        let n = self.used.len();
        let free = self.free_count();
        // Largest contiguous free run, number of free runs, and a sample of the first
        // used-block positions (to see whether used blocks cluster low or scatter).
        let (mut largest, mut cur, mut runs, mut in_free) = (0usize, 0usize, 0usize, false);
        let mut first_used: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
        for i in 0..n {
            if self.used[i] {
                if first_used.len() < 24 {
                    first_used.push(i);
                }
                in_free = false;
                cur = 0;
            } else {
                if !in_free {
                    in_free = true;
                    runs += 1;
                }
                cur += 1;
                if cur > largest {
                    largest = cur;
                }
            }
        }
        Some(alloc::format!(
            "blocks={n} free={free} ({} MiB) largest_free_run={largest} blk ({} KiB) free_runs={runs} used={}\nfirst_used_blocks={first_used:?}\nperf: resolve_hits={} resolve_miss={} block_reads={}",
            free * BS as u64 / (1024 * 1024),
            largest * BS / 1024,
            n as u64 - free,
            RESOLVE_HITS.load(Ordering::Relaxed),
            RESOLVE_MISS.load(Ordering::Relaxed),
            BLOCK_READS.load(Ordering::Relaxed),
        ))
    }

    /// Scrub/fsck (S7): verify the superblock, EVERY inode checksum, and the
    /// structural consistency (extents within the disk, no cross-links, and the
    /// referenced blocks match the free-space bitmap).
    fn scrub(&self) -> crate::fs::ScrubReport {
        let mut r = crate::fs::ScrubReport { superblock_ok: true, bitmap_ok: true, ..Default::default() };
        // 1) Superblock: check BOTH A/B slots separately (re-reading from disk).
        //    1 degraded slot is still mountable AND repairable (see `repair`); only
        //    with two corrupt slots is the superblock truly lost.
        match EuroFsSuperblock::degraded_slots(&self.dev) {
            0 => {}
            1 => {
                r.errors += 1;
                r.messages.push(String::from(
                    "superblock: 1 A/B slot degraded (valid copy intact, mountable — recoverable via repair)",
                ));
            }
            _ => {
                r.superblock_ok = false;
                r.errors += 1;
                r.messages.push(String::from("superblock: BOTH slots corrupt (magic/checksum)"));
            }
        }
        // 2) Cross-reference all inodes + extents with a fresh reference bitmap.
        let total = self.dev.block_count();
        let mut referenced = alloc::vec![false; self.used.len()];
        for (&oid, &blk) in &self.objmap {
            r.objects += 1;
            if (blk as usize) < referenced.len() {
                referenced[blk as usize] = true; // the inode block itself
            }
            match self.read_inode(oid) {
                Ok(inode) => {
                    // Data-path scrub: verify the XXH3 over the content, so that bit-rot
                    // in a data block (outside the inode itself) is detected — not
                    // only corruption of the inode or the structure.
                    if inode.data_checksum != 0 {
                        match self.read_data(&inode) {
                            Ok(_) => r.data_verified += 1,
                            Err(_) => {
                                r.errors += 1;
                                // One disk, no redundancy → not recoverable (B3 mirror needed).
                                r.data_unrecoverable += 1;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!(
                                        "oid {oid}: DATA checksum mismatch (bit-rot) — UNRECOVERABLE (no redundancy)"
                                    ));
                                }
                            }
                        }
                    }
                    for &(phys, cnt) in &inode.extents {
                        r.blocks_referenced += cnt as u64;
                        if phys + cnt as u64 > total {
                            r.errors += 1;
                            if r.messages.len() < 8 {
                                r.messages.push(alloc::format!("oid {oid}: extent {phys}+{cnt} outside disk"));
                            }
                            continue;
                        }
                        for b in phys..phys + cnt as u64 {
                            let bi = b as usize;
                            if bi >= referenced.len() {
                                continue;
                            }
                            if referenced[bi] {
                                r.errors += 1;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!("block {b}: DOUBLE referenced (cross-link)"));
                                }
                            }
                            referenced[bi] = true;
                            if !self.used[bi] {
                                r.bitmap_ok = false;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!("block {b}: referenced but not marked as used"));
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    r.errors += 1;
                    if r.messages.len() < 8 {
                        r.messages.push(alloc::format!("oid {oid} @ block {blk}: inode magic/checksum CORRUPT"));
                    }
                }
            }
        }
        r
    }

    fn set_clock(&mut self, now: u64) {
        self.now = now;
    }

    fn repair(&mut self) -> crate::fs::ScrubReport {
        // First heal the A/B superblock redundancy: if one slot is corrupt and the
        // other valid, we rewrite the corrupt one from the valid copy. The
        // filesystem then has two valid superblock copies again.
        let healed = EuroFsSuperblock::heal_slots(&mut self.dev).unwrap_or(0);
        // Report the state AFTER the healing (so the report shows the repair).
        let mut r = self.scrub();
        r.repaired = healed;
        if healed > 0 {
            r.messages.push(alloc::format!(
                "REPAIR: {healed} superblock slot restored from the valid A/B copy"
            ));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemoryBlockDevice;

    fn dev(blocks: u64) -> MemoryBlockDevice {
        MemoryBlockDevice::new(blocks, BS as u32)
    }

    #[test]
    fn format_en_root_leeg() {
        let fs = EuroFs::format(dev(256), [1; 16], 100).unwrap();
        assert!(fs.exists("/"));
        assert_eq!(fs.list_dir("/").unwrap().len(), 0);
        let ckpt = fs.superblock().checkpoint_id;
        assert_eq!(ckpt, 2); // format commit
    }

    #[test]
    fn path_cache_invalidates_on_remove_and_rename() {
        // PERF-001: the path→oid cache must never serve a stale mapping after a
        // remove/rename. create/write must NOT need invalidation; remove/rename/rmdir must.
        let mut fs = EuroFs::format(dev(512), [9; 16], 1).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/a.txt", b"first").unwrap();
        assert_eq!(fs.read_file("/d/a.txt").unwrap(), b"first"); // populates the cache
        // remove → a cached path must resolve NotFound, not stale.
        fs.remove_file("/d/a.txt").unwrap();
        assert_eq!(fs.read_file("/d/a.txt"), Err(FsError::NotFound));
        // recreate at the same path (new oid + content) → must read the NEW content.
        fs.write_file("/d/a.txt", b"second").unwrap();
        assert_eq!(fs.read_file("/d/a.txt").unwrap(), b"second");
        // rename → old name gone, new name resolves.
        fs.rename("/d/a.txt", "/d/b.txt").unwrap();
        assert_eq!(fs.read_file("/d/a.txt"), Err(FsError::NotFound));
        assert_eq!(fs.read_file("/d/b.txt").unwrap(), b"second");
        // rmdir + recreate the directory → no stale /d oid.
        fs.remove_file("/d/b.txt").unwrap();
        fs.remove_dir("/d").unwrap();
        assert_eq!(fs.list_dir("/d"), Err(FsError::NotFound));
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/c.txt", b"third").unwrap();
        assert_eq!(fs.read_file("/d/c.txt").unwrap(), b"third");
    }

    #[test]
    fn schrijf_lees_klein_bestand() {
        let mut fs = EuroFs::format(dev(256), [2; 16], 1).unwrap();
        fs.write_file("/hallo.txt", b"Hello EuroKernel").unwrap();
        assert_eq!(fs.read_file("/hallo.txt").unwrap(), b"Hello EuroKernel");
        assert_eq!(fs.list_dir("/").unwrap()[0].name, "hallo.txt");
    }

    #[test]
    fn groot_bestand_via_extents() {
        let mut fs = EuroFs::format(dev(512), [3; 16], 1).unwrap();
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/big.bin", &big).unwrap();
        assert_eq!(fs.read_file("/big.bin").unwrap(), big);
        assert_eq!(fs.metadata("/big.bin").unwrap().size, 20_000);
    }

    #[test]
    fn inode_met_te_veel_extents_is_corruptie() {
        // Audit #5: an inode block that claims ext_count > MAX_EXTENTS may NOT be
        // silently truncated (lost extents → data loss), but must fail as corruption.
        let good = Inode {
            oid: 1,
            parent: 0,
            otype: 1,
            flags: 0,
            mode: 0,
            size: 0,
            mtime: 0,
            data_checksum: 0,
            extents: Vec::new(),
            inline: Vec::new(),
        };
        let mut buf = good.encode();
        wr_u32(&mut buf, OFF_EXTENT_COUNT, (MAX_EXTENTS + 1) as u32);
        // Recompute the checksum so it does NOT trip on the checksum but on the count.
        let cs = xxh3_64(&buf[..OFF_CHECKSUM]);
        wr_u64(&mut buf, OFF_CHECKSUM, cs);
        assert!(matches!(Inode::decode(&buf), Err(FsError::Corruption)));
    }

    #[test]
    fn overloop_veilige_extent_blijft_geldig() {
        // The saturating_add path must not break a normal large file.
        let mut fs = EuroFs::format(dev(512), [7; 16], 1).unwrap();
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 97) as u8).collect();
        fs.write_file("/x.bin", &big).unwrap();
        assert_eq!(fs.read_file("/x.bin").unwrap(), big);
    }

    #[test]
    fn data_path_scrub_detecteert_bitrot_in_datablok() {
        let mut fs = EuroFs::format(dev(512), [9; 16], 1).unwrap();
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/big.bin", &big).unwrap();

        // Healthy scrub: the file's data checksum is verified, 0 errors.
        let r = fs.scrub();
        assert!(r.data_verified >= 1, "scrub should verify the file data");
        assert_eq!(r.errors, 0);

        // Corrupt one byte in the first data block of the file, directly
        // on the device — this simulates bit-rot outside the inode.
        let oid = fs.resolve("/big.bin").unwrap();
        let (phys, _) = fs.read_inode(oid).unwrap().extents[0];
        let mut buf = [0u8; BS];
        fs.read_block(phys, &mut buf).unwrap();
        buf[10] ^= 0xFF;
        fs.write_block(phys, &buf).unwrap();

        // Reading now yields a Corruption error instead of silently-wrong bytes.
        assert_eq!(fs.read_file("/big.bin"), Err(FsError::Corruption));
        // And the scrub detects + reports the data corruption as UNRECOVERABLE
        // (one disk, no redundancy).
        let r2 = fs.scrub();
        assert!(r2.errors >= 1);
        assert_eq!(r2.data_unrecoverable, 1);
        assert!(r2.messages.iter().any(|m| m.contains("UNRECOVERABLE")));
        // The mirror-repair interface exists but is (one disk) not supported.
        assert_eq!(fs.repair_block(phys, &[0u8; BS]), Err(FsError::Unsupported));
    }

    #[test]
    fn data_checksum_overleeft_remount() {
        let mut dev = dev(512);
        {
            let mut fs = EuroFs::format(&mut dev, [10; 16], 1).unwrap();
            fs.write_file("/big.bin", &[42u8; 9000]).unwrap();
        }
        // After remount the data checksum remains valid and is verified.
        let fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.read_file("/big.bin").unwrap(), vec![42u8; 9000]);
        assert!(fs.scrub().data_verified >= 1);
    }

    #[test]
    fn mappen_en_nesting() {
        let mut fs = EuroFs::format(dev(256), [4; 16], 1).unwrap();
        fs.create_dir("/etc").unwrap();
        fs.create_dir("/etc/net").unwrap();
        fs.write_file("/etc/net/hosts", b"127.0.0.1 localhost\n").unwrap();
        assert_eq!(fs.read_file("/etc/net/hosts").unwrap(), b"127.0.0.1 localhost\n");
        let etc = fs.list_dir("/etc").unwrap();
        assert!(etc.iter().any(|e| e.name == "net" && e.kind == EntryKind::Directory));
    }

    #[test]
    fn persistentie_na_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [5; 16], 1).unwrap();
            fs.create_dir("/boot").unwrap();
            fs.write_file("/boot/version", b"EuroKernel v0.1\n").unwrap();
            fs.write_file("/boot/grote", &[7u8; 9000]).unwrap();
        }
        // Mount the same device again.
        let fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.read_file("/boot/version").unwrap(), b"EuroKernel v0.1\n");
        assert_eq!(fs.read_file("/boot/grote").unwrap(), vec![7u8; 9000]);
    }

    #[test]
    fn overschrijven_cow() {
        let mut fs = EuroFs::format(dev(256), [6; 16], 1).unwrap();
        fs.write_file("/f", b"old").unwrap();
        fs.write_file("/f", b"new content").unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"new content");
        assert_eq!(fs.list_dir("/").unwrap().len(), 1); // no duplicate
    }

    #[test]
    fn verwijderen() {
        let mut fs = EuroFs::format(dev(256), [7; 16], 1).unwrap();
        fs.write_file("/tmp", b"x").unwrap();
        assert!(fs.exists("/tmp"));
        fs.remove_file("/tmp").unwrap();
        assert!(!fs.exists("/tmp"));
        assert_eq!(fs.read_file("/tmp"), Err(FsError::NotFound));
    }

    #[test]
    fn crash_voor_checkpoint_behoudt_oude_staat() {
        // Prove crash consistency: a commit that "is lost" right before the atomic
        // superblock update must not corrupt the old state.
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [8; 16], 1).unwrap();
        fs.write_file("/f", b"OLD").unwrap();

        // Save the current (committed) superblock.
        let mut sb_old = [0u8; BS];
        fs.read_block(1, &mut sb_old).unwrap();

        // New write — commits fully.
        fs.write_file("/f", b"NEW").unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"NEW");

        // Simulate: the superblock update of the NEW commit never landed
        // (power loss right before step 5). Restore the old superblock on block 1 and 2.
        fs.write_block(1, &sb_old).unwrap();
        fs.write_block(2, &sb_old).unwrap();
        drop(fs);

        // Remount → must see the OLD content, not corrupt.
        let fs = EuroFs::mount(&mut dev, 9).unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"OLD");
    }

    #[test]
    fn inode_checksum_detecteert_corruptie() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [9; 16], 1).unwrap();
        fs.write_file("/a", b"data").unwrap();
        let blk = *fs.objmap.get(&fs.resolve("/a").unwrap()).unwrap();
        let mut buf = [0u8; BS];
        fs.read_block(blk, &mut buf).unwrap();
        buf[OFF_INLINE] ^= 0xFF; // corrupt inline data
        fs.write_block(blk, &buf).unwrap();
        assert_eq!(fs.read_file("/a"), Err(FsError::Corruption));
    }

    #[test]
    fn repair_heelt_gedegradeerd_backup_slot() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [12; 16], 1).unwrap();
            fs.write_file("/data.txt", b"important").unwrap();
            // Corrupt the BACKUP slot DURING operation (after mount; no remount, so
            // the auto-heal of mount does not pre-empt this — we test `repair` itself).
            fs.write_block(SUPERBLOCK_BACKUP_BLOCK, &[0xFFu8; BS]).unwrap();
            let before = fs.scrub();
            assert!(before.errors >= 1 && before.superblock_ok);
            let rep = fs.repair();
            assert_eq!(rep.repaired, 1);
            assert_eq!(rep.errors, 0); // no superblock error left after healing
            assert_eq!(fs.read_file("/data.txt").unwrap(), b"important");
        }
        // Both slots valid again.
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0);
    }

    #[test]
    fn repair_heelt_primair_slot_uit_backup() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [13; 16], 1).unwrap();
            fs.write_block(SUPERBLOCK_BLOCK, &[0u8; BS]).unwrap(); // primary corrupt
            assert_eq!(fs.repair().repaired, 1);
        }
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0);
    }

    #[test]
    fn mount_heelt_gedegradeerd_slot_automatisch() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [15; 16], 1).unwrap();
            fs.write_file("/x", b"y").unwrap();
        }
        // Corrupt a slot on the RAW disk (as if a crash wrecked it).
        dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 1);
        {
            // Mount restores the slot automatically (self-healing), without manual fsck.
            let fs = EuroFs::mount(&mut dev, 2).unwrap();
            assert_eq!(fs.read_file("/x").unwrap(), b"y");
        }
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0); // automatically healed
    }

    #[test]
    fn repair_kan_niet_helen_bij_twee_corrupte_slots() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK, SUPERBLOCK_BLOCK};
        let mut dev = dev(256);
        {
            let _ = EuroFs::format(&mut dev, [14; 16], 1).unwrap();
        }
        dev.write_blocks(SUPERBLOCK_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 2);
        // No valid source → heal does (safely) nothing.
        assert_eq!(EuroFsSuperblock::heal_slots(&mut dev).unwrap(), 0);
    }

    #[test]
    fn mtime_en_mode_bij_aanmaak() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [20; 16], 1000).unwrap();
        fs.write_file("/a.txt", b"hi").unwrap();
        fs.create_dir("/d").unwrap();
        let f = fs.metadata("/a.txt").unwrap();
        assert_eq!(f.mtime, 1000);
        assert_eq!(f.mode, 0o644);
        let d = fs.metadata("/d").unwrap();
        assert_eq!(d.mtime, 1000);
        assert_eq!(d.mode, 0o755);
    }

    #[test]
    fn mtime_loopt_mee_met_de_klok() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [21; 16], 1000).unwrap();
        fs.write_file("/x", b"v1").unwrap();
        assert_eq!(fs.metadata("/x").unwrap().mtime, 1000);
        // Clock forward + rewrite → mtime follows.
        fs.set_clock(2500);
        fs.write_file("/x", b"v2-with-longer-content").unwrap();
        assert_eq!(fs.metadata("/x").unwrap().mtime, 2500);
    }

    #[test]
    fn rename_bestand_zelfde_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [30; 16], 1).unwrap();
        fs.write_file("/a.txt", b"content").unwrap();
        fs.rename("/a.txt", "/b.txt").unwrap();
        assert!(!fs.exists("/a.txt"));
        assert_eq!(fs.read_file("/b.txt").unwrap(), b"content");
    }

    #[test]
    fn rename_verplaatst_naar_andere_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [31; 16], 1).unwrap();
        fs.create_dir("/sub").unwrap();
        fs.write_file("/x", b"data").unwrap();
        fs.rename("/x", "/sub/y").unwrap();
        assert!(!fs.exists("/x"));
        assert_eq!(fs.read_file("/sub/y").unwrap(), b"data");
    }

    #[test]
    fn rename_vervangt_bestaand_bestand() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [32; 16], 1).unwrap();
        fs.write_file("/a", b"new").unwrap();
        fs.write_file("/b", b"old").unwrap();
        fs.rename("/a", "/b").unwrap();
        assert!(!fs.exists("/a"));
        assert_eq!(fs.read_file("/b").unwrap(), b"new");
    }

    #[test]
    fn l1_immutable_blokkeert_wijzigingen() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [40; 16], 1).unwrap();
        fs.write_file("/sys", b"kernel-config").unwrap();
        fs.set_flags("/sys", FLAG_IMMUTABLE).unwrap();
        assert_eq!(fs.get_flags("/sys").unwrap(), FLAG_IMMUTABLE);
        // Writing, removing, renaming → all refused.
        assert_eq!(fs.write_file("/sys", b"hacked"), Err(FsError::PermissionDenied));
        assert_eq!(fs.remove_file("/sys"), Err(FsError::PermissionDenied));
        assert_eq!(fs.rename("/sys", "/elsewhere"), Err(FsError::PermissionDenied));
        // Reading still works, content unchanged.
        assert_eq!(fs.read_file("/sys").unwrap(), b"kernel-config");
        // Clear the flag → modifiable again (the L2 cap check lives in the kernel layer).
        fs.set_flags("/sys", 0).unwrap();
        fs.write_file("/sys", b"now-allowed").unwrap();
        assert_eq!(fs.read_file("/sys").unwrap(), b"now-allowed");
    }

    #[test]
    fn l1_append_only_alleen_uitbreiden() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [41; 16], 1).unwrap();
        fs.write_file("/audit.log", b"line1\n").unwrap();
        fs.set_flags("/audit.log", FLAG_APPEND_ONLY).unwrap();
        // Extending (same prefix + longer) → OK.
        fs.write_file("/audit.log", b"line1\nline2\n").unwrap();
        assert_eq!(fs.read_file("/audit.log").unwrap(), b"line1\nline2\n");
        // Shortening or a different prefix → refused (tamper-evident).
        assert_eq!(fs.write_file("/audit.log", b"line1\n"), Err(FsError::PermissionDenied));
        assert_eq!(fs.write_file("/audit.log", b"FORGED\n..........."), Err(FsError::PermissionDenied));
        // Removing → refused.
        assert_eq!(fs.remove_file("/audit.log"), Err(FsError::PermissionDenied));
    }

    #[test]
    fn l1_vlaggen_overleven_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [42; 16], 1).unwrap();
            fs.write_file("/boot/kernel", b"ELF...").unwrap_or_else(|_| {
                fs.create_dir("/boot").unwrap();
                fs.write_file("/boot/kernel", b"ELF...").unwrap();
            });
            fs.set_flags("/boot/kernel", FLAG_IMMUTABLE).unwrap();
        }
        // After remount the flag is persistent → writing remains refused.
        let mut fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.get_flags("/boot/kernel").unwrap(), FLAG_IMMUTABLE);
        assert_eq!(fs.write_file("/boot/kernel", b"x"), Err(FsError::PermissionDenied));
    }

    #[test]
    fn snap_create_modify_rollback() {
        let mut dev = dev(512);
        let mut fs = EuroFs::format(&mut dev, [50; 16], 1).unwrap();
        fs.write_file("/data", b"original-content").unwrap();
        let snap = fs.snapshot_create("before-change", crate::fs::SNAP_READONLY).unwrap();
        // Change after the snapshot.
        fs.write_file("/data", b"changed").unwrap();
        fs.write_file("/nieuw", b"added").unwrap();
        assert_eq!(fs.read_file("/data").unwrap(), b"changed");
        assert!(fs.exists("/nieuw"));
        // Rollback → back to the frozen state.
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/data").unwrap(), b"original-content");
        assert!(!fs.exists("/nieuw")); // the post-snapshot file is gone
    }

    #[test]
    fn snap_pint_grote_bestand_blokken() {
        // The real test: after a snapshot the (extent) blocks of the frozen
        // large-file version must stay PINNED, even when a lot is written afterwards.
        let mut dev = dev(1024);
        let mut fs = EuroFs::format(&mut dev, [53; 16], 1).unwrap();
        let big_a = alloc::vec![0xAAu8; 40000]; // > INLINE_CAP → real data extents
        fs.write_file("/big", &big_a).unwrap();
        let snap = fs.snapshot_create("big-v1", 0).unwrap();
        // Overwrite + many extra allocations: without pinning the old blocks would be
        // reused and the snapshot data overwritten.
        fs.write_file("/big", &alloc::vec![0xBBu8; 40000]).unwrap();
        for i in 0..8 {
            fs.write_file(&alloc::format!("/f{i}"), &alloc::vec![0xCCu8; 9000]).unwrap();
        }
        // Rollback → the old large data must be BYTE-for-BYTE intact.
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/big").unwrap(), big_a);
    }

    #[test]
    fn snap_overleeft_remount() {
        let mut dev = dev(512);
        let snap;
        {
            let mut fs = EuroFs::format(&mut dev, [51; 16], 1).unwrap();
            fs.write_file("/x", b"snap-state").unwrap();
            snap = fs.snapshot_create("s1", 0).unwrap();
            fs.write_file("/x", b"after-snap").unwrap();
        }
        // After remount the snapshot table is persistent.
        let mut fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.snapshot_list().len(), 1);
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/x").unwrap(), b"snap-state");
    }

    #[test]
    fn snap_list_en_delete_gc() {
        let mut dev = dev(512);
        let mut fs = EuroFs::format(&mut dev, [52; 16], 1).unwrap();
        fs.write_file("/a", b"1").unwrap();
        let s1 = fs.snapshot_create("first", 0).unwrap();
        fs.write_file("/a", b"2").unwrap();
        let s2 = fs.snapshot_create("second", 0).unwrap();
        assert_eq!(fs.snapshot_list().len(), 2);
        let free_before = fs.space_info().1;
        fs.snapshot_delete(s1).unwrap();
        assert_eq!(fs.snapshot_list().len(), 1);
        assert_eq!(fs.snapshot_delete(s1), Err(FsError::NotFound));
        // GC freed space (or kept it equal), never less.
        assert!(fs.space_info().1 >= free_before);
        // s2 stays usable for rollback.
        fs.write_file("/a", b"3").unwrap();
        fs.snapshot_rollback(s2).unwrap();
        assert_eq!(fs.read_file("/a").unwrap(), b"2");
    }

    #[test]
    fn rename_map_met_inhoud() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [33; 16], 1).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/f", b"in-dir").unwrap();
        fs.create_dir("/dest").unwrap();
        fs.rename("/d", "/dest/d2").unwrap();
        assert!(!fs.exists("/d"));
        assert_eq!(fs.read_file("/dest/d2/f").unwrap(), b"in-dir");
        assert_eq!(fs.metadata("/dest/d2").unwrap().kind, EntryKind::Directory);
    }

    #[test]
    fn rename_weigert_map_overschrijven_en_lus() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [34; 16], 1).unwrap();
        fs.create_dir("/a").unwrap();
        fs.create_dir("/b").unwrap();
        assert_eq!(fs.rename("/a", "/b"), Err(FsError::AlreadyExists)); // dir → dir
        assert_eq!(fs.rename("/a", "/a/sub"), Err(FsError::InvalidPath)); // loop
        assert_eq!(fs.rename("/weg", "/x"), Err(FsError::NotFound)); // source gone
    }

    #[test]
    fn rename_overleeft_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [35; 16], 1).unwrap();
            fs.create_dir("/etc").unwrap();
            fs.write_file("/old.conf", b"cfg").unwrap();
            fs.rename("/old.conf", "/etc/new.conf").unwrap();
        }
        let fs = EuroFs::mount(&mut dev, 0).unwrap();
        assert!(!fs.exists("/old.conf"));
        assert_eq!(fs.read_file("/etc/new.conf").unwrap(), b"cfg");
    }

    #[test]
    fn mtime_en_mode_overleven_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [22; 16], 7777).unwrap();
            fs.write_file("/keep", b"data").unwrap();
        }
        let fs = EuroFs::mount(&mut dev, 0).unwrap();
        assert_eq!(fs.metadata("/keep").unwrap().mtime, 7777);
        // list_dir ALSO yields mtime/mode per entry.
        let e = &fs.list_dir("/").unwrap()[0];
        assert_eq!(e.name, "keep");
        assert_eq!(e.mtime, 7777);
        assert_eq!(e.mode, 0o644);
    }

    #[test]
    fn remove_dir_lege_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [40; 16], 1).unwrap();
        fs.create_dir("/leeg").unwrap();
        assert!(fs.exists("/leeg"));
        fs.remove_dir("/leeg").unwrap();
        assert!(!fs.exists("/leeg"));
    }

    #[test]
    fn remove_dir_weigert_niet_leeg_bestand_en_root() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [41; 16], 1).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/f", b"x").unwrap();
        assert_eq!(fs.remove_dir("/d"), Err(FsError::NotEmpty)); // not empty
        fs.write_file("/file", b"y").unwrap();
        assert_eq!(fs.remove_dir("/file"), Err(FsError::NotADirectory)); // file
        assert_eq!(fs.remove_dir("/weg"), Err(FsError::NotFound)); // does not exist
        assert_eq!(fs.remove_dir("/"), Err(FsError::InvalidPath)); // root
        // Emptied → now removable.
        fs.remove_file("/d/f").unwrap();
        fs.remove_dir("/d").unwrap();
        assert!(!fs.exists("/d"));
    }

    #[test]
    fn symlink_create_readlink_and_follow() {
        let mut fs = EuroFs::format(dev(256), [9; 16], 100).unwrap();
        fs.write_file("/target.txt", b"hello via symlink").unwrap();
        // Absolute-target symlink.
        fs.create_symlink("/link", "/target.txt").unwrap();
        // read_link does NOT follow → returns the stored target.
        assert_eq!(fs.read_link("/link").unwrap(), "/target.txt");
        // metadata/list_dir report it as a symlink.
        assert_eq!(fs.metadata("/link").map(|m| m.kind), Ok(EntryKind::File)); // metadata follows → target is a file
        let entries = fs.list_dir("/").unwrap();
        assert!(entries.iter().any(|e| e.name == "link" && e.kind == EntryKind::Symlink));
        // Reading through the symlink follows to the target's contents.
        assert_eq!(fs.read_file("/link").unwrap(), b"hello via symlink");
        // read_link on a non-symlink is EINVAL.
        assert_eq!(fs.read_link("/target.txt"), Err(FsError::InvalidPath));
    }

    #[test]
    fn symlink_relative_target_and_intermediate_dir() {
        let mut fs = EuroFs::format(dev(256), [9; 16], 100).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/f.txt", b"in d").unwrap();
        // Relative target resolves against the directory holding the link (/d).
        fs.create_symlink("/d/rel", "f.txt").unwrap();
        assert_eq!(fs.read_file("/d/rel").unwrap(), b"in d");
        // Symlink to a directory used as an intermediate path component.
        fs.create_symlink("/dlink", "/d").unwrap();
        assert_eq!(fs.read_file("/dlink/f.txt").unwrap(), b"in d");
    }

    #[test]
    fn trim_discards_freed_cow_blocks() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [7; 16], 1).unwrap();
            // A multi-block file, then overwrite + remove it. Each commit supersedes the
            // previous CoW blocks, which rebuild_allocator_trim should TRIM.
            fs.write_file("/big", &alloc::vec![0xAB; 40 * 1024]).unwrap();
            fs.write_file("/big", &alloc::vec![0xCD; 40 * 1024]).unwrap();
            fs.remove_file("/big").unwrap();
        }
        assert!(
            dev.discarded_blocks() > 0,
            "expected TRIM (discard) of CoW-superseded blocks, got none"
        );
    }

    #[test]
    fn write_through_symlink_writes_target_not_corrupt() {
        let mut fs = EuroFs::format(dev(256), [9; 16], 100).unwrap();
        fs.write_file("/target.txt", b"old").unwrap();
        fs.create_symlink("/link", "/target.txt").unwrap();
        // Writing through the symlink must update the TARGET, and leave the link intact.
        fs.write_file("/link", b"new via link").unwrap();
        assert_eq!(fs.read_file("/target.txt").unwrap(), b"new via link");
        assert_eq!(fs.read_link("/link").unwrap(), "/target.txt"); // link still a symlink
        assert_eq!(fs.read_file("/link").unwrap(), b"new via link"); // follows to target
        // A self-referential symlink write must fail cleanly (ELOOP), not corrupt/hang.
        fs.create_symlink("/loop", "/loop").unwrap();
        assert_eq!(fs.write_file("/loop", b"x"), Err(FsError::InvalidPath));
    }

    #[test]
    fn symlink_loop_is_bounded() {
        let mut fs = EuroFs::format(dev(256), [9; 16], 100).unwrap();
        fs.create_symlink("/a", "/b").unwrap();
        fs.create_symlink("/b", "/a").unwrap();
        assert_eq!(fs.read_file("/a"), Err(FsError::InvalidPath)); // ELOOP, not a hang
    }

    #[test]
    fn symlink_survives_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [9; 16], 1).unwrap();
            fs.write_file("/t", b"persisted").unwrap();
            fs.create_symlink("/l", "/t").unwrap();
        }
        let fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.read_link("/l").unwrap(), "/t");
        assert_eq!(fs.read_file("/l").unwrap(), b"persisted");
    }
}
