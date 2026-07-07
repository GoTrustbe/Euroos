//! **EuroExFAT** — a mountable exFAT read **and write** driver (Sprint IO-4).
//!
//! Implements [`eurofs::FileSystem`] (read + write) over a 512-byte
//! [`eurofs::BlockDevice`], so an exFAT volume — large USB sticks / SD cards that ship
//! exFAT for >32 GB media — can be mounted into the EuroOS VFS and its files read and
//! modified.
//!
//! exFAT differs from FAT32: a dedicated boot region, a single FAT (with a
//! "NoFatChain" contiguous-file optimisation), an allocation bitmap, an up-case table,
//! and 32-byte **directory entry sets** (0x85 File + 0xC0 Stream-Extension + 0xC1
//! File-Name entries).
//!
//! ## Write support
//! New files / directories are allocated from the on-disk **allocation bitmap** and
//! linked with an **explicit FAT chain** (we never use the NoFatChain optimisation on
//! write, so every chain is recorded in the FAT and is unambiguously correct). Entry
//! sets are built with the proper exFAT **NameHash** and **SetChecksum**. The parent
//! directory is grown by a cluster when it has no free 32-byte slot.
//!
//! Pure `no_std`, no `unsafe`. Host-tested against a real `mkfs.exfat` image:
//! files written by this driver are read back byte-for-byte, and the originally
//! present data is never disturbed.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::{BlockDevice, DirEntry, EntryKind, FileSystem, FsError, FsResult};

const SECTOR: usize = 512;
const EOC: u32 = 0xFFFF_FFFF;
const BAD: u32 = 0xFFFF_FFF7;

/// Parsed exFAT boot sector geometry.
#[derive(Clone, Copy)]
struct Boot {
    fat_offset: u32,        // sectors
    cluster_heap_offset: u32, // sectors
    cluster_count: u32,
    root_first_cluster: u32,
    sectors_per_cluster: u32,
}

impl Boot {
    fn parse(s0: &[u8]) -> FsResult<Boot> {
        if s0.len() < SECTOR || &s0[3..11] != b"EXFAT   " || s0[510] != 0x55 || s0[511] != 0xAA {
            return Err(FsError::Corruption);
        }
        let bps_shift = s0[108];
        let spc_shift = s0[109];
        if bps_shift != 9 {
            return Err(FsError::Unsupported); // only 512-byte sectors
        }
        Ok(Boot {
            fat_offset: u32::from_le_bytes([s0[80], s0[81], s0[82], s0[83]]),
            cluster_heap_offset: u32::from_le_bytes([s0[88], s0[89], s0[90], s0[91]]),
            cluster_count: u32::from_le_bytes([s0[92], s0[93], s0[94], s0[95]]),
            root_first_cluster: u32::from_le_bytes([s0[96], s0[97], s0[98], s0[99]]),
            sectors_per_cluster: 1u32 << spc_shift,
        })
    }
    fn cluster_first_sector(&self, cl: u32) -> u32 {
        self.cluster_heap_offset + (cl - 2) * self.sectors_per_cluster
    }
    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }
}

/// A parsed child entry of a directory.
struct Child {
    name: String,
    first_cluster: u32,
    size: u64,
    is_dir: bool,
    no_fat_chain: bool,
}

/// Location of the allocation bitmap (a 0x81 entry in the root directory).
#[derive(Clone, Copy)]
struct Bitmap {
    first_cluster: u32,
    length: u64, // bytes
}

/// A mounted exFAT volume (read + write).
pub struct ExFat<D: BlockDevice> {
    dev: D,
    boot: Boot,
    /// Allocation bitmap, located on mount (None if the volume has none, which a real
    /// exFAT volume always has — writes then fail with `Corruption`).
    bitmap: Option<Bitmap>,
}

impl<D: BlockDevice> ExFat<D> {
    pub fn mount(dev: D) -> FsResult<Self> {
        if dev.block_size() != SECTOR as u32 {
            return Err(FsError::Unsupported);
        }
        let mut s0 = [0u8; SECTOR];
        dev.read_blocks(0, 1, &mut s0).map_err(|_| FsError::IoError)?;
        let boot = Boot::parse(&s0)?;
        let mut fs = ExFat { dev, boot, bitmap: None };
        fs.bitmap = fs.locate_bitmap();
        Ok(fs)
    }

    fn rsec(&self, lba: u32, buf: &mut [u8; SECTOR]) -> FsResult<()> {
        self.dev.read_blocks(lba as u64, 1, buf).map_err(|_| FsError::IoError)
    }

    fn wsec(&mut self, lba: u32, buf: &[u8; SECTOR]) -> FsResult<()> {
        self.dev.write_blocks(lba as u64, 1, buf).map_err(|_| FsError::IoError)
    }

