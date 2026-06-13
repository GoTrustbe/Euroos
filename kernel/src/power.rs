//! Power management (Run 7 / doc §3): ECHTE shutdown + reboot via ACPI.
//! Een echt OS kan zichzelf uitzetten en herstarten — dit doet dat.

use core::sync::atomic::{AtomicU32, Ordering};

use x86_64::instructions::port::Port;

use crate::serial_println;

/// De uit de ACPI-DSDT geëvalueerde `\_S5`-sleep-type-waarden (SLP_TYPa | SLP_TYPb<<8),
/// met bit31 als "is-gezet"-vlag. Door de I3-AML-interpreter ingevuld bij boot; zo
/// gebruikt de shutdown de FIRMWARE-correcte SLP_TYP i.p.v. een hardcoded 0 (cruciaal
/// op echte hardware waar S5's SLP_TYP per bord verschilt).
static S5_SLP_TYP: AtomicU32 = AtomicU32::new(0);

/// Door de AML-interpreter (I3) aangeroepen met de `\_S5`-package-waarden.
pub fn set_s5_slp_typ(slp_typ_a: u8, slp_typ_b: u8) {
    S5_SLP_TYP.store(0x8000_0000 | slp_typ_a as u32 | ((slp_typ_b as u32) << 8), Ordering::Relaxed);
}

/// De PM1a_CNT-schrijfwaarde voor S5: `(SLP_TYPa << 10) | SLP_EN(bit13)`. Gebruikt de
/// AML-`_S5`-waarde indien geëvalueerd, anders SLP_TYPa=0 (QEMU-default).
fn s5_pm1a_value() -> u16 {
    let v = S5_SLP_TYP.load(Ordering::Relaxed);
    let slp_typ_a = if v & 0x8000_0000 != 0 { (v & 0x7) as u16 } else { 0 };
    (slp_typ_a << 10) | (1 << 13)
}

/// Geef (PM1a_CNT-poort, S5-schrijfwaarde) terug ZONDER af te sluiten — voor een
/// boot-veilige gereedheidscontrole van het shutdown-pad.
pub fn s5_ready() -> (u16, u16) {
    let port = crate::acpi::fadt().map(|f| f.pm1a_cnt).filter(|&p| p != 0).unwrap_or(0x604);
    (port, s5_pm1a_value())
}

/// Heeft de AML-`\_S5` een firmware-SLP_TYP geleverd (i.p.v. de QEMU-default 0)?
pub fn s5_from_aml() -> bool {
    S5_SLP_TYP.load(Ordering::Relaxed) & 0x8000_0000 != 0
}

/// Zet het systeem uit (ACPI S5 soft-off). Schrijft SLP_EN|SLP_TYP naar het
/// PM1a-control-register uit de FADT; SLP_TYP komt uit de AML-`\_S5` (of 0 op QEMU).
/// Met fallbacks naar de bekende QEMU-poorten. Keert nooit terug.
pub fn shutdown() -> ! {
    crate::rootblk::cache_flush(); // vuile schijf-cache wegschrijven vóór poweroff
    crate::virtio_blk::flush(); // + de schijf z'n eigen cache naar het medium dwingen (duurzaam)
    let s5 = s5_pm1a_value();
    serial_println!("[power] systeem afsluiten (ACPI S5, PM1a={s5:#06x} uit \\_S5-AML)...");
    let port = crate::acpi::fadt().map(|f| f.pm1a_cnt).filter(|&p| p != 0).unwrap_or(0x604);
    unsafe {
        // SLP_EN (bit13) | SLP_TYPa<<10, met SLP_TYPa uit de AML-geëvalueerde \_S5.
        Port::<u16>::new(port).write(s5);
        // Extra fallbacks voor verschillende QEMU-machines/firmware.
        Port::<u16>::new(0x604).write(s5);
        Port::<u16>::new(0xB004).write(s5);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    loop {
        x86_64::instructions::hlt();
    }
}

/// Herstart het systeem. Gebruikt het FADT-reset-register indien ondersteund,
/// anders de PCI-reset-poort 0xCF9 en als laatste de keyboard-controller (0xFE).
pub fn reboot() -> ! {
    crate::rootblk::cache_flush(); // vuile schijf-cache wegschrijven vóór reboot
    crate::virtio_blk::flush(); // + de schijf z'n eigen cache naar het medium dwingen (duurzaam)
    serial_println!("[power] systeem herstarten...");
    if let Some(f) = crate::acpi::fadt() {
        if f.reset_supported && f.reset_is_io && f.reset_addr != 0 {
            unsafe { Port::<u8>::new(f.reset_addr as u16).write(f.reset_val) };
        }
    }
    unsafe {
        Port::<u8>::new(0xCF9).write(0x0E); // PCI/q35 reset
        Port::<u8>::new(0x64).write(0xFE); // 8042 pulse reset-lijn
    }
    loop {
        x86_64::instructions::hlt();
    }
}
