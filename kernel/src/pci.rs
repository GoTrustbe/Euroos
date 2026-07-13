//! PCI enumeration and configuration access — Run 7 / doc §4, extended by M1
//! (docs/SPRINT-PLAN-METAL.md) with **ECAM** (PCIe memory-mapped config via the
//! ACPI MCFG table), a shared **capability walker**, a **driver-claim registry**
//! and the `hwprobe` inventory dump.
//!
//! Config access strategy: if the MCFG published an ECAM window covering the
//! bus (the modern path — full 4 KiB config space per function), reads/writes go
//! through memory-mapped I/O; otherwise they fall back to the legacy 0xCF8/0xCFC
//! ports (first 256 bytes only). `init_ecam` verifies ECAM against the ports on
//! real devices before switching over — never trust a table blindly.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Active ECAM window (segment 0). 0 = not initialized/verified → legacy ports.
static ECAM_BASE: AtomicU64 = AtomicU64::new(0);
static ECAM_BUS_START: AtomicU8 = AtomicU8::new(0);
static ECAM_BUS_END: AtomicU8 = AtomicU8::new(0);

/// Which driver claimed which PCI function — feeds `hwprobe` (M1-3) so a
/// hardware report shows not just what is present but what EuroOS *drives*.
/// Only written from driver init in the boot task (never from IF=0 contexts).
static CLAIMS: Mutex<Vec<(u8, u8, u8, &'static str)>> = Mutex::new(Vec::new());

/// Record that `driver` operates the function at `bus:dev.func`.
pub fn claim(bus: u8, dev: u8, func: u8, driver: &'static str) {
    let mut c = CLAIMS.lock();
    if !c.iter().any(|&(b, d, f, _)| (b, d, f) == (bus, dev, func)) {
        c.push((bus, dev, func, driver));
    }
}

fn claimed_by(bus: u8, dev: u8, func: u8) -> Option<&'static str> {
    CLAIMS.lock().iter().find(|&&(b, d, f, _)| (b, d, f) == (bus, dev, func)).map(|&(_, _, _, n)| n)
}

/// The ECAM MMIO address of a config dword, or `None` if no (verified) ECAM
/// window covers this bus. Layout per PCIe spec: bus<<20 | dev<<15 | func<<12.
fn ecam_addr(bus: u8, dev: u8, func: u8, off: u16) -> Option<u64> {
    let base = ECAM_BASE.load(Ordering::Relaxed);
    if base == 0
        || bus < ECAM_BUS_START.load(Ordering::Relaxed)
        || bus > ECAM_BUS_END.load(Ordering::Relaxed)
    {
        return None;
    }
    let rel_bus = (bus - ECAM_BUS_START.load(Ordering::Relaxed)) as u64;
    Some(base + (rel_bus << 20 | (dev as u64) << 15 | (func as u64) << 12) + (off as u64 & !3))
}

/// Read a config dword at an **extended** offset (0..4096, ECAM only).
/// Returns `0xFFFF_FFFF` (master-abort value) when only legacy ports exist and
/// the offset is beyond their 256-byte reach.
pub fn cfg_read32_ext(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    match ecam_addr(bus, dev, func, off) {
        Some(a) => unsafe { (a as *const u32).read_volatile() },
        None if off < 0x100 => cfg_read32_ports(bus, dev, func, off as u8),
        None => 0xFFFF_FFFF,
    }
}

/// Initialize ECAM from the ACPI MCFG (M1-1). Conservative: the window is only
/// activated after a read-back verification against the legacy ports on the
/// host bridge and every function the port scan finds. Returns
/// `Some((base, bus_start, bus_end))` when ECAM is live, `None` on fallback.
pub fn init_ecam() -> Option<(u64, u8, u8)> {
    let windows = crate::acpi::mcfg()?;
    let w = windows.iter().find(|w| w.segment == 0)?;
    // Stage the window so ecam_addr works, but verify before declaring victory.
    ECAM_BASE.store(w.base, Ordering::Relaxed);
    ECAM_BUS_START.store(w.bus_start, Ordering::Relaxed);
    ECAM_BUS_END.store(w.bus_end, Ordering::Relaxed);
    // Verification: every device the LEGACY scan sees must read identically
    // (vendor/device + class) through ECAM. A mismatch = table lies → fall back.
    for d in enumerate_with(cfg_read32_ports) {
        let via_ecam_id = cfg_read32_ext(d.bus, d.dev, d.func, 0x00);
        let via_ports_id = cfg_read32_ports(d.bus, d.dev, d.func, 0x00);
        let via_ecam_cls = cfg_read32_ext(d.bus, d.dev, d.func, 0x08);
        let via_ports_cls = cfg_read32_ports(d.bus, d.dev, d.func, 0x08);
        if via_ecam_id != via_ports_id || via_ecam_cls != via_ports_cls {
            ECAM_BASE.store(0, Ordering::Relaxed);
            return None;
        }
    }
    Some((w.base, w.bus_start, w.bus_end))
}