    fn fat_next(&self, cl: u32) -> u32 {
        let byte = self.boot.fat_offset as u64 * SECTOR as u64 + cl as u64 * 4;
        let sec = (byte / SECTOR as u64) as u32;
        let off = (byte % SECTOR as u64) as usize;
        let mut buf = [0u8; SECTOR];
        if self.rsec(sec, &mut buf).is_err() {
            return EOC;
        }
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    /// Write a single FAT entry (LE u32) for cluster `cl`.
    fn fat_set(&mut self, cl: u32, val: u32) -> FsResult<()> {
        let byte = self.boot.fat_offset as u64 * SECTOR as u64 + cl as u64 * 4;
        let sec = (byte / SECTOR as u64) as u32;
        let off = (byte % SECTOR as u64) as usize;
        let mut buf = [0u8; SECTOR];
        self.rsec(sec, &mut buf)?;
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
        self.wsec(sec, &buf)
    }

    /// Read `byte_len` bytes of a cluster chain starting at `first`. If `no_fat_chain`,
    /// the clusters are contiguous; otherwise follow the FAT. `byte_len == 0` with
    /// `want_all` reads until end-of-chain (for directories of unknown size).
    fn read_chain(&self, first: u32, byte_len: usize, no_fat_chain: bool, want_all: bool) -> FsResult<Vec<u8>> {
        let cb = self.boot.cluster_bytes();
        let mut out = Vec::new();
        let mut cl = first;
        let mut guard = 0u32;
        while cl >= 2 && cl < BAD && guard < 1 << 24 {
            guard += 1;
            let base = self.boot.cluster_first_sector(cl);
            for s in 0..self.boot.sectors_per_cluster {
                if !want_all && out.len() >= byte_len {
                    break;
                }
                let mut buf = [0u8; SECTOR];
                self.rsec(base + s, &mut buf)?;
                out.extend_from_slice(&buf);
            }
            if !want_all && out.len() >= byte_len {
                break;
            }
            cl = if no_fat_chain { cl + 1 } else { self.fat_next(cl) };
            let _ = cb;
        }
        if !want_all {
            out.truncate(byte_len);
        }
        Ok(out)
    }

    /// Parse a directory (entry sets) into its children.
    fn read_dir(&self, first: u32, byte_len: usize, no_fat_chain: bool) -> FsResult<Vec<Child>> {
        let want_all = byte_len == 0;
        let bytes = self.read_chain(first, byte_len, no_fat_chain, want_all)?;
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + 32 <= bytes.len() {
            let typ = bytes[i];
            if typ == 0x00 {
                break; // end of directory
            }
            if typ != 0x85 {
                i += 32; // not an in-use File entry (bitmap/upcase/label/deleted) → skip
                continue;
            }
            // File entry set: 0x85 + SecondaryCount secondaries.
            let secondary = bytes[i + 1] as usize;
            let attrs = u16::from_le_bytes([bytes[i + 4], bytes[i + 5]]);
            let is_dir = attrs & 0x10 != 0;
            let mut first_cluster = 0u32;
            let mut size = 0u64;
            let mut no_chain = false;
            let mut name_len = 0usize;
            let mut name_u16: Vec<u16> = Vec::new();
            for k in 1..=secondary {
                let p = i + k * 32;
                if p + 32 > bytes.len() {
                    break;
                }
                match bytes[p] {
                    0xC0 => {
                        // Stream Extension.
                        no_chain = bytes[p + 1] & 0x02 != 0;
                        name_len = bytes[p + 3] as usize;
                        first_cluster = u32::from_le_bytes([bytes[p + 20], bytes[p + 21], bytes[p + 22], bytes[p + 23]]);
                        size = u64::from_le_bytes([
                            bytes[p + 24], bytes[p + 25], bytes[p + 26], bytes[p + 27],
                            bytes[p + 28], bytes[p + 29], bytes[p + 30], bytes[p + 31],
                        ]);
                    }
                    0xC1 => {
                        // File Name entry: offset 1 = flags, offsets 2..32 = 15 UTF-16 units.
                        for j in 0..15 {
                            let o = p + 2 + j * 2;
                            name_u16.push(u16::from_le_bytes([bytes[o], bytes[o + 1]]));
                        }
                    }
                    _ => {}
                }
            }
            name_u16.truncate(name_len);
            let name = String::from_utf16_lossy(&name_u16);
            if !name.is_empty() {
                out.push(Child { name, first_cluster, size, is_dir, no_fat_chain: no_chain });
            }
            i += (1 + secondary) * 32;
        }
        Ok(out)
    }

    // ──────────────────────────────────────────────────────────────────────
    // Write support
    // ──────────────────────────────────────────────────────────────────────

    /// Scan the root directory for the 0x81 Allocation-Bitmap entry and return its
    /// location. exFAT always lays this out as the *first* in-use entry of the root.
    fn locate_bitmap(&self) -> Option<Bitmap> {
        let bytes = self
            .read_chain(self.boot.root_first_cluster, 0, false, true)
            .ok()?;
        let mut i = 0usize;
        while i + 32 <= bytes.len() {
            let typ = bytes[i];
            if typ == 0x00 {
                break;
            }
            if typ == 0x81 {
                let first_cluster =
                    u32::from_le_bytes([bytes[i + 20], bytes[i + 21], bytes[i + 22], bytes[i + 23]]);
                let length = u64::from_le_bytes([
                    bytes[i + 24], bytes[i + 25], bytes[i + 26], bytes[i + 27],
                    bytes[i + 28], bytes[i + 29], bytes[i + 30], bytes[i + 31],
                ]);
                return Some(Bitmap { first_cluster, length });
            }
            i += 32;
        }
        None
    }

    /// Absolute (volume) sector + byte offset of bitmap *byte index* `byte_idx`,
    /// following the bitmap's own (FAT-chained) cluster chain.
    fn bitmap_sector_for_byte(&self, byte_idx: u64) -> FsResult<(u32, usize)> {
        let bm = self.bitmap.ok_or(FsError::Corruption)?;
        if byte_idx >= bm.length {
            return Err(FsError::NoSpace); // bit beyond the bitmap → no such cluster
        }
        let cb = self.boot.cluster_bytes() as u64;
        let cluster_index = byte_idx / cb; // which cluster of the bitmap
        let within = byte_idx % cb;
        // Walk the FAT chain to the target cluster.
        let mut cl = bm.first_cluster;
        let mut step = 0u64;
        while step < cluster_index {
            cl = self.fat_next(cl);
            if cl < 2 || cl >= BAD {
                return Err(FsError::Corruption);
            }
            step += 1;
        }
        let sec = self.boot.cluster_first_sector(cl) + (within / SECTOR as u64) as u32;
        let off = (within % SECTOR as u64) as usize;
        Ok((sec, off))
    }

    /// Test whether cluster `cl` is marked allocated in the bitmap. Bit (cl-2).
    fn bitmap_get(&self, cl: u32) -> FsResult<bool> {
        let bit = (cl - 2) as u64;
        let (sec, off) = self.bitmap_sector_for_byte(bit / 8)?;
        let mut buf = [0u8; SECTOR];
        self.rsec(sec, &mut buf)?;
        Ok(buf[off] & (1u8 << (bit % 8)) != 0)
    }

    /// Set (`alloc=true`) or clear (`alloc=false`) cluster `cl`'s allocation bit and
    /// write the affected bitmap sector back.
    fn bitmap_set(&mut self, cl: u32, alloc: bool) -> FsResult<()> {
        let bit = (cl - 2) as u64;
        let (sec, off) = self.bitmap_sector_for_byte(bit / 8)?;
        let mut buf = [0u8; SECTOR];
        self.rsec(sec, &mut buf)?;
        let mask = 1u8 << (bit % 8);
        if alloc {
            buf[off] |= mask;
        } else {
            buf[off] &= !mask;
        }
        self.wsec(sec, &buf)
    }

    /// Find the first free cluster (scanning the bitmap), mark it allocated, and return
    /// it. Does not touch the FAT. Returns `NoSpace` if the volume is full.
    fn alloc_one_cluster(&mut self) -> FsResult<u32> {
        let count = self.boot.cluster_count;
        for cl in 2..(2 + count) {
            if !self.bitmap_get(cl)? {
                self.bitmap_set(cl, true)?;
                return Ok(cl);
            }
        }
        Err(FsError::NoSpace)
    }

    /// Allocate `n` (>=1) clusters, mark them in the bitmap, and link them as an
    /// explicit FAT chain (last entry = EOC). Returns the first cluster.
    fn alloc_chain(&mut self, n: usize) -> FsResult<u32> {
        debug_assert!(n >= 1);
        let mut clusters = Vec::with_capacity(n);
        for _ in 0..n {
            match self.alloc_one_cluster() {
                Ok(c) => clusters.push(c),
                Err(e) => {
                    // Roll back any partial allocation so we don't leak bitmap bits.
                    for &c in &clusters {
                        let _ = self.bitmap_set(c, false);
                    }
                    return Err(e);
                }
            }
        }
        for w in 0..clusters.len() {
            let val = if w + 1 < clusters.len() { clusters[w + 1] } else { EOC };
            self.fat_set(clusters[w], val)?;
        }
        Ok(clusters[0])
    }

    /// Free an explicit FAT cluster chain starting at `first`: clear each bitmap bit and
    /// reset its FAT entry to 0 (free).
    fn free_chain(&mut self, first: u32) -> FsResult<()> {
        let mut cl = first;
        let mut guard = 0u32;
        while cl >= 2 && cl < BAD && guard < 1 << 24 {
            guard += 1;
            let next = self.fat_next(cl);
            self.bitmap_set(cl, false)?;
            self.fat_set(cl, 0)?;
            cl = next;
        }
        Ok(())
    }

    /// Write `data` into the FAT-chained cluster chain starting at `first`, zero-padding
    /// the tail of the last cluster. The chain must already be long enough.
    fn write_chain(&mut self, first: u32, data: &[u8]) -> FsResult<()> {
        let cb = self.boot.cluster_bytes();
        let mut cl = first;
        let mut pos = 0usize;
        let mut guard = 0u32;
        while pos < data.len() && cl >= 2 && cl < BAD && guard < 1 << 24 {
            guard += 1;
            let base = self.boot.cluster_first_sector(cl);
            for s in 0..self.boot.sectors_per_cluster {
                let mut buf = [0u8; SECTOR];
                let take = core::cmp::min(SECTOR, data.len().saturating_sub(pos));
                if take > 0 {
                    buf[..take].copy_from_slice(&data[pos..pos + take]);
                    pos += take;
                }
                self.wsec(base + s, &buf)?;
            }
            let _ = cb;
            if pos >= data.len() {
                break;
            }
            cl = self.fat_next(cl);
        }
        Ok(())
    }

    /// exFAT up-case for a single UTF-16 unit. **Limitation:** ASCII-only — `a`–`z`
    /// are mapped to `A`–`Z`, everything else (including non-ASCII Unicode) is left as
    /// is. This matches the NameHash that `mkfs.exfat`/Windows compute for our ASCII
    /// test names; a fully correct driver would consult the volume's 0x82 up-case
    /// table for the complete Unicode case mapping.
    fn upcase_unit(u: u16) -> u16 {
        if (0x61..=0x7A).contains(&u) {
            u - 0x20
        } else {
            u
        }
    }

    /// exFAT NameHash over the up-cased UTF-16 name (MS exFAT spec, NameHash
    /// algorithm): rolling 16-bit hash over the little-endian bytes of each up-cased
    /// UTF-16 unit.
    fn name_hash(name_u16: &[u16]) -> u16 {
        let mut hash: u16 = 0;
        for &u in name_u16 {
            let up = Self::upcase_unit(u);
            let bytes = up.to_le_bytes();
            for &b in &bytes {
                hash = ((hash << 15) | (hash >> 1)).wrapping_add(b as u16);
            }
        }
        hash
    }

    /// exFAT entry-set SetChecksum (MS exFAT spec): rolling 16-bit checksum over every
    /// byte of all entries in the set, *skipping* bytes 2 and 3 of the first (0x85)
    /// entry (the checksum field itself).
    fn set_checksum(entries: &[u8]) -> u16 {
        let mut sum: u16 = 0;
        for (idx, &b) in entries.iter().enumerate() {
            if idx == 2 || idx == 3 {
                continue;
            }
            sum = ((sum << 15) | (sum >> 1)).wrapping_add(b as u16);
        }
        sum
    }

    /// Number of secondary entries (0xC0 + 0xC1×ceil(len/15)) for a name of
    /// `name_len` UTF-16 units.
    fn name_entries(name_len: usize) -> usize {
        (name_len + 14) / 15
    }

    /// Locate the parent directory of `path` (its first cluster) and the final
    /// component name. The parent must exist and be a directory. Root → parent is the
    /// root directory itself.
    fn split_parent<'a>(&self, path: &'a str) -> FsResult<(u32, &'a str)> {
        let trimmed = path.trim_end_matches('/');
        let (parent_path, name) = match trimmed.rfind('/') {
            Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let parent = self.resolve_dir(parent_path)?;
        Ok((parent, name))
    }

    /// Resolve a directory path to its first cluster (root for empty path).
    fn resolve_dir(&self, path: &str) -> FsResult<u32> {
        let mut cur = self.boot.root_first_cluster;
        let mut cur_size = 0u64;
        let mut cur_chain = false;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let children = self.read_dir(cur, cur_size as usize, cur_chain)?;
            let ch = children
                .into_iter()
                .find(|c| c.name.eq_ignore_ascii_case(part))
                .ok_or(FsError::NotFound)?;
            if !ch.is_dir {
                return Err(FsError::NotADirectory);
            }
            cur = ch.first_cluster;
            cur_size = ch.size;
            cur_chain = ch.no_fat_chain;
        }
        Ok(cur)
    }

