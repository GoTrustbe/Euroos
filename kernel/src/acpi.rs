//! Minimal ACPI parser: find the **MADT** (APIC table) and count the CPU cores +
//! the IO-APIC. This is the foundation for SMP (multiple cores) and for
//! routing IRQs via the IO-APIC instead of the 8259 PIC.
//!
//! The RSDP pointer is taken from the UEFI configuration table before
//! `ExitBootServices` (see `set_rsdp`). All ACPI tables lie in identity-mapped RAM
//! (<4 GiB), so we read them directly physically.

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

static RSDP: AtomicU64 = AtomicU64::new(0);

/// Store the physical RSDP address (call before ExitBootServices).
pub fn set_rsdp(addr: u64) {
    RSDP.store(addr, Ordering::Relaxed);
}

#[derive(Clone, Copy)]
pub struct Core {
    pub apic_id: u8,
    pub enabled: bool,
}

/// Interrupt Source Override: an ISA IRQ may sit on a different GSI + different
/// polarity/trigger than the identity (e.g. IRQ0 -> GSI2 in QEMU).
#[derive(Clone, Copy)]
pub struct Override {
    pub source_irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

/// Result of parsing the MADT.
pub struct Madt {
    pub cores: Vec<Core>,
    pub lapic_addr: u32,
    pub ioapic_addr: u32,
    pub ioapic_gsi_base: u32,
    pub overrides: Vec<Override>,
}

impl Madt {
    /// Translate an ISA IRQ to its GSI via the overrides (identity if none).
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

/// Find an ACPI table by signature (e.g. `b"FACP"`); returns the physical address.
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

/// Fixed ACPI Description Table fields that we need for power management.
pub struct Fadt {
    pub pm1a_cnt: u16, // PM1a control register port (S5/shutdown)
    pub reset_supported: bool,
    pub reset_is_io: bool,
    pub reset_addr: u64,
    pub reset_val: u8,
    // M5-2: ACPI event delivery (power button etc.).
    pub sci_int: u16,       // SCI interrupt (GSI) — level, active-low
    pub pm1a_evt: u16,      // PM1a event register block I/O port (status @ +0)
    pub pm1_evt_len: u8,    // total length; enable register = block + len/2
    pub smi_cmd: u32,       // SMI command port (0 = already in ACPI mode)
    pub acpi_enable: u8,    // value to write to SMI_CMD to enter ACPI mode
}

/// Parse the FADT (signature "FACP") for shutdown/reboot + ACPI events.
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
            sci_int: rd::<u16>(f + 46),
            pm1a_evt: rd::<u32>(f + 56) as u16,
            pm1_evt_len: rd::<u8>(f + 88),
            smi_cmd: rd::<u32>(f + 48),
            acpi_enable: rd::<u8>(f + 52),
        })
    }
}

/// Return (address, length) of the DSDT AML body (after the 36-byte SDT header). The DSDT
/// is not in the RSDT/XSDT list but is pointed to by the FADT: DSDT @ +40
/// (32-bit) or X_DSDT @ +140 (64-bit). For the AML interpreter (I3).
pub fn dsdt_aml() -> Option<(u64, usize)> {
    let f = find_table(b"FACP")?;
    unsafe {
        let len_fadt: u32 = rd(f + 4);
        // X_DSDT (64-bit) takes precedence if the FADT is long enough and the field is set.
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

/// One MCFG allocation: an ECAM window for a PCI segment group (M1-1).
#[derive(Clone, Copy)]
pub struct EcamWindow {
    pub base: u64,     // physical base of the ECAM region (identity-mapped)
    pub segment: u16,  // PCI segment group (0 on q35 and most machines)
    pub bus_start: u8, // first decoded bus
    pub bus_end: u8,   // last decoded bus
}

/// Parse the MCFG table (PCIe memory-mapped configuration — ECAM). Modern
/// machines publish the config space this way; the legacy 0xCF8/0xCFC ports
/// only reach the first 256 bytes and only segment 0.
pub fn mcfg() -> Option<alloc::vec::Vec<EcamWindow>> {
    let t = find_table(b"MCFG")?;
    unsafe {
        let len: u32 = rd(t + 4);
        if len < 44 {
            return None; // header (36) + reserved (8), no allocations
        }
        // Allocations start at offset 44; each entry is 16 bytes.
        let n = ((len as u64 - 44) / 16) as usize;
        let mut out = alloc::vec::Vec::with_capacity(n);
        for i in 0..n {
            let e = t + 44 + (i as u64) * 16;
            out.push(EcamWindow {
                base: rd(e),
                segment: rd(e + 8),
                bus_start: rd(e + 10),
                bus_end: rd(e + 11),
            });
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

/// Parse the ACPI tables and return the MADT contents (cores + IO-APIC).
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

        // Walk the (X)SDT pointers, look for the "APIC" table (MADT).
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

        // MADT header: 36 bytes, +36 local_apic_address (u32), +40 flags, +44 entries.
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
                break; // corrupted table — stop
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
