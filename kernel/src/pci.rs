//! PCI-enumeratie via de legacy config-poorten (0xCF8/0xCFC) — Run 7 / doc §4.
//! Ontdekt de aanwezige hardware (netwerk, opslag, ...) zodat drivers gekoppeld
//! kunnen worden. Fundament voor virtio-blk (echte schijf) en meer.

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

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
    /// IRQ-lijn (interrupt line).
    pub fn irq_line(&self) -> u8 {
        cfg_read32(self.bus, self.dev, self.func, 0x3C) as u8
    }
    /// Zet de command-register bits (bv. bus-master + I/O/MMIO enable).
    pub fn enable(&self, bits: u16) {
        let cur = cfg_read32(self.bus, self.dev, self.func, 0x04);
        let new = (cur & 0xFFFF_0000) | (cur as u16 | bits) as u32;
        cfg_write32(self.bus, self.dev, self.func, 0x04, new);
    }

    /// Het MMIO-basisadres van BAR `n` (fysiek = identity-mapped virtueel), met de
    /// lage flag-bits gemaskeerd. Ondersteunt 64-bit memory-BARs (type 0b10) — die
    /// gebruikt de moderne virtio-transport. Geeft 0 voor een I/O-BAR.
    pub fn bar_addr(&self, n: u8) -> u64 {
        let lo = cfg_read32(self.bus, self.dev, self.func, 0x10 + n * 4);
        if lo & 0x1 == 1 {
            return 0; // I/O-BAR (legacy), geen MMIO
        }
        if lo & 0x6 == 0x4 {
            // 64-bit memory-BAR: hoge helft staat in de volgende BAR.
            let hi = cfg_read32(self.bus, self.dev, self.func, 0x10 + (n + 1) * 4);
            (((hi as u64) << 32) | (lo as u64)) & !0xFu64
        } else {
            (lo as u64) & !0xFu64
        }
    }

    /// Vind de virtio-PCI-capability met het gevraagde `cfg_type`
    /// (1=common, 2=notify, 3=isr, 4=device). De moderne virtio-1.0-transport
    /// publiceert zo waar elk registerblok in welke BAR + offset ligt.
    pub fn virtio_cap(&self, want_cfg_type: u8) -> Option<VirtioCap> {
        // Capability-lijst aanwezig? (status-register bit 4)
        let status = (cfg_read32(self.bus, self.dev, self.func, 0x04) >> 16) as u16;
        if status & 0x10 == 0 {
            return None;
        }
        let mut ptr = (cfg_read32(self.bus, self.dev, self.func, 0x34) & 0xFC) as u8;
        let mut guard = 0;
        while ptr != 0 && guard < 48 {
            guard += 1;
            let w0 = cfg_read32(self.bus, self.dev, self.func, ptr);
            let cap_id = (w0 & 0xFF) as u8;
            let next = ((w0 >> 8) & 0xFF) as u8;
            // virtio-vendor-cap = 0x09; cfg_type staat in byte 3.
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
            ptr = next & 0xFC;
        }
        None
    }
}

/// Een virtio-modern-registerblok: het identity-mapped MMIO-adres + lengte
/// (en, voor de notify-cap, de notify-offset-multiplier).
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

pub fn cfg_read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    unsafe {
        Port::<u32>::new(0xCF8).write(cfg_addr(bus, dev, func, off));
        Port::<u32>::new(0xCFC).read()
    }
}

pub fn cfg_write32(bus: u8, dev: u8, func: u8, off: u8, val: u32) {
    unsafe {
        Port::<u32>::new(0xCF8).write(cfg_addr(bus, dev, func, off));
        Port::<u32>::new(0xCFC).write(val);
    }
}

/// Doorzoek de PCI-bussen (0..=8 dekt QEMU/q35) en verzamel alle apparaten.
pub fn enumerate() -> Vec<PciDevice> {
    let mut out = Vec::new();
    for bus in 0..=8u8 {
        for dev in 0..32u8 {
            let vd = cfg_read32(bus, dev, 0, 0);
            if (vd & 0xFFFF) as u16 == 0xFFFF {
                continue;
            }
            let header = (cfg_read32(bus, dev, 0, 0x0C) >> 16) as u8;
            let nfunc = if header & 0x80 != 0 { 8 } else { 1 };
            for func in 0..nfunc {
                let vd = cfg_read32(bus, dev, func, 0);
                let vendor = (vd & 0xFFFF) as u16;
                if vendor == 0xFFFF {
                    continue;
                }
                let cls = cfg_read32(bus, dev, func, 0x08);
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

/// Zoek het eerste apparaat dat aan een predicaat voldoet (voor drivers).
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
        0x0C => "Serial Bus (USB)",
        _ => "Other",
    }
}

/// Een leesbare naam voor bekende vendor/device-IDs (vooral virtio).
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
