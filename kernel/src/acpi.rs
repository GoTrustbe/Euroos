//! Minimale ACPI-parser: vind de **MADT** (APIC-tabel) en tel de CPU-cores +
//! de IO-APIC. Dit is het fundament voor SMP (meerdere cores) en voor het
//! routeren van IRQ's via de IO-APIC i.p.v. de 8259-PIC.
//!
//! De RSDP-pointer wordt vóór `ExitBootServices` uit de UEFI-configuratietabel
//! gehaald (zie `set_rsdp`). Alle ACPI-tabellen liggen in identity-mapped RAM
//! (<4 GiB), dus we lezen ze rechtstreeks fysiek.

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

static RSDP: AtomicU64 = AtomicU64::new(0);

/// Sla het fysieke RSDP-adres op (aanroepen vóór ExitBootServices).
pub fn set_rsdp(addr: u64) {
    RSDP.store(addr, Ordering::Relaxed);
}

#[derive(Clone, Copy)]
pub struct Core {
    pub apic_id: u8,
    pub enabled: bool,
}

/// Interrupt Source Override: een ISA-IRQ kan op een andere GSI + andere
/// polariteit/trigger zitten dan de identiteit (bv. IRQ0 -> GSI2 in QEMU).
#[derive(Clone, Copy)]
pub struct Override {
    pub source_irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

/// Resultaat van het MADT-parsen.
pub struct Madt {
    pub cores: Vec<Core>,
    pub lapic_addr: u32,
    pub ioapic_addr: u32,
    pub ioapic_gsi_base: u32,
    pub overrides: Vec<Override>,
}

impl Madt {
    /// Vertaal een ISA-IRQ naar z'n GSI via de overrides (identiteit als geen).
    pub fn gsi_for(&self, irq: u8) -> u32 {
        for o in &self.overrides {
            if o.source_irq == irq {
                return o.gsi;
            }
        }
        irq as u32
    }
}

impl Madt {
    pub fn enabled_cores(&self) -> usize {
        self.cores.iter().filter(|c| c.enabled).count()
    }
}

#[inline]
unsafe fn rd<T: Copy>(addr: u64) -> T {
    (addr as *const T).read_unaligned()
}

fn signature(addr: u64) -> [u8; 4] {
    unsafe { rd::<[u8; 4]>(addr) }
}

/// Vind een ACPI-tabel op signatuur (bv. `b"FACP"`); geeft het fysieke adres.
pub fn find_table(sig: &[u8; 4]) -> Option<u64> {
    let rsdp = RSDP.load(Ordering::Relaxed);
    if rsdp == 0 {
        return None;
    }
    unsafe {
        let revision: u8 = rd(rsdp + 15);
        let (sdt, esize) = if revision >= 2 {
            let x: u64 = rd(rsdp + 24);
            if x != 0 {
                (x, 8usize)
            } else {
                (rd::<u32>(rsdp + 16) as u64, 4)
            }
        } else {
            (rd::<u32>(rsdp + 16) as u64, 4)
        };
        let len: u32 = rd(sdt + 4);
        let entries = (len as usize).saturating_sub(36) / esize;
        for i in 0..entries {
            let ep = sdt + 36 + (i * esize) as u64;
            let ptr = if esize == 8 { rd::<u64>(ep) } else { rd::<u32>(ep) as u64 };
            if ptr != 0 && &signature(ptr) == sig {
                return Some(ptr);
            }
        }
    }
    None
}

/// Fixed ACPI Description Table-velden die we voor power management nodig hebben.
pub struct Fadt {
    pub pm1a_cnt: u16, // PM1a control register-poort (S5/shutdown)
    pub reset_supported: bool,
    pub reset_is_io: bool,
    pub reset_addr: u64,
    pub reset_val: u8,
}

/// Parse de FADT (signatuur "FACP") voor shutdown/reboot.
pub fn fadt() -> Option<Fadt> {
    let f = find_table(b"FACP")?;
    unsafe {
        let pm1a_cnt: u32 = rd(f + 64);
        let flags: u32 = rd(f + 112);
        // RESET_REG = Generic Address Structure @ offset 116: +0 space_id, +4 address(u64).
        let reset_space: u8 = rd(f + 116);
        let reset_addr: u64 = rd(f + 116 + 4);
        let reset_val: u8 = rd(f + 128);
        Some(Fadt {
            pm1a_cnt: pm1a_cnt as u16,
            reset_supported: flags & (1 << 10) != 0,
            reset_is_io: reset_space == 1,
            reset_addr,
            reset_val,
        })
    }
}

/// Geef (adres, lengte) van de DSDT-AML-body (ná de 36-byte SDT-header). De DSDT
/// staat niet in de RSDT/XSDT-lijst maar wordt door de FADT aangewezen: DSDT @ +40
/// (32-bit) of X_DSDT @ +140 (64-bit). Voor de AML-interpreter (I3).
pub fn dsdt_aml() -> Option<(u64, usize)> {
    let f = find_table(b"FACP")?;
    unsafe {
        let len_fadt: u32 = rd(f + 4);
        // X_DSDT (64-bit) heeft voorrang als de FADT lang genoeg is en 't veld gezet is.
        let dsdt: u64 = if len_fadt >= 148 {
            let x: u64 = rd(f + 140);
            if x != 0 {
                x
            } else {
                rd::<u32>(f + 40) as u64
            }
        } else {
            rd::<u32>(f + 40) as u64
        };
        if dsdt == 0 || &signature(dsdt) != b"DSDT" {
            return None;
        }
        let total: u32 = rd(dsdt + 4);
        if total < 36 {
            return None;
        }
        Some((dsdt + 36, (total - 36) as usize))
    }
}

/// Parse de ACPI-tabellen en geef de MADT-inhoud terug (cores + IO-APIC).
pub fn parse() -> Option<Madt> {
    let rsdp = RSDP.load(Ordering::Relaxed);
    if rsdp == 0 {
        return None;
    }
    unsafe {
        // RSDP: revision @15, rsdt_address (u32) @16, xsdt_address (u64) @24.
        let revision: u8 = rd(rsdp + 15);
        let (sdt, esize) = if revision >= 2 {
            let xsdt: u64 = rd(rsdp + 24);
            if xsdt != 0 {
                (xsdt, 8usize)
            } else {
                (rd::<u32>(rsdp + 16) as u64, 4usize)
            }
        } else {
            (rd::<u32>(rsdp + 16) as u64, 4usize)
        };

        // Doorloop de (X)SDT-pointers, zoek de "APIC"-tabel (MADT).
        let len: u32 = rd(sdt + 4);
        let entries = (len as usize).saturating_sub(36) / esize;
        let mut madt_addr = 0u64;
        for i in 0..entries {
            let ep = sdt + 36 + (i * esize) as u64;
            let ptr = if esize == 8 { rd::<u64>(ep) } else { rd::<u32>(ep) as u64 };
            if ptr != 0 && &signature(ptr) == b"APIC" {
                madt_addr = ptr;
                break;
            }
        }
        if madt_addr == 0 {
            return None;
        }

        // MADT-header: 36 bytes, +36 local_apic_address (u32), +40 flags, +44 entries.
        let lapic_addr: u32 = rd(madt_addr + 36);
        let madt_len: u32 = rd(madt_addr + 4);
        let mut cores: Vec<Core> = Vec::new();
        let mut ioapic_addr = 0u32;
        let mut ioapic_gsi_base = 0u32;
        let mut overrides: Vec<Override> = Vec::new();

        let end = madt_addr + madt_len as u64;
        let mut p = madt_addr + 44;
        while p + 2 <= end {
            let etype: u8 = rd(p);
            let elen: u8 = rd(p + 1);
            if elen < 2 {
                break; // beschadigde tabel — stop
            }
            match etype {
                0 => {
                    // Processor Local APIC: +2 acpi_id, +3 apic_id, +4 flags(u32, bit0=enabled).
                    let apic_id: u8 = rd(p + 3);
                    let flags: u32 = rd(p + 4);
                    cores.push(Core { apic_id, enabled: flags & 1 != 0 });
                }
                1 => {
                    // IO APIC: +4 address (u32), +8 gsi_base (u32).
                    if ioapic_addr == 0 {
                        ioapic_addr = rd(p + 4);
                        ioapic_gsi_base = rd(p + 8);
                    }
                }
                2 => {
                    // Interrupt Source Override: +3 source_irq, +4 gsi (u32), +8 flags (u16).
                    overrides.push(Override {
                        source_irq: rd(p + 3),
                        gsi: rd(p + 4),
                        flags: rd(p + 8),
                    });
                }
                _ => {}
            }
            p += elen as u64;
        }

        Some(Madt { cores, lapic_addr, ioapic_addr, ioapic_gsi_base, overrides })
    }
}
