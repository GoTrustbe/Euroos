//! Device grants: the capability a userspace driver holds.
//!
//! A grant names one PCI function and exactly two memory windows: the MMIO
//! window (one BAR, page-rounded) the driver may map, and the DMA window the
//! device may reach through its IOMMU domain. The kernel builds a grant only
//! through [`DeviceGrant::new`], which refuses anything that is not
//! page-aligned, empty, too large, wraps, overlaps the other window, or
//! overlaps a range the kernel declares forbidden (RAM for the MMIO window,
//! for instance). The DMA window must lie in the identity-mapped 512 GiB
//! (IOVA == virtual == physical). An MMIO window may lie above it (QEMU puts
//! the 64-bit NVMe BAR near 768 GiB): the driver then sees it at
//! [`HIGH_MMIO_VA`] instead of at its physical address ([`DeviceGrant::mmio_va`]).

/// Page size of every mapping a grant produces.
pub const PAGE: u64 = 4096;
/// EuroOS identity-maps the lower 512 GiB; a DMA window must lie below it.
pub const IDENTITY_LIMIT: u64 = 512 << 30;
/// x86-64 physical addresses have at most 52 bits; an MMIO window must lie below.
pub const PHYS_LIMIT: u64 = 1 << 52;
/// Where a driver sees an MMIO window that lies above the identity map: a
/// virtual range of its own address space whose identity counterpart (508 GiB)
/// is no RAM and no device on any machine EuroOS runs on.
pub const HIGH_MMIO_VA: u64 = 508 << 30;
/// Largest MMIO window a grant may carry (a BAR larger than this is refused).
pub const MAX_MMIO: u64 = 16 << 20;
/// Largest DMA window a grant may carry, in pages.
pub const MAX_DMA_PAGES: u64 = 256;

/// A physical range `[base, base + len)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Window {
    pub base: u64,
    pub len: u64,
}

impl Window {
    pub const fn new(base: u64, len: u64) -> Window {
        Window { base, len }
    }

    /// The exclusive end, or None when it would wrap.
    pub fn end(&self) -> Option<u64> {
        self.base.checked_add(self.len)
    }

    /// Does `[addr, addr + len)` lie entirely inside this window?
    /// A zero-length access is never "inside" (it names no byte).
    pub fn contains(&self, addr: u64, len: u64) -> bool {
        let (Some(end), Some(aend)) = (self.end(), addr.checked_add(len)) else {
            return false;
        };
        len > 0 && addr >= self.base && aend <= end
    }

    pub fn overlaps(&self, o: &Window) -> bool {
        match (self.end(), o.end()) {
            (Some(a), Some(b)) => self.len > 0 && o.len > 0 && self.base < b && o.base < a,
            _ => true, // a wrapping window overlaps everything: refuse
        }
    }

    pub fn pages(&self) -> u64 {
        self.len / PAGE
    }

    /// Page addresses of the window, in order.
    pub fn page_addrs(&self) -> impl Iterator<Item = u64> {
        let base = self.base;
        (0..self.pages()).map(move |i| base + i * PAGE)
    }

    /// The smallest page-aligned window covering `[base, base + len)`.
    pub fn page_round(base: u64, len: u64) -> Option<Window> {
        let start = base & !(PAGE - 1);
        let end = base.checked_add(len)?.checked_add(PAGE - 1)? & !(PAGE - 1);
        Some(Window { base: start, len: end - start })
    }
}

