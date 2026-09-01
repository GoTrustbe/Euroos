//! **EuroFatFs** — a mountable FAT32 filesystem driver.
//!
//! Implements [`eurofs::FileSystem`] over a 512-byte [`eurofs::BlockDevice`], so a
//! FAT32 disk/partition (a USB stick, an SD card, an ESP) can be `mount()`-ed into the
//! EuroOS VFS and its files read like any other path. IO-1 ships the READ path
//! (BPB + FAT-chain traversal + subdirectories + LFN long names + reading a file across
//! clusters); the write path (IO-2) builds on the same structures.
//!
//! Pure `no_std`, no `unsafe`. Host-tested against images built by the `eurofat`
//! builder (valid, fsck-clean FAT32) — a round-trip proof independent of any host tool.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::{BlockDevice, DirEntry, EntryKind, FileSystem, FsError, FsResult};

const SECTOR: usize = 512;
const EOC: u32 = 0x0FFF_FFF8; // ≥ this = end-of-chain (FAT32)
const ATTR_DIR: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LFN: u8 = 0x0F;

/// Parsed BIOS Parameter Block (sector 0 of the volume).
#[derive(Clone, Copy)]
struct Bpb {
    reserved: u32,
    num_fats: u32,
    spf: u32,
    spc: u32,
    root_cluster: u32,
    total_sectors: u32,
}

impl Bpb {
    fn parse(s0: &[u8]) -> FsResult<Bpb> {
        if s0.len() < SECTOR || s0[510] != 0x55 || s0[511] != 0xAA {
            return Err(FsError::Corruption);
        }
        let bps = u16::from_le_bytes([s0[11], s0[12]]) as u32;
        if bps != SECTOR as u32 {
            return Err(FsError::Unsupported); // only 512-byte sectors
        }
        let spc = s0[13] as u32;
        let reserved = u16::from_le_bytes([s0[14], s0[15]]) as u32;
        let num_fats = s0[16] as u32;
        let spf = u32::from_le_bytes([s0[36], s0[37], s0[38], s0[39]]);
        let root_cluster = u32::from_le_bytes([s0[44], s0[45], s0[46], s0[47]]);
        let total16 = u16::from_le_bytes([s0[19], s0[20]]) as u32;
        let total32 = u32::from_le_bytes([s0[32], s0[33], s0[34], s0[35]]);
        let total_sectors = if total16 != 0 { total16 } else { total32 };
        if spc == 0 || spf == 0 || root_cluster < 2 {
            return Err(FsError::Corruption);
        }
        Ok(Bpb { reserved, num_fats, spf, spc, root_cluster, total_sectors })
    }
    fn data_start(&self) -> u32 {
        self.reserved + self.num_fats * self.spf
    }
    fn cluster_first_sector(&self, cl: u32) -> u32 {
        self.data_start() + (cl - 2) * self.spc
    }
    fn cluster_bytes(&self) -> usize {
        self.spc as usize * SECTOR
    }
    fn total_clusters(&self) -> u32 {
        (self.total_sectors.saturating_sub(self.data_start())) / self.spc
    }
}

/// A mounted FAT32 volume.
pub struct FatFs<D: BlockDevice> {
    dev: D,
    bpb: Bpb,
    /// Next-free-cluster hint: allocation scans forward from here so filling a volume is
    /// O(n) instead of O(n²) (a fresh format leaves free clusters contiguous from 3).
    next_free: u32,
}

impl<D: BlockDevice> FatFs<D> {
    /// Mount a FAT32 volume from a 512-byte block device. Fails if the BPB is not a
    /// valid 512-byte-sector FAT32.
    pub fn mount(dev: D) -> FsResult<Self> {
        if dev.block_size() != SECTOR as u32 {
            return Err(FsError::Unsupported);
        }
        let mut s0 = [0u8; SECTOR];
        dev.read_blocks(0, 1, &mut s0).map_err(|_| FsError::IoError)?;
        let bpb = Bpb::parse(&s0)?;
        // Seed the allocation cursor from the FSInfo "next free" hint when valid.
        let mut next_free = 3u32;
        let mut fsi = [0u8; SECTOR];
        if dev.read_blocks(1, 1, &mut fsi).is_ok()
            && u32::from_le_bytes([fsi[0], fsi[1], fsi[2], fsi[3]]) == 0x4161_5252
        {
            let nf = u32::from_le_bytes([fsi[492], fsi[493], fsi[494], fsi[495]]);
            if nf >= 2 && nf != 0xFFFF_FFFF {
                next_free = nf;
            }
        }
        Ok(FatFs { dev, bpb, next_free })
    }

    fn rsec(&self, lba: u32, buf: &mut [u8; SECTOR]) -> FsResult<()> {
        self.dev.read_blocks(lba as u64, 1, buf).map_err(|_| FsError::IoError)
    }