    /// Read the full raw bytes of directory `first_cluster` (FAT-chained), returning
    /// the bytes plus the list of clusters that back it (so callers can map a byte
    /// offset to a volume sector for in-place updates).
    fn read_dir_raw(&self, first_cluster: u32) -> FsResult<(Vec<u8>, Vec<u32>)> {
        let cb = self.boot.cluster_bytes();
        let mut out = Vec::new();
        let mut clusters = Vec::new();
        let mut cl = first_cluster;
        let mut guard = 0u32;
        while cl >= 2 && cl < BAD && guard < 1 << 24 {
            guard += 1;
            clusters.push(cl);
            let base = self.boot.cluster_first_sector(cl);
            for s in 0..self.boot.sectors_per_cluster {
                let mut buf = [0u8; SECTOR];
                self.rsec(base + s, &mut buf)?;
                out.extend_from_slice(&buf);
            }
            let _ = cb;
            cl = self.fat_next(cl);
        }
        Ok((out, clusters))
    }

    /// Map a byte offset inside a directory (described by its cluster list) to the
    /// absolute volume sector and in-sector offset.
    fn dir_byte_to_sector(&self, clusters: &[u32], byte_off: usize) -> Option<(u32, usize)> {
        let cb = self.boot.cluster_bytes();
        let ci = byte_off / cb;
        let within = byte_off % cb;
        let cl = *clusters.get(ci)?;
        let sec = self.boot.cluster_first_sector(cl) + (within / SECTOR) as u32;
        Some((sec, within % SECTOR))
    }

