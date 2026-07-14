//! NIC dispatch (Metal M3-1): one polled ethernet surface for EuroNet, backed
//! by whichever driver binds — virtio-net (VMs, tried first: it is the tuned
//! path for our own test/e2e setups) or the Intel e1000/e1000e class driver
//! (modern wired metal, and q35's default NIC). The whole network stack
//! (DHCP/ARP/ICMP/DNS/TCP/TLS) runs unchanged on either.

use core::sync::atomic::{AtomicU8, Ordering};

use euromm::FrameAllocator;

const KIND_NONE: u8 = 0;
const KIND_VIRTIO: u8 = 1;
const KIND_E1000: u8 = 2;
const KIND_USBNET: u8 = 3;

static KIND: AtomicU8 = AtomicU8::new(KIND_NONE);

/// Initialize the first NIC that binds. Returns true when one is up.
pub fn init(falloc: &mut FrameAllocator) -> bool {
    if crate::virtio_net::init(falloc) {
        KIND.store(KIND_VIRTIO, Ordering::Relaxed);
        return true;
    }
    if crate::e1000::init(falloc) {
        KIND.store(KIND_E1000, Ordering::Relaxed);
        return true;
    }
    // CDC-ECM USB ethernet (M3-3) — already brought up during xHCI init.
    if crate::xhci::usbnet_present() {
        KIND.store(KIND_USBNET, Ordering::Relaxed);
        return true;
    }
    false
}

/// Late rebind (M3-3): USB ethernet only exists after xHCI enumeration, which
/// runs AFTER the main net bring-up. When nothing bound earlier, adopt the
/// CDC-ECM function now. Returns true when usbnet was adopted.
pub fn late_bind_usbnet() -> bool {
    if KIND.load(Ordering::Relaxed) == KIND_NONE && crate::xhci::usbnet_present() {
        KIND.store(KIND_USBNET, Ordering::Relaxed);
        return true;
    }
    false
}

/// Human-readable name of the bound driver (for the boot report / hwprobe).
pub fn kind() -> &'static str {
    match KIND.load(Ordering::Relaxed) {
        KIND_VIRTIO => "virtio-net",
        KIND_E1000 => "e1000",
        KIND_USBNET => "usb-ethernet (CDC-ECM)",
        _ => "none",
    }
}

pub fn mac() -> Option<[u8; 6]> {
    match KIND.load(Ordering::Relaxed) {
        KIND_VIRTIO => crate::virtio_net::mac(),
        KIND_E1000 => crate::e1000::mac(),
        KIND_USBNET => crate::xhci::usbnet_mac(),
        _ => None,
    }
}

pub fn send(frame: &[u8]) -> bool {
    match KIND.load(Ordering::Relaxed) {
        KIND_VIRTIO => crate::virtio_net::send(frame),
        KIND_E1000 => crate::e1000::send(frame),
        KIND_USBNET => crate::xhci::usbnet_send(frame),
        _ => false,
    }
}

pub fn poll_recv() -> Option<alloc::vec::Vec<u8>> {
    match KIND.load(Ordering::Relaxed) {
        KIND_VIRTIO => crate::virtio_net::poll_recv(),
        KIND_E1000 => crate::e1000::poll_recv(),
        KIND_USBNET => crate::xhci::usbnet_poll_recv(),
        _ => None,
    }
}
