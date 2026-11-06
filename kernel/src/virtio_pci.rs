//! Shared virtio PCI transport: **modern virtio 1.x** (register blocks found
//! through the vendor-specific PCI capabilities, in memory BARs) with the
//! **legacy 0.9.5** I/O-BAR transport as a fallback for devices that only
//! offer that.
//!
//! One `Transport` per device does the handshake of spec 3.1.1 (reset,
//! ACKNOWLEDGE, DRIVER, 64-bit feature negotiation, FEATURES_OK read back,
//! queues, DRIVER_OK), split-virtqueue setup with 64-bit ring addresses,
//! notification, ISR, device-config access (generation-checked on the modern
//! transport) and MSI-X vector assignment. The decisions themselves (which
//! capability, which features, ring layout) live in the host-tested
//! `eurovirtio` crate.
//!
//! Interrupts: MSI-X when the device has it (per queue, config change =
//! NO_VECTOR). Without MSI-X the drivers poll their used rings (they all do
//! already; the interrupt is an accelerator, never the only completion path).
//! INTx is not routed: that needs the DSDT `_PRT`, which EuroOS does not
//! evaluate yet.
//!
//! PCI configuration access goes through `pci::cfg_read32` (ECAM when a
//! verified MCFG exists, else 0xCF8/0xCFC); the virtio capabilities all lie in
//! the first 256 bytes, so a machine without MCFG works the same.
//!
//! Written from the OASIS virtio 1.x specification (sections 3.1, 4.1).

use core::sync::atomic::{fence, AtomicBool, Ordering};

use eurovirtio::{self as ev, common as cc, legacy as lg, status as st, RingLayout};
pub use eurovirtio::feat;
use x86_64::instructions::port::Port;

use crate::pci::{self, PciDevice};

/// Boot option `virtio.transport=legacy`: drive transitional devices through
/// the legacy I/O BAR instead of the modern transport (the old behaviour).
pub static PREFER_LEGACY: AtomicBool = AtomicBool::new(false);

unsafe fn r8(a: u64) -> u8 { core::ptr::read_volatile(a as *const u8) }
unsafe fn w8(a: u64, v: u8) { core::ptr::write_volatile(a as *mut u8, v) }
unsafe fn r16(a: u64) -> u16 { core::ptr::read_volatile(a as *const u16) }
unsafe fn w16(a: u64, v: u16) { core::ptr::write_volatile(a as *mut u16, v) }
unsafe fn r32(a: u64) -> u32 { core::ptr::read_volatile(a as *const u32) }
unsafe fn w32(a: u64, v: u32) { core::ptr::write_volatile(a as *mut u32, v) }
/// 64-bit common-cfg fields as two 32-bit accesses, low half first (spec 4.1.3.1
/// allows it; some devices accept nothing else).
unsafe fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

#[derive(Clone, Copy)]
enum Regs {
    Legacy { io: u16 },
    Modern { common: u64, notify: u64, notify_mult: u32, isr: u64, device: u64, device_len: u32 },
}

/// A split-virtqueue descriptor (spec 2.7.5).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// One split virtqueue in one contiguous region (identity-mapped: the address
/// is both the CPU pointer and the DMA address).
pub struct Queue {
    pub index: u16,
    pub size: u16,
    pub base: u64,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
    pub last_used: u16,
    /// Modern: the queue's notify register. Legacy: 0 (the I/O port is used).
    notify: u64,
}

impl Queue {
    pub fn desc(&self, i: u16) -> *mut Desc {
        (self.desc + (i as u64) * 16) as *mut Desc
    }
    pub fn avail_idx_ptr(&self) -> *mut u16 {
        (self.avail + 2) as *mut u16
    }
    pub fn avail_ring(&self, i: u16) -> *mut u16 {
        (self.avail + 4 + (i as u64) * 2) as *mut u16
    }
    pub fn used_idx(&self) -> u16 {
        unsafe { ((self.used + 2) as *const u16).read_volatile() }
    }
    pub fn used_elem(&self, i: u16) -> (u32, u32) {
        let p = (self.used + 4 + (i as u64) * 8) as *const u32;
        unsafe { (p.read_volatile(), p.add(1).read_volatile()) }
    }
    /// Publish descriptor head `head` in the avail ring (does not notify).
    pub fn push_avail(&mut self, head: u16) {
        unsafe {
            let idx = self.avail_idx_ptr().read_volatile();
            self.avail_ring(idx % self.size).write_volatile(head);
            fence(Ordering::SeqCst);
            self.avail_idx_ptr().write_volatile(idx.wrapping_add(1));
            fence(Ordering::SeqCst);
        }
    }
}

