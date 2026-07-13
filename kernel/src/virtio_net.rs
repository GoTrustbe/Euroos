//! Legacy virtio-net (virtio 0.9.5 / transitional) driver — EuroNet's real NIC.
//!
//! Until now EuroNet only built/parsed packets. This driver talks to a
//! `virtio-net-pci` device (QEMU, `disable-modern=on` → legacy PIO) and sends
//! and receives REAL Ethernet frames: PCI scan, feature negotiation, two
//! virtqueues (RX=0, TX=1) and the notify/used-ring handling.
//!
//! Memory: the virtqueues + buffers come from the frame allocator. Because the kernel
//! identity-maps the lower 1 GiB (virt == phys), an allocated frame address
//! is directly the physical address the device needs.

use core::sync::atomic::{compiler_fence, Ordering};
use euromm::FrameAllocator;
use x86_64::instructions::port::Port;

// PCI configuration access (port 0xCF8/0xCFC).
fn pci_cfg_read32(bus: u8, slot: u8, func: u8, off: u8) -> u32 {
    let addr = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        Port::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).read()
    }
}
fn pci_cfg_write32(bus: u8, slot: u8, func: u8, off: u8, val: u32) {
    let addr = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        Port::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).write(val);
    }
}

// Legacy virtio I/O register offsets (from BAR0, without MSI-X).
const VIRTIO_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_DRIVER_FEATURES: u16 = 0x04;
const VIRTIO_QUEUE_PFN: u16 = 0x08;
const VIRTIO_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_STATUS: u16 = 0x12;
const VIRTIO_NET_CFG_MAC: u16 = 0x14; // 6 bytes MAC (without MSI-X)

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const RX_BUFS: usize = 16;
const BUF_SIZE: usize = 2048;
const NET_HDR_LEN: usize = 10; // legacy virtio_net_hdr (no mergeable rx)

#[repr(C)]
#[derive(Clone, Copy)]
struct VqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// A set-up virtqueue (legacy split-ring layout, one contiguous region).
struct VirtQueue {
    size: u16,
    base: u64,      // start of the ring region (= phys = virt)
    desc: u64,      // descriptor table
    avail: u64,     // available ring
    used: u64,      // used ring
    last_used: u16, // last processed used index
}

impl VirtQueue {
    fn desc(&self, i: u16) -> *mut VqDesc {
        (self.desc + (i as u64) * 16) as *mut VqDesc
    }
    // avail: u16 flags, u16 idx, u16 ring[size]
    fn avail_idx_ptr(&self) -> *mut u16 {
        (self.avail + 2) as *mut u16
    }
    fn avail_ring(&self, i: u16) -> *mut u16 {
        (self.avail + 4 + (i as u64) * 2) as *mut u16
    }
    // used: u16 flags, u16 idx, {u32 id, u32 len}[size]
    fn used_idx(&self) -> u16 {
        unsafe { ((self.used + 2) as *const u16).read_volatile() }
    }
    fn used_elem(&self, i: u16) -> (u32, u32) {
        let p = (self.used + 4 + (i as u64) * 8) as *const u32;
        unsafe { (p.read_volatile(), p.add(1).read_volatile()) }
    }
}

pub struct VirtioNet {
    io: u16,        // BAR0 I/O base
    pub mac: [u8; 6],
    rx: VirtQueue,
    tx: VirtQueue,
    rx_bufs: [u64; RX_BUFS],
    tx_buf: u64,
}

static mut NIC: Option<VirtioNet> = None;

fn vring_size(qsz: usize) -> usize {
    let align = 4096;
    let desc = 16 * qsz;
    let avail = 6 + 2 * qsz;
    let used = 6 + 8 * qsz;
    ((desc + avail + align - 1) & !(align - 1)) + used
}

