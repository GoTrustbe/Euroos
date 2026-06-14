//! Bitmap-based physical frame allocator.

use alloc::vec;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;

/// A physical memory region from the firmware memory map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub start: u64,
    pub len: u64,
    pub usable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    OutOfMemory,
    DoubleFree,
    OutOfBounds,
}

/// Bitmap allocator: 1 bit per 4 KiB frame, 64 frames per `u64`.
pub struct FrameAllocator {
    bitmap: Vec<u64>,
    total_frames: usize,
    free_frames: usize,
    /// Number of frames that were usable at init (installed RAM, before allocations).
    usable_total: usize,
    hint: usize,
    /// Number of detected double-frees (memory-hardening diagnostics, S6).
    double_frees: usize,
    /// Peak usage (highest number of simultaneously allocated frames) — for diagnostics
    /// + capacity planning.
    high_water: usize,
}

impl FrameAllocator {
    /// Create an allocator for `total_frames` frames, all marked as "in use".
    pub fn new(total_frames: usize) -> Self {
        let words = total_frames.div_ceil(64);
        Self {
            bitmap: vec![u64::MAX; words],
            total_frames,
            free_frames: 0,
            usable_total: 0,
            hint: 0,
            double_frees: 0,
            high_water: 0,
        }
    }

    /// Build from the firmware memory map: usable regions become free,
    /// then the first `reserve_below` bytes (low memory) and
    /// non-usable regions are reserved.
    pub fn from_regions(regions: &[MemoryRegion], reserve_below: u64) -> Self {
        // Size on the highest USABLE region — not on distant MMIO/reserved
        // addresses (which would needlessly make the bitmap enormous).
        let highest = regions
            .iter()
            .filter(|r| r.usable)
            .map(|r| r.start + r.len)
            .max()
            .unwrap_or(0);
        let total_frames = (highest / PAGE_SIZE) as usize;
        let mut a = Self::new(total_frames);
        for r in regions.iter().filter(|r| r.usable) {
            a.set_range(r.start, r.len, false);
        }
        for r in regions.iter().filter(|r| !r.usable) {
            a.set_range(r.start, r.len, true);
        }
        // Reserve low memory (IVT/BIOS/kernel-image protection).
        a.set_range(0, reserve_below, true);
        a.recount();
        a.usable_total = a.free_frames;
        a
    }

    fn set_range(&mut self, start: u64, len: u64, used: bool) {
        let first = (start / PAGE_SIZE) as usize;
        let count = (len / PAGE_SIZE) as usize;
        for f in first..(first + count).min(self.total_frames) {
            let (w, b) = (f / 64, f % 64);
            if used {
                self.bitmap[w] |= 1 << b;
            } else {
                self.bitmap[w] &= !(1u64 << b);
            }
        }
    }

    fn recount(&mut self) {
        let mut free = 0;
        for f in 0..self.total_frames {
            if self.bitmap[f / 64] & (1 << (f % 64)) == 0 {
                free += 1;
            }
        }
        self.free_frames = free;
    }

    /// Allocate one frame; returns the physical start address.
    pub fn allocate(&mut self) -> Result<u64, FrameError> {
        for w in (self.hint..self.bitmap.len()).chain(0..self.hint) {
            if self.bitmap[w] != u64::MAX {
                let b = self.bitmap[w].trailing_ones() as usize;
                let frame = w * 64 + b;
                if frame >= self.total_frames {
                    continue;
                }
                self.bitmap[w] |= 1 << b;
                self.free_frames -= 1;
                self.note_high_water();
                self.hint = w;
                return Ok(frame as u64 * PAGE_SIZE);
            }
        }
        Err(FrameError::OutOfMemory)
    }

