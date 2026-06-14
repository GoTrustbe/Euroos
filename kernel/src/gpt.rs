//! Minimal GPT (GUID Partition Table) — Run 7. Writes/reads a GPT with a single
//! EuroFS partition so the OS runs from a REAL partitioned disk
//! (like an installed Windows/Linux), instead of a ramdisk.
//!
//! Layout: LBA0 = protective MBR, LBA1 = GPT header, LBA2.. = partition array
//! (128×128 B). The EuroFS partition starts at LBA 2048 (1 MiB alignment).
//! (Backup GPT at the end: later refinement — our reader uses the primary one.)

use alloc::vec;

const PART_FIRST_LBA: u64 = 2048;
const ENTRY_LBA: u64 = 2;
const NUM_ENTRIES: u32 = 128;
const ENTRY_SIZE: u32 = 128;

/// Own partition-type GUID for EuroFS (raw 16 bytes, written/read consistently).
const EUROFS_TYPE: [u8; 16] =
    [0x45, 0x55, 0x52, 0x4f, 0x46, 0x53, 0x00, 0x01, 0x80, 0x00, 0x00, 0x45, 0x55, 0x52, 0x4f, 0x53];

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

fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(v)
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[o..o + 4]);
    u32::from_le_bytes(v)
}

/// Read in the GPT partition array from disk 0. Returns (array_bytes, entry_size, num).
fn read_part_array() -> Option<(alloc::vec::Vec<u8>, usize, usize)> {
    let mut hdr = [0u8; 512];
    if !crate::virtio_blk::read_sector(1, &mut hdr) || &hdr[..8] != b"EFI PART" {
        return None;
    }
    // Verify the GPT header CRC (audit H10): the CRC covers the first `hdr_size`
    // bytes with the CRC field (16..20) zeroed. A torn/corrupt header → reject
    // instead of trusting bogus LBA/partition fields.
    let hdr_size = (rd_u32(&hdr, 12) as usize).clamp(92, 512);
    let stored_hcrc = rd_u32(&hdr, 16);
    let mut hcopy = hdr;
    hcopy[16..20].copy_from_slice(&[0, 0, 0, 0]);
    if crc32(&hcopy[..hdr_size]) != stored_hcrc {
        return None;
    }

    let ent_lba = rd_u64(&hdr, 72);
    let num = rd_u32(&hdr, 80).min(NUM_ENTRIES) as usize;
    let esz = rd_u32(&hdr, 84) as usize;
    if esz < 128 || ent_lba == 0 {
        return None;
    }
    let sectors = (num * esz).div_ceil(512);
    let mut arr = vec![0u8; sectors * 512];
    for s in 0..sectors {
        let mut tmp = [0u8; 512];
        if !crate::virtio_blk::read_sector(ent_lba + s as u64, &mut tmp) {
            return None;
        }
        arr[s * 512..s * 512 + 512].copy_from_slice(&tmp);
    }
    // Verify the partition-array CRC against the header field (offset 88).
    if crc32(&arr[..num * esz]) != rd_u32(&hdr, 88) {
        return None;
    }
    Some((arr, esz, num))
}

