//! EuroFSID — **filesystem identification** from a volume's superblock.
//!
//! Before EuroOS can mount a foreign disk it must recognise what it is. This
//! reads the on-disk magic (and label/UUID where cheap) to classify a volume as
//! btrfs, XFS, NTFS, exFAT, FAT32 or ext — the auto-detect step the VFS mount
//! path uses. It is *identification*, not a full driver: NTFS is read
//! ([`eurontfs`]) and FAT/exFAT/ext are read+; **btrfs and XFS are recognised
//! here but full file read is a separate, large effort** (their B-tree metadata
//! is deliberately out of scope for now — the honest boundary). Pure `no_std`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Btrfs,
    Xfs,
    Ntfs,
    ExFat,
    Fat32,
    Ext,
    Unknown,
}

impl FsKind {
    pub fn name(self) -> &'static str {
        match self {
            FsKind::Btrfs => "btrfs",
            FsKind::Xfs => "xfs",
            FsKind::Ntfs => "ntfs",
            FsKind::ExFat => "exfat",
            FsKind::Fat32 => "fat32",
            FsKind::Ext => "ext",
            FsKind::Unknown => "unknown",
        }
    }
    /// Whether EuroOS can read file contents from this filesystem today.
    pub fn readable(self) -> bool {
        matches!(self, FsKind::Ntfs | FsKind::ExFat | FsKind::Fat32 | FsKind::Ext)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsInfo {
    pub kind: FsKind,
    pub label: String,
    pub uuid: [u8; 16],
}

fn trimmed(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().into()
}

/// Identify a filesystem from the start of its volume (needs the first ~68 KiB
/// so the btrfs superblock at 0x10000 is visible; smaller inputs still detect
/// the front-of-volume filesystems).
pub fn identify(img: &[u8]) -> FsInfo {
    let mut info = FsInfo { kind: FsKind::Unknown, label: String::new(), uuid: [0u8; 16] };
    let at = |o: usize, n: usize| img.get(o..o + n);

    // NTFS / exFAT / FAT: OEM name in the boot sector.
    if let Some(oem) = at(3, 8) {
        if oem == b"NTFS    " {
            info.kind = FsKind::Ntfs;
            return info;
        }
        if oem == b"EXFAT   " {
            info.kind = FsKind::ExFat;
            return info;
        }
    }
    if let Some(t) = at(0x52, 5) {
        if t == b"FAT32" {
            info.kind = FsKind::Fat32;
            if let Some(l) = at(0x47, 11) {
                info.label = trimmed(l);
            }
            return info;
        }
    }

    // XFS: "XFSB" at offset 0 (big-endian volume). Label at 0x6c, UUID at 0x20.
    if at(0, 4) == Some(b"XFSB") {
        info.kind = FsKind::Xfs;
        if let Some(l) = at(0x6c, 12) {
            info.label = trimmed(l);
        }
        if let Some(u) = at(0x20, 16) {
            info.uuid.copy_from_slice(u);
        }
        return info;
    }

    // btrfs: superblock at 0x10000, magic "_BHRfS_M" at +0x40, fsid +0x20, label +0x12b.
    if at(0x10040, 8) == Some(b"_BHRfS_M") {
        info.kind = FsKind::Btrfs;
        if let Some(u) = at(0x10020, 16) {
            info.uuid.copy_from_slice(u);
        }
        if let Some(l) = at(0x1012b, 256) {
            info.label = trimmed(l);
        }
        return info;
    }

    // ext2/3/4: magic 0xEF53 at superblock offset 0x38 (i.e. file offset 0x438).
    if let Some(m) = at(0x438, 2) {
        if m == [0x53, 0xEF] {
            info.kind = FsKind::Ext;
            if let Some(l) = at(0x478, 16) {
                info.label = trimmed(l);
            }
            return info;
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_real_btrfs() {
        let img = include_bytes!("../tests/btrfs_sb.bin");
        let i = identify(img);
        assert_eq!(i.kind, FsKind::Btrfs);
        assert_eq!(i.label, "EUROBTR");
        assert!(!i.kind.readable()); // recognised, not yet readable (honest)
    }

    #[test]
    fn identifies_real_xfs() {
        let img = include_bytes!("../tests/xfs_sb.bin");
        let i = identify(img);
        assert_eq!(i.kind, FsKind::Xfs);
        assert_eq!(i.label, "EUROXFS");
    }

    #[test]
    fn identifies_real_ntfs() {
        let img = include_bytes!("../tests/ntfs_sb.bin");
        let i = identify(img);
        assert_eq!(i.kind, FsKind::Ntfs);
        assert!(i.kind.readable()); // NTFS is read by eurontfs
    }

    #[test]
    fn unknown_is_unknown() {
        assert_eq!(identify(&[0u8; 4096]).kind, FsKind::Unknown);
    }
}
