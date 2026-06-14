//! **EuroExFAT** — a mountable exFAT *read* driver (Sprint IO-4).
//!
//! Implements [`eurofs::FileSystem`] (read path) over a 512-byte
//! [`eurofs::BlockDevice`], so an exFAT volume — large USB sticks / SD cards that ship
//! exFAT for >32 GB media — can be mounted into the EuroOS VFS and its files read.
//!
//! exFAT differs from FAT32: a dedicated boot region, a single FAT (with a
//! "NoFatChain" contiguous-file optimisation), an allocation bitmap, an up-case table,
//! and 32-byte **directory entry sets** (0x85 File + 0xC0 Stream-Extension + 0xC1
//! File-Name entries). Writing exFAT (bitmap + entry-set management) is deferred.
//!
//! Pure `no_std`, no `unsafe`. Host-tested against a real `mkfs.exfat` image.

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

/// A mounted exFAT volume (read-only).
pub struct ExFat<D: BlockDevice> {
    dev: D,
    boot: Boot,
}

impl<D: BlockDevice> ExFat<D> {
    pub fn mount(dev: D) -> FsResult<Self> {
        if dev.block_size() != SECTOR as u32 {
            return Err(FsError::Unsupported);
        }
        let mut s0 = [0u8; SECTOR];
        dev.read_blocks(0, 1, &mut s0).map_err(|_| FsError::IoError)?;
        let boot = Boot::parse(&s0)?;
        Ok(ExFat { dev, boot })
    }

    fn rsec(&self, lba: u32, buf: &mut [u8; SECTOR]) -> FsResult<()> {
        self.dev.read_blocks(lba as u64, 1, buf).map_err(|_| FsError::IoError)
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

    // ── Write path deferred (IO-4 ships read-only). ──
    fn write_file(&mut self, _path: &str, _data: &[u8]) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn remove_file(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
    }
    fn create_dir(&mut self, _path: &str) -> FsResult<()> {
        Err(FsError::Unsupported)
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

    #[test]
    fn writes_are_unsupported() {
        let mut fs = fixture();
        assert_eq!(fs.write_file("/x", b"x"), Err(FsError::Unsupported));
    }
}
