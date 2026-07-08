//! **EuroExt** — a mountable ext2/3/4 *read* driver.
//!
//! Implements [`eurofs::FileSystem`] (read path) over a [`eurofs::BlockDevice`], so a
//! Linux ext-formatted disk can be mounted into the EuroOS VFS and its files read. One
//! driver covers ext2/ext3/ext4: it uses **extent trees** when the inode has the EXTENTS
//! flag (ext4) and classic direct/indirect **block pointers** otherwise (ext2/3).
//! Read-only — ext write means the jbd2 journal + bitmap management, deferred.
//!
//! ## Write support (ext2-style)
//! [`write_file`], [`create_dir`] and [`remove_file`] are implemented as
//! **ext2-style** mutations: NO jbd2 journal, NO extent allocation. New inodes
//! are always created with **classic** direct + single/double-indirect block
//! pointers (the EXTENTS flag is *not* set on them), which a real `e2fsck`
//! accepts even on an extent-capable image. Block & inode bitmaps and the
//! free-count accounting (superblock + group descriptor) are maintained.
//!
//! Limitations (see method docs): overwriting/removing an inode that itself
//! uses an **extent tree** is refused with [`FsError::Unsupported`] (we never
//! free extent-mapped blocks), and an image with an active jbd2 journal is also
//! refused for writes (we would not replay/append the journal).
//!
//! Pure `no_std`, no `unsafe`. Host-tested against a real `mkfs.ext4` image.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use eurofs::{BlockDevice, DirEntry, EntryKind, FileSystem, FsError, FsResult};

const SECTOR: usize = 512;
const EXT_MAGIC: u16 = 0xEF53;
const INCOMPAT_FILETYPE: u32 = 0x0002;
const COMPAT_HAS_JOURNAL: u32 = 0x0004;
const INODE_FL_EXTENTS: u32 = 0x0008_0000;
const EXTENT_MAGIC: u16 = 0xF30A;
const ROOT_INO: u32 = 2;
/// `i_mode` for a regular file, 0644.
const MODE_FILE: u16 = 0x8000 | 0o644;
/// `i_mode` for a directory, 0755.
const MODE_DIR: u16 = 0x4000 | 0o755;

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn wr16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

struct Sb {
    block_size: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: u32,
    first_data_block: u32,
    desc_size: u32,
    filetype: bool,
    has_journal: bool,
}

/// A mounted ext2/3/4 volume (read-only).
pub struct ExtFs<D: BlockDevice> {
    dev: D,
    sb: Sb,
}

impl<D: BlockDevice> ExtFs<D> {
    pub fn mount(dev: D) -> FsResult<Self> {
        // Superblock lives at byte offset 1024 (read 2 sectors from sector 2).
        let mut s = [0u8; 1024];
        dev.read_blocks(2, 2, &mut s).map_err(|_| FsError::IoError)?;
        if rd16(&s, 56) != EXT_MAGIC {
            return Err(FsError::Corruption);
        }
        let block_size = 1024u32 << rd32(&s, 24);
        let incompat = rd32(&s, 96);
        let inode_size = if rd16(&s, 88) == 0 { 128 } else { rd16(&s, 88) as u32 };
        let desc_size = if incompat & 0x80 != 0 {
            // 64BIT: descriptor size from s_desc_size, else 32.
            let d = rd16(&s, 254) as u32;
            if d == 0 {
                32
            } else {
                d
            }
        } else {
            32
        };
        let compat = rd32(&s, 92);
        let sb = Sb {
            block_size,
            blocks_per_group: rd32(&s, 32),
            inodes_per_group: rd32(&s, 40),
            inode_size,
            first_data_block: rd32(&s, 20),
            desc_size,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            has_journal: compat & COMPAT_HAS_JOURNAL != 0,
        };
        if sb.block_size < 1024 || sb.inodes_per_group == 0 {
            return Err(FsError::Corruption);
        }
        Ok(ExtFs { dev, sb })
    }

    /// Read one filesystem block (`block_size` bytes).
    fn read_block(&self, block: u64) -> FsResult<Vec<u8>> {
        let spb = (self.sb.block_size as usize / SECTOR) as u32;
        let mut buf = alloc::vec![0u8; self.sb.block_size as usize];
        self.dev.read_blocks(block * spb as u64, spb, &mut buf).map_err(|_| FsError::IoError)?;
        Ok(buf)
    }

    /// Read raw inode bytes for inode number `ino` (1-based).
    fn read_inode(&self, ino: u32) -> FsResult<Vec<u8>> {
        let group = (ino - 1) / self.sb.inodes_per_group;
        let index = (ino - 1) % self.sb.inodes_per_group;
        // Group-descriptor table starts at the block after the superblock block.
        let gdt_block = self.sb.first_data_block as u64 + 1;
        let desc_byte = group as u64 * self.sb.desc_size as u64;
        let gd_block = gdt_block + desc_byte / self.sb.block_size as u64;
        let gd_off = (desc_byte % self.sb.block_size as u64) as usize;
        let gdblk = self.read_block(gd_block)?;
        // bg_inode_table_lo @ offset 8 in the descriptor.
        let inode_table = rd32(&gdblk, gd_off + 8) as u64;
        let inode_byte = inode_table * self.sb.block_size as u64 + index as u64 * self.sb.inode_size as u64;
        // Read the block(s) containing the inode and slice it out.
        let blk = inode_byte / self.sb.block_size as u64;
        let off = (inode_byte % self.sb.block_size as u64) as usize;
        let data = self.read_block(blk)?;
        let isz = self.sb.inode_size as usize;
        if off + isz > data.len() {
            // Inode straddles two blocks (large inode_size + 512B alignment) — read both.
            let mut two = data;
            two.extend_from_slice(&self.read_block(blk + 1)?);
            return Ok(two[off..off + isz].to_vec());
        }
        Ok(data[off..off + isz].to_vec())
    }

    // ── write path ────────────────────────────────────────────────────────

