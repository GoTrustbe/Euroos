//! EuroVirtio — the device-independent half of EuroOS's virtio PCI transport:
//! capability parsing, feature negotiation, split-ring layout, the virtio-net
//! header length and the virtio-input event decoder. `no_std`, host-testable.
//!
//! Written from the OASIS "Virtual I/O Device (VIRTIO) Version 1.x"
//! specification (sections 2, 4.1, 5.2, 5.1, 5.8). No code from other
//! operating systems is copied here.
//!
//! The kernel (`kernel/src/virtio_pci.rs`) does the register access; this crate
//! only decides *what* to read and write, so the decisions are unit-tested.

#![no_std]

pub mod input;

// ── Feature bits (spec 6, 5.1.3, 5.2.3) ──────────────────────────────────────
pub mod feat {
    /// Device-independent.
    pub const RING_INDIRECT_DESC: u64 = 1 << 28;
    pub const RING_EVENT_IDX: u64 = 1 << 29;
    pub const VERSION_1: u64 = 1 << 32;
    pub const ACCESS_PLATFORM: u64 = 1 << 33;
    pub const RING_PACKED: u64 = 1 << 34;
    // virtio-blk.
    pub const BLK_SIZE_MAX: u64 = 1 << 1;
    pub const BLK_SEG_MAX: u64 = 1 << 2;
    pub const BLK_RO: u64 = 1 << 5;
    pub const BLK_BLK_SIZE: u64 = 1 << 6;
    pub const BLK_FLUSH: u64 = 1 << 9;
    pub const BLK_DISCARD: u64 = 1 << 13;
    // virtio-net.
    pub const NET_CSUM: u64 = 1 << 0;
    pub const NET_GUEST_CSUM: u64 = 1 << 1;
    pub const NET_MAC: u64 = 1 << 5;
    pub const NET_GUEST_TSO4: u64 = 1 << 7;
    pub const NET_MRG_RXBUF: u64 = 1 << 15;
    pub const NET_STATUS: u64 = 1 << 16;
    pub const NET_CTRL_VQ: u64 = 1 << 17;
    pub const NET_MQ: u64 = 1 << 22;
}

// ── Device status bits (spec 2.1) ────────────────────────────────────────────
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const DEVICE_NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// `cfg_type` values of the virtio vendor capability (spec 4.1.4).
pub mod cfg {
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
    pub const PCI: u8 = 5;
}

/// virtio_pci_common_cfg field offsets (spec 4.1.4.3).
pub mod common {
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    pub const DEVICE_FEATURE: u64 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u64 = 0x08;
    pub const DRIVER_FEATURE: u64 = 0x0C;
    pub const CONFIG_MSIX_VECTOR: u64 = 0x10;
    pub const NUM_QUEUES: u64 = 0x12;
    pub const DEVICE_STATUS: u64 = 0x14;
    pub const CONFIG_GENERATION: u64 = 0x15;
    pub const QUEUE_SELECT: u64 = 0x16;
    pub const QUEUE_SIZE: u64 = 0x18;
    pub const QUEUE_MSIX_VECTOR: u64 = 0x1A;
    pub const QUEUE_ENABLE: u64 = 0x1C;
    pub const QUEUE_NOTIFY_OFF: u64 = 0x1E;
    pub const QUEUE_DESC: u64 = 0x20;
    pub const QUEUE_DRIVER: u64 = 0x28;
    pub const QUEUE_DEVICE: u64 = 0x30;
    /// Smallest common-cfg length that holds every field above.
    pub const MIN_LEN: u32 = 0x38;
}

