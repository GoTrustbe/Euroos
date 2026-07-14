//! MSI-X (Message Signaled Interrupts, extended) — foundation for plan J2.
//!
//! Modern PCIe devices deliver interrupts not over a shared INTx line but
//! by sending a **message** (a DMA write to a LAPIC address) — each
//! queue/endpoint gets its own vector, scalable across CPUs. This module walks the
//! **PCI capability list**, finds the MSI-X capability (id 0x11), maps the
//! **MSI-X table** (in a BAR), and programs one entry so the device sends its
//! interrupt to a Local-APIC vector on a chosen core.
//!
//! This is the reusable interrupt-delivery layer that will later feed virtio-blk/NVMe
//! completion (instead of busy-poll) and the xHCI event ring.

use crate::pci::{self, PciDevice};

const CAP_ID_MSIX: u8 = 0x11;

/// Read a (possibly 64-bit) BAR base address of `dev`.
fn bar_base(dev: &PciDevice, bir: u8) -> u64 {
    let lo = dev.bar(bir);
    if lo & 0x6 == 0x4 {
        // 64-bit memory BAR: high half in the next BAR.
        ((dev.bar(bir + 1) as u64) << 32) | (lo as u64 & 0xFFFF_FFF0)
    } else {
        lo as u64 & 0xFFFF_FFF0
    }
}

/// Program MSI-X table entry `entry` of `dev` so the device sends an interrupt
/// on `vector` to core `dest_apic`, and enable MSI-X (+ disable INTx). Returns
/// the number of table entries (0 = no MSI-X capability).
pub fn enable(dev: &PciDevice, entry: u16, vector: u8, dest_apic: u8) -> u16 {
    // M1-2: capability lookup via the shared walker in `pci` (one bounded,
    // ECAM-aware implementation instead of a private copy per driver).
    let Some(ptr) = dev.find_cap(CAP_ID_MSIX) else {
        return 0;
    };
    let head = pci::cfg_read32(dev.bus, dev.dev, dev.func, ptr);
    let mc = (head >> 16) as u16;
    let table_size = (mc & 0x7FF) + 1;
    // Table-offset/BIR register at cap+4.
    let tbl = pci::cfg_read32(dev.bus, dev.dev, dev.func, ptr + 4);
    let bir = (tbl & 0x7) as u8;
    let off = (tbl & !0x7) as u64;
    let table = bar_base(dev, bir) + off;
    if table == 0 {
        return 0;
    }
    unsafe {
        // Entry = 16 bytes: [msg_addr_lo][msg_addr_hi][msg_data][vector_control].
        let e = table + entry as u64 * 16;
        // LAPIC MSI address: 0xFEE0_0000 | (destination-APIC-id << 12).
        ((e) as *mut u32).write_volatile(0xFEE0_0000 | ((dest_apic as u32) << 12));
        ((e + 4) as *mut u32).write_volatile(0);
        ((e + 8) as *mut u32).write_volatile(vector as u32); // fixed-delivery, edge
        ((e + 12) as *mut u32).write_volatile(0); // vector-control: unmask (bit0=0)
    }
    // Enable MSI-X (bit15) + clear function-mask (bit14).
    let new_mc = (mc | (1 << 15)) & !(1 << 14);
    pci::cfg_write32(
        dev.bus, dev.dev, dev.func, ptr,
        (head & 0x0000_FFFF) | ((new_mc as u32) << 16),
    );
    // Disable legacy INTx (command register bit10) so only MSI-X delivers.
    let cmd = pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04);
    pci::cfg_write32(dev.bus, dev.dev, dev.func, 0x04, cmd | (1 << 10));
    table_size
}
