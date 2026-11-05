//! virtio-net driver — EuroNet's in-kernel NIC on virtual machines.
//!
//! Runs over the shared virtio PCI transport (`virtio_pci.rs`): modern
//! virtio 1.x (1af4:1041, `disable-legacy=on`, Kyvernon) or legacy 0.9.5
//! (1af4:1000, `disable-modern=on`). Two virtqueues (RX=0, TX=1).
//!
//! Features accepted: VERSION_1 (modern), MAC, STATUS. Nothing else: no
//! checksum or segmentation offload (the stack computes its own checksums and
//! sends MTU-sized frames), no MRG_RXBUF (each RX buffer holds a whole frame),
//! no MQ / CTRL_VQ, no EVENT_IDX. With VERSION_1 the header in front of every
//! frame is the 12-byte `virtio_net_hdr_v1` (num_buffers always present, 1);
//! on the legacy transport without MRG_RXBUF it is the 10-byte header.
//!
//! `nic::init` tries this driver before the e1000 class, independent of
//! `net.driver`: `net.driver=user` only hands an *e1000* to the userspace
//! driver e1000d. A machine with virtio-net and no e1000 therefore gets the
//! kernel virtio-net driver (logged as `NIC: virtio-net`).
//!
//! Memory: the virtqueues + buffers come from the frame allocator. The kernel
//! identity-maps RAM (virt == phys), so a frame address is the DMA address.

use core::sync::atomic::{compiler_fence, Ordering};
use euromm::FrameAllocator;

use crate::virtio_pci::{feat, Queue, Transport, DESC_WRITE};

// 128 RX buffers of 2 KiB (256 KiB): a TLS flight plus an HTTP response fits
// before the ring is recycled. Never more than the queue has descriptors.
const RX_BUFS: usize = 128;
const BUF_SIZE: usize = 2048;

pub struct VirtioNet {
    tp: Transport,
    pub mac: [u8; 6],
    rx: Queue,
    tx: Queue,
    rx_bufs: [u64; RX_BUFS],
    nrx: usize,
    tx_buf: u64,
    hdr_len: usize,
}

static mut NIC: Option<VirtioNet> = None;
/// The receive queue raises MSI-X (set at init).
static RX_MSIX: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Does the receive queue interrupt (MSI-X)? Without it the network worker
/// polls the ring every tick.
pub fn rx_interrupts() -> bool {
    RX_MSIX.load(Ordering::Relaxed)
}

/// Find + initialize the first virtio-net device. Returns true on success.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    let Some(dev) = crate::virtio_pci::find_all(1).into_iter().next() else {
        crate::serial_println!("[net] no virtio-net device found");
        return false;
    };
    let Some(mut tp) = Transport::probe(&dev) else {
        return false;
    };
    crate::pci::claim(dev.bus, dev.dev, dev.func, "virtio-net"); // hwprobe (M1-3)
    crate::serial_println!("[net] virtio-net @ PCI {:02x}:{:02x}.{}, {}", dev.bus, dev.dev, dev.func, tp.describe());

    tp.begin();
    let Some(acc) = tp.negotiate(feat::NET_MAC | feat::NET_STATUS) else {
        crate::serial_println!("[net] virtio-net: feature negotiation refused (offered {:#x})", tp.device_features());
        return false;
    };
    let hdr_len = eurovirtio::net_hdr_len(acc);

    // MAC from the device config (offset 0), before MSI-X moves the legacy config.
    let mut mac = [0u8; 6];
    if acc & feat::NET_MAC != 0 {
        for (i, b) in mac.iter_mut().enumerate() {
            *b = tp.cfg8(i as u16);
        }
    } else {
        // No MAC from the device: a locally administered one (02:..).
        mac = [0x02, 0x45, 0x55, 0x52, dev.dev, dev.func];
    }

    let mut alloc = |pages: usize| falloc.allocate_contiguous(pages).ok();
    let Some(rx) = tp.setup_queue(0, 256, &mut alloc) else { return false };
    let Some(tx) = tp.setup_queue(1, 256, &mut alloc) else { return false };

    // RX buffers into the avail ring (device writes into them).
    let nrx = RX_BUFS.min(rx.size as usize);
    let mut rx_bufs = [0u64; RX_BUFS];
    unsafe {
        for i in 0..nrx {
            let Ok(buf) = falloc.allocate() else { return false };
            rx_bufs[i] = buf;
            let d = rx.desc(i as u16);
            (*d).addr = buf;
            (*d).len = BUF_SIZE as u32;
            (*d).flags = DESC_WRITE; // device → us
            (*d).next = 0;
            rx.avail_ring(i as u16).write_volatile(i as u16);
        }
        compiler_fence(Ordering::SeqCst);
        rx.avail_idx_ptr().write_volatile(nrx as u16);
    }
    let Ok(tx_buf) = falloc.allocate() else { return false };

    // MSI-X on the RECEIVE queue, before DRIVER_OK so the first frames are covered.
    let msix_n = tp.enable_msix(0, crate::interrupts::VIRTIO_NET_MSIX_VECTOR, crate::apic::lapic_id() as u8);
    if msix_n > 0 {
        let rb = tp.set_queue_vector(0, 0);
        RX_MSIX.store(rb != eurovirtio::NO_VECTOR, Ordering::Relaxed);
        tp.set_queue_vector(1, eurovirtio::NO_VECTOR);
        tp.set_config_vector(eurovirtio::NO_VECTOR);
        crate::serial_println!(
            "[net] virtio-net MSI-X on ({msix_n} entries) -> vector {:#x}, queue readback={rb:#06x}",
            crate::interrupts::VIRTIO_NET_MSIX_VECTOR
        );
    } else {
        crate::serial_println!("[net] virtio-net: no MSI-X, the receive ring is polled");
    }

    tp.driver_ok();
    tp.notify(&rx);
    crate::serial_println!(
        "[net] virtio-net OK — MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (RX {} bufs, {}-byte header, features {:#x})",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], nrx, hdr_len, acc
    );
    unsafe {
        NIC = Some(VirtioNet { tp, mac, rx, tx, rx_bufs, nrx, tx_buf, hdr_len });
    }
    true
}

