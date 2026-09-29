//! EuroOS two-stage A/B loader (G4).
//!
//! The UEFI firmware starts THIS small `.efi` (BOOTX64.EFI). It reads the A/B
//! `slot_config`, chooses the slot to boot, and loads+starts the kernel image of that
//! slot (`eurokernel-A.efi` / `eurokernel-B.efi`) via UEFI `LoadImage`/`StartImage`
//! — the Android/ChromeOS model. If the chosen slot fails, it falls back to A.
//!
//! This makes the A/B update truly two-stage: the loader (not the kernel) chooses which
//! system image runs, and can thus roll back to another slot if a kernel
//! does not even boot. The kernel keeps managing `slot_config` (attempt counter, mark-good).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use euroupdate::{Slot, SlotConfig};
use uefi::boot::{self, LoadImageSource};
use uefi::fs::{FileSystem, Path};
use uefi::prelude::*;
use uefi::proto::media::block::BlockIO;
use uefi::{cstr16, CStr16};

// ── COM1 serial via direct port I/O (works under Boot Services) ──
#[inline]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    v
}
fn com1_init() {
    unsafe {
        outb(0x3F9, 0x00);
        outb(0x3FB, 0x80);
        outb(0x3F8, 0x01);
        outb(0x3F9, 0x00);
        outb(0x3FB, 0x03);
        outb(0x3FA, 0xC7);
        outb(0x3FC, 0x0B);
    }
}
fn putc(b: u8) {
    unsafe {
        while inb(0x3FD) & 0x20 == 0 {}
        outb(0x3F8, b);
    }
}
fn puts(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            putc(b'\r');
        }
        putc(b);
    }
}

fn read_file(path: &CStr16) -> Option<Vec<u8>> {
    let proto = boot::get_image_file_system(boot::image_handle()).ok()?;
    let mut fs = FileSystem::new(proto);
    fs.read(Path::new(path)).ok()
}

/// An ESP kernel file is only acceptable with a valid detached signature next
/// to it (`<file>.sig`, 64 bytes): a fresh install or the preview medium ships
/// both. An unsigned or tampered file is refused, never booted.
fn read_verified_file(path: &CStr16, sig_path: &CStr16) -> Option<Vec<u8>> {
    let img = read_file(path)?;
    let Some(sig) = read_file(sig_path) else {
        puts("[loader] ESP kernel file has no signature file — refused\n");
        return None;
    };
    if sig.len() != 64 || !signature_ok(&img, &sig) {
        puts("[loader] ESP kernel file SIGNATURE invalid — refused\n");
        return None;
    }
    puts("[loader] kernel image from the ESP file (Ed25519 verified)\n");
    Some(img)
}

/// The chosen slot could not be verified and the other one boots instead: say
/// so in slot_config (rollback to it when it is Good), so the kernel's
/// mark_good confirms the slot that really runs and not the broken one.
fn record_fallback(other: Slot) {
    let Some(data) = read_file(cstr16!("\\slot_config")) else { return };
    let Some(mut cfg) = SlotConfig::deserialize(&data) else { return };
    if cfg.rollback() && cfg.next_boot == other {
        let _ = write_file(cstr16!("\\slot_config"), &cfg.serialize());
        puts("[loader] slot_config rolled back to the slot that boots now\n");
    }
}