/// True when config access runs over ECAM (after successful `init_ecam`).
pub fn ecam_active() -> bool {
    ECAM_BASE.load(Ordering::Relaxed) != 0
}

#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

impl PciDevice {
    /// BAR-waarde (Base Address Register) n (0..5).
    pub fn bar(&self, n: u8) -> u32 {
        cfg_read32(self.bus, self.dev, self.func, 0x10 + n * 4)
    }
    /// IRQ line (interrupt line).
    pub fn irq_line(&self) -> u8 {
        cfg_read32(self.bus, self.dev, self.func, 0x3C) as u8
    }
    /// Set the command-register bits (e.g. bus-master + I/O/MMIO enable).
    pub fn enable(&self, bits: u16) {
        let cur = cfg_read32(self.bus, self.dev, self.func, 0x04);
        let new = (cur & 0xFFFF_0000) | (cur as u16 | bits) as u32;
        cfg_write32(self.bus, self.dev, self.func, 0x04, new);
    }

    /// The MMIO base address of BAR `n` (physical = identity-mapped virtual), with the
    /// low flag bits masked off. Supports 64-bit memory BARs (type 0b10) — used by
    /// the modern virtio transport. Returns 0 for an I/O BAR.
    pub fn bar_addr(&self, n: u8) -> u64 {
        let lo = cfg_read32(self.bus, self.dev, self.func, 0x10 + n * 4);
        if lo & 0x1 == 1 {
            return 0; // I/O BAR (legacy), no MMIO
        }
        if lo & 0x6 == 0x4 {
            // 64-bit memory BAR: high half is in the next BAR.
            let hi = cfg_read32(self.bus, self.dev, self.func, 0x10 + (n + 1) * 4);
            (((hi as u64) << 32) | (lo as u64)) & !0xFu64
        } else {
            (lo as u64) & !0xFu64
        }
    }

    /// Iterate the standard PCI capability list: yields `(cap_id, cfg_offset)`.
    /// The shared walker (M1-2) — MSI-X, the virtio transport and `hwprobe`
    /// all walk through this one implementation.
    pub fn caps(&self) -> CapIter {
        // Capability list present? (status-register bit 4)
        let status = (cfg_read32(self.bus, self.dev, self.func, 0x04) >> 16) as u16;
        let ptr = if status & 0x10 != 0 {
            (cfg_read32(self.bus, self.dev, self.func, 0x34) & 0xFC) as u8
        } else {
            0
        };
        CapIter { dev: *self, ptr, guard: 0 }
    }

    /// First capability with the given id (e.g. 0x11 = MSI-X, 0x05 = MSI).
    pub fn find_cap(&self, id: u8) -> Option<u8> {
        self.caps().find(|&(cid, _)| cid == id).map(|(_, off)| off)
    }

    /// Find the virtio PCI capability with the requested `cfg_type`
    /// (1=common, 2=notify, 3=isr, 4=device). The modern virtio-1.0 transport
    /// publishes this way which register block lies in which BAR + offset.
    pub fn virtio_cap(&self, want_cfg_type: u8) -> Option<VirtioCap> {
        for (cap_id, ptr) in self.caps() {
            // virtio-vendor-cap = 0x09; cfg_type is in byte 3 of the header.
            let w0 = cfg_read32(self.bus, self.dev, self.func, ptr);
            if cap_id == 0x09 && ((w0 >> 24) & 0xFF) as u8 == want_cfg_type {
                let bar = (cfg_read32(self.bus, self.dev, self.func, ptr + 4) & 0xFF) as u8;
                let offset = cfg_read32(self.bus, self.dev, self.func, ptr + 8);
                let length = cfg_read32(self.bus, self.dev, self.func, ptr + 12);
                let notify_mult = if want_cfg_type == 2 {
                    cfg_read32(self.bus, self.dev, self.func, ptr + 16)
                } else {
                    0
                };
                let base = self.bar_addr(bar);
                if base == 0 {
                    return None;
                }
                return Some(VirtioCap { addr: base + offset as u64, length, notify_mult });
            }
        }
        None
    }
}