fn setup_queue(io: u16, sel: u16, falloc: &mut FrameAllocator) -> Option<VirtQueue> {
    unsafe {
        Port::new(io + VIRTIO_QUEUE_SELECT).write(sel);
        let qsz: u16 = Port::new(io + VIRTIO_QUEUE_SIZE).read();
        if qsz == 0 {
            return None;
        }
        let bytes = vring_size(qsz as usize);
        let pages = (bytes + 4095) / 4096;
        let base = falloc.allocate_contiguous(pages).ok()?;
        core::ptr::write_bytes(base as *mut u8, 0, pages * 4096);
        let desc = base;
        let avail = desc + 16 * qsz as u64;
        let used = (avail + 6 + 2 * qsz as u64 + 4095) & !4095;
        // queue address = physical PFN (>>12)
        Port::new(io + VIRTIO_QUEUE_PFN).write((base >> 12) as u32);
        Some(VirtQueue { size: qsz, base, desc, avail, used, last_used: 0 })
    }
}

/// Find + initialize the virtio-net device. Returns true on success.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    // 1. PCI scan: look for 0x1AF4:0x1000 (legacy virtio-net).
    let mut found = None;
    'scan: for slot in 0u8..32 {
        let id = pci_cfg_read32(0, slot, 0, 0x00);
        if id == 0xFFFF_FFFF {
            continue;
        }
        if (id & 0xFFFF) == 0x1AF4 {
            let dev = (id >> 16) & 0xFFFF;
            if dev == 0x1000 || dev == 0x1041 {
                found = Some(slot);
                break 'scan;
            }
        }
    }
    let slot = match found {
        Some(s) => s,
        None => {
            crate::serial_println!("[net] no virtio-net device found");
            return false;
        }
    };
    crate::pci::claim(0, slot, 0, "virtio-net"); // hwprobe (M1-3)

    // 2. BAR0 = I/O port base; enable I/O + bus-master in the command register.
    let bar0 = pci_cfg_read32(0, slot, 0, 0x10);
    let io = (bar0 & 0xFFFC) as u16;
    let cmd = pci_cfg_read32(0, slot, 0, 0x04);
    pci_cfg_write32(0, slot, 0, 0x04, cmd | 0x5); // bit0 I/O, bit2 bus-master
    crate::serial_println!("[net] virtio-net @ PCI 0:{slot}.0, BAR0 I/O={io:#06x}");

    unsafe {
        // 3. Reset + ACKNOWLEDGE + DRIVER.
        let mut status: Port<u8> = Port::new(io + VIRTIO_STATUS);
        status.write(0);
        status.write(STATUS_ACK);
        status.write(STATUS_ACK | STATUS_DRIVER);

        // 4. Feature negotiation: MAC only (no mergeable rx, no csum offload).
        let dev_feat: u32 = Port::new(io + VIRTIO_DEVICE_FEATURES).read();
        let drv_feat = dev_feat & VIRTIO_NET_F_MAC;
        Port::new(io + VIRTIO_DRIVER_FEATURES).write(drv_feat);

        // 5. Read the MAC from the device config.
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = Port::<u8>::new(io + VIRTIO_NET_CFG_MAC + i as u16).read();
        }

        // 6. Set up RX (queue 0) and TX (queue 1) virtqueues.
        let mut rx = match setup_queue(io, 0, falloc) {
            Some(q) => q,
            None => return false,
        };
        let tx = match setup_queue(io, 1, falloc) {
            Some(q) => q,
            None => return false,
        };

        // 7. Allocate RX buffers and put them in the avail ring (device writes into them).
        let mut rx_bufs = [0u64; RX_BUFS];
        for i in 0..RX_BUFS {
            let buf = falloc.allocate().expect("rx-buf");
            rx_bufs[i] = buf;
            let d = rx.desc(i as u16);
            (*d).addr = buf;
            (*d).len = BUF_SIZE as u32;
            (*d).flags = DESC_WRITE; // device → us
            (*d).next = 0;
            rx.avail_ring(i as u16).write(i as u16);
        }
        compiler_fence(Ordering::SeqCst);
        rx.avail_idx_ptr().write(RX_BUFS as u16);

        // TX buffer (one, reused synchronously).
        let tx_buf = falloc.allocate().expect("tx-buf");

        // 8. DRIVER_OK — device is now live.
        status.write(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
        // Notify the RX queue so the device picks up the buffers.
        Port::<u16>::new(io + VIRTIO_QUEUE_NOTIFY).write(0);

        NIC = Some(VirtioNet { io, mac, rx, tx, rx_bufs, tx_buf });
        crate::serial_println!(
            "[net] virtio-net OK — MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (RX {} bufs)",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], RX_BUFS
        );
    }
    true
}