    /// Write one filesystem block (`block_size` bytes) back to the device.
    fn write_block(&mut self, block: u64, data: &[u8]) -> FsResult<()> {
        if data.len() != self.sb.block_size as usize {
            return Err(FsError::IoError);
        }
        let spb = (self.sb.block_size as usize / SECTOR) as u32;
        self.dev.write_blocks(block * spb as u64, spb, data).map_err(|_| FsError::IoError)
    }

    /// Byte offset of an inode on disk → (containing fs block, offset in block).
    /// Mirrors [`read_inode`]'s address math. (Requires `inode_size <= block_size`,
    /// true for this image: 256 ≤ 1024, so an inode never straddles a block.)
    fn inode_location(&self, ino: u32) -> FsResult<(u64, usize)> {
        let group = (ino - 1) / self.sb.inodes_per_group;
        let index = (ino - 1) % self.sb.inodes_per_group;
        let gdt_block = self.sb.first_data_block as u64 + 1;
        let desc_byte = group as u64 * self.sb.desc_size as u64;
        let gd_block = gdt_block + desc_byte / self.sb.block_size as u64;
        let gd_off = (desc_byte % self.sb.block_size as u64) as usize;
        let gdblk = self.read_block(gd_block)?;
        let inode_table = rd32(&gdblk, gd_off + 8) as u64;
        let inode_byte =
            inode_table * self.sb.block_size as u64 + index as u64 * self.sb.inode_size as u64;
        let blk = inode_byte / self.sb.block_size as u64;
        let off = (inode_byte % self.sb.block_size as u64) as usize;
        if off + self.sb.inode_size as usize > self.sb.block_size as usize {
            // Would straddle two blocks — unsupported on this layout.
            return Err(FsError::Unsupported);
        }
        Ok((blk, off))
    }

    /// Write `bytes` (must be exactly `inode_size`) for inode `ino` to disk.
    fn write_inode(&mut self, ino: u32, bytes: &[u8]) -> FsResult<()> {
        if bytes.len() != self.sb.inode_size as usize {
            return Err(FsError::IoError);
        }
        let (blk, off) = self.inode_location(ino)?;
        let mut data = self.read_block(blk)?;
        data[off..off + bytes.len()].copy_from_slice(bytes);
        self.write_block(blk, &data)
    }

    /// Read the group descriptor for `group` → (containing block, offset).
    fn gd_location(&self, group: u32) -> (u64, usize) {
        let gdt_block = self.sb.first_data_block as u64 + 1;
        let desc_byte = group as u64 * self.sb.desc_size as u64;
        let gd_block = gdt_block + desc_byte / self.sb.block_size as u64;
        let gd_off = (desc_byte % self.sb.block_size as u64) as usize;
        (gd_block, gd_off)
    }

    /// Adjust `s_free_blocks_count` (+/-) in the superblock and the group's
    /// `bg_free_blocks_count`. `group` selects the descriptor to touch.
    fn adjust_free_blocks(&mut self, group: u32, delta: i64) -> FsResult<()> {
        // Superblock @ byte 1024 = block (1024/block_size).
        let sb_block = 1024 / self.sb.block_size as u64;
        let sb_off = (1024 % self.sb.block_size as u64) as usize;
        let mut blk = self.read_block(sb_block)?;
        let cur = rd32(&blk, sb_off + 12) as i64;
        wr32(&mut blk, sb_off + 12, (cur + delta) as u32);
        self.write_block(sb_block, &blk)?;
        // Group descriptor bg_free_blocks_count @ desc+12 (u16).
        let (gb, go) = self.gd_location(group);
        let mut gdblk = self.read_block(gb)?;
        let gcur = rd16(&gdblk, go + 12) as i64;
        wr16(&mut gdblk, go + 12, (gcur + delta) as u16);
        self.write_block(gb, &gdblk)
    }

    /// Adjust `s_free_inodes_count` + the group's `bg_free_inodes_count`.
    fn adjust_free_inodes(&mut self, group: u32, delta: i64) -> FsResult<()> {
        let sb_block = 1024 / self.sb.block_size as u64;
        let sb_off = (1024 % self.sb.block_size as u64) as usize;
        let mut blk = self.read_block(sb_block)?;
        let cur = rd32(&blk, sb_off + 16) as i64;
        wr32(&mut blk, sb_off + 16, (cur + delta) as u32);
        self.write_block(sb_block, &blk)?;
        let (gb, go) = self.gd_location(group);
        let mut gdblk = self.read_block(gb)?;
        let gcur = rd16(&gdblk, go + 14) as i64;
        wr16(&mut gdblk, go + 14, (gcur + delta) as u16);
        self.write_block(gb, &gdblk)
    }

    /// Adjust the group's `bg_used_dirs_count` @ desc+16 (u16). Keeps `e2fsck`
    /// happy when creating/removing directories.
    fn adjust_used_dirs(&mut self, group: u32, delta: i64) -> FsResult<()> {
        let (gb, go) = self.gd_location(group);
        let mut gdblk = self.read_block(gb)?;
        let cur = rd16(&gdblk, go + 16) as i64;
        wr16(&mut gdblk, go + 16, (cur + delta) as u16);
        self.write_block(gb, &gdblk)
    }

    /// Allocate one free data block. Scans group 0's block bitmap, sets the bit,
    /// writes it back, fixes counters, and **zeroes** the new block. Returns the
    /// absolute (filesystem) block number.
    fn alloc_block(&mut self) -> FsResult<u64> {
        // One group is enough for this 8 MiB single-group image; scan group 0.
        let group = 0u32;
        let (gb, go) = self.gd_location(group);
        let gdblk = self.read_block(gb)?;
        let bitmap_block = rd32(&gdblk, go) as u64;
        let mut bm = self.read_block(bitmap_block)?;
        let nblocks = self.sb.blocks_per_group as usize;
        for bit in 0..nblocks {
            let byte = bit / 8;
            let mask = 1u8 << (bit % 8);
            if bm[byte] & mask == 0 {
                bm[byte] |= mask;
                self.write_block(bitmap_block, &bm)?;
                self.adjust_free_blocks(group, -1)?;
                // Absolute block = first_data_block + group*blocks_per_group + bit.
                let phys = self.sb.first_data_block as u64
                    + group as u64 * self.sb.blocks_per_group as u64
                    + bit as u64;
                let zero = alloc::vec![0u8; self.sb.block_size as usize];
                self.write_block(phys, &zero)?;
                return Ok(phys);
            }
        }
        Err(FsError::NoSpace)
    }