/// Decode the UTF-16LE name of a partition entry (offset 56, 36 characters).
fn entry_name_eq(e: &[u8], want: &str) -> bool {
    let mut buf = [0u16; 36];
    for (k, slot) in buf.iter_mut().enumerate() {
        *slot = u16::from_le_bytes([e[56 + k * 2], e[56 + k * 2 + 1]]);
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(36);
    want.encode_utf16().eq(buf[..n].iter().copied())
}

/// Find a EuroFS partition by name (G4: EuroOS-A/EuroOS-B/EuroVar/EuroBoot).
/// Returns (first_sector, number_of_4k_blocks).
/// Enumerate ALL non-empty GPT partitions on virtio device `dev` as (first_lba,
/// last_lba). Lenient (no CRC enforcement) so foreign/partitioned disks can be listed
/// for `lsblk`/`mount`. Empty if there is no GPT.
pub fn all_partitions_on(dev: usize) -> alloc::vec::Vec<(u64, u64)> {
    let mut out = alloc::vec::Vec::new();
    let mut hdr = [0u8; 512];
    if !crate::virtio_blk::read_io_dev(dev, 1, &mut hdr) || &hdr[..8] != b"EFI PART" {
        return out;
    }
    let ent_lba = rd_u64(&hdr, 72);
    let num = rd_u32(&hdr, 80).min(NUM_ENTRIES) as usize;
    let esz = rd_u32(&hdr, 84) as usize;
    if esz < 128 || ent_lba == 0 {
        return out;
    }
    let sectors = (num * esz).div_ceil(512);
    let mut arr = vec![0u8; sectors * 512];
    for s in 0..sectors {
        let mut t = [0u8; 512];
        if !crate::virtio_blk::read_io_dev(dev, ent_lba + s as u64, &mut t) {
            return out;
        }
        arr[s * 512..s * 512 + 512].copy_from_slice(&t);
    }
    for i in 0..num {
        let e = &arr[i * esz..i * esz + 128];
        if e[..16].iter().all(|&b| b == 0) {
            continue; // unused entry
        }
        let (first, last) = (rd_u64(e, 32), rd_u64(e, 40));
        if last >= first && first != 0 {
            out.push((first, last));
        }
    }
    out
}

pub fn find_partition_by_name(name: &str) -> Option<(u64, u64)> {
    let (arr, esz, num) = read_part_array()?;
    for i in 0..num {
        let e = &arr[i * esz..i * esz + 128];
        if e[..16] == EUROFS_TYPE && entry_name_eq(e, name) {
            let (first, last) = (rd_u64(e, 32), rd_u64(e, 40));
            if last >= first {
                return Some((first, (last - first + 1) / 8));
            }
        }
    }
    None
}

/// Find the FIRST EuroFS partition (= slot A in the A/B layout). Returns
/// (first_sector, number_of_4k_blocks). Keeps the root-mount path unchanged.
pub fn find_eurofs_partition() -> Option<(u64, u64)> {
    let (arr, esz, num) = read_part_array()?;
    for i in 0..num {
        let e = &arr[i * esz..i * esz + 128];
        if e[..16] == EUROFS_TYPE {
            let (first, last) = (rd_u64(e, 32), rd_u64(e, 40));
            if last >= first {
                return Some((first, (last - first + 1) / 8));
            }
        }
    }
    None
}

fn align8(x: u64) -> u64 {
    x & !7
}

/// Fill a partition entry: EuroFS type, unique GUID, [first..last], UTF-16 name.
fn fill_entry(e: &mut [u8], first: u64, last: u64, name: &str, guid_seed: u8) {
    e[..16].copy_from_slice(&EUROFS_TYPE);
    for (k, slot) in e[16..32].iter_mut().enumerate() {
        *slot = guid_seed.wrapping_add(k as u8); // unique partition GUID
    }
    e[32..40].copy_from_slice(&first.to_le_bytes());
    e[40..48].copy_from_slice(&last.to_le_bytes());
    for (k, c) in name.encode_utf16().enumerate() {
        if 56 + k * 2 + 2 <= 128 {
            e[56 + k * 2..56 + k * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
    }
}

/// Write a fresh GPT with the **A/B layout** (G4): four EuroFS partitions —
/// `EuroOS-A` (root slot A), `EuroOS-B` (root slot B, for updates), `EuroVar`
/// (writable data), `EuroBoot` (kernel images/config). Returns slot A's
/// (first_sector, 4k blocks) so the root-mount path stays unchanged.
pub fn install(total_sectors: u64) -> (u64, u64) {
    let last_usable = total_sectors.saturating_sub(34);
    let usable = last_usable.saturating_sub(PART_FIRST_LBA);
    // Split: slot A 34%, slot B 34%, /var 20%, /boot the rest. 8-sector (4 KiB) aligned.
    let slot = align8(usable * 34 / 100).max(8);
    let varsz = align8(usable * 20 / 100).max(8);
    let a_first = PART_FIRST_LBA;
    let a_last = a_first + slot - 1;
    let b_first = a_last + 1;
    let b_last = b_first + slot - 1;
    let v_first = b_last + 1;
    let v_last = v_first + varsz - 1;
    let bt_first = v_last + 1;
    let bt_last = last_usable - 1;

    // Partition array: 4 entries.
    let mut arr = vec![0u8; (NUM_ENTRIES * ENTRY_SIZE) as usize];
    fill_entry(&mut arr[0..128], a_first, a_last, "EuroOS-A", 0x11);
    fill_entry(&mut arr[128..256], b_first, b_last, "EuroOS-B", 0x31);
    fill_entry(&mut arr[256..384], v_first, v_last, "EuroVar", 0x51);
    fill_entry(&mut arr[384..512], bt_first, bt_last, "EuroBoot", 0x71);
    let arr_crc = crc32(&arr);

    // GPT header.
    let mut hdr = [0u8; 512];
    hdr[..8].copy_from_slice(b"EFI PART");
    hdr[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    hdr[12..16].copy_from_slice(&92u32.to_le_bytes());
    hdr[24..32].copy_from_slice(&1u64.to_le_bytes());
    hdr[32..40].copy_from_slice(&total_sectors.saturating_sub(1).to_le_bytes());
    hdr[40..48].copy_from_slice(&34u64.to_le_bytes());
    hdr[48..56].copy_from_slice(&last_usable.to_le_bytes());
    for (k, slot) in hdr[56..72].iter_mut().enumerate() {
        *slot = 0x22 + k as u8; // disk GUID
    }
    hdr[72..80].copy_from_slice(&ENTRY_LBA.to_le_bytes());
    hdr[80..84].copy_from_slice(&NUM_ENTRIES.to_le_bytes());
    hdr[84..88].copy_from_slice(&ENTRY_SIZE.to_le_bytes());
    hdr[88..92].copy_from_slice(&arr_crc.to_le_bytes());
    let hcrc = crc32(&hdr[..92]);
    hdr[16..20].copy_from_slice(&hcrc.to_le_bytes());

    // Protective MBR.
    let mut mbr = [0u8; 512];
    mbr[450] = 0xEE;
    mbr[454..458].copy_from_slice(&1u32.to_le_bytes());
    mbr[458..462].copy_from_slice(&(total_sectors.min(0xFFFF_FFFF) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    crate::virtio_blk::write_sector(0, &mbr);
    crate::virtio_blk::write_sector(1, &hdr);
    for s in 0..(arr.len() / 512) {
        crate::virtio_blk::write_sector(ENTRY_LBA + s as u64, &arr[s * 512..s * 512 + 512]);
    }
    let mib = |first: u64, last: u64| (last - first + 1) * 512 / (1024 * 1024);
    crate::serial_println!(
        "[gpt] A/B GPT written — EuroOS-A @ LBA {} ({} MiB) · EuroOS-B @ {} ({} MiB) · EuroVar @ {} ({} MiB) · EuroBoot @ {} ({} MiB)",
        a_first, mib(a_first, a_last),
        b_first, mib(b_first, b_last),
        v_first, mib(v_first, v_last),
        bt_first, mib(bt_first, bt_last)
    );
    // Slot A = root (the first EuroFS partition); the root-mount path stays unchanged.
    (a_first, (a_last - a_first + 1) / 8)
}
