//! Kernel side of **EuroExt** (Sprint IO-7): mount a Linux ext2/3/4 disk into the VFS
//! (read-only). The `[io7]` self-test mounts an ext4 disk presented by the harness and
//! reads a file — real ext4 (made by `mkfs.ext4`), not a mock.

use crate::fatmount::SectorDev;
use eurofs::FileSystem;

/// Does virtio device `dev` hold an ext2/3/4 volume? (superblock magic 0xEF53 at the
/// fixed byte offset 1024 → sector 2, field offset 56).
pub fn is_ext(dev: usize) -> bool {
    let mut s = [0u8; 512];
    // Superblock starts at byte 1024 = sector 2; s_magic is at offset 56 within it.
    crate::virtio_blk::read_io_dev(dev, 2, &mut s) && u16::from_le_bytes([s[56], s[57]]) == 0xEF53
}

/// **[io7]** — mount the ext4 disk on virtio device 0 (placed there by the harness),
/// read a file and list the root: real ext2/3/4 read in the kernel.
pub fn selftest() {
    let total = crate::virtio_blk::capacity_sectors_dev(0);
    let fs = match euroext::ExtFs::mount(SectorDev::new(0, 0, total)) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[io7] ext mount of disk0 FAILED ✗");
            return;
        }
    };
    let readme = fs.read_file("/readme.txt").ok();
    let entries = fs.list_dir("/").map(|e| e.len()).unwrap_or(0);
    let big = fs.read_file("/big.dat").map(|d| d.len()).unwrap_or(0);
    let read_ok = readme.as_deref().map(|d| d.starts_with(b"hello from a real ext4 volume")).unwrap_or(false);
    crate::serial_println!(
        "[io7] ext2/3/4 read driver: mounted disk0, {entries} root entries, readme.txt {} B + big.dat {big} B (extents) → {}",
        readme.as_ref().map(|d| d.len()).unwrap_or(0),
        if read_ok && entries > 0 && big > 0 { "OK ✓" } else { "FAILED ✗" }
    );
}