    /// Allocate `count` contiguous frames.
    pub fn allocate_contiguous(&mut self, count: usize) -> Result<u64, FrameError> {
        if count == 0 {
            return Err(FrameError::OutOfBounds);
        }
        let mut f = 0;
        'outer: while f + count <= self.total_frames {
            for j in 0..count {
                if self.bitmap[(f + j) / 64] & (1 << ((f + j) % 64)) != 0 {
                    f += j + 1;
                    continue 'outer;
                }
            }
            for j in 0..count {
                self.bitmap[(f + j) / 64] |= 1 << ((f + j) % 64);
            }
            self.free_frames -= count;
            self.note_high_water();
            return Ok(f as u64 * PAGE_SIZE);
        }
        Err(FrameError::OutOfMemory)
    }

    /// Allocate `count` contiguous frames with a start frame aligned to
    /// `align` frames (e.g. `align = 512` → 2 MiB boundary). Returns an aligned physical
    /// address without the "over-allocate-and-align" waste of
    /// [`allocate_contiguous`] (which has to reserve up to `align-1` extra frames).
    pub fn allocate_aligned(&mut self, count: usize, align: usize) -> Result<u64, FrameError> {
        if count == 0 || align == 0 {
            return Err(FrameError::OutOfBounds);
        }
        let mut f = 0usize;
        while f + count <= self.total_frames {
            if f % align != 0 {
                f += align - (f % align); // jump to the next aligned boundary
                continue;
            }
            let mut free = true;
            for j in 0..count {
                if self.bitmap[(f + j) / 64] & (1 << ((f + j) % 64)) != 0 {
                    free = false;
                    break;
                }
            }
            if free {
                for j in 0..count {
                    self.bitmap[(f + j) / 64] |= 1 << ((f + j) % 64);
                }
                self.free_frames -= count;
                self.note_high_water();
                return Ok(f as u64 * PAGE_SIZE);
            }
            f += align; // next aligned boundary
        }
        Err(FrameError::OutOfMemory)
    }

    /// Update the peak-usage counter (high-water) after an allocation.
    fn note_high_water(&mut self) {
        let used = self.usable_total.saturating_sub(self.free_frames);
        if used > self.high_water {
            self.high_water = used;
        }
    }

    /// Number of detected double-frees (S6 hardening diagnostics).
    pub fn double_frees(&self) -> usize {
        self.double_frees
    }

    /// Peak frame usage (high-water) since boot.
    pub fn high_water_frames(&self) -> usize {
        self.high_water
    }

    /// Free a frame.
    pub fn free(&mut self, phys: u64) -> Result<(), FrameError> {
        let frame = (phys / PAGE_SIZE) as usize;
        if frame >= self.total_frames {
            return Err(FrameError::OutOfBounds);
        }
        let (w, b) = (frame / 64, frame % 64);
        if self.bitmap[w] & (1 << b) == 0 {
            self.double_frees += 1; // S6: count double-frees (memory hardening)
            return Err(FrameError::DoubleFree);
        }
        self.bitmap[w] &= !(1u64 << b);
        self.free_frames += 1;
        if w < self.hint {
            self.hint = w;
        }
        Ok(())
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }
    pub fn free_frames(&self) -> usize {
        self.free_frames
    }
    pub fn used_frames(&self) -> usize {
        self.total_frames - self.free_frames
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_frames as u64 * PAGE_SIZE
    }
    pub fn free_bytes(&self) -> u64 {
        self.free_frames as u64 * PAGE_SIZE
    }
    /// Usable RAM at init (installed, before allocations).
    pub fn usable_frames(&self) -> usize {
        self.usable_total
    }
    pub fn usable_bytes(&self) -> u64 {
        self.usable_total as u64 * PAGE_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Vec<MemoryRegion> {
        vec![
            MemoryRegion { start: 0, len: 0x10_0000, usable: false }, // < 1 MiB reserved
            MemoryRegion { start: 0x10_0000, len: 0x40_0000, usable: true }, // 4 MiB usable
        ]
    }

    #[test]
    fn init_telt_vrije_frames() {
        let a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        // 4 MiB usable = 1024 frames of 4 KiB.
        assert_eq!(a.total_frames(), (0x50_0000 / 4096) as usize);
        assert_eq!(a.free_frames(), 1024);
    }

    #[test]
    fn alloceren_geeft_oplopende_frames() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let f1 = a.allocate().unwrap();
        let f2 = a.allocate().unwrap();
        assert_eq!(f1, 0x10_0000); // first usable frame
        assert_eq!(f2, 0x10_1000);
        assert_eq!(a.free_frames(), 1022);
    }

    #[test]
    fn free_en_heralloceren() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let f = a.allocate().unwrap();
        a.free(f).unwrap();
        assert_eq!(a.allocate().unwrap(), f); // the same frame back
    }

    #[test]
    fn dubbele_free_gedetecteerd() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let f = a.allocate().unwrap();
        a.free(f).unwrap();
        assert_eq!(a.free(f), Err(FrameError::DoubleFree));
    }

    #[test]
    fn contiguous_allocatie() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let start = a.allocate_contiguous(8).unwrap();
        assert_eq!(start, 0x10_0000);
        assert_eq!(a.free_frames(), 1024 - 8);
        // The next single allocation lies directly after it.
        assert_eq!(a.allocate().unwrap(), 0x10_0000 + 8 * PAGE_SIZE);
    }

    #[test]
    fn aligned_allocatie() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let p = a.allocate_aligned(4, 16).unwrap(); // 4 frames, 64 KiB aligned
        assert_eq!(p % (16 * PAGE_SIZE), 0); // address is aligned
        assert!(p >= 0x10_0000); // in the usable region
        let q = a.allocate_aligned(4, 16).unwrap();
        assert_eq!(q % (16 * PAGE_SIZE), 0);
        assert!(q >= p + 4 * PAGE_SIZE); // not overlapping with p
        assert_eq!(a.free_frames(), 1024 - 8);
    }

    #[test]
    fn aligned_faalt_correct() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        assert_eq!(a.allocate_aligned(100_000, 512), Err(FrameError::OutOfMemory));
        assert_eq!(a.allocate_aligned(4, 0), Err(FrameError::OutOfBounds));
        assert_eq!(a.allocate_aligned(0, 16), Err(FrameError::OutOfBounds));
    }

    #[test]
    fn out_of_memory() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        for _ in 0..1024 {
            a.allocate().unwrap();
        }
        assert_eq!(a.allocate(), Err(FrameError::OutOfMemory));
        assert_eq!(a.free_frames(), 0);
    }
}