pub struct Transport {
    pub dev: PciDevice,
    regs: Regs,
    msix: bool,
    /// Negotiated feature bits (after `negotiate`).
    pub features: u64,
}

impl Transport {
    /// Bind the transport to `dev`: the modern register blocks when the device
    /// publishes them (and the BARs are reachable), else the legacy I/O BAR 0.
    /// Enables memory or I/O decoding plus bus mastering accordingly.
    pub fn probe(dev: &PciDevice) -> Option<Transport> {
        let status = (pci::cfg_read32(dev.bus, dev.dev, dev.func, 0x04) >> 16) as u16;
        let caps = ev::parse_caps(status, &|off| pci::cfg_read32(dev.bus, dev.dev, dev.func, off));
        let bar0 = dev.bar(0);
        let legacy_io = bar0 & 1 == 1 && bar0 & 0xFFFC != 0;
        let want_legacy = legacy_io && PREFER_LEGACY.load(Ordering::Relaxed);
        if caps.modern_usable() && !want_legacy {
            if let Some(t) = Self::modern(dev, &caps) {
                return Some(t);
            }
        }
        if legacy_io {
            dev.enable(0x5); // I/O + bus master
            return Some(Transport {
                dev: *dev,
                regs: Regs::Legacy { io: (bar0 & 0xFFFC) as u16 },
                msix: false,
                features: 0,
            });
        }
        crate::serial_println!(
            "[virtio] {:02x}:{:02x}.{} {:04x}:{:04x}: neither usable modern capabilities nor a legacy I/O BAR",
            dev.bus, dev.dev, dev.func, dev.vendor, dev.device
        );
        None
    }

    fn modern(dev: &PciDevice, caps: &ev::CapSet) -> Option<Transport> {
        // Memory decoding + bus master before touching the BARs (COMMAND.MEMORY).
        dev.enable(0x6);
        let map = |c: Option<ev::Cap>, min: u32| -> Option<(u64, u32)> {
            let c = c?;
            let base = dev.bar_addr(c.bar);
            if base == 0 || c.length < min {
                return None;
            }
            let phys = base + c.offset as u64;
            Some((crate::paging::mmio_virt(phys, c.length.max(1) as u64)?, c.length))
        };
        let (common, _) = map(caps.common, cc::MIN_LEN)?;
        let (notify, _) = map(caps.notify, 2)?;
        let (isr, _) = map(caps.isr, 1)?;
        let (device, device_len) = map(caps.device, 1).unwrap_or((0, 0));
        Some(Transport {
            dev: *dev,
            regs: Regs::Modern {
                common,
                notify,
                notify_mult: caps.notify.map(|c| c.notify_off_multiplier).unwrap_or(0),
                isr,
                device,
                device_len,
            },
            msix: false,
            features: 0,
        })
    }

    pub fn is_modern(&self) -> bool {
        matches!(self.regs, Regs::Modern { .. })
    }

    /// "modern virtio 1.x (MMIO)" / "legacy virtio 0.9.5 (I/O 0x....)", for logs.
    pub fn describe(&self) -> alloc::string::String {
        match self.regs {
            Regs::Modern { common, .. } => alloc::format!("modern virtio-1.x, common cfg @ {common:#x}"),
            Regs::Legacy { io } => alloc::format!("legacy virtio-0.9.5, I/O BAR {io:#06x}"),
        }
    }

