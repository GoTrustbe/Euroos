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
const ESP_MIN_BYTES: u64 = 40 * 1024 * 1024; // comfortably ≥ FAT32 minimum (≈34 MiB)
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
    let fs_first = align8(esp_last + 1);
    let fs_last = last_usable - 1;
    Layout {
        esp_first,
        esp_sectors,
        eurofs_first: fs_first,
        eurofs_sectors: fs_last + 1 - fs_first,
        backup_lba: total_sectors - 1,
    }
}

fn part_array(l: &Layout) -> Vec<u8> {
    let mut arr = vec![0u8; (NUM_ENTRIES * ENTRY_SIZE) as usize];
    fill_entry(&mut arr, 0, &ESP_TYPE, l.esp_first, l.esp_first + l.esp_sectors - 1, "EFI System Partition", 0x10);
    fill_entry(&mut arr, 1, &EUROFS_TYPE, l.eurofs_first, l.eurofs_first + l.eurofs_sectors - 1, "EuroOS-A", 0x30);
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
pub fn write_boot_disk<W: FnMut(u64, &[u8])>(
    total_sectors: u64,
    volume_id: u32,
    loader: &[u8],
    kernel_a: &[u8],
    kernel_b: &[u8],
    slot_config: &[u8],
    mut write: W,
) -> Layout {
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

    // ── ESP (FAT32, incl. optional slot_config) streamed to its LBA ──
    let esp = build_esp_cfg(layout.esp_sectors, volume_id, loader, kernel_a, kernel_b, slot_config);
    write_blob(layout.esp_first, &esp, &mut write);
    drop(esp);

    // ── Zero the first sectors of the EuroFS partition (force a fresh format) ──
    let zeros = [0u8; CHUNK];
    for s in 0..16u64 {
        write(layout.eurofs_first + s * 8, &zeros);
    }

    // ── Backup GPT: array at last_usable+1.., header at the last sector ──
    write_blob(last_usable + 1, &arr, &mut write);
    write(layout.backup_lba, &gpt_header(false, total_sectors, last_usable, arr_crc));

    layout
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
    let layout = write_boot_disk(total_sectors, volume_id, loader, kernel_a, kernel_b, &[], |lba, bytes| {
        let o = lba as usize * SECTOR;
        img[o..o + bytes.len()].copy_from_slice(bytes);
    });
    (img, layout)
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
        let total = 256 * 1024 * 1024 / SECTOR as u64;
        let (img, _l) = build_boot_disk(total, 7, &[1, 2, 3], &[4; 1000], &[5; 1000]);
        let mut streamed = vec![0u8; total as usize * SECTOR];
        write_boot_disk(total, 7, &[1, 2, 3], &[4; 1000], &[5; 1000], &[], |lba, b| {
            let o = lba as usize * SECTOR;
            streamed[o..o + b.len()].copy_from_slice(b);
        });
        assert_eq!(img, streamed);
    }
}