    /// Write `entries` (a contiguous entry set, multiple of 32 bytes) into the
    /// directory starting at byte offset `byte_off`. The directory must already be
    /// large enough (callers grow it first).
    fn dir_write_entries(&mut self, clusters: &[u32], byte_off: usize, entries: &[u8]) -> FsResult<()> {
        for (k, chunk) in entries.chunks(32).enumerate() {
            let off = byte_off + k * 32;
            let (sec, so) = self
                .dir_byte_to_sector(clusters, off)
                .ok_or(FsError::Corruption)?;
            let mut buf = [0u8; SECTOR];
            self.rsec(sec, &mut buf)?;
            buf[so..so + 32].copy_from_slice(chunk);
            self.wsec(sec, &buf)?;
        }
        Ok(())
    }

    /// Find a run of `need` consecutive free 32-byte slots in directory `first_cluster`.
    /// A slot is free if its type byte is 0x00 (terminator) or has InUse bit clear
    /// (0xE5-deleted, i.e. type & 0x80 == 0). If no run is found, grow the directory by
    /// one cluster (linking it into the dir's FAT chain) and return the offset at the
    /// start of the new cluster. Returns (byte_offset, fresh_cluster_list).
    fn dir_find_slot(&mut self, first_cluster: u32, need: usize) -> FsResult<(usize, Vec<u32>)> {
        let (bytes, clusters) = self.read_dir_raw(first_cluster)?;
        let n_slots = bytes.len() / 32;
        let mut run_start: Option<usize> = None;
        let mut run = 0usize;
        for slot in 0..n_slots {
            let typ = bytes[slot * 32];
            let free = typ == 0x00 || (typ & 0x80) == 0;
            if free {
                if run == 0 {
                    run_start = Some(slot);
                }
                run += 1;
                if run >= need {
                    return Ok((run_start.unwrap() * 32, clusters));
                }
            } else {
                run = 0;
                run_start = None;
            }
            // A 0x00 terminator means the rest of the cluster(s) is unused → all free.
            if typ == 0x00 {
                // From here on everything is free; if there's enough room, use it.
                let remaining = n_slots - run_start.unwrap();
                if remaining >= need {
                    return Ok((run_start.unwrap() * 32, clusters));
                }
                break;
            }
        }
        // No room: grow the directory by one cluster, link it into the chain.
        let new_cl = self.alloc_one_cluster()?;
        self.fat_set(new_cl, EOC)?;
        // Zero the new cluster.
        let base = self.boot.cluster_first_sector(new_cl);
        let zero = [0u8; SECTOR];
        for s in 0..self.boot.sectors_per_cluster {
            self.wsec(base + s, &zero)?;
        }
        // Link: walk to the last cluster of the dir chain and point it at new_cl.
        let last = *clusters.last().ok_or(FsError::Corruption)?;
        self.fat_set(last, new_cl)?;
        // The slot offset is the start of the freshly added cluster.
        let new_offset = clusters.len() * self.boot.cluster_bytes();
        let mut new_clusters = clusters;
        new_clusters.push(new_cl);
        Ok((new_offset, new_clusters))
    }