    pub fn status(&self) -> u8 {
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => r8(common + cc::DEVICE_STATUS),
                Regs::Legacy { io } => Port::<u8>::new(io + lg::DEVICE_STATUS).read(),
            }
        }
    }

    pub fn set_status(&self, s: u8) {
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => w8(common + cc::DEVICE_STATUS, s),
                Regs::Legacy { io } => Port::<u8>::new(io + lg::DEVICE_STATUS).write(s),
            }
        }
    }

    /// Reset (status 0, wait until the device reads back 0), then ACKNOWLEDGE
    /// and DRIVER.
    pub fn begin(&self) {
        self.set_status(0);
        for _ in 0..1_000_000 {
            if self.status() == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        self.set_status(st::ACKNOWLEDGE);
        self.set_status(st::ACKNOWLEDGE | st::DRIVER);
    }

    pub fn device_features(&self) -> u64 {
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => {
                    w32(common + cc::DEVICE_FEATURE_SELECT, 0);
                    let lo = r32(common + cc::DEVICE_FEATURE) as u64;
                    w32(common + cc::DEVICE_FEATURE_SELECT, 1);
                    let hi = r32(common + cc::DEVICE_FEATURE) as u64;
                    hi << 32 | lo
                }
                Regs::Legacy { io } => Port::<u32>::new(io + lg::DEVICE_FEATURES).read() as u64,
            }
        }
    }

    /// Accept `offered & wanted` (+ VERSION_1 on modern), set FEATURES_OK and
    /// read it back. On refusal the device is marked FAILED and None returned.
    pub fn negotiate(&mut self, wanted: u64) -> Option<u64> {
        let offered = self.device_features();
        let acc = match ev::negotiate(offered, wanted, self.is_modern()) {
            Ok(a) => a,
            Err(_) => {
                self.set_status(st::FAILED);
                return None;
            }
        };
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => {
                    w32(common + cc::DRIVER_FEATURE_SELECT, 0);
                    w32(common + cc::DRIVER_FEATURE, acc as u32);
                    w32(common + cc::DRIVER_FEATURE_SELECT, 1);
                    w32(common + cc::DRIVER_FEATURE, (acc >> 32) as u32);
                    self.set_status(st::ACKNOWLEDGE | st::DRIVER | st::FEATURES_OK);
                    if self.status() & st::FEATURES_OK == 0 {
                        self.set_status(st::FAILED);
                        return None;
                    }
                }
                Regs::Legacy { io } => Port::<u32>::new(io + lg::DRIVER_FEATURES).write(acc as u32),
            }
        }
        self.features = acc;
        Some(acc)
    }

    pub fn has(&self, f: u64) -> bool {
        self.features & f != 0
    }

    /// Set up queue `index` with at most `cap` entries (modern may shrink the
    /// queue; legacy takes the device size). `alloc(pages)` returns a zeroed,
    /// physically contiguous region. `vector` = MSI-X table entry for this
    /// queue, or `NO_VECTOR`.
    pub fn setup_queue(&self, index: u16, cap: u16, alloc: &mut dyn FnMut(usize) -> Option<u64>) -> Option<Queue> {
        unsafe {
            match self.regs {
                Regs::Modern { common, notify, notify_mult, .. } => {
                    w16(common + cc::QUEUE_SELECT, index);
                    let max = r16(common + cc::QUEUE_SIZE);
                    let size = ev::pick_queue_size(max, cap, true);
                    if size == 0 {
                        return None;
                    }
                    if size != max {
                        w16(common + cc::QUEUE_SIZE, size);
                    }
                    let l = RingLayout::new(size);
                    let base = alloc(l.pages())?;
                    core::ptr::write_bytes(base as *mut u8, 0, l.pages() * 4096);
                    w64(common + cc::QUEUE_DESC, base + l.desc as u64);
                    w64(common + cc::QUEUE_DRIVER, base + l.avail as u64);
                    w64(common + cc::QUEUE_DEVICE, base + l.used as u64);
                    let off = r16(common + cc::QUEUE_NOTIFY_OFF);
                    let n = ev::notify_addr(notify, off, notify_mult);
                    w16(common + cc::QUEUE_ENABLE, 1);
                    Some(Queue {
                        index,
                        size,
                        base,
                        desc: base + l.desc as u64,
                        avail: base + l.avail as u64,
                        used: base + l.used as u64,
                        last_used: 0,
                        notify: n,
                    })
                }
                Regs::Legacy { io } => {
                    Port::<u16>::new(io + lg::QUEUE_SELECT).write(index);
                    let size: u16 = Port::<u16>::new(io + lg::QUEUE_SIZE).read();
                    if size == 0 || size == 0xFFFF {
                        return None;
                    }
                    let l = RingLayout::new(size);
                    let base = alloc(l.pages())?;
                    core::ptr::write_bytes(base as *mut u8, 0, l.pages() * 4096);
                    // The legacy QUEUE_PFN register is 32 bits of page number.
                    if base >> 12 > u32::MAX as u64 {
                        return None;
                    }
                    Port::<u32>::new(io + lg::QUEUE_PFN).write((base >> 12) as u32);
                    Some(Queue {
                        index,
                        size,
                        base,
                        desc: base + l.desc as u64,
                        avail: base + l.avail as u64,
                        used: base + l.used as u64,
                        last_used: 0,
                        notify: 0,
                    })
                }
            }
        }
    }

    /// Tell the device that queue `q` has new buffers.
    pub fn notify(&self, q: &Queue) {
        fence(Ordering::SeqCst);
        unsafe {
            match self.regs {
                Regs::Modern { .. } => w16(q.notify, q.index),
                Regs::Legacy { io } => Port::<u16>::new(io + lg::QUEUE_NOTIFY).write(q.index),
            }
        }
    }

    /// DRIVER_OK: the device is live.
    pub fn driver_ok(&self) {
        let fo = if self.is_modern() { st::FEATURES_OK } else { 0 };
        self.set_status(st::ACKNOWLEDGE | st::DRIVER | fo | st::DRIVER_OK);
    }

    /// Read (and so acknowledge) the ISR status byte.
    pub fn isr(&self) -> u8 {
        unsafe {
            match self.regs {
                Regs::Modern { isr, .. } => r8(isr),
                Regs::Legacy { io } => Port::<u8>::new(io + lg::ISR).read(),
            }
        }
    }

    /// Program MSI-X table entry `entry` → `vector` on `dest_apic` and turn
    /// MSI-X on. Returns the table size (0 = no MSI-X; the driver then polls).
    /// Legacy: afterwards the device config moves from 0x14 to 0x18.
    pub fn enable_msix(&mut self, entry: u16, vector: u8, dest_apic: u8) -> u16 {
        let n = crate::msix::enable(&self.dev, entry, vector, dest_apic);
        if n > 0 {
            self.msix = true;
        }
        n
    }

    /// Route queue `index` to MSI-X entry `entry` (NO_VECTOR to detach).
    /// Returns the read-back value (NO_VECTOR = the device refused).
    pub fn set_queue_vector(&self, index: u16, entry: u16) -> u16 {
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => {
                    w16(common + cc::QUEUE_SELECT, index);
                    w16(common + cc::QUEUE_MSIX_VECTOR, entry);
                    r16(common + cc::QUEUE_MSIX_VECTOR)
                }
                Regs::Legacy { io } => {
                    if !self.msix {
                        return ev::NO_VECTOR;
                    }
                    Port::<u16>::new(io + lg::QUEUE_SELECT).write(index);
                    Port::<u16>::new(io + lg::QUEUE_MSIX_VECTOR).write(entry);
                    Port::<u16>::new(io + lg::QUEUE_MSIX_VECTOR).read()
                }
            }
        }
    }

    /// Route configuration-change interrupts to MSI-X entry `entry`.
    pub fn set_config_vector(&self, entry: u16) -> u16 {
        unsafe {
            match self.regs {
                Regs::Modern { common, .. } => {
                    w16(common + cc::CONFIG_MSIX_VECTOR, entry);
                    r16(common + cc::CONFIG_MSIX_VECTOR)
                }
                Regs::Legacy { io } => {
                    if !self.msix {
                        return ev::NO_VECTOR;
                    }
                    Port::<u16>::new(io + lg::CONFIG_MSIX_VECTOR).write(entry);
                    Port::<u16>::new(io + lg::CONFIG_MSIX_VECTOR).read()
                }
            }
        }
    }

    pub fn msix_on(&self) -> bool {
        self.msix
    }

    fn legacy_cfg(&self, io: u16) -> u16 {
        io + if self.msix { lg::DEVICE_CFG_MSIX } else { lg::DEVICE_CFG }
    }

    fn generation(&self) -> u8 {
        match self.regs {
            Regs::Modern { common, .. } => unsafe { r8(common + cc::CONFIG_GENERATION) },
            Regs::Legacy { .. } => 0,
        }
    }

    /// Read a consistent snapshot of device config with `f` (spec 2.5.1:
    /// retry while config_generation changes underneath the read).
    fn consistent<T>(&self, f: impl Fn() -> T) -> T {
        loop {
            let g0 = self.generation();
            let v = f();
            if self.generation() == g0 {
                return v;
            }
        }
    }

    /// Device-config reads at byte offset `off`. Out of range → all ones.
    pub fn cfg8(&self, off: u16) -> u8 {
        self.consistent(|| unsafe {
            match self.regs {
                Regs::Modern { device, device_len, .. } => {
                    if device == 0 || off as u32 + 1 > device_len { 0xFF } else { r8(device + off as u64) }
                }
                Regs::Legacy { io } => Port::<u8>::new(self.legacy_cfg(io) + off).read(),
            }
        })
    }

    pub fn cfg16(&self, off: u16) -> u16 {
        self.consistent(|| unsafe {
            match self.regs {
                Regs::Modern { device, device_len, .. } => {
                    if device == 0 || off as u32 + 2 > device_len { 0xFFFF } else { r16(device + off as u64) }
                }
                Regs::Legacy { io } => Port::<u16>::new(self.legacy_cfg(io) + off).read(),
            }
        })
    }

    pub fn cfg32(&self, off: u16) -> u32 {
        self.consistent(|| unsafe {
            match self.regs {
                Regs::Modern { device, device_len, .. } => {
                    if device == 0 || off as u32 + 4 > device_len { 0xFFFF_FFFF } else { r32(device + off as u64) }
                }
                Regs::Legacy { io } => Port::<u32>::new(self.legacy_cfg(io) + off).read(),
            }
        })
    }

    /// 64-bit config field as two 32-bit reads under one generation.
    pub fn cfg64(&self, off: u16) -> u64 {
        self.consistent(|| {
            let lo = self.cfg32_raw(off) as u64;
            let hi = self.cfg32_raw(off + 4) as u64;
            hi << 32 | lo
        })
    }

    fn cfg32_raw(&self, off: u16) -> u32 {
        unsafe {
            match self.regs {
                Regs::Modern { device, device_len, .. } => {
                    if device == 0 || off as u32 + 4 > device_len { 0xFFFF_FFFF } else { r32(device + off as u64) }
                }
                Regs::Legacy { io } => Port::<u32>::new(self.legacy_cfg(io) + off).read(),
            }
        }
    }

    pub fn cfg_write8(&self, off: u16, v: u8) {
        unsafe {
            match self.regs {
                Regs::Modern { device, device_len, .. } => {
                    if device != 0 && (off as u32) < device_len {
                        w8(device + off as u64, v)
                    }
                }
                Regs::Legacy { io } => Port::<u8>::new(self.legacy_cfg(io) + off).write(v),
            }
        }
    }
}

/// All virtio functions (vendor 0x1AF4) of virtio device type `ty`
/// (1 net, 2 block, 16 gpu, 18 input, 25 sound), modern or transitional id.
pub fn find_all(ty: u16) -> alloc::vec::Vec<PciDevice> {
    pci::enumerate()
        .into_iter()
        .filter(|d| d.vendor == 0x1AF4 && ev::device_type(d.device) == Some(ty))
        .collect()
}
