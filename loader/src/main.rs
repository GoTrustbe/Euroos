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

#[entry]
fn main() -> Status {
    com1_init();
    puts("\n[loader] EuroOS two-stage A/B loader (G4)\n");

    let slot = decide_slot().unwrap_or(Slot::A);
    let (name, path): (&str, &CStr16) = match slot {
        Slot::A => ("A", cstr16!("\\EFI\\BOOT\\eurokernel-A.efi")),
        Slot::B => ("B", cstr16!("\\EFI\\BOOT\\eurokernel-B.efi")),
    };
    puts("[loader] slot_config → boot slot ");
    puts(name);
    puts("\n");

    let image = match read_file(path) {
        Some(b) => b,
        None => {
            puts("[loader] ERROR: kernel image of the slot not found — trying slot A\n");
            match read_file(cstr16!("\\EFI\\BOOT\\eurokernel-A.efi")) {
                Some(b) => b,
                None => {
                    puts("[loader] FATAL: no kernel image\n");
                    return Status::LOAD_ERROR;
                }
            }
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