/// Legacy (virtio 0.9.5) I/O BAR register offsets (spec 4.1.4.8).
pub mod legacy {
    pub const DEVICE_FEATURES: u16 = 0x00;
    pub const DRIVER_FEATURES: u16 = 0x04;
    pub const QUEUE_PFN: u16 = 0x08;
    pub const QUEUE_SIZE: u16 = 0x0C;
    pub const QUEUE_SELECT: u16 = 0x0E;
    pub const QUEUE_NOTIFY: u16 = 0x10;
    pub const DEVICE_STATUS: u16 = 0x12;
    pub const ISR: u16 = 0x13;
    pub const CONFIG_MSIX_VECTOR: u16 = 0x14;
    pub const QUEUE_MSIX_VECTOR: u16 = 0x16;
    /// Device config starts here without MSI-X, at 0x18 with MSI-X enabled.
    pub const DEVICE_CFG: u16 = 0x14;
    pub const DEVICE_CFG_MSIX: u16 = 0x18;
}

/// "No vector" value for the MSI-X vector registers.
pub const NO_VECTOR: u16 = 0xFFFF;

/// One parsed virtio vendor capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    /// Only meaningful for the notify capability.
    pub notify_off_multiplier: u32,
}

/// The capabilities a modern driver needs, first instance of each type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapSet {
    pub common: Option<Cap>,
    pub notify: Option<Cap>,
    pub isr: Option<Cap>,
    pub device: Option<Cap>,
    /// MSI-X capability offset in config space (id 0x11), if any.
    pub msix: Option<u8>,
}

impl CapSet {
    /// A device is usable through the modern transport when it publishes at
    /// least common, notify and ISR (device cfg is optional for some types).
    pub fn modern_usable(&self) -> bool {
        self.common.map_or(false, |c| c.length >= common::MIN_LEN)
            && self.notify.is_some()
            && self.isr.is_some()
    }
}

/// Walk the standard capability list through `read32(offset)` (config-space
/// dword reads, offset 4-byte aligned) and collect the virtio vendor caps.
/// `status` is the PCI status register (bit 4 = capability list present).
/// Bounded: at most 48 entries, pointers below 0x40 end the walk.
pub fn parse_caps(status: u16, read32: &dyn Fn(u8) -> u32) -> CapSet {
    let mut set = CapSet::default();
    if status & 0x10 == 0 {
        return set;
    }
    let mut ptr = (read32(0x34) & 0xFC) as u8;
    let mut guard = 0;
    while ptr >= 0x40 && guard < 48 {
        guard += 1;
        let w0 = read32(ptr);
        let id = (w0 & 0xFF) as u8;
        let next = ((w0 >> 8) & 0xFC) as u8;
        if id == 0x11 && set.msix.is_none() {
            set.msix = Some(ptr);
        }
        if id == 0x09 {
            let cap_len = ((w0 >> 16) & 0xFF) as u8;
            let cfg_type = ((w0 >> 24) & 0xFF) as u8;
            let bar = (read32(ptr + 4) & 0xFF) as u8;
            // Spec 4.1.4: a driver ignores caps with a reserved BAR number or
            // an unknown cfg_type, and uses the first one of each type.
            if bar <= 5 && cap_len >= 16 {
                let cap = Cap {
                    cfg_type,
                    bar,
                    offset: read32(ptr + 8),
                    length: read32(ptr + 12),
                    notify_off_multiplier: if cfg_type == cfg::NOTIFY && cap_len >= 20 {
                        read32(ptr + 16)
                    } else {
                        0
                    },
                };
                let slot = match cfg_type {
                    cfg::COMMON => Some(&mut set.common),
                    cfg::NOTIFY => Some(&mut set.notify),
                    cfg::ISR => Some(&mut set.isr),
                    cfg::DEVICE => Some(&mut set.device),
                    _ => None,
                };
                if let Some(s) = slot {
                    if s.is_none() {
                        *s = Some(cap);
                    }
                }
            }
        }
        ptr = next;
    }
    set
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NegotiateError {
    /// Modern transport, but the device does not offer VIRTIO_F_VERSION_1.
    NoVersion1,
}

/// Decide the driver features: only bits the driver implements (`wanted`) AND
/// the device offers. On the modern transport VERSION_1 is mandatory and always
/// accepted; on the legacy transport only the low 32 bits exist. Ring features
/// the drivers do not implement (EVENT_IDX, INDIRECT, PACKED) never pass even
/// when a caller lists them by mistake.
pub fn negotiate(offered: u64, wanted: u64, modern: bool) -> Result<u64, NegotiateError> {
    const NEVER: u64 = feat::RING_EVENT_IDX | feat::RING_INDIRECT_DESC | feat::RING_PACKED;
    let mut acc = offered & wanted & !NEVER;
    if modern {
        if offered & feat::VERSION_1 == 0 {
            return Err(NegotiateError::NoVersion1);
        }
        acc |= feat::VERSION_1;
    } else {
        acc &= 0xFFFF_FFFF & !feat::VERSION_1;
    }
    Ok(acc)
}

/// Byte layout of one split virtqueue in a single contiguous region (spec 2.7).
/// The legacy transport requires exactly this layout (used ring on the next
/// 4 KiB boundary, base passed as a page number); the modern transport takes
/// three addresses and accepts it as well, so both use one allocator path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingLayout {
    pub size: u16,
    pub desc: usize,
    pub avail: usize,
    pub used: usize,
    pub total: usize,
}