/// The MAC address of the NIC (after init).
pub fn mac() -> Option<[u8; 6]> {
    unsafe { (*core::ptr::addr_of!(NIC)).as_ref().map(|n| n.mac) }
}

/// Send one Ethernet frame (virtio-net header prepended). Synchronous.
pub fn send(frame: &[u8]) -> bool {
    // IF=0 for the one TX descriptor: the network worker sends too, and a sender
    // parked by the timer mid-way would hand the device a half-written buffer.
    let _g = crate::sched::IfOffGuard::new();
    unsafe {
        let nic = match (*core::ptr::addr_of_mut!(NIC)).as_mut() {
            Some(n) => n,
            None => return false,
        };
        let hl = nic.hdr_len;
        if hl + frame.len() > BUF_SIZE {
            return false;
        }
        // Zero header: no checksum offload, no GSO (num_buffers is RX-only).
        core::ptr::write_bytes(nic.tx_buf as *mut u8, 0, hl);
        core::ptr::copy_nonoverlapping(frame.as_ptr(), (nic.tx_buf + hl as u64) as *mut u8, frame.len());
        let d = nic.tx.desc(0);
        (*d).addr = nic.tx_buf;
        (*d).len = (hl + frame.len()) as u32;
        (*d).flags = 0;
        (*d).next = 0;
        nic.tx.push_avail(0);
        nic.tp.notify(&nic.tx);
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

/// Poll one received frame (without the virtio-net header). None if there is nothing.
pub fn poll_recv() -> Option<alloc::vec::Vec<u8>> {
    // IF=0 for the ring manipulation. The card's MSI-X handler calls this too,
    // and an interrupt landing between reading the used ring and republishing the
    // buffer would corrupt both views of the queue.
    let _g = crate::sched::IfOffGuard::new();
    unsafe {
        let nic = (*core::ptr::addr_of_mut!(NIC)).as_mut()?;
        let used = nic.rx.used_idx();
        if used == nic.rx.last_used {
            return None;
        }
        let slot = nic.rx.last_used % nic.rx.size;
        let (id, len) = nic.rx.used_elem(slot);
        let id = id as usize;
        // The device-reported length must NEVER exceed the buffer (a malicious or
        // buggy device could otherwise make us read past it). Clamp to BUF_SIZE.
        let total = (len as usize).min(BUF_SIZE);
        let hl = nic.hdr_len;
        let mut out = alloc::vec::Vec::new();
        if total > hl && id < nic.nrx {
            let buf = nic.rx_bufs[id];
            let frame = core::slice::from_raw_parts((buf + hl as u64) as *const u8, total - hl);
            out.extend_from_slice(frame);
        }
        nic.rx.last_used = nic.rx.last_used.wrapping_add(1);
        // Return the buffer to the device (only a descriptor we own).
        if id < nic.nrx {
            nic.rx.push_avail(id as u16);
            nic.tp.notify(&nic.rx);
        }
        Some(out)
    }
}