    /// Next cluster in the chain (or ≥ EOC at the end).
    fn fat_next(&self, cl: u32) -> u32 {
        let byte = self.bpb.reserved as u64 * SECTOR as u64 + cl as u64 * 4;
        let sec = (byte / SECTOR as u64) as u32;
        let off = (byte % SECTOR as u64) as usize;
        let mut buf = [0u8; SECTOR];
        if self.rsec(sec, &mut buf).is_err() {
            return EOC;
        }
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) & 0x0FFF_FFFF
    }

    /// Read the full byte content of a cluster chain, truncated to `size`.
    fn read_chain(&self, start: u32, size: usize) -> FsResult<Vec<u8>> {
        let mut out = Vec::with_capacity(size);
        let mut cl = start;
        let mut guard = 0u32;
        let cb = self.bpb.cluster_bytes();
        while (2..EOC).contains(&cl) && out.len() < size && guard < 1 << 24 {
            guard += 1;
            let base = self.bpb.cluster_first_sector(cl);
            for s in 0..self.bpb.spc {
                if out.len() >= size {
                    break;
                }
                let mut buf = [0u8; SECTOR];
                self.rsec(base + s, &mut buf)?;
                let take = (size - out.len()).min(SECTOR);
                out.extend_from_slice(&buf[..take]);
            }
            let _ = cb;
            cl = self.fat_next(cl);
        }
        out.truncate(size);
        Ok(out)
    }

    /// Collect all raw 32-byte directory entries of a directory cluster chain.
    fn read_dir_raw(&self, start: u32) -> FsResult<Vec<[u8; 32]>> {
        let mut out = Vec::new();
        let mut cl = start;
        let mut guard = 0u32;
        'outer: while (2..EOC).contains(&cl) && guard < 1 << 20 {
            guard += 1;
            let base = self.bpb.cluster_first_sector(cl);
            for s in 0..self.bpb.spc {
                let mut buf = [0u8; SECTOR];
                self.rsec(base + s, &mut buf)?;
                let mut e = 0;
                while e + 32 <= SECTOR {
                    if buf[e] == 0x00 {
                        break 'outer; // end of directory
                    }
                    let mut ent = [0u8; 32];
                    ent.copy_from_slice(&buf[e..e + 32]);
                    out.push(ent);
                    e += 32;
                }
            }
            cl = self.fat_next(cl);
        }
        Ok(out)
    }

    /// Parse a directory's entries → (name, first_cluster, size, is_dir), reconstructing
    /// LFN long names and skipping `.`/`..`, the volume label and deleted entries.
    fn parse_dir(&self, dir_cluster: u32) -> FsResult<Vec<(String, u32, u32, bool)>> {
        let raw = self.read_dir_raw(dir_cluster)?;
        let mut out = Vec::new();
        let mut lfn = String::new();
        for ent in &raw {
            if ent[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if ent[11] == ATTR_LFN {
                let mut s = lfn_chars(ent);
                s.push_str(&lfn);
                lfn = s;
                continue;
            }
            if ent[11] & ATTR_VOLUME_ID != 0 {
                lfn.clear();
                continue; // volume-label entry
            }
            let long = lfn.trim_end_matches('\u{0}');
            let name = if !long.is_empty() { String::from(long) } else { sfn_name(ent) };
            lfn.clear();
            if name == "." || name == ".." {
                continue;
            }
            let first = ((u16::from_le_bytes([ent[20], ent[21]]) as u32) << 16)
                | u16::from_le_bytes([ent[26], ent[27]]) as u32;
            let size = u32::from_le_bytes([ent[28], ent[29], ent[30], ent[31]]);
            let is_dir = ent[11] & ATTR_DIR != 0;
            out.push((name, first, size, is_dir));
        }
        Ok(out)
    }

    fn find_in_dir(&self, dir_cluster: u32, name: &str) -> FsResult<(u32, u32, bool)> {
        for (n, cl, size, is_dir) in self.parse_dir(dir_cluster)? {
            if n.eq_ignore_ascii_case(name) {
                return Ok((cl, size, is_dir));
            }
        }
        Err(FsError::NotFound)
    }

    /// Resolve a path → (first_cluster, size, is_dir). The root is the BPB root cluster.
    fn resolve(&self, path: &str) -> FsResult<(u32, u32, bool)> {
        let mut cluster = self.bpb.root_cluster;
        let mut size = 0u32;
        let mut is_dir = true;
        for (i, part) in path.split('/').filter(|p| !p.is_empty()).enumerate() {
            if !is_dir {
                return Err(FsError::NotADirectory);
            }
            let (cl, sz, dir) = self.find_in_dir(cluster, part)?;
            cluster = if cl >= 2 { cl } else { cluster };
            size = sz;
            is_dir = dir;
            let _ = i;
        }
        Ok((cluster, size, is_dir))
    }

    // ── Write path (IO-2) ───────────────────────────────────────────────────

    fn wsec(&mut self, lba: u32, buf: &[u8; SECTOR]) -> FsResult<()> {
        self.dev.write_blocks(lba as u64, 1, buf).map_err(|_| FsError::IoError)
    }

    /// Set the FAT entry for `cl` to `val` in every FAT copy (preserving the top nibble).
    fn set_fat(&mut self, cl: u32, val: u32) -> FsResult<()> {
        for f in 0..self.bpb.num_fats {
            let byte = (self.bpb.reserved + f * self.bpb.spf) as u64 * SECTOR as u64 + cl as u64 * 4;
            let sec = (byte / SECTOR as u64) as u32;
            let off = (byte % SECTOR as u64) as usize;
            let mut buf = [0u8; SECTOR];
            self.rsec(sec, &mut buf)?;
            let existing = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
            let merged = (existing & 0xF000_0000) | (val & 0x0FFF_FFFF);
            buf[off..off + 4].copy_from_slice(&merged.to_le_bytes());
            self.wsec(sec, &buf)?;
        }
        Ok(())
    }

    /// Find a free cluster (FAT entry == 0), mark it end-of-chain, return it. Scans
    /// forward from the `next_free` cursor (then wraps), so sequential allocation is
    /// O(1) amortised — filling a volume is O(n), not O(n²).
    fn alloc_cluster(&mut self) -> FsResult<u32> {
        let max = self.bpb.total_clusters() + 2;
        let start = self.next_free.clamp(2, max.saturating_sub(1));
        // Two passes: [start..max) then [2..start) so we never miss a freed cluster.
        for cl in (start..max).chain(2..start) {
            if self.fat_next(cl) == 0 {
                self.set_fat(cl, 0x0FFF_FFFF)?;
                self.next_free = cl + 1;
                return Ok(cl);
            }
        }
        Err(FsError::NoSpace)
    }

    fn zero_cluster(&mut self, cl: u32) -> FsResult<()> {
        let base = self.bpb.cluster_first_sector(cl);
        let zero = [0u8; SECTOR];
        for s in 0..self.bpb.spc {
            self.wsec(base + s, &zero)?;
        }
        Ok(())
    }

    /// Free an entire cluster chain (set each entry to 0).
    fn free_chain(&mut self, start: u32) -> FsResult<()> {
        let mut cl = start;
        let mut guard = 0u32;
        while (2..EOC).contains(&cl) && guard < 1 << 24 {
            guard += 1;
            let next = self.fat_next(cl);
            self.set_fat(cl, 0)?;
            if cl < self.next_free {
                self.next_free = cl; // reuse freed space first
            }
            cl = next;
        }
        Ok(())
    }

    /// Allocate a chain and write `data` into it. Returns the first cluster (0 if empty).
    fn write_new_chain(&mut self, data: &[u8]) -> FsResult<u32> {
        if data.is_empty() {
            return Ok(0);
        }
        let cb = self.bpb.cluster_bytes();
        let nclusters = data.len().div_ceil(cb);
        let mut clusters = Vec::with_capacity(nclusters);
        for _ in 0..nclusters {
            clusters.push(self.alloc_cluster()?);
        }
        // Link the chain.
        for i in 0..nclusters {
            let val = if i + 1 == nclusters { 0x0FFF_FFFF } else { clusters[i + 1] };
            self.set_fat(clusters[i], val)?;
        }
        // Write the data sector by sector.
        for (i, &cl) in clusters.iter().enumerate() {
            let base = self.bpb.cluster_first_sector(cl);
            for s in 0..self.bpb.spc {
                let off = i * cb + s as usize * SECTOR;
                if off >= data.len() {
                    break;
                }
                let mut buf = [0u8; SECTOR];
                let end = (off + SECTOR).min(data.len());
                buf[..end - off].copy_from_slice(&data[off..end]);
                self.wsec(base + s, &buf)?;
            }
        }
        Ok(clusters[0])
    }

    /// All directory-entry slots of a directory: (lba, offset, first_byte).
    fn dir_slots(&self, dir_cluster: u32) -> FsResult<Vec<(u32, usize, u8)>> {
        let mut out = Vec::new();
        let mut cl = dir_cluster;
        let mut guard = 0u32;
        while (2..EOC).contains(&cl) && guard < 1 << 16 {
            guard += 1;
            let base = self.bpb.cluster_first_sector(cl);
            for s in 0..self.bpb.spc {
                let mut buf = [0u8; SECTOR];
                self.rsec(base + s, &mut buf)?;
                let mut e = 0;
                while e + 32 <= SECTOR {
                    out.push((base + s, e, buf[e]));
                    e += 32;
                }
            }
            cl = self.fat_next(cl);
        }
        Ok(out)
    }

    /// Reserve `need` CONSECUTIVE free directory slots (a run may span the cluster
    /// boundary — directory entries are a contiguous stream over the chain). Extends the
    /// chain by zeroed clusters until such a run exists. Returns the (lba, offset) slots.
    ///
    /// Crucial: we never leave a `0x00` slot *before* written entries (that would
    /// terminate a directory scan early), because the run is taken from the contiguous
    /// free tail of the stream, extended as one continuous region.
    fn reserve_dir_slots(&mut self, dir_cluster: u32, need: usize) -> FsResult<Vec<(u32, usize)>> {
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 1024 {
                return Err(FsError::NoSpace);
            }
            let slots = self.dir_slots(dir_cluster)?;
            let free = |b: u8| b == 0x00 || b == 0xE5;
            let mut run: Vec<(u32, usize)> = Vec::new();
            for &(lba, off, first) in &slots {
                if free(first) {
                    run.push((lba, off));
                    if run.len() == need {
                        return Ok(run);
                    }
                } else {
                    run.clear();
                }
            }
            // No contiguous run of `need` free slots yet (the free tail is too short or
            // split by the cluster boundary) → append one zeroed cluster and retry. The
            // new cluster's slots extend the contiguous free tail, so the next pass finds
            // a run that may straddle the boundary — no premature 0x00 is introduced.
            let mut last = dir_cluster;
            while self.fat_next(last) < EOC && self.fat_next(last) >= 2 {
                last = self.fat_next(last);
            }
            let newcl = self.alloc_cluster()?;
            self.zero_cluster(newcl)?;
            self.set_fat(last, newcl)?;
        }
    }

    fn write_entry(&mut self, lba: u32, off: usize, ent: &[u8; 32]) -> FsResult<()> {
        let mut buf = [0u8; SECTOR];
        self.rsec(lba, &mut buf)?;
        buf[off..off + 32].copy_from_slice(ent);
        self.wsec(lba, &buf)
    }

    /// Existing short (8.3) names in a directory (to avoid collisions).
    fn used_shorts(&self, dir_cluster: u32) -> FsResult<Vec<[u8; 11]>> {
        let mut out = Vec::new();
        for ent in self.read_dir_raw(dir_cluster)? {
            if ent[0] != 0xE5 && ent[11] != ATTR_LFN && ent[0] != 0x00 {
                let mut s = [0u8; 11];
                s.copy_from_slice(&ent[0..11]);
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Add a directory entry (LFN + SFN) for `name` in `dir_cluster`.
    fn add_dir_entry(&mut self, dir_cluster: u32, name: &str, attr: u8, first: u32, size: u32) -> FsResult<()> {
        let used = self.used_shorts(dir_cluster)?;
        let short = unique_short(name, &used);
        let need_lfn = short83(name).map(|s| s != short).unwrap_or(true);
        let entries = make_entries(name, short, attr, first, size, need_lfn);
        let slots = self.reserve_dir_slots(dir_cluster, entries.len())?;
        for (ent, (lba, off)) in entries.iter().zip(slots.iter()) {
            self.write_entry(*lba, *off, ent)?;
        }
        Ok(())
    }

    /// Find the SFN entry of `name` in `dir_cluster` and the LFN slots before it:
    /// returns (sfn_lba, sfn_off, Vec<(lfn_lba, lfn_off)>).
    fn find_entry_slots(&self, dir_cluster: u32, name: &str) -> FsResult<(u32, usize, Vec<(u32, usize)>)> {
        let slots = self.dir_slots(dir_cluster)?;
        let mut lfn = String::new();
        let mut lfn_pos: Vec<(u32, usize)> = Vec::new();
        for &(lba, off, first) in &slots {
            if first == 0x00 {
                break;
            }
            let mut buf = [0u8; SECTOR];
            self.rsec(lba, &mut buf)?;
            let ent = &buf[off..off + 32];
            if ent[0] == 0xE5 {
                lfn.clear();
                lfn_pos.clear();
                continue;
            }
            if ent[11] == ATTR_LFN {
                let mut e = [0u8; 32];
                e.copy_from_slice(ent);
                let mut s = lfn_chars(&e);
                s.push_str(&lfn);
                lfn = s;
                lfn_pos.push((lba, off));
                continue;
            }
            if ent[11] & ATTR_VOLUME_ID != 0 {
                lfn.clear();
                lfn_pos.clear();
                continue;
            }
            let long = lfn.trim_end_matches('\u{0}');
            let ename = if !long.is_empty() { String::from(long) } else { sfn_name(ent) };
            if ename.eq_ignore_ascii_case(name) {
                return Ok((lba, off, lfn_pos));
            }
            lfn.clear();
            lfn_pos.clear();
        }
        Err(FsError::NotFound)
    }

    /// Update the first-cluster + size fields of `name`'s SFN entry.
    fn update_entry(&mut self, dir_cluster: u32, name: &str, first: u32, size: u32) -> FsResult<()> {
        let (lba, off, _lfn) = self.find_entry_slots(dir_cluster, name)?;
        let mut buf = [0u8; SECTOR];
        self.rsec(lba, &mut buf)?;
        buf[off + 20..off + 22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        buf[off + 26..off + 28].copy_from_slice(&(first as u16).to_le_bytes());
        buf[off + 28..off + 32].copy_from_slice(&size.to_le_bytes());
        self.wsec(lba, &buf)
    }

    /// Mark `name`'s SFN + LFN slots deleted (0xE5).
    fn delete_entry(&mut self, dir_cluster: u32, name: &str) -> FsResult<()> {
        let (lba, off, lfn) = self.find_entry_slots(dir_cluster, name)?;
        for (l, o) in lfn.into_iter().chain(core::iter::once((lba, off))) {
            let mut buf = [0u8; SECTOR];
            self.rsec(l, &mut buf)?;
            buf[o] = 0xE5;
            self.wsec(l, &buf)?;
        }
        Ok(())
    }

    fn split_parent<'a>(&self, path: &'a str) -> (String, &'a str) {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(i) => (String::from(&trimmed[..i + 1]), &trimmed[i + 1..]),
            None => (String::from("/"), trimmed),
        }
    }

    fn flush_dev(&mut self) {
        let _ = self.dev.flush();
    }
}

impl<D: BlockDevice> FileSystem for FatFs<D> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let (cluster, size, is_dir) = self.resolve(path)?;
        if is_dir {
            return Err(FsError::NotAFile);
        }
        if size == 0 || cluster < 2 {
            return Ok(Vec::new());
        }
        self.read_chain(cluster, size as usize)
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let (cluster, _size, is_dir) = self.resolve(path)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        let mut out = Vec::new();
        for (name, _cl, size, dir) in self.parse_dir(cluster)? {
            out.push(DirEntry {
                name,
                kind: if dir { EntryKind::Directory } else { EntryKind::File },
                size: size as u64,
                mode: if dir { 0o755 } else { 0o644 },
                mtime: 0,
            });
        }
        Ok(out)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let (_cl, size, is_dir) = self.resolve(path)?;
        let name = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/").into();
        Ok(DirEntry {
            name,
            kind: if is_dir { EntryKind::Directory } else { EntryKind::File },
            size: size as u64,
            mode: if is_dir { 0o755 } else { 0o644 },
            mtime: 0,
        })
    }

    fn space_info(&self) -> (u64, u64) {
        // Total from geometry; free from the FSInfo sector if it holds a real count.
        let total = self.bpb.total_clusters() as u64 * self.bpb.cluster_bytes() as u64;
        let mut fsinfo = [0u8; SECTOR];
        let free = if self.rsec(1, &mut fsinfo).is_ok()
            && u32::from_le_bytes([fsinfo[0], fsinfo[1], fsinfo[2], fsinfo[3]]) == 0x4161_5252
        {
            let fc = u32::from_le_bytes([fsinfo[488], fsinfo[489], fsinfo[490], fsinfo[491]]);
            if fc == 0xFFFF_FFFF {
                0
            } else {
                fc as u64 * self.bpb.cluster_bytes() as u64
            }
        } else {
            0
        };
        (total, free.min(total))
    }

    // ── Write path (IO-2) ──
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let (parent, name) = self.split_parent(path);
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let (dir_cluster, _sz, is_dir) = self.resolve(&parent)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        // Existing file? free its old chain first.
        let existed = match self.find_in_dir(dir_cluster, name) {
            Ok((cl, _s, d)) => {
                if d {
                    return Err(FsError::NotAFile);
                }
                if cl >= 2 {
                    self.free_chain(cl)?;
                }
                true
            }
            Err(FsError::NotFound) => false,
            Err(e) => return Err(e),
        };
        let first = self.write_new_chain(data)?;
        if existed {
            self.update_entry(dir_cluster, name, first, data.len() as u32)?;
        } else {
            self.add_dir_entry(dir_cluster, name, 0, first, data.len() as u32)?;
        }
        self.flush_dev();
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        let (parent, name) = self.split_parent(path);
        let (dir_cluster, _s, is_dir) = self.resolve(&parent)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        let (cl, _size, d) = self.find_in_dir(dir_cluster, name)?;
        if d {
            return Err(FsError::NotAFile);
        }
        if cl >= 2 {
            self.free_chain(cl)?;
        }
        self.delete_entry(dir_cluster, name)?;
        self.flush_dev();
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        let (parent, name) = self.split_parent(path);
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let (dir_cluster, _s, is_dir) = self.resolve(&parent)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        if self.find_in_dir(dir_cluster, name).is_ok() {
            return Err(FsError::AlreadyExists);
        }
        // Allocate + initialize the new directory cluster with "." and ".." entries.
        let cl = self.alloc_cluster()?;
        self.zero_cluster(cl)?;
        let parent_cl = if dir_cluster == self.bpb.root_cluster { 0 } else { dir_cluster };
        let dot = sfn_entry(b".          ", ATTR_DIR, cl, 0);
        let dotdot = sfn_entry(b"..         ", ATTR_DIR, parent_cl, 0);
        let base = self.bpb.cluster_first_sector(cl);
        let mut buf = [0u8; SECTOR];
        buf[0..32].copy_from_slice(&dot);
        buf[32..64].copy_from_slice(&dotdot);
        self.wsec(base, &buf)?;
        self.add_dir_entry(dir_cluster, name, ATTR_DIR, cl, 0)?;
        self.flush_dev();
        Ok(())
    }

    /// Rename/move a FILE (copy its data to the new path, then remove the old entry).
    /// Works across directories on the same volume; directory rename is not supported.
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let data = self.read_file(old)?; // errors NotAFile for a directory
        self.write_file(new, &data)?;
        self.remove_file(old)
    }

    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let (cl, _s, is_dir) = self.resolve(path)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        if !self.parse_dir(cl)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        let (parent, name) = self.split_parent(path);
        let (dir_cluster, _s2, _d) = self.resolve(&parent)?;
        self.free_chain(cl)?;
        self.delete_entry(dir_cluster, name)?;
        self.flush_dev();
        Ok(())
    }
}