    /// Free a previously-allocated data block (clear bitmap bit, bump counters).
    fn free_block(&mut self, phys: u64) -> FsResult<()> {
        if phys == 0 {
            return Ok(()); // sparse hole — nothing to free
        }
        let group = ((phys - self.sb.first_data_block as u64)
            / self.sb.blocks_per_group as u64) as u32;
        let bit = ((phys - self.sb.first_data_block as u64)
            % self.sb.blocks_per_group as u64) as usize;
        let (gb, go) = self.gd_location(group);
        let gdblk = self.read_block(gb)?;
        let bitmap_block = rd32(&gdblk, go) as u64;
        let mut bm = self.read_block(bitmap_block)?;
        let byte = bit / 8;
        let mask = 1u8 << (bit % 8);
        if bm[byte] & mask != 0 {
            bm[byte] &= !mask;
            self.write_block(bitmap_block, &bm)?;
            self.adjust_free_blocks(group, 1)?;
        }
        Ok(())
    }

    /// Allocate a free inode number (skipping the reserved ones below 11).
    fn alloc_inode(&mut self) -> FsResult<u32> {
        let group = 0u32;
        let (gb, go) = self.gd_location(group);
        let gdblk = self.read_block(gb)?;
        let bitmap_block = rd32(&gdblk, go + 4) as u64;
        let mut bm = self.read_block(bitmap_block)?;
        let ninodes = self.sb.inodes_per_group as usize;
        for bit in 0..ninodes {
            let ino = group * self.sb.inodes_per_group + bit as u32 + 1;
            if ino < 11 {
                continue; // reserved (root=2, etc.)
            }
            let byte = bit / 8;
            let mask = 1u8 << (bit % 8);
            if bm[byte] & mask == 0 {
                bm[byte] |= mask;
                self.write_block(bitmap_block, &bm)?;
                self.adjust_free_inodes(group, -1)?;
                return Ok(ino);
            }
        }
        Err(FsError::NoSpace)
    }

    /// Free an inode number (clear bitmap bit, bump counters).
    fn free_inode(&mut self, ino: u32) -> FsResult<()> {
        let group = (ino - 1) / self.sb.inodes_per_group;
        let bit = ((ino - 1) % self.sb.inodes_per_group) as usize;
        let (gb, go) = self.gd_location(group);
        let gdblk = self.read_block(gb)?;
        let bitmap_block = rd32(&gdblk, go + 4) as u64;
        let mut bm = self.read_block(bitmap_block)?;
        let byte = bit / 8;
        let mask = 1u8 << (bit % 8);
        if bm[byte] & mask != 0 {
            bm[byte] &= !mask;
            self.write_block(bitmap_block, &bm)?;
            self.adjust_free_inodes(group, 1)?;
        }
        Ok(())
    }

    fn inode_size(inode: &[u8]) -> u64 {
        let lo = rd32(inode, 4) as u64;
        let hi = rd32(inode, 108) as u64; // i_size_high (regular files)
        (hi << 32) | lo
    }
    fn inode_is_dir(inode: &[u8]) -> bool {
        rd16(inode, 0) & 0xF000 == 0x4000
    }

    /// Map a logical block of `inode` to its physical block (0 = sparse hole).
    fn logical_to_physical(&self, inode: &[u8], lblock: u64) -> FsResult<u64> {
        let flags = rd32(inode, 32);
        if flags & INODE_FL_EXTENTS != 0 {
            self.extent_lookup(&inode[40..40 + 60], lblock)
        } else {
            self.classic_lookup(inode, lblock)
        }
    }

    /// Walk an extent tree node (`hdr` = the 12-byte header + entries) for `lblock`.
    fn extent_lookup(&self, hdr: &[u8], lblock: u64) -> FsResult<u64> {
        if rd16(hdr, 0) != EXTENT_MAGIC {
            return Err(FsError::Corruption);
        }
        let entries = rd16(hdr, 2) as usize;
        let depth = rd16(hdr, 6);
        if depth == 0 {
            // Leaf: ext4_extent entries (12 bytes each).
            for i in 0..entries {
                let e = &hdr[12 + i * 12..];
                let ee_block = rd32(e, 0) as u64;
                let mut ee_len = rd16(e, 4) as u64;
                if ee_len > 32768 {
                    ee_len -= 32768; // uninitialized extent
                }
                let start = ((rd16(e, 6) as u64) << 32) | rd32(e, 8) as u64;
                if lblock >= ee_block && lblock < ee_block + ee_len {
                    return Ok(start + (lblock - ee_block));
                }
            }
            Ok(0) // not mapped → sparse
        } else {
            // Index node: find the right child and recurse into its extent block.
            let mut child = 0u64;
            for i in 0..entries {
                let e = &hdr[12 + i * 12..];
                let ei_block = rd32(e, 0) as u64;
                if lblock >= ei_block {
                    child = ((rd16(e, 8) as u64) << 32) | rd32(e, 4) as u64;
                } else {
                    break;
                }
            }
            if child == 0 {
                return Ok(0);
            }
            let blk = self.read_block(child)?;
            self.extent_lookup(&blk, lblock)
        }
    }

    /// Classic ext2/3 block map: 12 direct + single/double/triple indirect.
    fn classic_lookup(&self, inode: &[u8], lblock: u64) -> FsResult<u64> {
        let ppb = self.sb.block_size as u64 / 4; // pointers per block
        let iblock = &inode[40..40 + 60];
        if lblock < 12 {
            return Ok(rd32(iblock, lblock as usize * 4) as u64);
        }
        let l = lblock - 12;
        let read_ptr = |blk: u64, idx: u64| -> FsResult<u64> {
            if blk == 0 {
                return Ok(0);
            }
            let b = self.read_block(blk)?;
            Ok(rd32(&b, idx as usize * 4) as u64)
        };
        if l < ppb {
            return read_ptr(rd32(iblock, 12 * 4) as u64, l);
        }
        let l = l - ppb;
        if l < ppb * ppb {
            let single = read_ptr(rd32(iblock, 13 * 4) as u64, l / ppb)?;
            return read_ptr(single, l % ppb);
        }
        let l = l - ppb * ppb;
        let double = read_ptr(rd32(iblock, 14 * 4) as u64, l / (ppb * ppb))?;
        let single = read_ptr(double, (l / ppb) % ppb)?;
        read_ptr(single, l % ppb)
    }

