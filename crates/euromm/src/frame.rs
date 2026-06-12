//! Bitmap-gebaseerde fysieke frame-allocator.

use alloc::vec;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;

/// Een fysieke geheugenregio uit de firmware-geheugenkaart.
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

/// Bitmap-allocator: 1 bit per 4 KiB-frame, 64 frames per `u64`.
pub struct FrameAllocator {
    bitmap: Vec<u64>,
    total_frames: usize,
    free_frames: usize,
    /// Aantal frames dat bij init bruikbaar was (installeerd RAM, vóór allocaties).
    usable_total: usize,
    hint: usize,
    /// Aantal gedetecteerde double-frees (geheugen-hardening-diagnostiek, S6).
    double_frees: usize,
    /// Piek-gebruik (hoogste aantal tegelijk gealloceerde frames) — voor diagnostiek
    /// + capaciteitsplanning.
    high_water: usize,
}

impl FrameAllocator {
    /// Maak een allocator voor `total_frames` frames, allemaal als "in gebruik".
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

    /// Bouw vanuit de firmware-geheugenkaart: bruikbare regio's worden vrij,
    /// daarna worden de eerste `reserve_below` bytes (laag geheugen) en
    /// niet-bruikbare regio's gereserveerd.
    pub fn from_regions(regions: &[MemoryRegion], reserve_below: u64) -> Self {
        // Dimensioneer op de hoogste BRUIKBARE regio — niet op verre MMIO/reserved
        // adressen (die zouden de bitmap nodeloos enorm maken).
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
        // Reserveer laag geheugen (IVT/BIOS/kernel-image bescherming).
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

    /// Alloceer één frame; geeft het fysieke startadres terug.
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

    /// Alloceer `count` aaneengesloten frames.
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

    /// Alloceer `count` aaneengesloten frames met een startframe uitgelijnd op
    /// `align` frames (bv. `align = 512` → 2 MiB-grens). Geeft een uitgelijnd fysiek
    /// adres terug zonder de "over-alloceer-en-lijn-uit"-verspilling van
    /// [`allocate_contiguous`] (die tot `align-1` extra frames moet reserveren).
    pub fn allocate_aligned(&mut self, count: usize, align: usize) -> Result<u64, FrameError> {
        if count == 0 || align == 0 {
            return Err(FrameError::OutOfBounds);
        }
        let mut f = 0usize;
        while f + count <= self.total_frames {
            if f % align != 0 {
                f += align - (f % align); // spring naar de volgende uitgelijnde grens
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
            f += align; // volgende uitgelijnde grens
        }
        Err(FrameError::OutOfMemory)
    }

    /// Werk de piek-gebruik-teller (high-water) bij na een allocatie.
    fn note_high_water(&mut self) {
        let used = self.usable_total.saturating_sub(self.free_frames);
        if used > self.high_water {
            self.high_water = used;
        }
    }

    /// Aantal gedetecteerde double-frees (S6 hardening-diagnostiek).
    pub fn double_frees(&self) -> usize {
        self.double_frees
    }

    /// Piek-frame-gebruik (high-water) sinds boot.
    pub fn high_water_frames(&self) -> usize {
        self.high_water
    }

    /// Geef een frame vrij.
    pub fn free(&mut self, phys: u64) -> Result<(), FrameError> {
        let frame = (phys / PAGE_SIZE) as usize;
        if frame >= self.total_frames {
            return Err(FrameError::OutOfBounds);
        }
        let (w, b) = (frame / 64, frame % 64);
        if self.bitmap[w] & (1 << b) == 0 {
            self.double_frees += 1; // S6: tel double-frees (geheugen-hardening)
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
    /// Bruikbaar RAM bij init (installeerd, vóór allocaties).
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
            MemoryRegion { start: 0, len: 0x10_0000, usable: false }, // < 1 MiB gereserveerd
            MemoryRegion { start: 0x10_0000, len: 0x40_0000, usable: true }, // 4 MiB bruikbaar
        ]
    }

    #[test]
    fn init_telt_vrije_frames() {
        let a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        // 4 MiB bruikbaar = 1024 frames van 4 KiB.
        assert_eq!(a.total_frames(), (0x50_0000 / 4096) as usize);
        assert_eq!(a.free_frames(), 1024);
    }

    #[test]
    fn alloceren_geeft_oplopende_frames() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let f1 = a.allocate().unwrap();
        let f2 = a.allocate().unwrap();
        assert_eq!(f1, 0x10_0000); // eerste bruikbare frame
        assert_eq!(f2, 0x10_1000);
        assert_eq!(a.free_frames(), 1022);
    }

    #[test]
    fn free_en_heralloceren() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let f = a.allocate().unwrap();
        a.free(f).unwrap();
        assert_eq!(a.allocate().unwrap(), f); // hetzelfde frame terug
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
        // De volgende losse allocatie ligt direct erna.
        assert_eq!(a.allocate().unwrap(), 0x10_0000 + 8 * PAGE_SIZE);
    }

    #[test]
    fn aligned_allocatie() {
        let mut a = FrameAllocator::from_regions(&regions(), 0x10_0000);
        let p = a.allocate_aligned(4, 16).unwrap(); // 4 frames, 64 KiB-uitgelijnd
        assert_eq!(p % (16 * PAGE_SIZE), 0); // adres is uitgelijnd
        assert!(p >= 0x10_0000); // in de bruikbare regio
        let q = a.allocate_aligned(4, 16).unwrap();
        assert_eq!(q % (16 * PAGE_SIZE), 0);
        assert!(q >= p + 4 * PAGE_SIZE); // niet overlappend met p
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
