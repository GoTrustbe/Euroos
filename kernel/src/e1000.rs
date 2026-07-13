//! Intel e1000/e1000e NIC driver (Metal M3-1, docs/SPRINT-PLAN-METAL.md).
//!
//! One driver for the Intel gigabit family via the **legacy descriptor**
//! format, which both the classic 82540EM ("e1000", QEMU default on older
//! machines) and the 82574L ("e1000e", the q35 default NIC) implement with
//! the same queue-0 register block. This is the wired-metal counterpart of
//! virtio-net: same polled send/receive surface, dispatched through `nic.rs`.
//!
//! Scope, honestly: QEMU-verified on 8086:100E and 8086:10D3. Real laptop
//! I219/I225 PHYs are related but not identical; they stay unclaimed until
//! validated on hardware via `hwprobe` (the driver only binds the two known
//! ids). Polled, single queue pair, no TSO/checksum offload, interrupts
//! masked. Buffers/rings live in identity-mapped DMA frames like the other
//! drivers.

use euromm::FrameAllocator;

use crate::pci;

// Register offsets (shared by 82540/82574 for the legacy queue-0 block).
const R_CTRL: u64 = 0x0000;
const R_RCTL: u64 = 0x0100;
const R_TCTL: u64 = 0x0400;
const R_RDBAL: u64 = 0x2800;
const R_RDBAH: u64 = 0x2804;
const R_RDLEN: u64 = 0x2808;
const R_RDH: u64 = 0x2810;
const R_RDT: u64 = 0x2818;
const R_TDBAL: u64 = 0x3800;
const R_TDBAH: u64 = 0x3804;
const R_TDLEN: u64 = 0x3808;
const R_TDH: u64 = 0x3810;
const R_TDT: u64 = 0x3818;
const R_IMC: u64 = 0x00D8;
const R_MTA: u64 = 0x5200; // 128 dwords multicast table
const R_RAL0: u64 = 0x5400;
const R_RAH0: u64 = 0x5404;

const CTRL_SLU: u32 = 1 << 6; // set link up
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15; // accept broadcast
const RCTL_SECRC: u32 = 1 << 26; // strip ethernet CRC
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;

const NDESC: usize = 16; // descriptors per ring (16 B each)
const BUF_SIZE: usize = 2048;

#[inline]
unsafe fn rd32(a: u64) -> u32 {
    (a as *const u32).read_volatile()
}
#[inline]
unsafe fn wr32(a: u64, v: u32) {
    (a as *mut u32).write_volatile(v);
}

struct E1000 {
    mmio: u64,
    rx_ring: u64, // NDESC legacy RX descriptors
    tx_ring: u64, // NDESC legacy TX descriptors
    rx_bufs: u64, // NDESC contiguous BUF_SIZE buffers
    tx_bufs: u64,
    rx_head: usize, // next descriptor we expect the NIC to fill
    tx_tail: usize, // next descriptor we fill
    mac: [u8; 6],
}

static mut NIC: Option<E1000> = None;