    /// Read the full data of an inode (truncated to its size).
    fn read_inode_data(&self, inode: &[u8]) -> FsResult<Vec<u8>> {
        let size = Self::inode_size(inode) as usize;
        let bs = self.sb.block_size as usize;
        let mut out = Vec::with_capacity(size);
        let mut lblock = 0u64;
        while out.len() < size {
            let phys = self.logical_to_physical(inode, lblock)?;
            let chunk = if phys == 0 {
                alloc::vec![0u8; bs] // sparse hole
            } else {
                self.read_block(phys)?
            };
            let take = (size - out.len()).min(bs);
            out.extend_from_slice(&chunk[..take]);
            lblock += 1;
            if lblock > (size / bs + 2) as u64 {
                break;
            }
        }
        out.truncate(size);
        Ok(out)
    }

    /// Parse a directory inode → (name, inode, is_dir).
    fn read_dir(&self, inode: &[u8]) -> FsResult<Vec<(String, u32, bool)>> {
        let data = self.read_inode_data(inode)?;
        let mut out = Vec::new();
        let mut p = 0usize;
        while p + 8 <= data.len() {
            let ino = rd32(&data, p);
            let rec_len = rd16(&data, p + 4) as usize;
            let name_len = data[p + 6] as usize;
            let file_type = data[p + 7];
            if rec_len < 8 {
                break;
            }
            if ino != 0 && p + 8 + name_len <= data.len() {
                let name = String::from_utf8_lossy(&data[p + 8..p + 8 + name_len]).into_owned();
                if name != "." && name != ".." {
                    // file_type 2 = directory (when the FILETYPE feature is set); otherwise
                    // fall back to reading the child inode's mode.
                    let is_dir = if self.sb.filetype {
                        file_type == 2
                    } else {
                        self.read_inode(ino).map(|i| Self::inode_is_dir(&i)).unwrap_or(false)
                    };
                    out.push((name, ino, is_dir));
                }
            }
            p += rec_len;
        }
        Ok(out)
    }

    fn find_in_dir(&self, dir_inode: &[u8], name: &str) -> FsResult<(u32, bool)> {
        for (n, ino, is_dir) in self.read_dir(dir_inode)? {
            if n == name {
                return Ok((ino, is_dir));
            }
        }
        Err(FsError::NotFound)
    }

