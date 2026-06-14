//! **Mount framework** (Sprint IO-1/IO-2): attach foreign FAT32 volumes into the VFS.
//!
//! `SectorDev` exposes a virtio-blk device (optionally a partition window) as a
//! 512-byte [`eurofs::BlockDevice`], which `eurofatfs::FatFs` mounts as a real
//! filesystem. The shell `mount`/`umount`/`lsblk` commands drive it. The `[io1]`/`[io2]`
//! boot self-tests prove the FAT32 read + write driver works in the kernel (no_std).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use eurofs::{BlockDevice, BlockError, BlockResult, FileSystem};

/// A 512-byte block device over a virtio-blk device, optionally windowed to a partition
/// (`start` = first absolute sector, `count` = number of sectors).
pub struct SectorDev {
    dev: usize,
    start: u64,
    count: u64,
}

impl SectorDev {
    pub fn new(dev: usize, start: u64, count: u64) -> Self {
        SectorDev { dev, start, count }
    }
}

impl BlockDevice for SectorDev {
    fn block_size(&self) -> u32 {
        512
    }
    fn block_count(&self) -> u64 {
        self.count
    }
    fn read_blocks(&self, start_block: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        if buf.len() != count as usize * 512 {
            return Err(BlockError::NotAligned);
        }
        for i in 0..count as u64 {
            if start_block + i >= self.count {
                return Err(BlockError::OutOfBounds);
            }
            let mut sec = [0u8; 512];
            if !crate::virtio_blk::read_io_dev(self.dev, self.start + start_block + i, &mut sec) {
                return Err(BlockError::IoError);
            }
            buf[i as usize * 512..][..512].copy_from_slice(&sec);
        }
        Ok(())
    }
    fn write_blocks(&mut self, start_block: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        if buf.len() != count as usize * 512 {
            return Err(BlockError::NotAligned);
        }
        for i in 0..count as u64 {
            if start_block + i >= self.count {
                return Err(BlockError::OutOfBounds);
            }
            let sec = &buf[i as usize * 512..][..512];
            if !crate::virtio_blk::write_io_dev(self.dev, self.start + start_block + i, sec) {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> BlockResult<()> {
        crate::virtio_blk::flush_dev(self.dev);
        Ok(())
    }
}

/// Is sector `s0` an exFAT boot sector?
fn is_exfat(s0: &[u8]) -> bool {
    s0.len() >= 512 && &s0[3..11] == b"EXFAT   " && s0[510] == 0x55 && s0[511] == 0xAA
}

/// Is sector `s0` a FAT (BPB) boot sector with 512-byte sectors?
fn is_fat_bpb(s0: &[u8]) -> bool {
    if is_exfat(s0) {
        return false; // exFAT shares the 0x55AA signature but is a different format
    }
    s0.len() >= 512
        && s0[510] == 0x55
        && s0[511] == 0xAA
        && u16::from_le_bytes([s0[11], s0[12]]) == 512
        && (&s0[82..87] == b"FAT32" || &s0[54..59] == b"FAT16" || &s0[54..62] == b"FAT12   ")
}

/// Locate a FAT volume on virtio device `dev`: either the whole device (LBA 0 is a BPB)
/// or the first FAT-typed/looking GPT partition. Returns (start_sector, sector_count).
fn find_fat_volume(dev: usize) -> Option<(u64, u64)> {
    if !crate::virtio_blk::present_dev(dev) {
        return None;
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let mut s0 = [0u8; 512];
    if crate::virtio_blk::read_io_dev(dev, 0, &mut s0) && is_fat_bpb(&s0) {
        return Some((0, total)); // bare FAT volume (no partition table)
    }
    // GPT? scan partitions for one whose first sector is a FAT BPB.
    for (first, last) in crate::gpt::all_partitions_on(dev) {
        let mut p0 = [0u8; 512];
        if crate::virtio_blk::read_io_dev(dev, first, &mut p0) && is_fat_bpb(&p0) {
            return Some((first, last.saturating_sub(first) + 1));
        }
    }
    None
}

/// Locate an exFAT volume on virtio device `dev` (whole-disk or first exFAT partition).
fn find_exfat_volume(dev: usize) -> Option<(u64, u64)> {
    if !crate::virtio_blk::present_dev(dev) {
        return None;
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let mut s0 = [0u8; 512];
    if crate::virtio_blk::read_io_dev(dev, 0, &mut s0) && is_exfat(&s0) {
        return Some((0, total));
    }
    for (first, last) in crate::gpt::all_partitions_on(dev) {
        let mut p0 = [0u8; 512];
        if crate::virtio_blk::read_io_dev(dev, first, &mut p0) && is_exfat(&p0) {
            return Some((first, last.saturating_sub(first) + 1));
        }
    }
    None
}

/// `lsblk` — list virtio block devices + detected filesystem.
pub fn lsblk() -> Vec<String> {
    let mut out = Vec::new();
    out.push("DEVICE   SIZE       TYPE".to_string());
    let n = crate::virtio_blk::device_count();
    if n == 0 {
        out.push("(no virtio-blk devices — running in live/RAM mode)".to_string());
        return out;
    }
    for dev in 0..n {
        if !crate::virtio_blk::present_dev(dev) {
            continue;
        }
        let total = crate::virtio_blk::capacity_sectors_dev(dev);
        let mib = total * 512 / (1024 * 1024);
        let mut s0 = [0u8; 512];
        let typ = if crate::virtio_blk::read_io_dev(dev, 0, &mut s0) {
            if is_exfat(&s0) {
                "exFAT"
            } else if is_fat_bpb(&s0) {
                "FAT"
            } else if crate::extmount::is_ext(dev) {
                "ext2/3/4"
            } else if s0[510] == 0x55 && s0[511] == 0xAA && s0[450] == 0xEE {
                "GPT (EuroOS or partitioned)"
            } else {
                "unknown/blank"
            }
        } else {
            "unreadable"
        };
        out.push(format!("vblk{dev:<4} {mib:>6} MiB  {typ}"));
        if find_fat_volume(dev).is_some() {
            out.push(format!("  └─ FAT volume present — mount with: mount {dev} /mnt/fat"));
        }
    }
    out
}

/// `mount <devN> <point>` — find a FAT (or EuroFS) volume on virtio device N and mount
/// it into the VFS.
pub fn mount_cmd(fs: &mut dyn FileSystem, dev_arg: &str, point: &str) -> Vec<String> {
    if dev_arg.is_empty() || point.is_empty() {
        return alloc::vec!["usage: mount <devN> <mountpoint>   (e.g. mount 1 /mnt/usb)".to_string()];
    }
    let dev: usize = match dev_arg.trim_start_matches("vblk").parse() {
        Ok(d) => d,
        Err(_) => return alloc::vec![format!("mount: invalid device '{dev_arg}'")],
    };
    if !crate::virtio_blk::present_dev(dev) {
        return alloc::vec![format!("mount: no such device {dev}")];
    }
    // 1) A FAT volume (whole-disk or a partition)?
    if let Some((start, count)) = find_fat_volume(dev) {
        return match eurofatfs::FatFs::mount(SectorDev::new(dev, start, count)) {
            Ok(fatfs) => match fs.mount_fs(point, Box::new(fatfs)) {
                Ok(()) => alloc::vec![format!("mounted FAT (device {dev}, LBA {start}) at {point}")],
                Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
            },
            Err(_) => alloc::vec![format!("mount: device {dev} FAT volume is invalid")],
        };
    }
    // 2) An exFAT volume (whole-disk or a partition)?
    if let Some((start, count)) = find_exfat_volume(dev) {
        return match euroexfat::ExFat::mount(SectorDev::new(dev, start, count)) {
            Ok(exfs) => match fs.mount_fs(point, Box::new(exfs)) {
                Ok(()) => alloc::vec![format!("mounted exFAT (device {dev}, LBA {start}) at {point} [read-only]")],
                Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
            },
            Err(_) => alloc::vec![format!("mount: device {dev} exFAT volume is invalid")],
        };
    }
    // 2.5) A Linux ext2/3/4 volume (read-only)?
    if crate::extmount::is_ext(dev) {
        let total = crate::virtio_blk::capacity_sectors_dev(dev);
        return match euroext::ExtFs::mount(SectorDev::new(dev, 0, total)) {
            Ok(efs) => match fs.mount_fs(point, Box::new(efs)) {
                Ok(()) => alloc::vec![format!("mounted ext2/3/4 (device {dev}) at {point} [read-only]")],
                Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
            },
            Err(_) => alloc::vec![format!("mount: device {dev} ext volume is invalid")],
        };
    }
    // 3) A native EuroFS volume (whole-disk)?
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let pdev = crate::rootblk::RootBlk::disk_on(dev, 0, total / 8);
    match eurofs::EuroFs::mount(pdev, crate::rtc::epoch()) {
        Ok(efs) => match fs.mount_fs(point, Box::new(efs)) {
            Ok(()) => alloc::vec![format!("mounted EuroFS (device {dev}) at {point}")],
            Err(_) => alloc::vec!["mount: the root filesystem is not a VFS (cannot mount here)".to_string()],
        },
        Err(_) => alloc::vec![format!("mount: no FAT or EuroFS volume found on device {dev}")],
    }
}

/// `format <devN> [--fs fat32|eurofs] [--label L] [--force]` — make a fresh, empty
/// filesystem on a whole virtio device. Refuses a device that already holds a
/// recognizable filesystem unless `--force` (this ERASES it).
pub fn format_cmd(dev_arg: &str, rest: &str) -> Vec<String> {
    if dev_arg.is_empty() {
        return alloc::vec!["usage: format <devN> [--fs fat32|eurofs] [--label L] [--force]".to_string()];
    }
    let dev: usize = match dev_arg.trim_start_matches("vblk").parse() {
        Ok(d) => d,
        Err(_) => return alloc::vec![format!("format: invalid device '{dev_arg}'")],
    };
    if !crate::virtio_blk::present_dev(dev) {
        return alloc::vec![format!("format: no such device {dev}")];
    }
    // Parse options.
    let mut kind = "fat32";
    let mut label = String::from("EURODATA");
    let mut force = false;
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            "--fs" => {
                if let Some(v) = toks.get(i + 1) {
                    kind = v;
                    i += 1;
                }
            }
            "--label" => {
                if let Some(v) = toks.get(i + 1) {
                    label = v.to_string();
                    i += 1;
                }
            }
            "--force" => force = true,
            _ => {}
        }
        i += 1;
    }
    // Safety: refuse to erase a device that already has a filesystem unless --force.
    let mut s0 = [0u8; 512];
    let readable = crate::virtio_blk::read_io_dev(dev, 0, &mut s0);
    let used = readable
        && (is_fat_bpb(&s0)
            || (s0[510] == 0x55 && s0[511] == 0xAA)
            || s0[..16].iter().any(|&b| b != 0));
    if used && !force {
        return alloc::vec![format!(
            "format: device {dev} appears to contain data — refusing. Re-run with --force to ERASE it."
        )];
    }
    let total = crate::virtio_blk::capacity_sectors_dev(dev);
    let vid = (crate::rtc::epoch() as u32) ^ 0xFA32_0000;
    match kind {
        "fat32" | "fat" | "vfat" => {
            if total > u32::MAX as u64 {
                return alloc::vec!["format: device too large for FAT32 (>2 TiB)".to_string()];
            }
            eurofat::format_fat32(total as u32, vid, &label, |lba, bytes| {
                let _ = crate::virtio_blk::write_io_dev(dev, lba, bytes);
            });
            crate::virtio_blk::flush_dev(dev);
            // Verify by mounting it back.
            let ok = find_fat_volume(dev).is_some();
            alloc::vec![format!(
                "format: device {dev} ({} MiB) formatted FAT32, label '{label}' → {}",
                total * 512 / 1024 / 1024,
                if ok { "OK (mountable)" } else { "written (verify failed)" }
            )]
        }
        "eurofs" | "euro" => {
            let pdev = crate::rootblk::RootBlk::disk_on(dev, 0, total / 8);
            match eurofs::EuroFs::format(pdev, [vid as u8; 16], crate::rtc::epoch()) {
                Ok(_) => {
                    crate::virtio_blk::flush_dev(dev);
                    alloc::vec![format!(
                        "format: device {dev} ({} MiB) formatted EuroFS, label '{label}' → OK",
                        total * 512 / 1024 / 1024
                    )]
                }
                Err(_) => alloc::vec![format!("format: EuroFS format of device {dev} FAILED")],
            }
        }
        other => alloc::vec![format!("format: unknown filesystem '{other}' (use fat32 or eurofs)")],
    }
}

/// `umount <point>`.
pub fn umount_cmd(fs: &mut dyn FileSystem, point: &str) -> Vec<String> {
    if point.is_empty() {
        return alloc::vec!["usage: umount <mountpoint>".to_string()];
    }
    match fs.umount_fs(point) {
        Ok(()) => alloc::vec![format!("unmounted {point}")],
        Err(_) => alloc::vec![format!("umount: nothing mounted at {point}")],
    }
}

/// **[io1] + [io2]** — prove the FAT32 read + write driver in the kernel (no_std):
/// build a FAT32 image in RAM, mount it, read a seeded file (io1), then write a new file,
/// create a directory, overwrite + remove, and verify every change reads back (io2).
pub fn selftest() {
    use eurofs::MemoryBlockDevice;
    // A small (4 MiB) FAT image built by the eurofat builder, in RAM.
    let sectors = 8192u32; // 4 MiB @ 512 B
    let mut fb = eurofat::FatFs::new(sectors, 0x10_2030, "EUROIO");
    fb.add_file("/readme.txt", b"mounted FAT32 in the EuroOS kernel");
    fb.add_file("/dir/seed.bin", &[0xABu8; 6000]); // multi-cluster, in a subdir
    let img = fb.build();
    let mut dev = MemoryBlockDevice::new(sectors as u64, 512);
    if dev.write_blocks(0, sectors, &img).is_err() {
        crate::serial_println!("[io1] FAT self-test: could not stage RAM image");
        return;
    }

    let mut fatfs = match eurofatfs::FatFs::mount(dev) {
        Ok(f) => f,
        Err(_) => {
            crate::serial_println!("[io1] FAT self-test: mount FAILED");
            return;
        }
    };

    // [io1] READ: seeded file + nested multi-cluster file + directory listing.
    let r1 = fatfs.read_file("/readme.txt").ok();
    let r2 = fatfs.read_file("/dir/seed.bin").ok();
    let listed = fatfs.list_dir("/").map(|e| e.iter().any(|x| x.name == "dir")).unwrap_or(false);
    let read_ok = r1.as_deref() == Some(b"mounted FAT32 in the EuroOS kernel")
        && r2.as_deref() == Some(&[0xABu8; 6000][..])
        && listed;
    crate::serial_println!(
        "[io1] FAT32 mount + read (kernel, no_std): readme={} seed-{}B={} ls-root={} → {}",
        r1.is_some(),
        r2.as_ref().map(|d| d.len()).unwrap_or(0),
        r2.as_deref() == Some(&[0xABu8; 6000][..]),
        listed,
        if read_ok { "OK ✓" } else { "FAILED ✗" }
    );

    // [io2] WRITE: create a file + a dir + nested file, overwrite (grow), remove.
    let w_create = fatfs.write_file("/euro.txt", b"written by the kernel FAT driver").is_ok();
    let w_mkdir = fatfs.create_dir("/eurodir").is_ok();
    let w_nested = fatfs.write_file("/eurodir/note.txt", b"nested write").is_ok();
    let big: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    let w_grow = fatfs.write_file("/euro.txt", &big).is_ok(); // overwrite small → large
    let read_back = fatfs.read_file("/euro.txt").map(|d| d == big).unwrap_or(false);
    let nested_ok = fatfs.read_file("/eurodir/note.txt").as_deref() == Ok(b"nested write");
    let w_rm = fatfs.remove_file("/euro.txt").is_ok();
    let gone = !fatfs.exists("/euro.txt");
    let write_ok = w_create && w_mkdir && w_nested && w_grow && read_back && nested_ok && w_rm && gone;
    crate::serial_println!(
        "[io2] FAT32 write (kernel): create={w_create} mkdir={w_mkdir} nested={w_nested} grow+readback={read_back} nested-read={nested_ok} remove={w_rm}/gone={gone} → {}",
        if write_ok { "OK ✓" } else { "FAILED ✗" }
    );

    // [io3] FORMAT: format a blank RAM volume with the streaming formatter, then mount +
    // use it — proves `format` produces a mountable FAT32 in the kernel.
    let fsectors = 16384u32; // 8 MiB blank volume
    let mut blank = MemoryBlockDevice::new(fsectors as u64, 512);
    eurofat::format_fat32(fsectors, 0x5050_5050, "EUROFMT", |lba, bytes| {
        let n = (bytes.len() / 512).max(1) as u32;
        let _ = blank.write_blocks(lba, n, bytes);
    });
    let fmt_ok = match eurofatfs::FatFs::mount(blank) {
        Ok(mut ffs) => {
            let empty = ffs.list_dir("/").map(|e| e.is_empty()).unwrap_or(false);
            let w = ffs.write_file("/fresh.txt", b"formatted in-kernel").is_ok();
            let r = ffs.read_file("/fresh.txt").as_deref() == Ok(b"formatted in-kernel");
            empty && w && r
        }
        Err(_) => false,
    };
    crate::serial_println!(
        "[io3] FAT32 format (kernel): blank volume formatted → mount + write + read back → {}",
        if fmt_ok { "OK ✓" } else { "FAILED ✗" }
    );
}