/// Find + initialize an Intel e1000/e1000e. Returns true when the NIC is up.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let dev = match pci::find(|d| {
        d.vendor == 0x8086 && (d.device == 0x100E || d.device == 0x10D3)
    }) {
        Some(d) => d,
        None => return false,
    };
    dev.enable(0x6); // memory space + bus master
    let mmio = dev.bar_addr(0);
    if mmio == 0 {
        return false;
    }
    pci::claim(dev.bus, dev.dev, dev.func, "e1000"); // hwprobe (M1-3)

    unsafe {
        wr32(mmio + R_IMC, 0xFFFF_FFFF); // mask every interrupt: we poll

        // MAC: the hardware preloads RAL0/RAH0 from its EEPROM at reset.
        let ral = rd32(mmio + R_RAL0);
        let rah = rd32(mmio + R_RAH0);
        let mac = [
            ral as u8,
            (ral >> 8) as u8,
            (ral >> 16) as u8,
            (ral >> 24) as u8,
            rah as u8,
            (rah >> 8) as u8,
        ];
        if mac == [0; 6] {
            return false; // no address programmed: don't guess
        }
        // Keep RAL/RAH valid (AV bit) and clear the multicast table.
        wr32(mmio + R_RAH0, rah | (1 << 31));
        for i in 0..128 {
            wr32(mmio + R_MTA + i * 4, 0);
        }

        // Rings: one frame each (16 descriptors × 16 B used). Buffers: NDESC
        // contiguous 2 KiB slots per direction (8 frames each).
        let rx_ring = match falloc.allocate() { Ok(a) => a, Err(_) => return false };
        let tx_ring = match falloc.allocate() { Ok(a) => a, Err(_) => return false };
        let rx_bufs = match falloc.allocate_aligned(NDESC * BUF_SIZE / 4096, 1) {
            Ok(a) => a,
            Err(_) => return false,
        };
        let tx_bufs = match falloc.allocate_aligned(NDESC * BUF_SIZE / 4096, 1) {
            Ok(a) => a,
            Err(_) => return false,
        };
        core::ptr::write_bytes(rx_ring as *mut u8, 0, 4096);
        core::ptr::write_bytes(tx_ring as *mut u8, 0, 4096);

        // RX descriptors: point each at its buffer, status clear.
        for i in 0..NDESC {
            let d = rx_ring + (i * 16) as u64;
            (d as *mut u64).write_volatile(rx_bufs + (i * BUF_SIZE) as u64);
        }
        wr32(mmio + R_RDBAL, (rx_ring & 0xFFFF_FFFF) as u32);
        wr32(mmio + R_RDBAH, (rx_ring >> 32) as u32);
        wr32(mmio + R_RDLEN, (NDESC * 16) as u32);
        wr32(mmio + R_RDH, 0);
        wr32(mmio + R_RDT, (NDESC - 1) as u32); // all but one slot available

        wr32(mmio + R_TDBAL, (tx_ring & 0xFFFF_FFFF) as u32);
        wr32(mmio + R_TDBAH, (tx_ring >> 32) as u32);
        wr32(mmio + R_TDLEN, (NDESC * 16) as u32);
        wr32(mmio + R_TDH, 0);
        wr32(mmio + R_TDT, 0);

        // RCTL: enable, accept broadcast, 2 KiB buffers (BSIZE=00), strip CRC.
        wr32(mmio + R_RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
        // TCTL: enable, pad short packets, sane collision defaults.
        wr32(mmio + R_TCTL, TCTL_EN | TCTL_PSP | (0x10 << 4) | (0x40 << 12));
        // Link up (internal PHY auto-negotiates; QEMU links instantly).
        wr32(mmio + R_CTRL, rd32(mmio + R_CTRL) | CTRL_SLU);

        NIC = Some(E1000 {
            mmio,
            rx_ring,
            tx_ring,
            rx_bufs,
            tx_bufs,
            rx_head: 0,
            tx_tail: 0,
            mac,
        });
        crate::serial_println!(
            "[e1000] {} up — MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (legacy descriptors, polled)",
            if dev.device == 0x10D3 { "82574L (e1000e)" } else { "82540EM (e1000)" },
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
    }
    true
}

pub fn mac() -> Option<[u8; 6]> {
    unsafe { (*core::ptr::addr_of!(NIC)).as_ref().map(|n| n.mac) }
}

/// Transmit one ethernet frame (blocking until the NIC reports DD; bounded).
pub fn send(frame: &[u8]) -> bool {
    unsafe {
        let n = match (*core::ptr::addr_of_mut!(NIC)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        if frame.is_empty() || frame.len() > BUF_SIZE {
            return false;
        }
        let i = n.tx_tail;
        let buf = n.tx_bufs + (i * BUF_SIZE) as u64;
        core::ptr::copy_nonoverlapping(frame.as_ptr(), buf as *mut u8, frame.len());
        let d = n.tx_ring + (i * 16) as u64;
        (d as *mut u64).write_volatile(buf);
        wr32(d + 8, frame.len() as u32); // length (low 16) + CSO 0
        // cmd byte @ +11: EOP | IFCS | RS; status byte @ +12: clear.
        (d as *mut u8).add(11).write_volatile(0x0B);
        (d as *mut u8).add(12).write_volatile(0);
        n.tx_tail = (i + 1) % NDESC;
        wr32(n.mmio + R_TDT, n.tx_tail as u32);
        // Wait for descriptor-done so the caller may reuse the slot freely.
        for _ in 0..2_000_000u64 {
            if (d as *const u8).add(12).read_volatile() & 1 != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

/// Non-blocking receive: the next pending frame, or None.
pub fn poll_recv() -> Option<alloc::vec::Vec<u8>> {
    unsafe {
        let n = (*core::ptr::addr_of_mut!(NIC)).as_mut()?;
        let i = n.rx_head;
        let d = n.rx_ring + (i * 16) as u64;
        let status = (d as *const u8).add(12).read_volatile();
        if status & 1 == 0 {
            return None; // DD clear: nothing new
        }
        let len = ((d as *const u16).add(4).read_volatile()) as usize; // length @ +8
        let buf = n.rx_bufs + (i * BUF_SIZE) as u64;
        let mut out = alloc::vec![0u8; len.min(BUF_SIZE)];
        core::ptr::copy_nonoverlapping(buf as *const u8, out.as_mut_ptr(), out.len());
        // Recycle the descriptor: clear status, hand it back via RDT.
        (d as *mut u8).add(12).write_volatile(0);
        wr32(n.mmio + R_RDT, i as u32);
        n.rx_head = (i + 1) % NDESC;
        Some(out)
    }
}
