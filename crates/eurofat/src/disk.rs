//! Complete **bootable disk image**: a GPT with an EFI System Partition
//! (FAT32, via [`crate::FatFs`]) + a EuroFS root partition. The exact same bytes
//! are written by the installer to a real virtio-blk disk and validated on the
//! host (QEMU boot/`gdisk`/`fsck`). Pure `no_std` logic.
//!
//! Kernel-friendly: [`write_boot_disk`] **streams** via a callback (≤ 4 KiB
//! per chunk) so the kernel never has to hold the whole disk in RAM — only
//! the ESP (≈ 40 MiB) briefly exists as a single buffer.

use alloc::vec;
use alloc::vec::Vec;

use crate::FatFs;

const SECTOR: usize = 512;
const ESP_FIRST_LBA: u64 = 2048; // 1 MiB alignment
const ENTRY_LBA: u64 = 2;
const NUM_ENTRIES: u32 = 128;
const ENTRY_SIZE: u32 = 128;
// The ESP carries the loader + BOTH kernel slot files (~53 MB each today); 40 MiB
// silently overflowed once the kernel grew past 20 MB. 256 MiB leaves room for growth.
const ESP_MIN_BYTES: u64 = 256 * 1024 * 1024;
/// A/B slot partitions: an over-the-air update writes the new kernel here
/// (header sector + image, see `kernel/src/update.rs`), the loader boots it from
/// here. Sized for the updater's 96 MiB image cap.
pub const SLOT_BYTES: u64 = 96 * 1024 * 1024;
/// GPT partition type of the two slot partitions (distinct from EuroFS, so
/// root-filesystem detection never mistakes a slot for the root).
pub const EUROSLOT_TYPE: [u8; 16] = [
    0x45, 0x55, 0x52, 0x4f, 0x53, 0x4c, 0x00, 0x01, 0x80, 0x00, 0x00, 0x45, 0x55, 0x52, 0x4f, 0x53,
];
const CHUNK: usize = 4096; // virtio-blk DATA_MAX (8 sectors)

/// Type GUID of an EFI System Partition (C12A7328-F81F-11D2-BA4B-00A0C93EC93B),
/// in GPT byte order (first 3 fields little-endian, last 2 big-endian).
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];

/// Own EuroFS partition type — MUST equal `kernel::gpt::EUROFS_TYPE`,
/// otherwise the kernel will not find the root partition (`find_eurofs_partition`).
const EUROFS_TYPE: [u8; 16] = [
    0x45, 0x55, 0x52, 0x4f, 0x46, 0x53, 0x00, 0x01, 0x80, 0x00, 0x00, 0x45, 0x55, 0x52, 0x4f, 0x53,
];

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn align8(x: u64) -> u64 {
    (x + 7) & !7
}

/// The partitions in the assembled disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub esp_first: u64,
    pub esp_sectors: u64,
    pub slot_a_first: u64,
    pub slot_b_first: u64,
    pub slot_sectors: u64,
    pub eurofs_first: u64,
    pub eurofs_sectors: u64,
    pub backup_lba: u64,
}

/// Compute the partition layout for a disk of `total_sectors`.
pub fn layout_for(total_sectors: u64) -> Layout {
    let last_usable = total_sectors.saturating_sub(34);
    let esp_sectors = align8(ESP_MIN_BYTES / SECTOR as u64);
    let esp_first = ESP_FIRST_LBA;
    let esp_last = esp_first + esp_sectors - 1;
    let slot_sectors = align8(SLOT_BYTES / SECTOR as u64);
    let slot_a_first = align8(esp_last + 1);
    let slot_b_first = slot_a_first + slot_sectors;
    let fs_first = slot_b_first + slot_sectors;
    // A disk too small for the slots gets an empty root range instead of an
    // arithmetic underflow; the installer refuses such a disk up front.
    let fs_last = last_usable.saturating_sub(1).max(fs_first);
    Layout {
        esp_first,
        esp_sectors,
        slot_a_first,
        slot_b_first,
        slot_sectors,
        eurofs_first: fs_first,
        eurofs_sectors: (fs_last + 1).saturating_sub(fs_first),
        backup_lba: total_sectors - 1,
    }
}

fn part_array(l: &Layout) -> Vec<u8> {
    let mut arr = vec![0u8; (NUM_ENTRIES * ENTRY_SIZE) as usize];
    fill_entry(&mut arr, 0, &ESP_TYPE, l.esp_first, l.esp_first + l.esp_sectors - 1, "EFI System Partition", 0x10);
    fill_entry(&mut arr, 1, &EUROFS_TYPE, l.eurofs_first, l.eurofs_first + l.eurofs_sectors - 1, "EuroOS-A", 0x30);
    fill_entry(&mut arr, 2, &EUROSLOT_TYPE, l.slot_a_first, l.slot_a_first + l.slot_sectors - 1, "EuroSlot-A", 0x50);
    fill_entry(&mut arr, 3, &EUROSLOT_TYPE, l.slot_b_first, l.slot_b_first + l.slot_sectors - 1, "EuroSlot-B", 0x70);
    arr
}