/// The MAC address of the NIC (after init).
pub fn mac() -> Option<[u8; 6]> {
    unsafe { (*core::ptr::addr_of!(NIC)).as_ref().map(|n| n.mac) }
}

/// Send one Ethernet frame (with virtio_net_hdr prepended). Synchronous.
pub fn send(frame: &[u8]) -> bool {
    unsafe {
        let nic = match (*core::ptr::addr_of_mut!(NIC)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        if NET_HDR_LEN + frame.len() > BUF_SIZE {
            return false;
        }
        // virtio_net_hdr (10 zero bytes) + frame in the TX buffer.
        core::ptr::write_bytes(nic.tx_buf as *mut u8, 0, NET_HDR_LEN);
        core::ptr::copy_nonoverlapping(frame.as_ptr(), (nic.tx_buf + NET_HDR_LEN as u64) as *mut u8, frame.len());
        let d = nic.tx.desc(0);
        (*d).addr = nic.tx_buf;
        (*d).len = (NET_HDR_LEN + frame.len()) as u32;
        (*d).flags = 0;
        (*d).next = 0;
        let idx = nic.tx.avail_idx_ptr().read();
        nic.tx.avail_ring(idx % nic.tx.size).write(0);
        compiler_fence(Ordering::SeqCst);
        nic.tx.avail_idx_ptr().write(idx.wrapping_add(1));
        compiler_fence(Ordering::SeqCst);
        Port::<u16>::new(nic.io + VIRTIO_QUEUE_NOTIFY).write(1);
        // Wait (briefly) until the device has processed the descriptor.
        for _ in 0..1_000_000 {
            if nic.tx.used_idx() != nic.tx.last_used {
                nic.tx.last_used = nic.tx.used_idx();
                return true;
            }
            core::hint::spin_loop();
        }
        true
    }
}

/// Poll one received frame (without the virtio_net_hdr). Returns None if there is nothing.
pub fn poll_recv() -> Option<alloc::vec::Vec<u8>> {
    unsafe {
        let nic = (*core::ptr::addr_of_mut!(NIC)).as_mut()?;
        let used = nic.rx.used_idx();
        if used == nic.rx.last_used {
            return None;
        }
        let slot = nic.rx.last_used % nic.rx.size;
        let (id, len) = nic.rx.used_elem(slot);
        let id = id as usize;
        // The length reported by the device must NEVER exceed the buffer:
        // a malicious/buggy virtio device claiming len > BUF_SIZE would otherwise cause an
        // out-of-bounds read from the rx buffer. Clamp to BUF_SIZE.
        let total = (len as usize).min(BUF_SIZE);
        let mut out = alloc::vec::Vec::new();
        if total > NET_HDR_LEN && id < RX_BUFS {
            let buf = nic.rx_bufs[id];
            let frame = core::slice::from_raw_parts((buf + NET_HDR_LEN as u64) as *const u8, total - NET_HDR_LEN);
            out.extend_from_slice(frame);
        }
        // Return the buffer to the device (make it available again).
        nic.rx.last_used = nic.rx.last_used.wrapping_add(1);
        let aidx = nic.rx.avail_idx_ptr().read();
        nic.rx.avail_ring(aidx % nic.rx.size).write(id as u16);
        compiler_fence(Ordering::SeqCst);
        nic.rx.avail_idx_ptr().write(aidx.wrapping_add(1));
        Port::<u16>::new(nic.io + VIRTIO_QUEUE_NOTIFY).write(0);
        Some(out)
    }
}