// ── 8.3 / LFN directory-entry generation (write path) ───────────────────────

fn short83(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let valid = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || "$%'-_@~`!(){}^#&".contains(c);
    if !base.chars().all(valid) || !ext.chars().all(valid) {
        return None;
    }
    let mut out = [b' '; 11];
    for (i, c) in base.bytes().enumerate() {
        out[i] = c;
    }
    for (i, c) in ext.bytes().enumerate() {
        out[8 + i] = c;
    }
    Some(out)
}

fn mangle83(name: &str, n: u32) -> [u8; 11] {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let clean = |s: &str| -> Vec<u8> {
        s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase() as u8).collect()
    };
    let cb = clean(base);
    let ce = clean(ext);
    let suffix = alloc::format!("~{n}");
    let keep = 8usize.saturating_sub(suffix.len());
    let mut out = [b' '; 11];
    let take = cb.len().min(keep);
    out[..take].copy_from_slice(&cb[..take]);
    for (i, c) in suffix.bytes().enumerate() {
        out[take + i] = c;
    }
    for (i, c) in ce.iter().take(3).enumerate() {
        out[8 + i] = *c;
    }
    out
}

fn unique_short(name: &str, used: &[[u8; 11]]) -> [u8; 11] {
    if let Some(s) = short83(name) {
        if !used.contains(&s) {
            return s;
        }
    }
    let mut n = 1u32;
    loop {
        let s = mangle83(name, n);
        if !used.contains(&s) {
            return s;
        }
        n += 1;
    }
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &c in short.iter() {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c);
    }
    sum
}

