//! **EuroFAT** — a sovereign, from-scratch FAT32 builder + reader for the EFI System
//! Partition (ESP) that the EuroOS installer writes. Produces a byte image of
//! a valid FAT32 volume (BPB, FSInfo, two FATs, directories with 8.3 + LFN, and
//! file data) that can be booted by UEFI firmware (OVMF) — no `mkfs.fat`,
//! no libfat. Pure `no_std` logic, host-tested (round-trip + `mtools` validation).
//!
//! Deliberately simple: 512 B/sector, 1 sector/cluster, two FATs, shallow tree. Enough
//! for an ESP with `\EFI\BOOT\BOOTX64.EFI` + the A/B kernel images.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod disk;
pub mod sectored;
pub use disk::{build_boot_disk, build_esp, build_esp_cfg, layout_for, write_boot_disk, Layout};
pub use sectored::{read_small_file, write_small_file};

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const SECTOR: usize = 512;
const SPC: u32 = 1; // sectors per cluster
const RESERVED: u32 = 32; // reserved sectors (boot + fsinfo + backup + room)
const NUM_FATS: u32 = 2;
const CLUSTER_BYTES: usize = SPC as usize * SECTOR;
const EOC: u32 = 0x0FFF_FFFF; // end-of-chain
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;

/// A node in the directory tree to be built.
struct Node {
    name: String,
    is_dir: bool,
    data: Vec<u8>,
    children: Vec<usize>,
    first_cluster: u32,
    clusters: u32,
}

/// The FAT32 builder: add files at a path, and `build()` produces the image.
pub struct FatFs {
    total_sectors: u32,
    volume_id: u32,
    label: [u8; 11],
    nodes: Vec<Node>, // nodes[0] = root
}

impl FatFs {
    /// New empty FAT32 volume of `total_sectors` × 512 B. `volume_id` is arbitrary
    /// (e.g. from the RTC); `label` ≤ 11 characters.
    pub fn new(total_sectors: u32, volume_id: u32, label: &str) -> Self {
        let mut lbl = [b' '; 11];
        for (i, b) in label.bytes().take(11).enumerate() {
            lbl[i] = b.to_ascii_uppercase();
        }
        FatFs {
            total_sectors,
            volume_id,
            label: lbl,
            nodes: vec![Node {
                name: String::new(),
                is_dir: true,
                data: Vec::new(),
                children: Vec::new(),
                first_cluster: 0,
                clusters: 0,
            }],
        }
    }