    /// Build an exFAT entry set (0x85 File + 0xC0 Stream-Ext + N×0xC1 File-Name) for
    /// `name` with the given data location. `is_dir` selects the directory attribute.
    /// `first_cluster`/`size` describe the data (0/0 for an empty file).
    fn build_entry_set(name: &str, is_dir: bool, first_cluster: u32, size: u64) -> FsResult<Vec<u8>> {
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_len = name_u16.len();
        if name_len == 0 || name_len > 255 {
            return Err(FsError::InvalidPath);
        }
        let n_name = Self::name_entries(name_len);
        let secondary = 1 + n_name; // 0xC0 + name entries
        let total = (1 + secondary) * 32;
        let mut e = alloc::vec![0u8; total];

        // 0x85 File entry.
        e[0] = 0x85;
        e[1] = secondary as u8;
        // [2..4] SetChecksum — filled last.
        let attrs: u16 = if is_dir { 0x10 } else { 0x20 }; // dir vs archive
        e[4..6].copy_from_slice(&attrs.to_le_bytes());
        // timestamps/[8..24] left zero (mtime unknown) — accepted by exFAT.

        // 0xC0 Stream Extension (second entry).
        let s = 32;
        e[s] = 0xC0;
        // flags: bit0 AllocationPossible (=1 when a cluster is allocated),
        //        bit1 NoFatChain (=0, we always use the FAT chain).
        e[s + 1] = if first_cluster >= 2 { 0x01 } else { 0x00 };
        e[s + 3] = name_len as u8;
        let hash = Self::name_hash(&name_u16);
        e[s + 4..s + 6].copy_from_slice(&hash.to_le_bytes());
        // ValidDataLength [8..16] and DataLength [24..32] both = size.
        e[s + 8..s + 16].copy_from_slice(&size.to_le_bytes());
        e[s + 20..s + 24].copy_from_slice(&first_cluster.to_le_bytes());
        e[s + 24..s + 32].copy_from_slice(&size.to_le_bytes());

        // 0xC1 File-Name entries.
        for k in 0..n_name {
            let p = (2 + k) * 32;
            e[p] = 0xC1;
            for j in 0..15 {
                let ni = k * 15 + j;
                let u = if ni < name_len { name_u16[ni] } else { 0 };
                let o = p + 2 + j * 2;
                e[o..o + 2].copy_from_slice(&u.to_le_bytes());
            }
        }

        let checksum = Self::set_checksum(&e);
        e[2..4].copy_from_slice(&checksum.to_le_bytes());
        Ok(e)
    }

