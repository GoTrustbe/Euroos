//! EuroNTFS — a from-scratch **read-only NTFS** driver.
//!
//! NTFS is the most common non-FAT foreign filesystem (Windows disks, many USB
//! drives). EuroOS mounts it read-only so a user can get their data off such a
//! disk without a Windows machine. This crate parses the layers NTFS actually
//! uses: the boot sector → the **$MFT** (Master File Table) → **FILE records**
//! (with the update-sequence "fixup") → **attributes** ($FILE_NAME, $DATA) →
//! **runlists** (the compact non-resident cluster mapping). It reads both small
//! *resident* files (stored inline in the record) and large *non-resident* files
//! (spread across cluster runs).
//!
//! Verified against a real `mkntfs` image (`tests/`). Pure `no_std`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= (b[o + i] as u64) << (8 * i);
    }
    v
}

/// A mounted read-only NTFS volume.
pub struct Ntfs<'a> {
    img: &'a [u8],
    cluster_size: usize,
    record_size: usize,
    /// The whole Master File Table, reassembled from its runs.
    mft: Vec<u8>,
}

/// Decode an NTFS runlist → absolute `(lcn, cluster_count)` runs. `lcn == -1`
/// marks a sparse (unallocated) run.
fn decode_runlist(rl: &[u8]) -> Vec<(i64, u64)> {
    let mut runs = Vec::new();
    let mut i = 0usize;
    let mut base: i64 = 0;
    while i < rl.len() && rl[i] != 0 {
        let header = rl[i];
        i += 1;
        let len_bytes = (header & 0x0F) as usize;
        let off_bytes = (header >> 4) as usize;
        if len_bytes == 0 || i + len_bytes + off_bytes > rl.len() {
            break;
        }
        let mut length: u64 = 0;
        for b in 0..len_bytes {
            length |= (rl[i + b] as u64) << (8 * b);
        }
        i += len_bytes;
        if off_bytes == 0 {
            runs.push((-1, length)); // sparse
            continue;
        }
        let mut offset: i64 = 0;
        for b in 0..off_bytes {
            offset |= (rl[i + b] as i64) << (8 * b);
        }
        // Sign-extend the offset delta.
        let sign = 1i64 << (8 * off_bytes - 1);
        if offset & sign != 0 {
            offset -= 1i64 << (8 * off_bytes);
        }
        i += off_bytes;
        base += offset;
        runs.push((base, length));
    }
    runs
}

/// Apply the update-sequence-array fixup to a FILE/INDX record in place.
fn apply_fixup(rec: &mut [u8]) -> bool {
    if rec.len() < 8 || &rec[0..4] != b"FILE" {
        return false;
    }
    let usa_off = u16le(rec, 4) as usize;
    let usa_cnt = u16le(rec, 6) as usize;
    if usa_cnt == 0 || usa_off + usa_cnt * 2 > rec.len() {
        return false;
    }
    let usn = [rec[usa_off], rec[usa_off + 1]];
    for i in 1..usa_cnt {
        let sector_end = i * 512;
        if sector_end > rec.len() {
            break;
        }
        // The last two bytes of each 512-byte sector must equal the USN; restore
        // the original two bytes stored in the USA.
        if rec[sector_end - 2] != usn[0] || rec[sector_end - 1] != usn[1] {
            return false; // torn/corrupt record
        }
        rec[sector_end - 2] = rec[usa_off + 2 * i];
        rec[sector_end - 1] = rec[usa_off + 2 * i + 1];
    }
    true
}

