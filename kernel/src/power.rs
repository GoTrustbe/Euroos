//! Power management (Run 7 / doc §3): REAL shutdown + reboot via ACPI.
//! A real OS can turn itself off and restart — this does exactly that.

use core::sync::atomic::{AtomicU32, Ordering};

use x86_64::instructions::port::Port;

use crate::serial_println;

/// The `\_S5` sleep-type values evaluated from the ACPI DSDT (SLP_TYPa | SLP_TYPb<<8),
/// with bit31 as the "is-set" flag. Filled in by the I3 AML interpreter at boot; that
/// way shutdown uses the FIRMWARE-correct SLP_TYP instead of a hardcoded 0 (crucial
/// on real hardware where S5's SLP_TYP differs per board).
static S5_SLP_TYP: AtomicU32 = AtomicU32::new(0);

/// Called by the AML interpreter (I3) with the `\_S5` package values.
pub fn set_s5_slp_typ(slp_typ_a: u8, slp_typ_b: u8) {
    S5_SLP_TYP.store(0x8000_0000 | slp_typ_a as u32 | ((slp_typ_b as u32) << 8), Ordering::Relaxed);
}

/// The PM1a_CNT write value for S5: `(SLP_TYPa << 10) | SLP_EN(bit13)`. Uses the
/// AML `_S5` value if evaluated, otherwise SLP_TYPa=0 (QEMU default).
fn s5_pm1a_value() -> u16 {
    let v = S5_SLP_TYP.load(Ordering::Relaxed);
    let slp_typ_a = if v & 0x8000_0000 != 0 { (v & 0x7) as u16 } else { 0 };
    (slp_typ_a << 10) | (1 << 13)
}

/// Return (PM1a_CNT port, S5 write value) WITHOUT shutting down — for a
/// boot-safe readiness check of the shutdown path.
pub fn s5_ready() -> (u16, u16) {
    let port = crate::acpi::fadt().map(|f| f.pm1a_cnt).filter(|&p| p != 0).unwrap_or(0x604);
    (port, s5_pm1a_value())
}

/// Did the AML `\_S5` provide a firmware SLP_TYP (instead of the QEMU default 0)?
pub fn s5_from_aml() -> bool {
    S5_SLP_TYP.load(Ordering::Relaxed) & 0x8000_0000 != 0
}

/// Turn the system off (ACPI S5 soft-off). Writes SLP_EN|SLP_TYP to the
/// PM1a control register from the FADT; SLP_TYP comes from the AML `\_S5` (or 0 on QEMU).
/// With fallbacks to the known QEMU ports. Never returns.
pub fn shutdown() -> ! {
    crate::rootblk::cache_flush(); // write out the dirty disk cache before poweroff
    crate::virtio_blk::flush(); // + force the disk's own cache to the medium (durable)
    let s5 = s5_pm1a_value();
    serial_println!("[power] shutting down system (ACPI S5, PM1a={s5:#06x} from \\_S5-AML)...");
    let port = crate::acpi::fadt().map(|f| f.pm1a_cnt).filter(|&p| p != 0).unwrap_or(0x604);
    unsafe {
        // SLP_EN (bit13) | SLP_TYPa<<10, with SLP_TYPa from the AML-evaluated \_S5.
        Port::<u16>::new(port).write(s5);
        // Extra fallbacks for different QEMU machines/firmware.
        Port::<u16>::new(0x604).write(s5);
        Port::<u16>::new(0xB004).write(s5);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    loop {
        x86_64::instructions::hlt();
    }
}

// ── M5-2: ACPI events (power button) ──────────────────────────────────────────
//
// A laptop's power button is a "fixed feature" ACPI event: pressing it sets
// PWRBTN_STS (bit 8) in the PM1 status register and raises the SCI interrupt.
// We enter ACPI mode (if the firmware asks), enable the power-button event and
// route the SCI so the press performs a clean OS-controlled shutdown instead of
// a hard power cut. QEMU's `system_powerdown` drives exactly this path.

const PM1_PWRBTN_STS: u16 = 1 << 8;
const PM1_PWRBTN_EN: u16 = 1 << 8;

static PM1A_STS_PORT: AtomicU32 = AtomicU32::new(0);

/// Enable ACPI power-button event delivery. Returns the SCI GSI to route, or
/// `None` when the firmware exposes no PM1 event block.
pub fn enable_power_button() -> Option<u16> {
    let f = crate::acpi::fadt()?;
    if f.pm1a_evt == 0 || f.pm1_evt_len == 0 {
        return None;
    }
    let sts_port = f.pm1a_evt;
    let en_port = f.pm1a_evt + (f.pm1_evt_len / 2) as u16;
    PM1A_STS_PORT.store(sts_port as u32, Ordering::Relaxed);
    unsafe {
        // Enter ACPI mode if the firmware uses a legacy/ACPI toggle (SMI_CMD).
        if f.smi_cmd != 0 && f.acpi_enable != 0 {
            Port::<u8>::new(f.smi_cmd as u16).write(f.acpi_enable);
        }
        // Clear a stale power-button status, then enable the event.
        Port::<u16>::new(sts_port).write(PM1_PWRBTN_STS);
        let en = Port::<u16>::new(en_port).read();
        Port::<u16>::new(en_port).write(en | PM1_PWRBTN_EN);
    }
    Some(f.sci_int)
}

/// Called from the SCI interrupt handler. Returns true when the power button was
/// the source (status cleared); the caller then performs the shutdown.
pub fn sci_is_power_button() -> bool {
    let sts_port = PM1A_STS_PORT.load(Ordering::Relaxed) as u16;
    if sts_port == 0 {
        return false;
    }
    unsafe {
        let sts = Port::<u16>::new(sts_port).read();
        if sts & PM1_PWRBTN_STS != 0 {
            Port::<u16>::new(sts_port).write(PM1_PWRBTN_STS); // write-1-clear
            return true;
        }
    }
    false
}

/// Restart the system. Uses the FADT reset register if supported,
/// otherwise the PCI reset port 0xCF9 and finally the keyboard controller (0xFE).
pub fn reboot() -> ! {
    crate::rootblk::cache_flush(); // write out the dirty disk cache before reboot
    crate::virtio_blk::flush(); // + force the disk's own cache to the medium (durable)
    serial_println!("[power] restarting system...");
    if let Some(f) = crate::acpi::fadt() {
        if f.reset_supported && f.reset_is_io && f.reset_addr != 0 {
            unsafe { Port::<u8>::new(f.reset_addr as u16).write(f.reset_val) };
        }
    }
    unsafe {
        Port::<u8>::new(0xCF9).write(0x0E); // PCI/q35 reset
        Port::<u8>::new(0x64).write(0xFE); // 8042 pulse reset line
    }
    loop {
        x86_64::instructions::hlt();
    }
}