fn write_file(path: &CStr16, data: &[u8]) -> bool {
    let proto = match boot::get_image_file_system(boot::image_handle()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut fs = FileSystem::new(proto);
    fs.write(Path::new(path), data).is_ok()
}

/// Two-stage A/B decision (the real ChromeOS/Android model): read `\slot_config`
/// from the ESP, run `on_boot()` (attempt counter −1, or automatic rollback to
/// the last-good slot when the attempts are exhausted), WRITE the updated config back
/// to the ESP, and return the slot to boot. This way the loader — not the kernel — handles
/// the rollback: even a kernel that does not even boot cannot brick the machine.
/// `None` → no/unreadable config → fall back to slot A.
fn decide_slot() -> Option<Slot> {
    let data = read_file(cstr16!("\\slot_config"))?;
    let mut cfg = SlotConfig::deserialize(&data)?;
    let before = (cfg.tries, cfg.next_boot);
    let booted = cfg.on_boot();
    // Persist the updated counter/choice before we start the kernel. If writing
    // fails, we still boot anyway (a read-only ESP must never be fatal).
    let _ = write_file(cstr16!("\\slot_config"), &cfg.serialize());
    puts("[loader] on_boot: ");
    puts(match before.1 {
        Slot::A => "A",
        Slot::B => "B",
    });
    puts(" tries ");
    putc(b'0' + before.0.min(9));
    puts(" → ");
    putc(b'0' + cfg.tries.min(9));
    puts("\n");
    Some(booted)
}

/// GPT type of the slot partitions (`eurofat::disk::EUROSLOT_TYPE`).
const EUROSLOT_TYPE: [u8; 16] =
    [0x45, 0x55, 0x52, 0x4f, 0x53, 0x4c, 0x00, 0x01, 0x80, 0x00, 0x00, 0x45, 0x55, 0x52, 0x4f, 0x53];
/// Header sector an update writes in front of a slot image (`kernel/src/update.rs`).
const SLOT_MAGIC: &[u8; 8] = b"EUROSLT2";
/// The same public keys the kernel embeds (daily + offline rotation key).
const PUBKEYS: [&[u8; 32]; 2] = [
    include_bytes!("../../toolchain/eupkg/keys/dev.pub"),
    include_bytes!("../../toolchain/eupkg/keys/rotation.pub"),
];
/// Ed25519 over the slot image, against either baked-in key (`verify_strict`).
fn signature_ok(image: &[u8], sig: &[u8]) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};
    let Ok(bytes) = <[u8; 64]>::try_from(sig) else { return false };
    let signature = Signature::from_bytes(&bytes);
    PUBKEYS.iter().any(|k| VerifyingKey::from_bytes(k).map(|vk| vk.verify_strict(image, &signature).is_ok()).unwrap_or(false))
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
fn name_eq(e: &[u8], want: &str) -> bool {
    let mut n = 0;
    let mut it = want.encode_utf16();
    loop {
        let c = u16::from_le_bytes([e[56 + n * 2], e[56 + n * 2 + 1]]);
        match it.next() {
            Some(w) => {
                if w != c || n >= 35 {
                    return false;
                }
            }
            None => return c == 0,
        }
        n += 1;
    }
}
/// Read `count` 512-byte sectors starting at `lba` from a BlockIO device.
fn read_sectors(bio: &BlockIO, lba: u64, count: usize) -> Option<Vec<u8>> {
    // Only 512-byte media: the LBA/buffer math below assumes it, and every disk
    // the kernel's own drivers write slot images to presents 512-byte sectors.
    // Anything else → None, and the loader falls back to the ESP file.
    let bs = bio.media().block_size() as usize;
    if bs != 512 {
        return None;
    }
    let mut out = alloc::vec![0u8; count * 512];
    // Read in 64 KiB pieces (firmware BlockIO dislikes huge single reads).
    let step = 128;
    let mut done = 0usize;
    while done < count {
        let n = (count - done).min(step);
        let media_id = bio.media().media_id();
        let blk = lba.checked_add(done as u64)?;
        bio.read_blocks(media_id, blk, &mut out[done * 512..(done + n) * 512]).ok()?;
        done += n;
    }
    Some(out)
}
/// The real A/B model: an update writes the new kernel into the `EuroOS-<slot>`
/// GPT partition (header sector + image). Find it on any whole disk, verify the
/// header hash, and hand the image to `LoadImage`. `None` → no such partition
/// or no valid image there (fresh install, plain preview image) → the caller
/// falls back to the ESP file.
fn load_from_partition(slot: Slot) -> Option<Vec<u8>> {
    let want = match slot {
        Slot::A => "EuroSlot-A",
        Slot::B => "EuroSlot-B",
    };
    let handles = boot::find_handles::<BlockIO>().ok()?;
    for h in handles {
        // GET_PROTOCOL, not exclusive: an exclusive open would disconnect the
        // firmware's filesystem driver on this very disk and break the ESP fallback.
        let params = boot::OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
        let Ok(bio) = (unsafe { boot::open_protocol::<BlockIO>(params, boot::OpenProtocolAttributes::GetProtocol) }) else { continue };
        if bio.media().is_logical_partition() || !bio.media().is_media_present() {
            continue;
        }
        let Some(hdr) = read_sectors(&bio, 1, 1) else { continue };
        if &hdr[..8] != b"EFI PART" {
            continue;
        }
        let ent_lba = rd_u64(&hdr, 72);
        let num = rd_u32(&hdr, 80).min(128) as usize;
        // Entry size is untrusted disk data: the spec allows 128 × 2^n; clamp so a
        // crafted header cannot make the loader allocate gigabytes.
        let esz = rd_u32(&hdr, 84) as usize;
        if esz < 128 || esz > 4096 || ent_lba == 0 {
            continue;
        }
        let Some(arr) = read_sectors(&bio, ent_lba, (num * esz).div_ceil(512)) else { continue };
        for i in 0..num {
            let e = &arr[i * esz..i * esz + 128];
            // Only the dedicated slot type: a root filesystem partition is never a
            // boot candidate, whatever its name or first sector say.
            if e[..16] != EUROSLOT_TYPE || !name_eq(e, want) {
                continue;
            }
            let (first, last) = (rd_u64(e, 32), rd_u64(e, 40));
            let Some(sh) = read_sectors(&bio, first, 1) else { continue };
            if &sh[..8] != SLOT_MAGIC {
                puts("[loader] slot partition has no image header (fresh install) → ESP file\n");
                return None;
            }
            let len = rd_u64(&sh, 8) as usize;
            let nsec = len.div_ceil(512);
            let end_ok = first.checked_add(1).and_then(|x| x.checked_add(nsec as u64)).map(|end| last.checked_add(1).is_some_and(|l| end <= l)).unwrap_or(false);
            if len == 0 || len > 128 * 1024 * 1024 || !end_ok {
                puts("[loader] slot image header out of range → ESP file\n");
                return None;
            }
            let mut img = read_sectors(&bio, first + 1, nsec)?;
            img.truncate(len);
            use sha2::Digest;
            let h = sha2::Sha256::digest(&img);
            if h[..] != sh[16..48] {
                puts("[loader] slot image sha256 MISMATCH — refusing it, ESP file instead\n");
                return None;
            }
            if !signature_ok(&img, &sh[48..112]) {
                puts("[loader] slot image SIGNATURE invalid — refusing it, ESP file instead\n");
                return None;
            }
            puts("[loader] kernel image from the slot partition (header + sha256 + Ed25519 verified)\n");
            return Some(img);
        }
    }
    None
}