impl RingLayout {
    pub fn new(size: u16) -> RingLayout {
        let q = size as usize;
        let desc = 0;
        let avail = 16 * q;
        // avail: flags, idx, ring[q], used_event (2 bytes even when unused).
        let avail_end = avail + 6 + 2 * q;
        let used = (avail_end + 4095) & !4095;
        // used: flags, idx, ring[q] of 8 bytes, avail_event.
        let total = used + 6 + 8 * q;
        RingLayout { size, desc, avail, used, total }
    }
    pub fn pages(&self) -> usize {
        (self.total + 4095) / 4096
    }
}

/// Queue size the driver uses: the device maximum, capped to `cap` and rounded
/// down to a power of two (the split ring on the modern transport only accepts
/// powers of two; the legacy transport cannot change the size at all, so there
/// the device value is used as is). 0 = queue unavailable.
pub fn pick_queue_size(device_max: u16, cap: u16, modern: bool) -> u16 {
    if device_max == 0 {
        return 0;
    }
    if !modern {
        return device_max;
    }
    let want = device_max.min(cap.max(1));
    1u16 << (15 - want.leading_zeros() as u16)
}

/// Address of a queue's notify register (spec 4.1.4.4).
pub fn notify_addr(notify_base: u64, queue_notify_off: u16, multiplier: u32) -> u64 {
    notify_base + queue_notify_off as u64 * multiplier as u64
}

/// virtio-net header length (spec 5.1.6): 12 bytes (`virtio_net_hdr_v1`, with
/// `num_buffers`) once VERSION_1 or MRG_RXBUF is negotiated, else the 10-byte
/// legacy header.
pub fn net_hdr_len(negotiated: u64) -> usize {
    if negotiated & (feat::VERSION_1 | feat::NET_MRG_RXBUF) != 0 {
        12
    } else {
        10
    }
}