    /// Mark the entry set for `name` in directory `parent_cluster` as deleted by
    /// clearing the InUse bit (bit 7) of every entry's type byte — the standard exFAT
    /// "deleted" representation (0x85→0x05, 0xC0→0x40, 0xC1→0x41). Does NOT free the
    /// data clusters (callers do that for files).
    fn remove_entry(&mut self, parent_cluster: u32, name: &str) -> FsResult<()> {
        let (bytes, clusters) = self.read_dir_raw(parent_cluster)?;
        let mut i = 0usize;
        while i + 32 <= bytes.len() {
            let typ = bytes[i];
            if typ == 0x00 {
                break;
            }
            if typ != 0x85 {
                i += 32;
                continue;
            }
            let secondary = bytes[i + 1] as usize;
            // Reconstruct the name from this set.
            let mut name_len = 0usize;
            let mut name_u16: Vec<u16> = Vec::new();
            for k in 1..=secondary {
                let p = i + k * 32;
                if p + 32 > bytes.len() {
                    break;
                }
                match bytes[p] {
                    0xC0 => name_len = bytes[p + 3] as usize,
                    0xC1 => {
                        for j in 0..15 {
                            let o = p + 2 + j * 2;
                            name_u16.push(u16::from_le_bytes([bytes[o], bytes[o + 1]]));
                        }
                    }
                    _ => {}
                }
            }
            name_u16.truncate(name_len);
            let this_name = String::from_utf16_lossy(&name_u16);
            if this_name.eq_ignore_ascii_case(name) {
                // Clear InUse on every entry of the set.
                for k in 0..=secondary {
                    let off = i + k * 32;
                    if off + 32 > bytes.len() {
                        break;
                    }
                    let (sec, so) = self
                        .dir_byte_to_sector(&clusters, off)
                        .ok_or(FsError::Corruption)?;
                    let mut buf = [0u8; SECTOR];
                    self.rsec(sec, &mut buf)?;
                    buf[so] &= 0x7F; // clear InUse bit
                    self.wsec(sec, &buf)?;
                }
                return Ok(());
            }
            i += (1 + secondary) * 32;
        }
        Err(FsError::NotFound)
    }

    /// Insert a freshly built entry set into directory `parent_cluster`.
    fn insert_entry(&mut self, parent_cluster: u32, entries: &[u8]) -> FsResult<()> {
        let need = entries.len() / 32;
        let (off, clusters) = self.dir_find_slot(parent_cluster, need)?;
        self.dir_write_entries(&clusters, off, entries)?;
        Ok(())
    }

    /// Resolve a path to its child record. The root directory has no entry set, so we
    /// represent it as (root_first_cluster, len=0/want-all, fat-chained, dir).
    fn resolve(&self, path: &str) -> FsResult<Child> {
        let mut cur = Child {
            name: String::new(),
            first_cluster: self.boot.root_first_cluster,
            size: 0,
            is_dir: true,
            no_fat_chain: false,
        };
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !cur.is_dir {
                return Err(FsError::NotADirectory);
            }
            let children = self.read_dir(cur.first_cluster, cur.size as usize, cur.no_fat_chain)?;
            cur = children
                .into_iter()
                .find(|c| c.name.eq_ignore_ascii_case(part))
                .ok_or(FsError::NotFound)?;
        }
        Ok(cur)
    }
}