/// A PCI function address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Bdf {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl Bdf {
    /// The requester id (source id) the IOMMU reports in a fault record.
    pub fn sid(&self) -> u16 {
        (self.bus as u16) << 8 | ((self.dev as u16 & 0x1f) << 3) | (self.func as u16 & 7)
    }

    pub fn from_sid(sid: u16) -> Bdf {
        Bdf { bus: (sid >> 8) as u8, dev: ((sid >> 3) & 0x1f) as u8, func: (sid & 7) as u8 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantError {
    Unaligned,
    Empty,
    TooLarge,
    AboveIdentity,
    Wraps,
    /// MMIO and DMA windows overlap each other.
    Overlap,
    /// A window overlaps a range the kernel declared off-limits.
    Forbidden,
}

/// What one driver instance may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceGrant {
    pub bdf: Bdf,
    pub mmio: Window,
    pub dma: Window,
    pub irq_vector: Option<u8>,
}

fn check_window(w: &Window, max: u64, limit: u64) -> Result<(), GrantError> {
    if w.len == 0 {
        return Err(GrantError::Empty);
    }
    if w.base % PAGE != 0 || w.len % PAGE != 0 {
        return Err(GrantError::Unaligned);
    }
    let end = w.end().ok_or(GrantError::Wraps)?;
    if w.len > max {
        return Err(GrantError::TooLarge);
    }
    if end > limit {
        return Err(GrantError::AboveIdentity);
    }
    Ok(())
}

impl DeviceGrant {
    /// Validate and build a grant. `mmio_forbidden` lists ranges the MMIO
    /// window must not touch (RAM: a BAR that overlaps RAM would hand RAM to
    /// user space); `dma_forbidden` ranges the DMA window must not touch
    /// (MMIO, the kernel image).
    pub fn new(
        bdf: Bdf,
        mmio: Window,
        dma: Window,
        irq_vector: Option<u8>,
        mmio_forbidden: &[Window],
        dma_forbidden: &[Window],
    ) -> Result<DeviceGrant, GrantError> {
        check_window(&mmio, MAX_MMIO, PHYS_LIMIT)?;
        check_window(&dma, MAX_DMA_PAGES * PAGE, IDENTITY_LIMIT)?;
        if mmio.overlaps(&dma) {
            return Err(GrantError::Overlap);
        }
        if mmio_forbidden.iter().any(|f| mmio.overlaps(f)) || dma_forbidden.iter().any(|f| dma.overlaps(f)) {
            return Err(GrantError::Forbidden);
        }
        Ok(DeviceGrant { bdf, mmio, dma, irq_vector })
    }

    /// The virtual address at which the driver sees the MMIO window: the
    /// physical address inside the identity map, [`HIGH_MMIO_VA`] above it.
    pub fn mmio_va(&self) -> u64 {
        match self.mmio.end() {
            Some(e) if e <= IDENTITY_LIMIT => self.mmio.base,
            _ => HIGH_MMIO_VA,
        }
    }

    /// May the driver access `[addr, addr + len)` through its MMIO mapping?
    pub fn may_mmio(&self, addr: u64, len: u64) -> bool {
        self.mmio.contains(addr, len)
    }

    /// Does the device's IOMMU domain allow a DMA to `[iova, iova + len)`?
    /// (The model of what `iommu::attach_runtime` programs: the DMA window, nothing else.)
    pub fn may_dma(&self, iova: u64, len: u64) -> bool {
        self.dma.contains(iova, len)
    }
}

/// Size of a 32-bit memory BAR from the value read back after writing all
/// ones (PCI Local Bus spec 6.2.5.1): clear the 4 flag bits, invert, add one.
/// None for an I/O BAR or an unimplemented (zero) BAR.
pub fn mem_bar_size(probe: u32) -> Option<u64> {
    if probe & 1 != 0 {
        return None; // I/O space
    }
    let mask = probe & !0xF;
    if mask == 0 {
        return None;
    }
    Some((!mask).wrapping_add(1) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAM: Window = Window::new(0, 0x8000_0000); // 2 GiB of RAM at 0
    const BDF: Bdf = Bdf { bus: 0, dev: 2, func: 0 };

    fn mmio() -> Window {
        Window::new(0xfeb8_0000, 0x2_0000)
    }
    fn dma() -> Window {
        Window::new(0x0123_4000, 32 * PAGE)
    }

    #[test]
    fn a_good_grant_builds_and_answers() {
        let g = DeviceGrant::new(BDF, mmio(), dma(), Some(0x4c), &[RAM], &[mmio()]).unwrap();
        assert!(g.may_mmio(0xfeb8_0000, 4));
        assert!(g.may_mmio(0xfeb9_fffc, 4));
        assert!(!g.may_mmio(0xfeb9_fffd, 4), "straddles the end");
        assert!(!g.may_mmio(0xfeba_0000, 4), "one byte past the BAR");
        assert!(!g.may_mmio(0xfeb7_fffc, 4), "below the BAR");
        assert!(g.may_dma(0x0123_4000, 2048));
        assert!(!g.may_dma(0x0123_3ff0, 32), "starts below the window");
        assert!(!g.may_dma(0x0125_4000, 1), "first byte past the window");
        assert!(!g.may_dma(0x1000, 4096), "kernel memory");
        assert_eq!(g.mmio.page_addrs().count(), 32);
        assert_eq!(g.dma.page_addrs().last(), Some(0x0123_4000 + 31 * PAGE));
    }

    #[test]
    fn bad_windows_are_refused() {
        let e = |m: Window, d: Window| DeviceGrant::new(BDF, m, d, None, &[], &[]).unwrap_err();
        assert_eq!(e(Window::new(0xfeb8_0001, 0x1000), dma()), GrantError::Unaligned);
        assert_eq!(e(Window::new(0xfeb8_0000, 0x10), dma()), GrantError::Unaligned);
        assert_eq!(e(Window::new(0xfeb8_0000, 0), dma()), GrantError::Empty);
        assert_eq!(e(Window::new(0xf000_0000, 32 << 20), dma()), GrantError::TooLarge);
        assert_eq!(e(mmio(), Window::new(IDENTITY_LIMIT, 0x1000)), GrantError::AboveIdentity);
        assert_eq!(e(Window::new(PHYS_LIMIT, 0x1000), dma()), GrantError::AboveIdentity);
        assert_eq!(e(Window::new(u64::MAX & !0xfff, 0x2000), dma()), GrantError::Wraps);
        assert_eq!(e(mmio(), Window::new(0x1000, (MAX_DMA_PAGES + 1) * PAGE)), GrantError::TooLarge);
        assert_eq!(e(mmio(), Window::new(0xfeb8_1000, 0x1000)), GrantError::Overlap);
    }

    #[test]
    fn a_high_bar_is_seen_at_the_high_mmio_va() {
        // QEMU's NVMe BAR near 768 GiB: allowed, mapped at HIGH_MMIO_VA.
        let high = Window::new(768 << 30, 0x4000);
        let g = DeviceGrant::new(BDF, high, dma(), None, &[RAM], &[high]).unwrap();
        assert_eq!(g.mmio_va(), HIGH_MMIO_VA);
        let low = DeviceGrant::new(BDF, mmio(), dma(), None, &[RAM], &[mmio()]).unwrap();
        assert_eq!(low.mmio_va(), mmio().base, "identity below 512 GiB, as before");
        // The high VA range is no RAM a grant could also hand out as DMA.
        assert!(HIGH_MMIO_VA + MAX_MMIO <= IDENTITY_LIMIT);
        assert!(!RAM.overlaps(&Window::new(HIGH_MMIO_VA, MAX_MMIO)));
    }

    #[test]
    fn a_bar_over_ram_is_refused() {
        // A misprogrammed BAR inside RAM would map RAM into the driver.
        let r = DeviceGrant::new(BDF, Window::new(0x1000_0000, 0x2_0000), dma(), None, &[RAM], &[]);
        assert_eq!(r.unwrap_err(), GrantError::Forbidden);
        // And a DMA window over the kernel image is refused too.
        let kernel = Window::new(0x0100_0000, 0x0100_0000);
        let r = DeviceGrant::new(BDF, mmio(), dma(), None, &[RAM], &[kernel]);
        assert_eq!(r.unwrap_err(), GrantError::Forbidden);
    }

    #[test]
    fn window_arithmetic_edges() {
        let w = Window::new(u64::MAX - 0xfff, 0x1000);
        assert_eq!(w.end(), None);
        assert!(!w.contains(u64::MAX - 0xfff, 1), "a wrapping window contains nothing");
        assert!(w.overlaps(&Window::new(0, 0x1000)), "a wrapping window overlaps everything");
        assert!(!Window::new(0, 0x1000).contains(0x10, 0), "zero-length access");
        assert!(!Window::new(0, 0x1000).contains(u64::MAX, 2), "access range wraps");
        assert!(!Window::new(0, 0x1000).overlaps(&Window::new(0x1000, 0x1000)), "adjacent");
        assert_eq!(Window::page_round(0xfeb8_0010, 0x10), Some(Window::new(0xfeb8_0000, 0x1000)));
        assert_eq!(Window::page_round(0x1fff, 2), Some(Window::new(0x1000, 0x2000)));
        assert_eq!(Window::page_round(u64::MAX - 1, 4), None);
    }

    #[test]
    fn bar_probe_decoding() {
        assert_eq!(mem_bar_size(0xfffe_0000), Some(0x2_0000)); // 128 KiB, the e1000 BAR0
        assert_eq!(mem_bar_size(0xffff_c008), Some(0x4000)); // prefetchable bit ignored
        assert_eq!(mem_bar_size(0xffff_ffe1), None); // I/O BAR
        assert_eq!(mem_bar_size(0), None); // unimplemented
    }

    #[test]
    fn sid_roundtrip() {
        let b = Bdf { bus: 3, dev: 0x1f, func: 7 };
        assert_eq!(b.sid(), 0x03ff);
        assert_eq!(Bdf::from_sid(b.sid()), b);
        assert_eq!(Bdf { bus: 0, dev: 2, func: 0 }.sid(), 0x0010);
    }
}