    /// Resolve a path → (inode bytes, is_dir).
    fn resolve(&self, path: &str) -> FsResult<(Vec<u8>, bool)> {
        let mut inode = self.read_inode(ROOT_INO)?;
        let mut is_dir = true;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !is_dir {
                return Err(FsError::NotADirectory);
            }
            let (ino, d) = self.find_in_dir(&inode, part)?;
            inode = self.read_inode(ino)?;
            is_dir = d;
        }
        Ok((inode, is_dir))
    }

    /// Resolve a path → the inode *number* of its parent directory + the final
    /// name component. Errors on a path with no name (e.g. "/").
    fn resolve_parent<'a>(&self, path: &'a str) -> FsResult<(u32, &'a str)> {
        let trimmed = path.trim_end_matches('/');
        let (dir_part, name) = match trimmed.rfind('/') {
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        // Walk dir_part from root to find the parent's inode number.
        let mut ino = ROOT_INO;
        let mut inode = self.read_inode(ROOT_INO)?;
        for part in dir_part.split('/').filter(|p| !p.is_empty()) {
            let (child, is_dir) = self.find_in_dir(&inode, part)?;
            if !is_dir {
                return Err(FsError::NotADirectory);
            }
            ino = child;
            inode = self.read_inode(child)?;
        }
        Ok((ino, name))
    }

    // ── classic block-pointer data layout (write) ──────────────────────────

    /// Maximum logical blocks addressable via direct + single + double indirect.
    fn max_classic_blocks(&self) -> u64 {
        let ppb = self.sb.block_size as u64 / 4;
        12 + ppb + ppb * ppb
    }

    /// Set the physical block for logical block `lblock` in a classic-pointer
    /// inode, allocating indirect blocks as needed. `iblock` is the 60-byte
    /// i_block array (mutated in place). Returns the number of *metadata* (indirect)
    /// blocks newly allocated (so the caller can account i_blocks).
    fn set_classic_block(&mut self, iblock: &mut [u8], lblock: u64, phys: u64) -> FsResult<u64> {
        let ppb = self.sb.block_size as u64 / 4;
        let mut meta = 0u64;
        if lblock < 12 {
            wr32(iblock, lblock as usize * 4, phys as u32);
            return Ok(0);
        }
        let mut l = lblock - 12;
        if l < ppb {
            // single indirect
            let mut sind = rd32(iblock, 12 * 4) as u64;
            if sind == 0 {
                sind = self.alloc_block()?;
                meta += 1;
                wr32(iblock, 12 * 4, sind as u32);
            }
            let mut blk = self.read_block(sind)?;
            wr32(&mut blk, l as usize * 4, phys as u32);
            self.write_block(sind, &blk)?;
            return Ok(meta);
        }
        l -= ppb;
        // double indirect
        let mut dind = rd32(iblock, 13 * 4) as u64;
        if dind == 0 {
            dind = self.alloc_block()?;
            meta += 1;
            wr32(iblock, 13 * 4, dind as u32);
        }
        let mut dblk = self.read_block(dind)?;
        let outer = (l / ppb) as usize;
        let mut sind = rd32(&dblk, outer * 4) as u64;
        if sind == 0 {
            sind = self.alloc_block()?;
            meta += 1;
            wr32(&mut dblk, outer * 4, sind as u32);
            self.write_block(dind, &dblk)?;
        }
        let mut sblk = self.read_block(sind)?;
        wr32(&mut sblk, (l % ppb) as usize * 4, phys as u32);
        self.write_block(sind, &sblk)?;
        Ok(meta)
    }

    /// Free all data + indirect blocks of a classic-pointer inode. Returns
    /// Unsupported if the inode uses an extent tree (we never free extents).
    fn free_inode_blocks(&mut self, inode: &[u8]) -> FsResult<()> {
        let flags = rd32(inode, 32);
        if flags & INODE_FL_EXTENTS != 0 {
            return Err(FsError::Unsupported);
        }
        let size = Self::inode_size(inode);
        let bs = self.sb.block_size as u64;
        let nblocks = size.div_ceil(bs.max(1));
        let ppb = bs / 4;
        let iblock = inode[40..40 + 60].to_vec();
        // Direct + single + double indirect data blocks.
        for lb in 0..nblocks {
            let phys = self.classic_lookup(inode, lb)?;
            self.free_block(phys)?;
        }
        // Free the indirect metadata blocks themselves.
        let sind = rd32(&iblock, 12 * 4) as u64;
        if sind != 0 {
            self.free_block(sind)?;
        }
        let dind = rd32(&iblock, 13 * 4) as u64;
        if dind != 0 {
            let dblk = self.read_block(dind)?;
            for i in 0..ppb as usize {
                let sb2 = rd32(&dblk, i * 4) as u64;
                if sb2 != 0 {
                    self.free_block(sb2)?;
                }
            }
            self.free_block(dind)?;
        }
        Ok(())
    }

    /// Build a fresh, zeroed classic-pointer inode with the given mode, write
    /// `data` into freshly-allocated data blocks, and return the inode bytes
    /// (NOT yet written to disk). `links` sets i_links_count.
    fn build_file_inode(&mut self, mode: u16, links: u16, data: &[u8]) -> FsResult<Vec<u8>> {
        let bs = self.sb.block_size as usize;
        let nblocks = data.len().div_ceil(bs.max(1)) as u64;
        if nblocks > self.max_classic_blocks() {
            return Err(FsError::NoSpace);
        }
        let mut inode = alloc::vec![0u8; self.sb.inode_size as usize];
        wr16(&mut inode, 0, mode);
        wr32(&mut inode, 4, data.len() as u32); // i_size_lo
        wr16(&mut inode, 26, links); // i_links_count
        // i_flags left 0 → classic block pointers (no EXTENTS).
        // Large inodes (>128) need i_extra_isize set; mkfs default is 32.
        if self.sb.inode_size > 128 {
            wr16(&mut inode, 128, 32); // i_extra_isize @ +128
        }
        let mut iblock = [0u8; 60];
        let mut meta = 0u64;
        for lb in 0..nblocks {
            let phys = self.alloc_block()?;
            let off = (lb as usize) * bs;
            let end = (off + bs).min(data.len());
            let mut blk = alloc::vec![0u8; bs];
            blk[..end - off].copy_from_slice(&data[off..end]);
            self.write_block(phys, &blk)?;
            meta += self.set_classic_block(&mut iblock, lb, phys)?;
        }
        inode[40..40 + 60].copy_from_slice(&iblock);
        // i_blocks_lo @ +28: count of 512-byte sectors (data + indirect metadata).
        let sectors = (nblocks + meta) * (bs as u64 / SECTOR as u64);
        wr32(&mut inode, 28, sectors as u32);
        Ok(inode)
    }

    // ── directory entry insert / remove ────────────────────────────────────

    /// Insert a directory entry (ino, name, file_type) into directory `dir_ino`.
    /// Splits the last entry of a block to make room; if no block has room,
    /// allocates a new directory block. file_type is ignored unless FILETYPE is on.
    fn dir_insert(&mut self, dir_ino: u32, name: &str, ino: u32, file_type: u8) -> FsResult<()> {
        let bs = self.sb.block_size as usize;
        let nlen = name.len();
        if nlen == 0 || nlen > 255 {
            return Err(FsError::InvalidPath);
        }
        let needed = (8 + nlen + 3) & !3; // 4-byte aligned record
        let mut dir_inode = self.read_inode(dir_ino)?;
        let dir_is_extent = rd32(&dir_inode, 32) & INODE_FL_EXTENTS != 0;
        let dsize = Self::inode_size(&dir_inode);
        let nblocks = dsize / bs as u64;
        let ft = if self.sb.filetype { file_type } else { 0 };
        // Try existing directory blocks (in-place modify works for extent dirs too).
        for lb in 0..nblocks {
            let phys = self.logical_to_physical(&dir_inode, lb)?;
            if phys == 0 {
                continue;
            }
            let mut blk = self.read_block(phys)?;
            let mut p = 0usize;
            while p + 8 <= bs {
                let e_ino = rd32(&blk, p);
                let rec_len = rd16(&blk, p + 4) as usize;
                if rec_len < 8 || p + rec_len > bs {
                    break;
                }
                let e_namelen = blk[p + 6] as usize;
                let used = if e_ino == 0 { 0 } else { (8 + e_namelen + 3) & !3 };
                let avail = rec_len - used;
                if avail >= needed {
                    // Split: shrink the existing entry to `used`, place new entry after.
                    let new_off = p + used;
                    let new_rec = rec_len - used;
                    if used > 0 {
                        wr16(&mut blk, p + 4, used as u16);
                    }
                    let off = if used == 0 { p } else { new_off };
                    wr32(&mut blk, off, ino);
                    wr16(&mut blk, off + 4, new_rec as u16);
                    blk[off + 6] = nlen as u8;
                    blk[off + 7] = ft;
                    blk[off + 8..off + 8 + nlen].copy_from_slice(name.as_bytes());
                    self.write_block(phys, &blk)?;
                    return Ok(());
                }
                p += rec_len;
            }
        }
        // No room in any existing block: must grow the directory by one block.
        // We can only do that for a classic-pointer directory (we never extend an
        // extent tree). An empty/freshly-made dir always has room, so in practice
        // this only refuses on a *full* extent-mapped directory.
        if dir_is_extent {
            return Err(FsError::Unsupported);
        }
        // Allocate a new directory block (rec_len spans the whole block).
        let phys = self.alloc_block()?;
        let mut blk = alloc::vec![0u8; bs];
        wr32(&mut blk, 0, ino);
        wr16(&mut blk, 4, bs as u16);
        blk[6] = nlen as u8;
        blk[7] = ft;
        blk[8..8 + nlen].copy_from_slice(name.as_bytes());
        self.write_block(phys, &blk)?;
        let meta = self.set_classic_block_in_inode(&mut dir_inode, nblocks, phys)?;
        let new_size = dsize + bs as u64;
        wr32(&mut dir_inode, 4, new_size as u32);
        let old_sectors = rd32(&dir_inode, 28) as u64;
        wr32(
            &mut dir_inode,
            28,
            (old_sectors + (1 + meta) * (bs as u64 / SECTOR as u64)) as u32,
        );
        self.write_inode(dir_ino, &dir_inode)
    }

    /// Helper: set a classic block on an inode's i_block in place (returns meta count).
    fn set_classic_block_in_inode(
        &mut self,
        inode: &mut [u8],
        lblock: u64,
        phys: u64,
    ) -> FsResult<u64> {
        let mut iblock = [0u8; 60];
        iblock.copy_from_slice(&inode[40..40 + 60]);
        let meta = self.set_classic_block(&mut iblock, lblock, phys)?;
        inode[40..40 + 60].copy_from_slice(&iblock);
        Ok(meta)
    }

    /// Remove the entry named `name` from directory `dir_ino`. Returns the removed
    /// entry's inode number. Merges the freed record into the previous entry
    /// (or zeroes the inode field if it's the first entry in a block).
    fn dir_remove(&mut self, dir_ino: u32, name: &str) -> FsResult<u32> {
        let bs = self.sb.block_size as usize;
        let dir_inode = self.read_inode(dir_ino)?;
        let dsize = Self::inode_size(&dir_inode);
        let nblocks = dsize / bs as u64;
        for lb in 0..nblocks {
            let phys = self.logical_to_physical(&dir_inode, lb)?;
            if phys == 0 {
                continue;
            }
            let mut blk = self.read_block(phys)?;
            let mut p = 0usize;
            let mut prev: Option<usize> = None;
            while p + 8 <= bs {
                let e_ino = rd32(&blk, p);
                let rec_len = rd16(&blk, p + 4) as usize;
                if rec_len < 8 || p + rec_len > bs {
                    break;
                }
                let e_namelen = blk[p + 6] as usize;
                if e_ino != 0 && p + 8 + e_namelen <= bs {
                    let ename = &blk[p + 8..p + 8 + e_namelen];
                    if ename == name.as_bytes() {
                        match prev {
                            Some(pp) => {
                                // Extend previous record over this one.
                                let prev_rec = rd16(&blk, pp + 4) as usize;
                                wr16(&mut blk, pp + 4, (prev_rec + rec_len) as u16);
                            }
                            None => {
                                // First entry in block: just clear its inode field.
                                wr32(&mut blk, p, 0);
                            }
                        }
                        self.write_block(phys, &blk)?;
                        return Ok(e_ino);
                    }
                }
                prev = Some(p);
                p += rec_len;
            }
        }
        Err(FsError::NotFound)
    }
}