/// Iterator over a function's standard capability list (bounded against loops).
pub struct CapIter {
    dev: PciDevice,
    ptr: u8,
    guard: u8,
}

impl Iterator for CapIter {
    type Item = (u8, u8); // (cap_id, cfg_offset)
    fn next(&mut self) -> Option<(u8, u8)> {
        if self.ptr == 0 || self.guard >= 48 {
            return None;
        }
        self.guard += 1;
        let here = self.ptr;
        let w0 = cfg_read32(self.dev.bus, self.dev.dev, self.dev.func, here);
        self.ptr = ((w0 >> 8) & 0xFC) as u8;
        Some(((w0 & 0xFF) as u8, here))
    }
}

/// A modern-virtio register block: the identity-mapped MMIO address + length
/// (and, for the notify cap, the notify-offset multiplier).
#[derive(Clone, Copy)]
pub struct VirtioCap {
    pub addr: u64,
    pub length: u32,
    pub notify_mult: u32,
}

fn cfg_addr(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    0x8000_0000
        | (bus as u32) << 16
        | (dev as u32) << 11
        | (func as u32) << 8
        | (off as u32 & 0xFC)
}

/// Legacy config read via the 0xCF8/0xCFC ports (first 256 bytes only).
fn cfg_read32_ports(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    unsafe {
        Port::<u32>::new(0xCF8).write(cfg_addr(bus, dev, func, off));
        Port::<u32>::new(0xCFC).read()
    }
}

pub fn cfg_read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    match ecam_addr(bus, dev, func, off as u16) {
        Some(a) => unsafe { (a as *const u32).read_volatile() },
        None => cfg_read32_ports(bus, dev, func, off),
    }
}

pub fn cfg_write32(bus: u8, dev: u8, func: u8, off: u8, val: u32) {
    match ecam_addr(bus, dev, func, off as u16) {
        Some(a) => unsafe { (a as *mut u32).write_volatile(val) },
        None => unsafe {
            Port::<u32>::new(0xCF8).write(cfg_addr(bus, dev, func, off));
            Port::<u32>::new(0xCFC).write(val);
        },
    }
}

/// Scan the PCI buses (0..=8 covers QEMU/q35) with the given config reader.
fn enumerate_with(read: fn(u8, u8, u8, u8) -> u32) -> Vec<PciDevice> {
    let mut out = Vec::new();
    for bus in 0..=8u8 {
        for dev in 0..32u8 {
            let vd = read(bus, dev, 0, 0);
            if (vd & 0xFFFF) as u16 == 0xFFFF {
                continue;
            }
            let header = (read(bus, dev, 0, 0x0C) >> 16) as u8;
            let nfunc = if header & 0x80 != 0 { 8 } else { 1 };
            for func in 0..nfunc {
                let vd = read(bus, dev, func, 0);
                let vendor = (vd & 0xFFFF) as u16;
                if vendor == 0xFFFF {
                    continue;
                }
                let cls = read(bus, dev, func, 0x08);
                out.push(PciDevice {
                    bus,
                    dev,
                    func,
                    vendor,
                    device: (vd >> 16) as u16,
                    class: (cls >> 24) as u8,
                    subclass: (cls >> 16) as u8,
                    prog_if: (cls >> 8) as u8,
                });
            }
        }
    }
    out
}

/// Scan the PCI buses and collect all devices (via ECAM when active).
pub fn enumerate() -> Vec<PciDevice> {
    enumerate_with(cfg_read32)
}

/// Find the first device that satisfies a predicate (for drivers).
pub fn find(pred: impl Fn(&PciDevice) -> bool) -> Option<PciDevice> {
    enumerate().into_iter().find(pred)
}

pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match class {
        0x01 => match subclass {
            0x06 => "Mass Storage (SATA/AHCI)",
            0x08 => "Mass Storage (NVMe)",
            _ => "Mass Storage",
        },
        0x02 => "Network",
        0x03 => "Display (GPU)",
        0x04 => "Multimedia (Audio)",
        0x06 => "Bridge",
        0x0C => match subclass {
            0x03 => "Serial Bus (USB)",
            0x05 => "SMBus",
            _ => "Serial Bus",
        },
        _ => "Other",
    }
}

/// `hwprobe` (M1-3): a copy-pasteable hardware/driver inventory for the HCL
/// (docs/HARDWARE-COMPAT.md). Shows what is present AND what EuroOS drives —
/// the honest gap list for any machine this boots on.
pub fn hwprobe_lines() -> Vec<String> {
    use alloc::format;
    let mut out = Vec::new();
    out.push(String::from("EuroOS hwprobe — paste this block into docs/HARDWARE-COMPAT.md"));
    out.push(format!(
        "config-access: {}",
        if ecam_active() {
            format!(
                "ECAM @ {:#x} (buses {}..={}, ACPI MCFG, port-verified)",
                ECAM_BASE.load(Ordering::Relaxed),
                ECAM_BUS_START.load(Ordering::Relaxed),
                ECAM_BUS_END.load(Ordering::Relaxed)
            )
        } else {
            String::from("legacy ports 0xCF8/0xCFC (no verified MCFG)")
        }
    ));
    if let Some(m) = crate::acpi::parse() {
        out.push(format!(
            "acpi: {} core(s), io-apic @ {:#x}",
            m.enabled_cores(),
            m.ioapic_addr
        ));
    }
    let mut driven = 0usize;
    let devs = enumerate();
    for d in &devs {
        let name = device_name(d.vendor, d.device);
        let caps: Vec<String> = d
            .caps()
            .map(|(id, _)| match id {
                0x05 => String::from("msi"),
                0x09 => String::from("vendor"),
                0x10 => String::from("pcie"),
                0x11 => String::from("msix"),
                other => format!("{other:#04x}"),
            })
            .collect();
        let who = claimed_by(d.bus, d.dev, d.func);
        if who.is_some() {
            driven += 1;
        }
        out.push(format!(
            "pci {:02x}:{:02x}.{} {:04x}:{:04x} {:<24} {:<14} caps[{}] driver={}",
            d.bus,
            d.dev,
            d.func,
            d.vendor,
            d.device,
            class_name(d.class, d.subclass),
            if name.is_empty() { "?" } else { name },
            caps.join(","),
            who.unwrap_or("-")
        ));
    }
    // M2-3: block-device inventory — what storage EuroOS can actually address.
    if crate::nvme::present() {
        out.push(format!(
            "disk nvme0: {} MiB (PRP-list I/O, 64 KiB window)",
            crate::nvme::capacity_sectors() * 512 / (1024 * 1024)
        ));
    }
    for i in 0..4 {
        let sectors = crate::ahci::disk_sectors(i);
        if sectors > 0 {
            out.push(format!(
                "disk sata{i}: {} MiB ({})",
                sectors * 512 / (1024 * 1024),
                if crate::ahci::disk_partitioned(i) { "partitioned" } else { "blank" }
            ));
        }
    }
    out.push(format!(
        "summary: {}/{} PCI function(s) driven by EuroOS drivers",
        driven,
        devs.len()
    ));
    out
}

/// A readable name for known vendor/device IDs (especially virtio).
pub fn device_name(vendor: u16, device: u16) -> &'static str {
    match (vendor, device) {
        (0x1AF4, 0x1000) | (0x1AF4, 0x1041) => "virtio-net",
        (0x1AF4, 0x1001) | (0x1AF4, 0x1042) => "virtio-blk",
        (0x1AF4, 0x1003) | (0x1AF4, 0x1043) => "virtio-console",
        (0x1AF4, 0x1005) | (0x1AF4, 0x1044) => "virtio-rng",
        (0x1AF4, 0x1050) => "virtio-gpu",
        (0x8086, 0x29C0) => "Intel Q35 host bridge",
        (0x8086, 0x2918) => "Intel ICH9 LPC",
        (0x8086, 0x2922) => "Intel ICH9 AHCI",
        (0x1234, 0x1111) => "QEMU VGA",
        _ => "",
    }
}