fn sfn_entry(short: &[u8; 11], attr: u8, first_cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(short);
    e[11] = attr;
    const DATE_1980: u16 = (1 << 5) | 1;
    e[16..18].copy_from_slice(&DATE_1980.to_le_bytes());
    e[18..20].copy_from_slice(&DATE_1980.to_le_bytes());
    e[24..26].copy_from_slice(&DATE_1980.to_le_bytes());
    e[20..22].copy_from_slice(&((first_cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(first_cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

fn lfn_entry(order: u8, chksum: u8, utf16: &[u16], start: usize) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = order;
    e[11] = ATTR_LFN;
    e[13] = chksum;
    let put = |e: &mut [u8; 32], slot: usize, off: usize| {
        let ch = if start + slot < utf16.len() {
            utf16[start + slot]
        } else if start + slot == utf16.len() {
            0x0000
        } else {
            0xFFFF
        };
        e[off..off + 2].copy_from_slice(&ch.to_le_bytes());
    };
    for i in 0..5 {
        put(&mut e, i, 1 + i * 2);
    }
    for i in 0..6 {
        put(&mut e, 5 + i, 14 + i * 2);
    }
    for i in 0..2 {
        put(&mut e, 11 + i, 28 + i * 2);
    }
    e
}

/// Build the directory entries (LFN entries reversed, then the SFN) for one child.
fn make_entries(name: &str, short: [u8; 11], attr: u8, first: u32, size: u32, need_lfn: bool) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    if need_lfn {
        let chksum = lfn_checksum(&short);
        let utf16: Vec<u16> = name.encode_utf16().collect();
        let groups = utf16.len().div_ceil(13);
        for g in (0..groups).rev() {
            let order = (g as u8 + 1) | if g + 1 == groups { 0x40 } else { 0 };
            out.push(lfn_entry(order, chksum, &utf16, g * 13));
        }
    }
    out.push(sfn_entry(&short, attr, first, size));
    out
}

// ── 8.3 + LFN helpers ───────────────────────────────────────────────────────

fn lfn_chars(ent: &[u8; 32]) -> String {
    let mut u: Vec<u16> = Vec::new();
    let mut push = |off: usize| {
        let c = u16::from_le_bytes([ent[off], ent[off + 1]]);
        if c != 0x0000 && c != 0xFFFF {
            u.push(c);
        }
    };
    for i in 0..5 {
        push(1 + i * 2);
    }
    for i in 0..6 {
        push(14 + i * 2);
    }
    for i in 0..2 {
        push(28 + i * 2);
    }
    String::from_utf16_lossy(&u)
}

fn sfn_name(ent: &[u8]) -> String {
    let base = core::str::from_utf8(&ent[0..8]).unwrap_or("").trim_end();
    let ext = core::str::from_utf8(&ent[8..11]).unwrap_or("").trim_end();
    if ext.is_empty() {
        String::from(base)
    } else {
        alloc::format!("{base}.{ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eurofs::MemoryBlockDevice;

    /// Build a valid FAT32 image with the eurofat builder, load it into a 512-byte
    /// MemoryBlockDevice, and mount it read-only with this driver.
    fn mounted(files: &[(&str, &[u8])]) -> FatFs<MemoryBlockDevice> {
        let sectors = 48 * 1024 * 1024 / SECTOR as u32; // ≥ 65525 clusters → valid FAT32
        let mut fb = eurofat::FatFs::new(sectors, 0xF00D_CAFE, "EUROTEST");
        for (p, d) in files {
            fb.add_file(p, d);
        }
        let img = fb.build();
        let mut dev = MemoryBlockDevice::new(sectors as u64, SECTOR as u32);
        dev.write_blocks(0, sectors, &img).unwrap();
        FatFs::mount(dev).expect("mount FAT32")
    }

    #[test]
    fn reads_root_and_nested_files() {
        let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let fs = mounted(&[
            ("/hello.txt", b"Hello EuroOS"),
            ("/dir/sub.txt", b"nested file"),
            ("/big.bin", &big),
        ]);
        assert_eq!(fs.read_file("/hello.txt").unwrap(), b"Hello EuroOS");
        assert_eq!(fs.read_file("/dir/sub.txt").unwrap(), b"nested file");
        assert_eq!(fs.read_file("/big.bin").unwrap(), big); // multi-cluster read
        assert!(fs.exists("/dir"));
        assert!(!fs.exists("/nope.txt"));
        assert_eq!(fs.read_file("/nope.txt"), Err(FsError::NotFound));
    }

    #[test]
    fn lists_directories_with_kinds() {
        let fs = mounted(&[("/a.txt", b"a"), ("/b.txt", b"bb"), ("/sub/c.txt", b"ccc")]);
        let root = fs.list_dir("/").unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt") && names.contains(&"b.txt") && names.contains(&"sub"));
        let sub = root.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Directory);
        let afile = root.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(afile.kind, EntryKind::File);
        assert_eq!(afile.size, 1);
        assert_eq!(fs.list_dir("/sub").unwrap()[0].name, "c.txt");
    }

    #[test]
    fn long_file_names_round_trip() {
        let fs = mounted(&[("/a-very-long-euroos-filename.config", b"x")]);
        assert!(fs.exists("/a-very-long-euroos-filename.config"));
        assert_eq!(fs.read_file("/a-very-long-euroos-filename.config").unwrap(), b"x");
    }

    /// Dump the whole device image (to cross-validate with the independent eurofat reader).
    fn dump(fs: &FatFs<MemoryBlockDevice>) -> Vec<u8> {
        let n = fs.dev.block_count();
        let mut img = alloc::vec![0u8; n as usize * SECTOR];
        for lba in 0..n {
            let mut b = [0u8; SECTOR];
            fs.dev.read_blocks(lba, 1, &mut b).unwrap();
            img[lba as usize * SECTOR..][..SECTOR].copy_from_slice(&b);
        }
        img
    }

    #[test]
    fn write_create_and_read_back_cross_validated() {
        let mut fs = mounted(&[]);
        fs.write_file("/new.txt", b"created by eurofatfs").unwrap();
        // Read back via our own driver...
        assert_eq!(fs.read_file("/new.txt").unwrap(), b"created by eurofatfs");
        // ...and via the INDEPENDENT eurofat reader → proves the on-disk format is valid.
        assert_eq!(eurofat::read_file(&dump(&fs), "/new.txt"), Some(b"created by eurofatfs".to_vec()));
        assert!(fs.list_dir("/").unwrap().iter().any(|e| e.name == "new.txt"));
    }

    #[test]
    fn overwrite_grow_then_shrink() {
        let mut fs = mounted(&[("/f.txt", b"small")]);
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 253) as u8).collect();
        fs.write_file("/f.txt", &big).unwrap(); // grow into many clusters
        assert_eq!(fs.read_file("/f.txt").unwrap(), big);
        assert_eq!(eurofat::read_file(&dump(&fs), "/f.txt"), Some(big.clone()));
        fs.write_file("/f.txt", b"tiny").unwrap(); // shrink (old clusters freed)
        assert_eq!(fs.read_file("/f.txt").unwrap(), b"tiny");
        assert_eq!(eurofat::read_file(&dump(&fs), "/f.txt"), Some(b"tiny".to_vec()));
    }

    #[test]
    fn create_dir_and_nested_write() {
        let mut fs = mounted(&[]);
        fs.create_dir("/docs").unwrap();
        assert_eq!(fs.create_dir("/docs"), Err(FsError::AlreadyExists));
        fs.write_file("/docs/note.txt", b"hi").unwrap();
        assert_eq!(fs.read_file("/docs/note.txt").unwrap(), b"hi");
        assert_eq!(eurofat::read_file(&dump(&fs), "/docs/note.txt"), Some(b"hi".to_vec()));
        let docs = fs.list_dir("/").unwrap();
        assert_eq!(docs.iter().find(|e| e.name == "docs").unwrap().kind, EntryKind::Directory);
    }

    #[test]
    fn remove_file_and_dir() {
        let mut fs = mounted(&[("/a.txt", b"a")]);
        fs.write_file("/b.txt", b"b").unwrap();
        fs.remove_file("/a.txt").unwrap();
        assert!(!fs.exists("/a.txt"));
        assert!(fs.exists("/b.txt"));
        fs.create_dir("/empty").unwrap();
        fs.remove_dir("/empty").unwrap();
        assert!(!fs.exists("/empty"));
        // non-empty dir refuses removal
        fs.create_dir("/full").unwrap();
        fs.write_file("/full/x", b"x").unwrap();
        assert_eq!(fs.remove_dir("/full"), Err(FsError::NotEmpty));
    }

    #[test]
    fn write_long_name_round_trip() {
        let mut fs = mounted(&[]);
        let name = "/a-rather-long-sovereign-filename-2026.config";
        fs.write_file(name, b"euro").unwrap();
        assert_eq!(fs.read_file(name).unwrap(), b"euro");
        assert_eq!(eurofat::read_file(&dump(&fs), name), Some(b"euro".to_vec()));
    }

    #[test]
    fn format_fat32_then_mount_and_use() {
        // IO-3: format a blank 64 MiB volume with the streaming formatter, then mount it
        // with this driver and round-trip files (cross-validated by the eurofat reader).
        let sectors = 64 * 1024 * 1024 / SECTOR as u32; // 131072 sectors → ≥65525 clusters
        let mut dev = MemoryBlockDevice::new(sectors as u64, SECTOR as u32);
        eurofat::format_fat32(sectors, 0xDEAD_BEEF, "DATA", |lba, bytes| {
            let n = (bytes.len() / SECTOR) as u32;
            dev.write_blocks(lba, n.max(1), bytes).unwrap();
        });
        let mut fs = FatFs::mount(dev).expect("mount freshly-formatted FAT32");
        assert!(fs.list_dir("/").unwrap().is_empty()); // empty after format
        fs.write_file("/first.txt", b"on a formatted volume").unwrap();
        fs.create_dir("/sub").unwrap();
        fs.write_file("/sub/x.bin", &[7u8; 5000]).unwrap();
        assert_eq!(fs.read_file("/first.txt").unwrap(), b"on a formatted volume");
        assert_eq!(fs.read_file("/sub/x.bin").unwrap(), [7u8; 5000]);
        assert_eq!(eurofat::read_file(&dump(&fs), "/sub/x.bin"), Some(alloc::vec![7u8; 5000]));
    }

    #[test]
    fn rejects_non_fat_device() {
        let dev = MemoryBlockDevice::new(64, SECTOR as u32); // all zeros, no BPB signature
        assert!(FatFs::mount(dev).is_err());
    }
}

#[cfg(test)]
mod loadtests {
    use super::*;
    use eurofs::MemoryBlockDevice;

    #[test]
    fn many_files_spanning_root_dir_clusters() {
        // Reproduce the [mdisk] finding: write 24 files (root dir spans >1 cluster) and
        // read every one back. 8 MiB volume, 256 KiB files.
        let sectors = 8 * 1024 * 1024 / SECTOR as u32;
        let mut dev = MemoryBlockDevice::new(sectors as u64, SECTOR as u32);
        eurofat::format_fat32(sectors, 1, "LOAD", |lba, b| {
            dev.write_blocks(lba, (b.len() / SECTOR).max(1) as u32, b).unwrap();
        });
        let mut fs = FatFs::mount(dev).unwrap();
        let buf = alloc::vec![0xA5u8; 256 * 1024];
        for i in 0..24 {
            fs.write_file(&alloc::format!("/f{i}.dat"), &buf).unwrap();
        }
        for i in 0..24 {
            let d = fs.read_file(&alloc::format!("/f{i}.dat")).unwrap();
            assert_eq!(d.len(), 256 * 1024, "file f{i}.dat wrong length");
            assert_eq!(d[0], 0xA5, "file f{i}.dat wrong content");
        }
        assert_eq!(fs.list_dir("/").unwrap().len(), 24);
    }

    /// Reproduce the [stress] `churn_disk` workload on a small (16 MiB) volume:
    /// write/rename/delete/rewrite over several rounds, then integrity-check.
    /// Guards against a hang/degenerate-geometry on sub-32-MiB FAT32 volumes.
    #[test]
    fn churn_small_volume_roundtrip() {
        let sectors = 16 * 1024 * 1024 / SECTOR as u32;
        let mut dev = MemoryBlockDevice::new(sectors as u64, SECTOR as u32);
        eurofat::format_fat32(sectors, 1, "STRESS", |lba, b| {
            dev.write_blocks(lba, (b.len() / SECTOR).max(1) as u32, b).unwrap();
        });
        let mut fs = FatFs::mount(dev).unwrap();
        let fsz = 96 * 1024usize;
        let buf = alloc::vec![0xC3u8; fsz];
        for r in 0..4 {
            for i in 0..12 {
                let _ = fs.write_file(&alloc::format!("/r{r}f{i}.dat"), &buf);
            }
            for i in 0..6 {
                let _ = fs.rename(&alloc::format!("/r{r}f{i}.dat"), &alloc::format!("/r{r}m{i}.dat"));
            }
            for i in 6..12 {
                let _ = fs.remove_file(&alloc::format!("/r{r}f{i}.dat"));
            }
            for i in 0..6 {
                let _ = fs.write_file(&alloc::format!("/r{r}m{i}.dat"), &buf);
            }
        }
        for r in 0..4 {
            for i in 0..6 {
                let d = fs.read_file(&alloc::format!("/r{r}m{i}.dat")).unwrap();
                assert_eq!(d.len(), fsz);
                assert_eq!(d[0], 0xC3);
            }
        }
    }
}