impl<D: BlockDevice> FileSystem for ExFat<D> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let c = self.resolve(path)?;
        if c.is_dir {
            return Err(FsError::NotAFile);
        }
        if c.size == 0 || c.first_cluster < 2 {
            return Ok(Vec::new());
        }
        self.read_chain(c.first_cluster, c.size as usize, c.no_fat_chain, false)
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let c = self.resolve(path)?;
        if !c.is_dir {
            return Err(FsError::NotADirectory);
        }
        let children = self.read_dir(c.first_cluster, c.size as usize, c.no_fat_chain)?;
        Ok(children
            .into_iter()
            .map(|ch| DirEntry {
                name: ch.name,
                kind: if ch.is_dir { EntryKind::Directory } else { EntryKind::File },
                size: ch.size,
                mode: if ch.is_dir { 0o755 } else { 0o644 },
                mtime: 0,
            })
            .collect())
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let c = self.resolve(path)?;
        let name = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/").into();
        Ok(DirEntry {
            name,
            kind: if c.is_dir { EntryKind::Directory } else { EntryKind::File },
            size: c.size,
            mode: if c.is_dir { 0o755 } else { 0o644 },
            mtime: 0,
        })
    }

    fn space_info(&self) -> (u64, u64) {
        // Total from the cluster heap; free would require scanning the allocation bitmap.
        let total = self.boot.cluster_count as u64 * self.boot.cluster_bytes() as u64;
        (total, 0)
    }

    /// Create (or overwrite) a regular file at `path` with `data`. A directory at the
    /// same name is rejected. An existing file with the same name is removed first
    /// (freeing its clusters) and a fresh entry is written.
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        if self.bitmap.is_none() {
            return Err(FsError::Corruption);
        }
        let (parent, name) = self.split_parent(path)?;

        // Reject a directory at the name; remove an existing file to overwrite.
        let existing = self.read_dir(parent, 0, false)?;
        if let Some(c) = existing.iter().find(|c| c.name.eq_ignore_ascii_case(name)) {
            if c.is_dir {
                return Err(FsError::AlreadyExists);
            }
            // Overwrite: remove the old entry AND free its cluster chain (else the old
            // clusters leak — they stay marked allocated with nothing referencing them).
            let old_first = c.first_cluster;
            self.remove_entry(parent, name)?;
            if old_first >= 2 {
                self.free_chain(old_first)?;
            }
        }

        // Allocate + write the data clusters (explicit FAT chain).
        let (first_cluster, size) = if data.is_empty() {
            (0u32, 0u64)
        } else {
            let cb = self.boot.cluster_bytes();
            let n = (data.len() + cb - 1) / cb;
            let first = self.alloc_chain(n)?;
            self.write_chain(first, data)?;
            (first, data.len() as u64)
        };

        let entries = Self::build_entry_set(name, false, first_cluster, size)?;
        self.insert_entry(parent, &entries)?;
        self.dev.flush().map_err(|_| FsError::IoError)?;
        Ok(())
    }

    /// Remove a regular file: delete its entry set and free its cluster chain.
    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        if self.bitmap.is_none() {
            return Err(FsError::Corruption);
        }
        let (parent, name) = self.split_parent(path)?;
        let children = self.read_dir(parent, 0, false)?;
        let c = children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .ok_or(FsError::NotFound)?;
        if c.is_dir {
            return Err(FsError::NotAFile);
        }
        let first = c.first_cluster;
        self.remove_entry(parent, name)?;
        if first >= 2 {
            self.free_chain(first)?;
        }
        self.dev.flush().map_err(|_| FsError::IoError)?;
        Ok(())
    }

    /// Create an empty directory at `path` (one zeroed cluster, FAT-chained).
    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        if self.bitmap.is_none() {
            return Err(FsError::Corruption);
        }
        let (parent, name) = self.split_parent(path)?;
        let existing = self.read_dir(parent, 0, false)?;
        if existing.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            return Err(FsError::AlreadyExists);
        }

        // One cluster, zeroed, EOC-terminated chain.
        let cl = self.alloc_chain(1)?;
        let base = self.boot.cluster_first_sector(cl);
        let zero = [0u8; SECTOR];
        for s in 0..self.boot.sectors_per_cluster {
            self.wsec(base + s, &zero)?;
        }
        // A directory's DataLength is its allocated size (one cluster).
        let size = self.boot.cluster_bytes() as u64;
        let entries = Self::build_entry_set(name, true, cl, size)?;
        self.insert_entry(parent, &entries)?;
        self.dev.flush().map_err(|_| FsError::IoError)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eurofs::MemoryBlockDevice;

    /// Mount the committed real-mkfs.exfat fixture (3 MiB, fsck-clean).
    fn fixture() -> ExFat<MemoryBlockDevice> {
        let img: &[u8] = include_bytes!("../testdata/exfat.img");
        let sectors = (img.len() / SECTOR) as u64;
        let mut dev = MemoryBlockDevice::new(sectors, SECTOR as u32);
        dev.write_blocks(0, sectors as u32, img).unwrap();
        ExFat::mount(dev).expect("mount real exFAT image")
    }

    /// Regression for the audit HIGH: overwriting a file must FREE the old cluster
    /// chain, not leak it. Count allocated clusters before/after repeated overwrites.
    #[test]
    fn overwrite_frees_old_chain_no_leak() {
        fn allocated(fs: &ExFat<MemoryBlockDevice>) -> u32 {
            let mut n = 0;
            for cl in 2..2 + fs.boot.cluster_count {
                if fs.bitmap_get(cl).unwrap() {
                    n += 1;
                }
            }
            n
        }
        let mut fs = fixture();
        let payload = alloc::vec![0xABu8; 9000]; // multi-cluster
        fs.write_file("/ow.bin", &payload).unwrap();
        let after_first = allocated(&fs);
        // Overwrite several times with the same-size data: allocation must not grow.
        for _ in 0..5 {
            fs.write_file("/ow.bin", &payload).unwrap();
        }
        let after_many = allocated(&fs);
        assert_eq!(after_first, after_many, "overwrite leaked clusters (old chain not freed)");
        assert_eq!(fs.read_file("/ow.bin").unwrap(), payload);
    }

    #[test]
    fn reads_files_from_real_exfat_image() {
        let fs = fixture();
        assert_eq!(fs.read_file("/hello.txt").unwrap(), b"Hello from exFAT, read by the EuroOS driver.");
        // 4500 deterministic 'A' bytes in a subdirectory (multi-cluster: 2 clusters).
        let blob = fs.read_file("/sub/blob.bin").unwrap();
        assert_eq!(blob.len(), 4500);
        assert!(blob.iter().all(|&b| b == b'A'));
    }

    #[test]
    fn lists_directories_and_long_names() {
        let fs = fixture();
        let root = fs.list_dir("/").unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello.txt"));
        assert!(names.contains(&"sub"));
        // exFAT long name with spaces.
        assert!(names.iter().any(|n| n.contains("long exfat filename 2026")));
        let sub = root.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Directory);
        assert_eq!(fs.list_dir("/sub").unwrap()[0].name, "blob.bin");
    }

    #[test]
    fn metadata_and_exists() {
        let fs = fixture();
        assert!(fs.exists("/sub/blob.bin"));
        assert!(!fs.exists("/nope"));
        assert_eq!(fs.metadata("/sub/blob.bin").unwrap().size, 4500);
        assert_eq!(fs.metadata("/sub").unwrap().kind, EntryKind::Directory);
    }

    // ── Write path (IO-4 write support) ──────────────────────────────────────

    #[test]
    fn write_small_file_roundtrip() {
        let mut fs = fixture();
        let data = b"EuroOS exFAT write path: a brand-new small file.";
        fs.write_file("/newfile.txt", data).unwrap();
        assert_eq!(fs.read_file("/newfile.txt").unwrap(), data);
        // Shows up in the listing.
        let names: Vec<String> = fs.list_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "newfile.txt"));
        assert!(fs.exists("/newfile.txt"));
    }

    #[test]
    fn write_multicluster_file_roundtrip() {
        let mut fs = fixture();
        // 200 KiB of deterministic but non-trivial bytes → many clusters.
        let data: Vec<u8> = (0..200 * 1024).map(|i| (i * 31 + 7) as u8).collect();
        fs.write_file("/big.bin", &data).unwrap();
        let back = fs.read_file("/big.bin").unwrap();
        assert_eq!(back.len(), data.len());
        assert_eq!(back, data);
    }

    #[test]
    fn create_dir_then_write_inside_and_list() {
        let mut fs = fixture();
        fs.create_dir("/mydir").unwrap();
        let meta = fs.metadata("/mydir").unwrap();
        assert_eq!(meta.kind, EntryKind::Directory);

        fs.write_file("/mydir/inner.txt", b"inside a freshly created dir").unwrap();
        assert_eq!(fs.read_file("/mydir/inner.txt").unwrap(), b"inside a freshly created dir");

        // list_dir shows the dir at root and the file inside it.
        let root: Vec<String> = fs.list_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(root.iter().any(|n| n == "mydir"));
        let inner: Vec<String> = fs.list_dir("/mydir").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(inner, vec!["inner.txt".to_string()]);
    }

    #[test]
    fn remove_file_then_gone() {
        let mut fs = fixture();
        fs.write_file("/todelete.bin", b"ephemeral").unwrap();
        assert!(fs.exists("/todelete.bin"));
        fs.remove_file("/todelete.bin").unwrap();
        assert!(!fs.exists("/todelete.bin"));
        assert_eq!(fs.read_file("/todelete.bin"), Err(FsError::NotFound));
        let names: Vec<String> = fs.list_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(!names.iter().any(|n| n == "todelete.bin"));
    }

    #[test]
    fn existing_data_survives_writes() {
        let mut fs = fixture();
        // Perform several writes that allocate clusters & grow structures.
        fs.write_file("/a.txt", b"alpha").unwrap();
        let big: Vec<u8> = (0..120 * 1024).map(|i| (i % 251) as u8).collect();
        fs.write_file("/b.bin", &big).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/c.txt", b"charlie").unwrap();
        fs.remove_file("/a.txt").unwrap();

        // The originally-present files must be intact and byte-identical.
        assert_eq!(
            fs.read_file("/hello.txt").unwrap(),
            b"Hello from exFAT, read by the EuroOS driver."
        );
        let blob = fs.read_file("/sub/blob.bin").unwrap();
        assert_eq!(blob.len(), 4500);
        assert!(blob.iter().all(|&b| b == b'A'));
        // And the new survivors read back correctly too.
        assert_eq!(fs.read_file("/b.bin").unwrap(), big);
        assert_eq!(fs.read_file("/d/c.txt").unwrap(), b"charlie");
    }

    #[test]
    fn overwrite_existing_file() {
        let mut fs = fixture();
        fs.write_file("/ow.txt", b"first version, longer").unwrap();
        fs.write_file("/ow.txt", b"second").unwrap();
        assert_eq!(fs.read_file("/ow.txt").unwrap(), b"second");
        // Exactly one entry for it.
        let count = fs
            .list_dir("/")
            .unwrap()
            .into_iter()
            .filter(|e| e.name == "ow.txt")
            .count();
        assert_eq!(count, 1);
    }
}