    /// Add a file at `path` (e.g. `/EFI/BOOT/BOOTX64.EFI`). Intermediate
    /// directories are created.
    pub fn add_file(&mut self, path: &str, data: &[u8]) {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return;
        }
        let mut cur = 0usize; // root
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            if last {
                // file
                let idx = self.nodes.len();
                self.nodes.push(Node {
                    name: String::from(*part),
                    is_dir: false,
                    data: data.to_vec(),
                    children: Vec::new(),
                    first_cluster: 0,
                    clusters: 0,
                });
                self.nodes[cur].children.push(idx);
            } else {
                // directory: reuse or create
                let existing = self.nodes[cur]
                    .children
                    .iter()
                    .copied()
                    .find(|&c| self.nodes[c].is_dir && self.nodes[c].name.eq_ignore_ascii_case(part));
                cur = match existing {
                    Some(c) => c,
                    None => {
                        let idx = self.nodes.len();
                        self.nodes.push(Node {
                            name: String::from(*part),
                            is_dir: true,
                            data: Vec::new(),
                            children: Vec::new(),
                            first_cluster: 0,
                            clusters: 0,
                        });
                        self.nodes[cur].children.push(idx);
                        idx
                    }
                };
            }
        }
    }

    /// How many 32-byte directory entries does a directory have (incl. LFN + ./.. + end marker)?
    fn dir_entry_count(&self, node: usize, is_root: bool) -> usize {
        let mut n = if is_root { 1 } else { 2 }; // root: volume label · subdir: "." + ".."
        for &c in &self.nodes[node].children {
            n += entries_for_name(&self.nodes[c].name);
        }
        n + 1 // end marker (0x00) counts as at least one entry slot
    }

    /// Compute for each node the number of clusters it needs.
    fn compute_cluster_counts(&mut self) {
        let n = self.nodes.len();
        for i in 0..n {
            if self.nodes[i].is_dir {
                let entries = self.dir_entry_count(i, i == 0);
                let bytes = entries * 32;
                self.nodes[i].clusters = ((bytes + CLUSTER_BYTES - 1) / CLUSTER_BYTES).max(1) as u32;
            } else {
                let bytes = self.nodes[i].data.len();
                self.nodes[i].clusters = ((bytes + CLUSTER_BYTES - 1) / CLUSTER_BYTES) as u32; // 0 for empty file
            }
        }
    }

    /// Allocate clusters (root = cluster 2), depth-first, and return the highest
    /// used cluster.
    fn allocate(&mut self) -> u32 {
        let mut next = 2u32;
        self.alloc_node(0, &mut next);
        next
    }
    fn alloc_node(&mut self, node: usize, next: &mut u32) {
        if self.nodes[node].clusters > 0 {
            self.nodes[node].first_cluster = *next;
            *next += self.nodes[node].clusters;
        } else {
            self.nodes[node].first_cluster = 0;
        }
        let children = self.nodes[node].children.clone();
        for c in children {
            self.alloc_node(c, next);
        }
    }

    /// Build the full FAT32 image (`total_sectors` × 512 B).
    pub fn build(&mut self) -> Vec<u8> {
        self.compute_cluster_counts();
        let highest = self.allocate();
        let cluster_count = highest.saturating_sub(2).max(1);

        // sectors_per_fat (mkfs.fat formula for FAT32).
        let tmp1 = self.total_sectors.saturating_sub(RESERVED);
        let tmp2 = (256 * SPC + NUM_FATS) / 2;
        let spf = (tmp1 + tmp2 - 1) / tmp2.max(1);
        let data_start = RESERVED + NUM_FATS * spf; // first data sector (cluster 2)

        let mut img = vec![0u8; self.total_sectors as usize * SECTOR];

        // ── Boot sector (BPB) ──
        self.write_boot_sector(&mut img, spf);
        // Backup at sector 6.
        let bs = img[0..SECTOR].to_vec();
        img[6 * SECTOR..7 * SECTOR].copy_from_slice(&bs);
        // FSInfo at sector 1 (+ backup at sector 7) — with the real free-cluster count.
        let total_clusters = self.total_sectors.saturating_sub(data_start) / SPC;
        let used: u32 = self.nodes.iter().map(|n| n.clusters).sum();
        let free = total_clusters.saturating_sub(used);
        let next_free = highest; // first cluster after what we used
        write_fsinfo(&mut img[SECTOR..2 * SECTOR], free, next_free);
        let fsi = img[SECTOR..2 * SECTOR].to_vec();
        img[7 * SECTOR..8 * SECTOR].copy_from_slice(&fsi);

        // ── Build the FAT table (in clusters) ──
        let mut fat = vec![0u32; (data_start as usize) + cluster_count as usize + 8];
        fat[0] = 0x0FFF_FFF8; // media descriptor
        fat[1] = EOC;
        for i in 0..self.nodes.len() {
            let start = self.nodes[i].first_cluster;
            let cnt = self.nodes[i].clusters;
            for k in 0..cnt {
                let cl = start + k;
                fat[cl as usize] = if k + 1 == cnt { EOC } else { cl + 1 };
            }
        }
        // Write the two FAT copies.
        for f in 0..NUM_FATS {
            let base = (RESERVED + f * spf) as usize * SECTOR;
            for (cl, &val) in fat.iter().enumerate() {
                let off = base + cl * 4;
                if off + 4 <= img.len() {
                    img[off..off + 4].copy_from_slice(&(val & 0x0FFF_FFFF).to_le_bytes());
                }
            }
        }

        // ── Directory and file data in the clusters ──
        let cluster_off = |cl: u32| (data_start as usize + (cl as usize - 2) * SPC as usize) * SECTOR;
        // Directories.
        for i in 0..self.nodes.len() {
            if !self.nodes[i].is_dir {
                continue;
            }
            let dir_bytes = self.build_dir_data(i, i == 0);
            let mut off = cluster_off(self.nodes[i].first_cluster);
            let end = off + self.nodes[i].clusters as usize * CLUSTER_BYTES;
            for chunk in dir_bytes.chunks(1) {
                if off >= end || off >= img.len() {
                    break;
                }
                img[off] = chunk[0];
                off += 1;
            }
        }
        // Files.
        for i in 0..self.nodes.len() {
            if self.nodes[i].is_dir || self.nodes[i].clusters == 0 {
                continue;
            }
            let off = cluster_off(self.nodes[i].first_cluster);
            let data = &self.nodes[i].data;
            let n = data.len().min(img.len().saturating_sub(off));
            img[off..off + n].copy_from_slice(&data[..n]);
        }

        img
    }

    fn write_boot_sector(&self, img: &mut [u8], spf: u32) {
        let b = &mut img[0..SECTOR];
        b[0] = 0xEB;
        b[1] = 0x58;
        b[2] = 0x90;
        b[3..11].copy_from_slice(b"MSWIN4.1");
        b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        b[13] = SPC as u8;
        b[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        b[16] = NUM_FATS as u8;
        // 17..19 root_entry_count = 0 (FAT32)
        // 19..21 total_sectors_16 = 0
        b[21] = 0xF8; // media
        // 22..24 sectors_per_fat_16 = 0
        b[24..26].copy_from_slice(&32u16.to_le_bytes()); // sectors per track
        b[26..28].copy_from_slice(&64u16.to_le_bytes()); // heads
        // 28..32 hidden sectors = 0 (the partition LBA is handled by the GPT layer)
        b[32..36].copy_from_slice(&self.total_sectors.to_le_bytes());
        b[36..40].copy_from_slice(&spf.to_le_bytes());
        // 40..42 ext flags = 0 (FAT mirroring on)
        // 42..44 fs version = 0
        b[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
        b[48..50].copy_from_slice(&1u16.to_le_bytes()); // fsinfo sector
        b[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
        b[64] = 0x80; // drive number
        b[66] = 0x29; // extended boot signature
        b[67..71].copy_from_slice(&self.volume_id.to_le_bytes());
        b[71..82].copy_from_slice(&self.label);
        b[82..90].copy_from_slice(b"FAT32   ");
        b[510] = 0x55;
        b[511] = 0xAA;
    }

    /// Build the directory-entry bytes for directory `node`.
    fn build_dir_data(&self, node: usize, is_root: bool) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        if is_root {
            // Volume-label entry (ATTR_VOLUME_ID 0x08) — mirrors the BPB label.
            let mut e = sfn_entry(&self.label, 0x08, 0, 0);
            e[0] = self.label[0];
            out.extend_from_slice(&e);
        } else {
            out.extend_from_slice(&dot_entry(b".          ", self.nodes[node].first_cluster));
            // ".." points to the parent; root parent = cluster 0.
            let parent_cl = self.parent_cluster(node);
            out.extend_from_slice(&dot_entry(b"..         ", parent_cl));
        }
        let mut used: Vec<[u8; 11]> = Vec::new();
        for &c in &self.nodes[node].children {
            let child = &self.nodes[c];
            let attr = if child.is_dir { ATTR_DIR } else { 0 };
            let size = if child.is_dir { 0 } else { child.data.len() as u32 };
            // Pick a UNIQUE 8.3 name in this directory (BASE~N on collision/LFN).
            let short = match short83(&child.name) {
                Some(s) if !used.contains(&s) => s,
                _ => {
                    let mut n = 1u32;
                    loop {
                        let s = mangle83(&child.name, n);
                        if !used.contains(&s) {
                            break s;
                        }
                        n += 1;
                    }
                }
            };
            used.push(short);
            out.extend_from_slice(&dir_entries_for(&child.name, short, attr, child.first_cluster, size));
        }
        out // the rest of the cluster is already 0 (end marker)
    }

    fn parent_cluster(&self, node: usize) -> u32 {
        for (i, n) in self.nodes.iter().enumerate() {
            if n.children.contains(&node) {
                return if i == 0 { 0 } else { self.nodes[i].first_cluster };
            }
        }
        0
    }
}

// ── 8.3 + LFN helpers ────────────────────────────────────────────────────────

/// Return the packed 8.3 name (11 bytes) if `name` is a valid short name, else None.
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

/// Mangle a long name into a unique 8.3 `BASE~N.EXT` form.
fn mangle83(name: &str, n: u32) -> [u8; 11] {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let clean = |s: &str| -> Vec<u8> {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase() as u8)
            .collect()
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

/// How many 32-byte entries does this name cost (LFN entries + 1 short entry)?
fn entries_for_name(name: &str) -> usize {
    if short83(name).is_some() {
        1
    } else {
        let len = name.chars().count();
        let lfn = (len + 12) / 13; // 13 UTF-16 characters per LFN entry
        lfn + 1
    }
}

/// Build the directory entries (LFN + 8.3) for one child with an already-chosen
/// (unique) short name `short`.
fn dir_entries_for(name: &str, short: [u8; 11], attr: u8, first_cluster: u32, size: u32) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    // LFN needed when the name is not a valid 8.3, or when the short name was
    // mangled (e.g. on a collision) — then the real name must be preserved in an LFN.
    let need_lfn = short83(name).map(|s| s != short).unwrap_or(true);
    if need_lfn {
        // LFN entries (reverse order), with the checksum of the short name.
        let chksum = lfn_checksum(&short);
        let utf16: Vec<u16> = name.encode_utf16().collect();
        let groups = (utf16.len() + 12) / 13;
        for g in (0..groups).rev() {
            let order = (g as u8 + 1) | if g + 1 == groups { 0x40 } else { 0 };
            out.extend_from_slice(&lfn_entry(order, chksum, &utf16, g * 13));
        }
    }
    out.extend_from_slice(&sfn_entry(&short, attr, first_cluster, size));
    out
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &c in short.iter() {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c);
    }
    sum
}

