//! EuroOS twee-traps A/B-loader (G4).
//!
//! De UEFI-firmware start DEZE kleine `.efi` (BOOTX64.EFI). Hij leest de A/B-
//! `slot_config`, kiest het te booten slot, en laadt+start de kernel-image van dat
//! slot (`eurokernel-A.efi` / `eurokernel-B.efi`) via UEFI `LoadImage`/`StartImage`
//! — het Android/ChromeOS-model. Faalt het gekozen slot, dan valt hij terug op A.
//!
//! Zo wordt de A/B-update echt twee-traps: de loader (niet de kernel) kiest welk
//! systeem-image draait, en kan dus naar een ander slot terugrollen als een kernel
//! niet eens boot. De kernel blijft `slot_config` beheren (poging-teller, mark-good).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use euroupdate::{Slot, SlotConfig};
use uefi::boot::{self, LoadImageSource};
use uefi::fs::{FileSystem, Path};
use uefi::prelude::*;
use uefi::{cstr16, CStr16};

// ── COM1-serial via directe poort-I/O (werkt onder Boot Services) ──
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

/// Twee-traps A/B-beslissing (het echte ChromeOS/Android-model): lees `\slot_config`
/// van de ESP, voer `on_boot()` uit (poging-teller −1, of automatische terugrol naar
/// het laatst-goede slot als de pogingen op zijn), SCHRIJF de bijgewerkte config terug
/// naar de ESP, en geef het te booten slot. Zo regelt de loader — niet de kernel — de
/// rollback: zelfs een kernel die niet eens boot, kan de machine niet bricken.
/// `None` → geen/onleesbare config → val terug op slot A.
fn decide_slot() -> Option<Slot> {
    let data = read_file(cstr16!("\\slot_config"))?;
    let mut cfg = SlotConfig::deserialize(&data)?;
    let before = (cfg.tries, cfg.next_boot);
    let booted = cfg.on_boot();
    // Persisteer de bijgewerkte teller/keuze vóór we de kernel starten. Lukt het
    // schrijven niet, dan booten we alsnog (read-only ESP mag nooit fataal zijn).
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
    puts("\n[loader] EuroOS twee-traps A/B-loader (G4)\n");

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
            puts("[loader] FOUT: kernel-image van het slot niet gevonden — probeer slot A\n");
            match read_file(cstr16!("\\EFI\\BOOT\\eurokernel-A.efi")) {
                Some(b) => b,
                None => {
                    puts("[loader] FATAAL: geen kernel-image\n");
                    return Status::LOAD_ERROR;
                }
            }
        }
    };
    puts("[loader] kernel-image geladen — LoadImage + StartImage...\n");

    let loaded = match boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer { buffer: &image, file_path: None },
    ) {
        Ok(h) => h,
        Err(_) => {
            puts("[loader] FOUT: LoadImage faalde\n");
            return Status::LOAD_ERROR;
        }
    };
    // De kernel doet zelf ExitBootServices; start_image keert normaal niet terug.
    let _ = boot::start_image(loaded);
    puts("[loader] kernel keerde onverwacht terug\n");
    Status::SUCCESS
}
