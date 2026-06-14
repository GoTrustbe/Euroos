//! **EuroExt** — a mountable ext2/3/4 *read* driver.
//!
//! Implements [`eurofs::FileSystem`] (read path) over a [`eurofs::BlockDevice`], so a
//! Linux ext-formatted disk can be mounted into the EuroOS VFS and its files read. One
//! driver covers ext2/ext3/ext4: it uses **extent trees** when the inode has the EXTENTS
//! flag (ext4) and classic direct/indirect **block pointers** otherwise (ext2/3).
//! Read-only — ext write means the jbd2 journal + bitmap management, deferred.
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
const INODE_FL_EXTENTS: u32 = 0x0008_0000;
const EXTENT_MAGIC: u16 = 0xF30A;
const ROOT_INO: u32 = 2;

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

struct Sb {
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    first_data_block: u32,
    desc_size: u32,
    filetype: bool,
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
        let sb = Sb {
            block_size,
            inodes_per_group: rd32(&s, 40),
            inode_size,
            first_data_block: rd32(&s, 20),
            desc_size,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
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
    fn write_file(&mut self, _p: &str, _d: &[u8]) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn remove_file(&mut self, _p: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn create_dir(&mut self, _p: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
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
    fn exists_and_writes_unsupported() {
        let mut fs = fixture();
        assert!(fs.exists("/docs/note.txt"));
        assert!(!fs.exists("/nope"));
        assert_eq!(fs.write_file("/x", b"x"), Err(FsError::Unsupported));
    }
}