/// PCI device id → virtio device type: 0x1040+type (modern), 0x1000..0x103F
/// transitional (spec 4.1.2.1). Vendor must be 0x1AF4.
pub fn device_type(device_id: u16) -> Option<u16> {
    match device_id {
        0x1000 => Some(1), // net
        0x1001 => Some(2), // block
        0x1002 => Some(5), // balloon
        0x1003 => Some(3), // console
        0x1004 => Some(8), // scsi
        0x1005 => Some(4), // rng
        0x1009 => Some(9), // 9p
        0x1040..=0x107F => Some(device_id - 0x1040),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake config space as QEMU lays out a modern virtio-blk-pci: MSI-X cap
    /// at 0x98, then vendor caps common/isr/device/notify/pci-cfg.
    fn qemu_like() -> [u32; 64] {
        let mut c = [0u32; 64];
        c[0x34 / 4] = 0x98;
        c[0x98 / 4] = 0x0011_8411; // MSI-X, next 0x84
        let mut vcap = |at: usize, next: u8, len: u8, ty: u8, bar: u8, off: u32, l: u32| {
            c[at / 4] = 0x09 | (next as u32) << 8 | (len as u32) << 16 | (ty as u32) << 24;
            c[at / 4 + 1] = bar as u32;
            c[at / 4 + 2] = off;
            c[at / 4 + 3] = l;
        };
        vcap(0x84, 0x70, 20, cfg::PCI, 0, 0, 0);
        vcap(0x70, 0x60, 20, cfg::NOTIFY, 4, 0x3000, 0x1000);
        vcap(0x60, 0x50, 16, cfg::DEVICE, 4, 0x2000, 0x1000);
        vcap(0x50, 0x40, 16, cfg::ISR, 4, 0x1000, 0x1000);
        vcap(0x40, 0x00, 16, cfg::COMMON, 4, 0x0000, 0x1000);
        c[0x70 / 4 + 4] = 4; // notify_off_multiplier
        c
    }

    #[test]
    fn parses_qemu_layout() {
        let c = qemu_like();
        let s = parse_caps(0x10, &|o| c[o as usize / 4]);
        assert_eq!(s.msix, Some(0x98));
        assert_eq!(s.common.unwrap().bar, 4);
        assert_eq!(s.common.unwrap().offset, 0);
        assert_eq!(s.isr.unwrap().offset, 0x1000);
        assert_eq!(s.device.unwrap().offset, 0x2000);
        let n = s.notify.unwrap();
        assert_eq!((n.offset, n.notify_off_multiplier), (0x3000, 4));
        assert!(s.modern_usable());
    }

    #[test]
    fn kyvernon_layout_single_bar0() {
        // Everything in BAR 0, notify multiplier 0 (all queues share one register).
        let mut c = [0u32; 64];
        c[0x34 / 4] = 0x40;
        let caps = [(0x40usize, 0x58u8, cfg::COMMON, 0x0u32), (0x58, 0x70, cfg::NOTIFY, 0x1000), (0x70, 0x88, cfg::ISR, 0x2000), (0x88, 0xA0, cfg::DEVICE, 0x3000)];
        for (at, next, ty, off) in caps {
            c[at / 4] = 0x09 | (next as u32) << 8 | 20 << 16 | (ty as u32) << 24;
            c[at / 4 + 1] = 0;
            c[at / 4 + 2] = off;
            c[at / 4 + 3] = 0x1000;
            c[at / 4 + 4] = 0; // notify_off_multiplier (only read for NOTIFY)
        }
        c[0xA0 / 4] = 0x0000_0011 | 0x7 << 16; // MSI-X, 8 entries, end
        let s = parse_caps(0x10, &|o| c[o as usize / 4]);
        assert!(s.modern_usable());
        assert_eq!(s.notify.unwrap().notify_off_multiplier, 0);
        assert_eq!(s.msix, Some(0xA0));
        assert_eq!(notify_addr(0xFE00_1000, 3, 0), 0xFE00_1000);
        assert_eq!(notify_addr(0xFE00_1000, 3, 4), 0xFE00_100C);
    }

    #[test]
    fn legacy_device_has_no_modern_caps() {
        let mut c = [0u32; 64];
        c[0x34 / 4] = 0x40;
        c[0x40 / 4] = 0x0011; // only MSI-X
        let s = parse_caps(0x10, &|o| c[o as usize / 4]);
        assert!(!s.modern_usable());
        assert_eq!(parse_caps(0x00, &|o| c[o as usize / 4]), CapSet::default());
    }

    #[test]
    fn looping_cap_list_terminates() {
        let mut c = [0u32; 64];
        c[0x34 / 4] = 0x40;
        c[0x40 / 4] = 0x09 | 0x40 << 8 | 16 << 16 | (cfg::COMMON as u32) << 24; // points to itself
        let s = parse_caps(0x10, &|o| c[o as usize / 4]);
        assert!(s.common.is_some());
    }

    #[test]
    fn reserved_bar_is_ignored() {
        let mut c = [0u32; 64];
        c[0x34 / 4] = 0x40;
        c[0x40 / 4] = 0x09 | 0x50 << 8 | 16 << 16 | (cfg::COMMON as u32) << 24;
        c[0x44 / 4] = 7; // reserved BAR
        c[0x50 / 4] = 0x09 | 16 << 16 | (cfg::COMMON as u32) << 24;
        c[0x54 / 4] = 2;
        c[0x5C / 4] = 0x38;
        let s = parse_caps(0x10, &|o| c[o as usize / 4]);
        assert_eq!(s.common.unwrap().bar, 2);
    }

    #[test]
    fn negotiation() {
        let kyv_blk = feat::VERSION_1 | feat::RING_EVENT_IDX | feat::BLK_FLUSH | feat::BLK_SEG_MAX | feat::BLK_BLK_SIZE | feat::BLK_RO;
        let want = feat::BLK_FLUSH | feat::BLK_RO | feat::BLK_DISCARD | feat::RING_EVENT_IDX;
        let got = negotiate(kyv_blk, want, true).unwrap();
        assert_eq!(got, feat::VERSION_1 | feat::BLK_FLUSH | feat::BLK_RO);
        assert_eq!(negotiate(feat::BLK_FLUSH, want, true), Err(NegotiateError::NoVersion1));
        // Legacy: VERSION_1 never set, high bits dropped.
        assert_eq!(negotiate(kyv_blk, want | feat::VERSION_1, false).unwrap(), feat::BLK_FLUSH | feat::BLK_RO);
        // net: offloads not wanted are not accepted.
        let kyv_net = feat::VERSION_1 | feat::NET_CSUM | feat::NET_GUEST_CSUM | feat::NET_GUEST_TSO4 | feat::NET_MAC | feat::NET_MRG_RXBUF | feat::NET_MQ | feat::NET_CTRL_VQ | feat::NET_STATUS;
        let n = negotiate(kyv_net, feat::NET_MAC | feat::NET_STATUS, true).unwrap();
        assert_eq!(n, feat::VERSION_1 | feat::NET_MAC | feat::NET_STATUS);
        assert_eq!(net_hdr_len(n), 12);
        assert_eq!(net_hdr_len(feat::NET_MAC), 10);
        assert_eq!(net_hdr_len(feat::NET_MRG_RXBUF), 12);
    }

    #[test]
    fn ring_layout() {
        let l = RingLayout::new(256);
        assert_eq!(l.avail, 4096);
        assert_eq!(l.used, 8192);
        assert_eq!(l.total, 8192 + 6 + 2048);
        assert_eq!(l.pages(), 3);
        let s = RingLayout::new(8);
        assert_eq!((s.avail, s.used), (128, 4096));
    }

    #[test]
    fn queue_size() {
        assert_eq!(pick_queue_size(1024, 256, true), 256);
        assert_eq!(pick_queue_size(256, 256, true), 256);
        assert_eq!(pick_queue_size(100, 256, true), 64);
        assert_eq!(pick_queue_size(1024, 256, false), 1024);
        assert_eq!(pick_queue_size(0, 256, true), 0);
        assert_eq!(pick_queue_size(3, 256, true), 2);
    }

    #[test]
    fn device_types() {
        assert_eq!(device_type(0x1042), Some(2));
        assert_eq!(device_type(0x1001), Some(2));
        assert_eq!(device_type(0x1041), Some(1));
        assert_eq!(device_type(0x1052), Some(18));
        assert_eq!(device_type(0x1050), Some(16));
        assert_eq!(device_type(0x1110), None);
    }
}