#[entry]
fn main() -> Status {
    com1_init();
    puts("\n[loader] EuroOS two-stage A/B loader (G4)\n");

    let slot = decide_slot().unwrap_or(Slot::A);
    let (name, path, sig_path): (&str, &CStr16, &CStr16) = match slot {
        Slot::A => ("A", cstr16!("\\EFI\\BOOT\\eurokernel-A.efi"), cstr16!("\\EFI\\BOOT\\eurokernel-A.efi.sig")),
        Slot::B => ("B", cstr16!("\\EFI\\BOOT\\eurokernel-B.efi"), cstr16!("\\EFI\\BOOT\\eurokernel-B.efi.sig")),
    };
    puts("[loader] slot_config → boot slot ");
    puts(name);
    puts("\n");

    // Every candidate is verified (sha256 + Ed25519 for a slot, Ed25519 over the
    // file for the ESP): 1. the chosen slot, 2. the other slot (and slot_config
    // is rolled back to it, so the failed slot is not blessed by mark_good),
    // 3. the ESP file of the chosen slot, 4. the ESP file of slot A. Nothing
    // unsigned is ever handed to LoadImage.
    let other = slot.other();
    let image = match load_from_partition(slot)
        .or_else(|| {
            puts("[loader] slot image unusable — trying the other slot partition\n");
            let img = load_from_partition(other)?;
            record_fallback(other);
            Some(img)
        })
        .or_else(|| read_verified_file(path, sig_path))
        .or_else(|| read_verified_file(cstr16!("\\EFI\\BOOT\\eurokernel-A.efi"), cstr16!("\\EFI\\BOOT\\eurokernel-A.efi.sig")))
    {
        Some(b) => b,
        None => {
            puts("[loader] FATAL: no verified kernel image (slots and ESP files all missing or unsigned)\n");
            return Status::LOAD_ERROR;
        }
    };
    puts("[loader] kernel image loaded — LoadImage + StartImage...\n");

    let loaded = match boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer { buffer: &image, file_path: None },
    ) {
        Ok(h) => h,
        Err(_) => {
            puts("[loader] ERROR: LoadImage failed\n");
            return Status::LOAD_ERROR;
        }
    };
    // The kernel does ExitBootServices itself; start_image normally does not return.
    let _ = boot::start_image(loaded);
    puts("[loader] kernel returned unexpectedly\n");
    Status::SUCCESS
}