fn lfn_entry(order: u8, chksum: u8, utf16: &[u16], start: usize) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0] = order;
    e[11] = ATTR_LFN;
    e[13] = chksum;
    // 13 characters spread over positions 1..11, 14..26, 28..32.
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

fn sfn_entry(short: &[u8; 11], attr: u8, first_cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(short);
    e[11] = attr;
    // Valid date 1980-01-01 (day 1, month 1, year 0) in creation/modify/access.
    const DATE_1980: u16 = (1 << 5) | 1;
    e[14..16].copy_from_slice(&0u16.to_le_bytes()); // creation time
    e[16..18].copy_from_slice(&DATE_1980.to_le_bytes()); // creation date
    e[18..20].copy_from_slice(&DATE_1980.to_le_bytes()); // last access
    e[22..24].copy_from_slice(&0u16.to_le_bytes()); // modify time
    e[24..26].copy_from_slice(&DATE_1980.to_le_bytes()); // modify date
    e[20..22].copy_from_slice(&((first_cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(first_cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

fn dot_entry(name: &[u8; 11], cluster: u32) -> [u8; 32] {
    let mut e = sfn_entry(&{ let mut a = [b' '; 11]; a.copy_from_slice(name); a }, ATTR_DIR, cluster, 0);
    e[0] = name[0];
    e
}

fn write_fsinfo(s: &mut [u8], free_count: u32, next_free: u32) {
    s[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    s[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    s[488..492].copy_from_slice(&free_count.to_le_bytes());
    s[492..496].copy_from_slice(&next_free.to_le_bytes());
    s[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
}

// ── Reader (for round-trip verification) ──────────────────────────────────────

/// Read a file from a FAT32 image at `path` (8.3 or LFN). None if not found.
pub fn read_file(img: &[u8], path: &str) -> Option<Vec<u8>> {
    let r = Reader::new(img)?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut cluster = r.root_cluster;
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let (cl, size, is_dir) = r.find_in_dir(cluster, part)?;
        if last {
            if is_dir {
                return None;
            }
            return Some(r.read_chain(cl, size as usize));
        }
        if !is_dir {
            return None;
        }
        cluster = cl;
    }
    None
}

struct Reader<'a> {
    img: &'a [u8],
    spf: u32,
    reserved: u32,
    num_fats: u32,
    data_start: u32,
    root_cluster: u32,
}

impl<'a> Reader<'a> {
    fn new(img: &'a [u8]) -> Option<Self> {
        if img.len() < SECTOR || img[510] != 0x55 || img[511] != 0xAA {
            return None;
        }
        let reserved = u16::from_le_bytes([img[14], img[15]]) as u32;
        let num_fats = img[16] as u32;
        let spf = u32::from_le_bytes([img[36], img[37], img[38], img[39]]);
        let root_cluster = u32::from_le_bytes([img[44], img[45], img[46], img[47]]);
        Some(Reader {
            img,
            spf,
            reserved,
            num_fats,
            data_start: reserved + num_fats * spf,
            root_cluster,
        })
    }
    fn fat_next(&self, cl: u32) -> u32 {
        let off = self.reserved as usize * SECTOR + cl as usize * 4;
        if off + 4 > self.img.len() {
            return EOC;
        }
        u32::from_le_bytes([self.img[off], self.img[off + 1], self.img[off + 2], self.img[off + 3]]) & 0x0FFF_FFFF
    }
    fn cluster_off(&self, cl: u32) -> usize {
        (self.data_start as usize + (cl as usize - 2) * SPC as usize) * SECTOR
    }
    fn read_chain(&self, start: u32, size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cl = start;
        while cl >= 2 && cl < EOC && out.len() < size + CLUSTER_BYTES {
            let off = self.cluster_off(cl);
            if off + CLUSTER_BYTES <= self.img.len() {
                out.extend_from_slice(&self.img[off..off + CLUSTER_BYTES]);
            }
            cl = self.fat_next(cl);
        }
        out.truncate(size);
        out
    }
    /// Collect the directory bytes (all clusters of the directory).
    fn dir_bytes(&self, start: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cl = start;
        while cl >= 2 && cl < EOC {
            let off = self.cluster_off(cl);
            if off + CLUSTER_BYTES <= self.img.len() {
                out.extend_from_slice(&self.img[off..off + CLUSTER_BYTES]);
            }
            cl = self.fat_next(cl);
        }
        out
    }
    /// Look up `name` (case-insensitive) in directory `dir_cluster`; return (cluster, size, is_dir).
    fn find_in_dir(&self, dir_cluster: u32, name: &str) -> Option<(u32, u32, bool)> {
        let bytes = self.dir_bytes(dir_cluster);
        let mut lfn = String::new();
        let mut e = 0;
        while e + 32 <= bytes.len() {
            let ent = &bytes[e..e + 32];
            e += 32;
            if ent[0] == 0x00 {
                break;
            }
            if ent[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if ent[11] == ATTR_LFN {
                // Prepend the LFN fragment.
                let frag = lfn_chars(ent);
                let mut s = frag;
                s.push_str(&lfn);
                lfn = s;
                continue;
            }
            // Short entry.
            let long = lfn.trim_end_matches('\u{0}');
            let short = sfn_name(ent);
            let matches = (!long.is_empty() && long.eq_ignore_ascii_case(name)) || short.eq_ignore_ascii_case(name);
            lfn.clear();
            if matches {
                let cl = ((u16::from_le_bytes([ent[20], ent[21]]) as u32) << 16)
                    | u16::from_le_bytes([ent[26], ent[27]]) as u32;
                let size = u32::from_le_bytes([ent[28], ent[29], ent[30], ent[31]]);
                let is_dir = ent[11] & ATTR_DIR != 0;
                return Some((cl, size, is_dir));
            }
        }
        None
    }
}

fn lfn_chars(ent: &[u8]) -> String {
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

    #[test]
    fn roundtrip_esp_layout() {
        // 48 MiB volume (valid FAT32: ≥ ~65525 clusters at 512 B/cluster).
        let sectors = 48 * 1024 * 1024 / SECTOR as u32;
        let mut fs = FatFs::new(sectors, 0x1234_5678, "EUROKERNEL");
        let loader = vec![0xABu8; 24 * 1024];
        let kernel_a: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let kernel_b: Vec<u8> = (0..200_000u32).map(|i| (i % 241) as u8).collect();
        fs.add_file("/EFI/BOOT/BOOTX64.EFI", &loader);
        fs.add_file("/EFI/BOOT/eurokernel-A.efi", &kernel_a);
        fs.add_file("/EFI/BOOT/eurokernel-B.efi", &kernel_b);
        let img = fs.build();
        assert_eq!(img.len(), sectors as usize * SECTOR);
        // BPB signature + FAT32 field.
        assert_eq!(&img[82..90], b"FAT32   ");
        assert_eq!(img[510], 0x55);
        assert_eq!(img[511], 0xAA);
        // Round-trip: read the three files back.
        assert_eq!(read_file(&img, "/EFI/BOOT/BOOTX64.EFI"), Some(loader));
        assert_eq!(read_file(&img, "/EFI/BOOT/eurokernel-A.efi"), Some(kernel_a));
        assert_eq!(read_file(&img, "/EFI/BOOT/eurokernel-B.efi"), Some(kernel_b));
        assert_eq!(read_file(&img, "/EFI/BOOT/ontbreekt.efi"), None);
    }

    #[test]
    fn short_and_long_names() {
        assert!(short83("BOOTX64.EFI").is_some());
        assert!(short83("EFI").is_some());
        assert!(short83("eurokernel-A.efi").is_none()); // lowercase letters + too long
        assert_eq!(entries_for_name("BOOTX64.EFI"), 1);
        assert_eq!(entries_for_name("eurokernel-A.efi"), 1 + 2); // 16 characters → 2 LFN
    }

    #[test]
    fn empty_file_has_no_cluster() {
        let sectors = 48 * 1024 * 1024 / SECTOR as u32;
        let mut fs = FatFs::new(sectors, 1, "EURO");
        fs.add_file("/leeg.txt", &[]);
        let img = fs.build();
        assert_eq!(read_file(&img, "/leeg.txt"), Some(Vec::new()));
    }
}