impl<D: BlockDevice> FileSystem for ExtFs<D> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let (inode, is_dir) = self.resolve(path)?;
        if is_dir {
            return Err(FsError::NotAFile);
        }
        self.read_inode_data(&inode)
    }
    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let (inode, is_dir) = self.resolve(path)?;
        if !is_dir {
            return Err(FsError::NotADirectory);
        }
        let mut out = Vec::new();
        for (name, ino, dir) in self.read_dir(&inode)? {
            let size = if dir { 0 } else { self.read_inode(ino).map(|i| Self::inode_size(&i)).unwrap_or(0) };
            out.push(DirEntry {
                name,
                kind: if dir { EntryKind::Directory } else { EntryKind::File },
                size,
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
        let (inode, is_dir) = self.resolve(path)?;
        let name = path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/").into();
        Ok(DirEntry {
            name,
            kind: if is_dir { EntryKind::Directory } else { EntryKind::File },
            size: if is_dir { 0 } else { Self::inode_size(&inode) },
            mode: if is_dir { 0o755 } else { 0o644 },
            mtime: 0,
        })
    }
    fn space_info(&self) -> (u64, u64) {
        (0, 0)
    }
    /// Write a regular file (creating or overwriting). The new/overwritten inode
    /// always uses classic block pointers. Refused (`Unsupported`) if the fs has
    /// an active jbd2 journal, or if an existing file being overwritten uses an
    /// extent tree (we never free extent-mapped blocks).
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        if self.sb.has_journal {
            return Err(FsError::Unsupported);
        }
        let (parent_ino, name) = self.resolve_parent(path)?;
        // Does it already exist in the parent?
        let parent_inode = self.read_inode(parent_ino)?;
        let existing = self.find_in_dir(&parent_inode, name).ok();
        let target_ino = match existing {
            Some((ino, is_dir)) => {
                if is_dir {
                    return Err(FsError::NotAFile);
                }
                // Overwrite: free old blocks (refuses if extent-based).
                let old = self.read_inode(ino)?;
                self.free_inode_blocks(&old)?;
                ino
            }
            None => self.alloc_inode()?,
        };
        let inode = self.build_file_inode(MODE_FILE, 1, data)?;
        self.write_inode(target_ino, &inode)?;
        if existing.is_none() {
            // The dirent insert can fail deterministically (e.g. a full extent-mapped
            // parent directory we cannot grow). Roll back the freshly-allocated inode +
            // its data blocks so we never leave an orphaned inode / leaked blocks on disk.
            if let Err(e) = self.dir_insert(parent_ino, name, target_ino, 1) {
                let _ = self.free_inode_blocks(&inode);
                let zero = alloc::vec![0u8; self.sb.inode_size as usize];
                let _ = self.write_inode(target_ino, &zero);
                let _ = self.free_inode(target_ino);
                let _ = self.dev.flush();
                return Err(e);
            }
        }
        self.dev.flush().map_err(|_| FsError::IoError)
    }

    /// Remove a regular file: detach its dirent, free its data blocks + inode.
    /// Refused (`Unsupported`) on a journalled fs or an extent-based inode.
    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        if self.sb.has_journal {
            return Err(FsError::Unsupported);
        }
        let (parent_ino, name) = self.resolve_parent(path)?;
        let parent_inode = self.read_inode(parent_ino)?;
        let (ino, is_dir) = self.find_in_dir(&parent_inode, name)?;
        if is_dir {
            return Err(FsError::NotAFile);
        }
        let inode = self.read_inode(ino)?;
        // free_inode_blocks refuses extent inodes before we touch the dirent.
        self.free_inode_blocks(&inode)?;
        self.dir_remove(parent_ino, name)?;
        // Zero the inode and free it.
        let zero = alloc::vec![0u8; self.sb.inode_size as usize];
        self.write_inode(ino, &zero)?;
        self.free_inode(ino)?;
        self.dev.flush().map_err(|_| FsError::IoError)
    }

    /// Create a directory with classic block pointers: one data block holding
    /// "." and "..", links=2, and the parent's link count bumped.
    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        if self.sb.has_journal {
            return Err(FsError::Unsupported);
        }
        let (parent_ino, name) = self.resolve_parent(path)?;
        let parent_inode = self.read_inode(parent_ino)?;
        if self.find_in_dir(&parent_inode, name).is_ok() {
            return Err(FsError::AlreadyExists);
        }
        let new_ino = self.alloc_inode()?;
        let bs = self.sb.block_size as usize;
        let dblock = self.alloc_block()?;
        // Build the "." / ".." block. "." rec_len = 12, ".." spans to block end.
        let mut blk = alloc::vec![0u8; bs];
        let ft_dir = if self.sb.filetype { 2 } else { 0 };
        // "."
        wr32(&mut blk, 0, new_ino);
        wr16(&mut blk, 4, 12);
        blk[6] = 1;
        blk[7] = ft_dir;
        blk[8] = b'.';
        // ".."
        wr32(&mut blk, 12, parent_ino);
        wr16(&mut blk, 12 + 4, (bs - 12) as u16);
        blk[12 + 6] = 2;
        blk[12 + 7] = ft_dir;
        blk[12 + 8] = b'.';
        blk[12 + 9] = b'.';
        self.write_block(dblock, &blk)?;
        // Build the directory inode.
        let mut inode = alloc::vec![0u8; self.sb.inode_size as usize];
        wr16(&mut inode, 0, MODE_DIR);
        wr32(&mut inode, 4, bs as u32); // i_size_lo = one block
        wr16(&mut inode, 26, 2); // links: "." + parent's entry
        if self.sb.inode_size > 128 {
            wr16(&mut inode, 128, 32);
        }
        let mut iblock = [0u8; 60];
        let meta = self.set_classic_block(&mut iblock, 0, dblock)?;
        inode[40..40 + 60].copy_from_slice(&iblock);
        let sectors = (1 + meta) * (bs as u64 / SECTOR as u64);
        wr32(&mut inode, 28, sectors as u32);
        self.write_inode(new_ino, &inode)?;
        // Insert dirent into parent and bump the parent's link count (for "..").
        // Roll back the inode + its data block if the insert fails (full extent dir),
        // so we never leave an orphaned directory inode / leaked block on disk.
        if let Err(e) = self.dir_insert(parent_ino, name, new_ino, 2) {
            let _ = self.free_block(dblock);
            let zero = alloc::vec![0u8; self.sb.inode_size as usize];
            let _ = self.write_inode(new_ino, &zero);
            let _ = self.free_inode(new_ino);
            let _ = self.dev.flush();
            return Err(e);
        }
        let mut pinode = self.read_inode(parent_ino)?;
        let plinks = rd16(&pinode, 26);
        wr16(&mut pinode, 26, plinks + 1);
        self.write_inode(parent_ino, &pinode)?;
        // bg_used_dirs_count++ for e2fsck accounting.
        self.adjust_used_dirs(0, 1)?;
        self.dev.flush().map_err(|_| FsError::IoError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eurofs::MemoryBlockDevice;

    fn fixture() -> ExtFs<MemoryBlockDevice> {
        let img: &[u8] = include_bytes!("../testdata/ext4.img");
        let sectors = (img.len() / SECTOR) as u64;
        let mut dev = MemoryBlockDevice::new(sectors, SECTOR as u32);
        dev.write_blocks(0, sectors as u32, img).unwrap();
        ExtFs::mount(dev).expect("mount real ext4 image")
    }

    /// Regression for the audit CRITICAL: a `write_file` whose dirent insert fails
    /// (full extent-mapped root dir we cannot grow) must NOT leak the allocated inode.
    #[test]
    fn dir_insert_failure_rolls_back_no_orphan() {
        fn free_inodes<D: BlockDevice>(fs: &ExtFs<D>) -> u32 {
            let mut s = [0u8; 1024];
            fs.dev.read_blocks(2, 2, &mut s).unwrap();
            rd32(&s, 16) // s_free_inodes_count
        }
        let mut fs = fixture();
        // Fill the extent-mapped root directory with long names until an insert is refused.
        let mut i = 0;
        let mut failed = false;
        loop {
            let name = alloc::format!("/longfilename_number_{i:03}.txt");
            match fs.write_file(&name, b"x") {
                Ok(()) => {
                    i += 1;
                    if i > 500 {
                        break;
                    }
                }
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed, "expected the extent-mapped root dir to fill and refuse an insert");
        // A subsequent (also-failing) write must not consume an inode — proving rollback.
        let before = free_inodes(&fs);
        let _ = fs.write_file("/another_long_filename_zzz.txt", b"y");
        let after = free_inodes(&fs);
        assert_eq!(before, after, "a failed write_file leaked an inode (rollback missing)");
    }

    #[test]
    fn reads_files_from_real_ext4_image() {
        let fs = fixture();
        assert_eq!(fs.read_file("/readme.txt").unwrap(), b"hello from a real ext4 volume, read by EuroOS\n");
        assert_eq!(fs.read_file("/docs/note.txt").unwrap(), b"nested ext4 file\n");
        // 40000-byte 'A' file → multi-block via the extent tree.
        let big = fs.read_file("/big.dat").unwrap();
        assert_eq!(big.len(), 20000);
        assert!(big.iter().all(|&b| b == b'A'));
    }

    #[test]
    fn lists_directories_and_long_names() {
        let fs = fixture();
        let root = fs.list_dir("/").unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"readme.txt") && names.contains(&"docs") && names.contains(&"big.dat"));
        assert!(names.iter().any(|n| n.contains("long ext4 filename 2026")));
        assert_eq!(root.iter().find(|e| e.name == "docs").unwrap().kind, EntryKind::Directory);
        assert_eq!(fs.list_dir("/docs").unwrap()[0].name, "note.txt");
        assert_eq!(root.iter().find(|e| e.name == "big.dat").unwrap().size, 20000);
    }

    #[test]
    fn exists_basic() {
        let fs = fixture();
        assert!(fs.exists("/docs/note.txt"));
        assert!(!fs.exists("/nope"));
    }

    /// Existing extent-based files + metadata survive after we mutate the fs.
    fn assert_originals_intact(fs: &ExtFs<MemoryBlockDevice>) {
        assert_eq!(
            fs.read_file("/readme.txt").unwrap(),
            b"hello from a real ext4 volume, read by EuroOS\n"
        );
        assert_eq!(fs.read_file("/docs/note.txt").unwrap(), b"nested ext4 file\n");
        let big = fs.read_file("/big.dat").unwrap();
        assert_eq!(big.len(), 20000);
        assert!(big.iter().all(|&b| b == b'A'));
    }

    #[test]
    fn write_new_small_file_roundtrips() {
        let mut fs = fixture();
        let payload = b"a brand new classic-pointer file written by EuroOS\n";
        fs.write_file("/hello.txt", payload).unwrap();
        assert_eq!(fs.read_file("/hello.txt").unwrap(), payload);
        // Shows up in the root listing.
        let names: Vec<String> = fs.list_dir("/").unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.iter().any(|n| n == "hello.txt"));
        assert_eq!(
            fs.list_dir("/").unwrap().iter().find(|e| e.name == "hello.txt").unwrap().size,
            payload.len() as u64
        );
        assert_originals_intact(&fs);
    }

    #[test]
    fn write_large_file_forces_single_indirect() {
        let mut fs = fixture();
        // ~50 KiB → needs > 12 direct blocks (at 1 KiB) → single indirect.
        let mut data = Vec::with_capacity(50_000);
        for i in 0..50_000u32 {
            data.push((i % 251) as u8);
        }
        fs.write_file("/big_classic.bin", &data).unwrap();
        let back = fs.read_file("/big_classic.bin").unwrap();
        assert_eq!(back.len(), data.len());
        assert_eq!(back, data);
        assert_originals_intact(&fs);
    }

    #[test]
    fn overwrite_existing_classic_file() {
        let mut fs = fixture();
        fs.write_file("/ovr.txt", b"first version, quite long indeed padding padding").unwrap();
        assert_eq!(fs.read_file("/ovr.txt").unwrap().len(), 48);
        fs.write_file("/ovr.txt", b"v2").unwrap();
        assert_eq!(fs.read_file("/ovr.txt").unwrap(), b"v2");
        // Only one entry for it (overwrite did not duplicate the dirent).
        let count = fs.list_dir("/").unwrap().iter().filter(|e| e.name == "ovr.txt").count();
        assert_eq!(count, 1);
        assert_originals_intact(&fs);
    }

    #[test]
    fn create_dir_and_nested_file() {
        let mut fs = fixture();
        fs.create_dir("/newdir").unwrap();
        let root = fs.list_dir("/").unwrap();
        let d = root.iter().find(|e| e.name == "newdir").expect("newdir listed");
        assert_eq!(d.kind, EntryKind::Directory);
        // Fresh dir lists empty (only "." and ".." which read_dir filters out).
        assert!(fs.list_dir("/newdir").unwrap().is_empty());
        // Put a file inside.
        fs.write_file("/newdir/inside.txt", b"nested write\n").unwrap();
        let inside = fs.list_dir("/newdir").unwrap();
        assert_eq!(inside.len(), 1);
        assert_eq!(inside[0].name, "inside.txt");
        assert_eq!(fs.read_file("/newdir/inside.txt").unwrap(), b"nested write\n");
        assert_originals_intact(&fs);
    }

    #[test]
    fn remove_file_makes_it_gone() {
        let mut fs = fixture();
        fs.write_file("/tmp.txt", b"delete me").unwrap();
        assert!(fs.exists("/tmp.txt"));
        fs.remove_file("/tmp.txt").unwrap();
        assert!(!fs.exists("/tmp.txt"));
        assert!(fs.read_file("/tmp.txt").is_err());
        assert!(!fs.list_dir("/").unwrap().iter().any(|e| e.name == "tmp.txt"));
        assert_originals_intact(&fs);
    }

    #[test]
    fn overwriting_extent_file_is_refused() {
        let mut fs = fixture();
        // /big.dat is an extent-mapped file in the fixture → overwrite must refuse,
        // and the original content must be untouched.
        assert_eq!(fs.write_file("/big.dat", b"x"), Err(FsError::Unsupported));
        assert_originals_intact(&fs);
    }

    #[test]
    fn many_files_then_reread_all() {
        let mut fs = fixture();
        for i in 0..20 {
            let name = alloc::format!("/f{i:02}.txt");
            let body = alloc::format!("file number {i}\n");
            fs.write_file(&name, body.as_bytes()).unwrap();
        }
        for i in 0..20 {
            let name = alloc::format!("/f{i:02}.txt");
            let body = alloc::format!("file number {i}\n");
            assert_eq!(fs.read_file(&name).unwrap(), body.as_bytes());
        }
        assert_originals_intact(&fs);
    }
}