fn fill_entry(arr: &mut [u8], idx: usize, typ: &[u8; 16], first: u64, last: u64, name: &str, guid_seed: u8) {
    let e = &mut arr[idx * 128..idx * 128 + 128];
    e[..16].copy_from_slice(typ);
    for (k, s) in e[16..32].iter_mut().enumerate() {
        *s = guid_seed.wrapping_add(k as u8);
    }
    e[32..40].copy_from_slice(&first.to_le_bytes());
    e[40..48].copy_from_slice(&last.to_le_bytes());
    for (k, c) in name.encode_utf16().enumerate() {
        if 56 + k * 2 + 2 <= 128 {
            e[56 + k * 2..56 + k * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
    }
}

fn gpt_header(primary: bool, total_sectors: u64, last_usable: u64, arr_crc: u32) -> [u8; 512] {
    let mut hdr = [0u8; 512];
    hdr[..8].copy_from_slice(b"EFI PART");
    hdr[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    hdr[12..16].copy_from_slice(&92u32.to_le_bytes());
    let backup_lba = total_sectors - 1;
    let (cur, bak, arr_lba) = if primary {
        (1u64, backup_lba, ENTRY_LBA)
    } else {
        (backup_lba, 1u64, last_usable + 1)
    };
    hdr[24..32].copy_from_slice(&cur.to_le_bytes());
    hdr[32..40].copy_from_slice(&bak.to_le_bytes());
    hdr[40..48].copy_from_slice(&34u64.to_le_bytes());
    hdr[48..56].copy_from_slice(&last_usable.to_le_bytes());
    for (k, s) in hdr[56..72].iter_mut().enumerate() {
        *s = 0x22 + k as u8;
    }
    hdr[72..80].copy_from_slice(&arr_lba.to_le_bytes());
    hdr[80..84].copy_from_slice(&NUM_ENTRIES.to_le_bytes());
    hdr[84..88].copy_from_slice(&ENTRY_SIZE.to_le_bytes());
    hdr[88..92].copy_from_slice(&arr_crc.to_le_bytes());
    let hcrc = crc32(&hdr[..92]);
    hdr[16..20].copy_from_slice(&hcrc.to_le_bytes());
    hdr
}

/// Build only the FAT32 ESP (loader + A/B kernel) as a single buffer.
pub fn build_esp(esp_sectors: u64, volume_id: u32, loader: &[u8], kernel_a: &[u8], kernel_b: &[u8]) -> Vec<u8> {
    build_esp_cfg(esp_sectors, volume_id, loader, kernel_a, kernel_b, &[])
}

/// Like [`build_esp`], but adds a `\slot_config` file (the A/B loader
/// reads it to choose the slot to boot). Empty = no slot_config.
pub fn build_esp_cfg(esp_sectors: u64, volume_id: u32, loader: &[u8], kernel_a: &[u8], kernel_b: &[u8], slot_config: &[u8]) -> Vec<u8> {
    let mut esp = FatFs::new(esp_sectors as u32, volume_id, "EUROKERNEL");
    esp.add_file("/EFI/BOOT/BOOTX64.EFI", loader);
    esp.add_file("/EFI/BOOT/eurokernel-A.efi", kernel_a);
    esp.add_file("/EFI/BOOT/eurokernel-B.efi", kernel_b);
    if !slot_config.is_empty() {
        esp.add_file("/slot_config", slot_config);
    }
    esp.build()
}

/// **Streaming** writer: build a bootable disk and deliver it in chunks
/// (≤ 4 KiB, LBA-aligned) to `write(lba, bytes)`. The kernel connects this to
/// `virtio_blk::write_io_dev`. NEVER materializes the whole disk — only the ESP.
/// The EuroFS partition stays unwritten (blank → the kernel formats it at boot).
/// `sig_a`/`sig_b` (64-byte Ed25519 signatures, may be empty) land next to the
/// kernels on the ESP so the loader can verify the file fallback. `slot_a_header`
/// (512 bytes, may be empty) + `kernel_a` are also written into the EuroSlot-A
/// partition: a fresh install then boots through the verified slot path from the
/// first boot on. Returns `None` (with the disk only partially written) when the
/// ESP files do not fit; the caller must refuse the install.
#[allow(clippy::too_many_arguments)]
pub fn write_boot_disk<W: FnMut(u64, &[u8])>(
    total_sectors: u64,
    volume_id: u32,
    loader: &[u8],
    kernel_a: &[u8],
    kernel_b: &[u8],
    sig_a: &[u8],
    sig_b: &[u8],
    slot_a_header: &[u8],
    slot_config: &[u8],
    mut write: W,
) -> Option<Layout> {
    let layout = layout_for(total_sectors);
    let last_usable = total_sectors.saturating_sub(34);
    let arr = part_array(&layout);
    let arr_crc = crc32(&arr);

    // ── Protective MBR (LBA0) ──
    let mut mbr = [0u8; SECTOR];
    mbr[450] = 0xEE;
    mbr[454..458].copy_from_slice(&1u32.to_le_bytes());
    mbr[458..462].copy_from_slice(&(total_sectors.min(0xFFFF_FFFF) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    write(0, &mbr);

    // ── Primary GPT header (LBA1) + array (LBA2..) ──
    write(1, &gpt_header(true, total_sectors, last_usable, arr_crc));
    write_blob(ENTRY_LBA, &arr, &mut write);

    // ── ESP (FAT32, incl. optional slot_config) streamed to its LBA, never
    //    materialized: the two kernel images are referenced, not copied. ──
    {
        let mut esp = FatFs::new(layout.esp_sectors as u32, volume_id, "EUROKERNEL");
        esp.add_file_ext("/EFI/BOOT/BOOTX64.EFI", 0, loader.len());
        esp.add_file_ext("/EFI/BOOT/eurokernel-A.efi", 1, kernel_a.len());
        esp.add_file_ext("/EFI/BOOT/eurokernel-B.efi", 2, kernel_b.len());
        if !sig_a.is_empty() {
            esp.add_file("/EFI/BOOT/eurokernel-A.efi.sig", sig_a);
        }
        if !sig_b.is_empty() {
            esp.add_file("/EFI/BOOT/eurokernel-B.efi.sig", sig_b);
        }
        if !slot_config.is_empty() {
            esp.add_file("/slot_config", slot_config);
        }
        let base = layout.esp_first;
        if !esp.build_streaming(&[loader, kernel_a, kernel_b], |sector, bytes| write(base + sector, bytes)) {
            return None;
        }
    }

    // ── Slot A: header + kernel image (the loader's primary, verified path) ──
    if slot_a_header.len() == SECTOR {
        let nsec = kernel_a.len().div_ceil(SECTOR) as u64;
        if nsec + 1 <= layout.slot_sectors {
            let mut lba = layout.slot_a_first + 1;
            let mut pad = [0u8; CHUNK];
            for c in kernel_a.chunks(CHUNK) {
                let n = c.len().div_ceil(SECTOR) * SECTOR;
                pad[..c.len()].copy_from_slice(c);
                pad[c.len()..n].iter_mut().for_each(|b| *b = 0);
                write(lba, &pad[..n]);
                lba += (n / SECTOR) as u64;
            }
            write(layout.slot_a_first, slot_a_header); // header last
        }
    }

    // ── Zero the first sectors of the EuroFS partition (force a fresh format) ──
    let zeros = [0u8; CHUNK];
    for s in 0..16u64 {
        write(layout.eurofs_first + s * 8, &zeros);
    }
    // ── Empty slot headers: no stale image may boot from a reused disk. Slot A
    //    keeps the header written above when the install is signed. ──
    if slot_a_header.len() != SECTOR {
        write(layout.slot_a_first, &zeros);
    }
    write(layout.slot_b_first, &zeros);

    // ── Backup GPT: array at last_usable+1.., header at the last sector ──
    write_blob(last_usable + 1, &arr, &mut write);
    write(layout.backup_lba, &gpt_header(false, total_sectors, last_usable, arr_crc));

    Some(layout)
}

/// Write `data` starting at `start_lba` in chunks of ≤ 4 KiB (8 sectors).
fn write_blob<W: FnMut(u64, &[u8])>(start_lba: u64, data: &[u8], write: &mut W) {
    let mut lba = start_lba;
    for c in data.chunks(CHUNK) {
        write(lba, c);
        lba += (c.len().div_ceil(SECTOR)) as u64;
    }
}

/// Host convenience: assemble the whole disk in memory (for validation + tests).
pub fn build_boot_disk(
    total_sectors: u64,
    volume_id: u32,
    loader: &[u8],
    kernel_a: &[u8],
    kernel_b: &[u8],
) -> (Vec<u8>, Layout) {
    let mut img = vec![0u8; total_sectors as usize * SECTOR];
    let layout = write_boot_disk(total_sectors, volume_id, loader, kernel_a, kernel_b, &[], &[], &[], &[], |lba, bytes| {
        let o = lba as usize * SECTOR;
        img[o..o + bytes.len()].copy_from_slice(bytes);
    });
    (img, layout.expect("the test files fit the ESP"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_bootable_disk() {
        let total = 512 * 1024 * 1024 / SECTOR as u64; // 512 MiB
        let loader = vec![0xAAu8; 24 * 1024];
        let ka = vec![0x55u8; 300_000];
        let kb = vec![0x33u8; 300_001];
        let (img, layout) = build_boot_disk(total, 0xCAFE, &loader, &ka, &kb);
        assert_eq!(img.len(), total as usize * SECTOR);
        assert_eq!(img[450], 0xEE);
        assert_eq!(&img[SECTOR..SECTOR + 8], b"EFI PART");
        // Backup GPT signature at the end.
        let bak = layout.backup_lba as usize * SECTOR;
        assert_eq!(&img[bak..bak + 8], b"EFI PART");
        // The ESP is a valid FAT32 with the three files.
        let esp_off = layout.esp_first as usize * SECTOR;
        let esp = &img[esp_off..esp_off + layout.esp_sectors as usize * SECTOR];
        assert_eq!(crate::read_file(esp, "/EFI/BOOT/BOOTX64.EFI"), Some(loader));
        assert_eq!(crate::read_file(esp, "/EFI/BOOT/eurokernel-A.efi"), Some(ka));
        assert_eq!(crate::read_file(esp, "/EFI/BOOT/eurokernel-B.efi"), Some(kb));
        assert!(layout.eurofs_first > layout.esp_first + layout.esp_sectors - 1);
    }

    #[test]
    fn streaming_matches_inmemory() {
        // The streaming writer and the in-memory build must be identical.
        // ESP 256 MiB + two 96 MiB slots + root: the smallest disk the layout accepts.
        let total = 640 * 1024 * 1024 / SECTOR as u64;
        let (img, _l) = build_boot_disk(total, 7, &[1, 2, 3], &[4; 1000], &[5; 1000]);
        let mut streamed = vec![0u8; total as usize * SECTOR];
        write_boot_disk(total, 7, &[1, 2, 3], &[4; 1000], &[5; 1000], &[], &[], &[], &[], |lba, b| {
            let o = lba as usize * SECTOR;
            streamed[o..o + b.len()].copy_from_slice(b);
        });
        assert_eq!(img, streamed);
    }

    #[test]
    fn signatures_and_slot_a_land_on_disk() {
        // Signed install: the .sig files sit next to the kernels on the ESP and
        // slot A holds header + image (header last, image sectors zero-padded).
        let total = 640 * 1024 * 1024 / SECTOR as u64;
        let (ka, kb) = (vec![4u8; 1000], vec![5u8; 777]);
        let (sa, sb) = (vec![0xAAu8; 64], vec![0xBBu8; 64]);
        let mut hdr = vec![0u8; SECTOR];
        hdr[..8].copy_from_slice(b"EUROSLT2");
        hdr[8..16].copy_from_slice(&(ka.len() as u64).to_le_bytes());
        let mut img = vec![0u8; total as usize * SECTOR];
        let layout = write_boot_disk(total, 7, &[1, 2, 3], &ka, &kb, &sa, &sb, &hdr, &[9, 9], |lba, b| {
            let o = lba as usize * SECTOR;
            img[o..o + b.len()].copy_from_slice(b);
        })
        .expect("fits");
        let esp_off = layout.esp_first as usize * SECTOR;
        let esp = &img[esp_off..esp_off + layout.esp_sectors as usize * SECTOR];
        assert_eq!(crate::read_file(esp, "/EFI/BOOT/eurokernel-A.efi.sig"), Some(sa));
        assert_eq!(crate::read_file(esp, "/EFI/BOOT/eurokernel-B.efi.sig"), Some(sb));
        assert_eq!(crate::read_file(esp, "/slot_config"), Some(vec![9, 9]));
        let a = layout.slot_a_first as usize * SECTOR;
        assert_eq!(&img[a..a + SECTOR], &hdr[..]);
        assert_eq!(&img[a + SECTOR..a + SECTOR + ka.len()], &ka[..]);
        assert!(img[a + SECTOR + ka.len()..a + 3 * SECTOR].iter().all(|&b| b == 0));
        // Slot B untouched.
        let b = layout.slot_b_first as usize * SECTOR;
        assert!(img[b..b + 2 * SECTOR].iter().all(|&x| x == 0));
    }

    #[test]
    fn oversized_esp_content_is_refused() {
        // 3 x 100 MiB does not fit a 256 MiB ESP: the writer must say so and
        // write no file data at all (it once spilled into the slot partitions).
        let total = 640 * 1024 * 1024 / SECTOR as u64;
        let big = vec![7u8; 100 * 1024 * 1024];
        let mut touched = 0usize;
        let r = write_boot_disk(total, 7, &big, &big, &big, &[], &[], &[], &[], |_lba, b| touched += b.len());
        assert!(r.is_none());
        // Only MBR + GPT header + partition array before the refusal.
        assert!(touched < 64 * 1024, "wrote {touched} bytes");
    }
}