impl<'a> Ntfs<'a> {
    /// Mount the volume from a raw image. `None` if it is not NTFS.
    pub fn open(img: &'a [u8]) -> Option<Ntfs<'a>> {
        if img.len() < 512 || &img[3..11] != b"NTFS    " {
            return None;
        }
        let bytes_per_sector = u16le(img, 0x0B) as usize;
        let sectors_per_cluster = img[0x0D] as usize;
        let cluster_size = bytes_per_sector.checked_mul(sectors_per_cluster)?;
        if cluster_size == 0 {
            return None;
        }
        let mft_lcn = u64le(img, 0x30) as usize;
        let cpr = img[0x40] as i8;
        let record_size = if cpr > 0 { (cpr as usize) * cluster_size } else { 1usize << ((-cpr) as u32) };
        if record_size == 0 || record_size > 1 << 20 {
            return None;
        }

        // Bootstrap: read MFT record 0 (at the MFT's first cluster) to learn the
        // MFT's own $DATA runlist, then reassemble the whole table.
        let mft0_off = mft_lcn.checked_mul(cluster_size)?;
        let mut rec0 = img.get(mft0_off..mft0_off + record_size)?.to_vec();
        if !apply_fixup(&mut rec0) {
            return None;
        }
        let mut me = Ntfs { img, cluster_size, record_size, mft: Vec::new() };
        let (runs, real_size) = me.data_runs(&rec0)?;
        me.mft = me.read_runs(&runs, real_size);
        Some(me)
    }

    fn read_runs(&self, runs: &[(i64, u64)], real_size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        for &(lcn, count) in runs {
            let bytes = (count as usize) * self.cluster_size;
            if lcn < 0 {
                out.resize(out.len() + bytes, 0); // sparse → zeros
            } else {
                let off = (lcn as usize) * self.cluster_size;
                match self.img.get(off..off + bytes) {
                    Some(s) => out.extend_from_slice(s),
                    None => break,
                }
            }
        }
        out.truncate(real_size as usize);
        out
    }

    /// Find attribute of `atype` in a record; returns the attribute slice.
    fn find_attr<'r>(&self, rec: &'r [u8], atype: u32) -> Option<&'r [u8]> {
        let mut p = u16le(rec, 0x14) as usize; // first attribute offset
        while p + 8 <= rec.len() {
            let t = u32le(rec, p);
            if t == ATTR_END {
                break;
            }
            let len = u32le(rec, p + 4) as usize;
            if len == 0 || p + len > rec.len() {
                break;
            }
            if t == atype {
                return Some(&rec[p..p + len]);
            }
            p += len;
        }
        None
    }

    /// The (runs, real_size) of a $DATA/$MFT attribute, resident or not.
    fn data_runs(&self, rec: &[u8]) -> Option<(Vec<(i64, u64)>, u64)> {
        let a = self.find_attr(rec, ATTR_DATA)?;
        if a[8] == 0 {
            return None; // resident (handled separately)
        }
        let real_size = u64le(a, 0x30);
        let rl_off = u16le(a, 0x20) as usize;
        Some((decode_runlist(a.get(rl_off..)?), real_size))
    }

    fn record(&self, n: u64) -> Option<Vec<u8>> {
        let off = (n as usize).checked_mul(self.record_size)?;
        let mut rec = self.mft.get(off..off + self.record_size)?.to_vec();
        if apply_fixup(&mut rec) && &rec[0..4] == b"FILE" {
            Some(rec)
        } else {
            None
        }
    }

    fn is_in_use(rec: &[u8]) -> bool {
        u16le(rec, 0x16) & 0x0001 != 0 // flags bit 0 = record in use
    }
    fn is_dir(rec: &[u8]) -> bool {
        u16le(rec, 0x16) & 0x0002 != 0 // flags bit 1 = directory
    }

    /// The best (non-DOS-short) file name of a record.
    fn name_of(&self, rec: &[u8]) -> Option<String> {
        let a = self.find_attr(rec, ATTR_FILE_NAME)?;
        if a[8] != 0 {
            return None; // $FILE_NAME is always resident
        }
        let voff = u16le(a, 0x14) as usize;
        let val = &a[voff..];
        let name_len = val[0x40] as usize;
        let namespace = val[0x41];
        if namespace == 2 {
            return None; // skip pure DOS 8.3 short names
        }
        let mut s = String::new();
        for i in 0..name_len {
            let c = u16le(val, 0x42 + i * 2);
            s.push(char::from_u32(c as u32).unwrap_or('\u{FFFD}'));
        }
        Some(s)
    }

    /// Read the $DATA of a record (resident inline, or non-resident via runlist).
    fn read_record_data(&self, rec: &[u8]) -> Option<Vec<u8>> {
        let a = self.find_attr(rec, ATTR_DATA)?;
        if a[8] == 0 {
            // resident: value at value_offset, length value_length.
            let vlen = u32le(a, 0x10) as usize;
            let voff = u16le(a, 0x14) as usize;
            return a.get(voff..voff + vlen).map(|s| s.to_vec());
        }
        let real_size = u64le(a, 0x30);
        let rl_off = u16le(a, 0x20) as usize;
        let runs = decode_runlist(a.get(rl_off..)?);
        Some(self.read_runs(&runs, real_size))
    }

    /// List the file names in the volume (scanning the MFT; non-directories).
    pub fn list_files(&self) -> Vec<String> {
        let count = self.mft.len() / self.record_size;
        let mut out = Vec::new();
        for n in 16..count as u64 {
            // records < 16 are NTFS metafiles ($MFT, $Bitmap, …)
            if let Some(rec) = self.record(n) {
                if Self::is_in_use(&rec) && !Self::is_dir(&rec) {
                    if let Some(name) = self.name_of(&rec) {
                        out.push(name);
                    }
                }
            }
        }
        out
    }

    /// Read a file by name (basename; a leading `/` is accepted). `None` if not
    /// found. Reads via a linear MFT scan (no index-tree walk needed).
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let want = path.trim_start_matches('/');
        let count = self.mft.len() / self.record_size;
        for n in 16..count as u64 {
            if let Some(rec) = self.record(n) {
                if Self::is_in_use(&rec) && !Self::is_dir(&rec) {
                    if let Some(name) = self.name_of(&rec) {
                        if name.eq_ignore_ascii_case(want) {
                            return self.read_record_data(&rec);
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static IMG: &[u8] = include_bytes!("../tests/ntfs.img");

    #[test]
    fn mounts_real_mkntfs_image() {
        assert!(Ntfs::open(IMG).is_some());
        // A FAT image is not NTFS.
        assert!(Ntfs::open(&[0u8; 4096]).is_none());
    }

    #[test]
    fn reads_a_known_file_verbatim() {
        let fs = Ntfs::open(IMG).unwrap();
        let data = fs.read_file("hello.txt").expect("hello.txt");
        assert_eq!(data, b"Hello from EuroNTFS! sovereign read.\n");
        // Path form with a leading slash works too.
        assert_eq!(fs.read_file("/hello.txt").unwrap(), data);
        // A missing file returns None.
        assert!(fs.read_file("nope.txt").is_none());
    }

    #[test]
    fn lists_the_user_file() {
        let fs = Ntfs::open(IMG).unwrap();
        let files = fs.list_files();
        assert!(files.iter().any(|f| f.eq_ignore_ascii_case("hello.txt")), "got {files:?}");
    }

    #[test]
    fn runlist_decoding_signed_deltas() {
        // 0x21 0x18 0x00 0x01 = length 0x18, offset +0x0100; then 0x00 terminator.
        let runs = decode_runlist(&[0x21, 0x18, 0x00, 0x01, 0x00]);
        assert_eq!(runs, alloc::vec![(0x100, 0x18)]);
    }
}
