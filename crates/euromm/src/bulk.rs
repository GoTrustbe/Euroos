//! All-or-nothing allocation of several frames.
//!
//! A driver that needs a handful of frames (xHCI: DCBAA, command ring, event
//! ring, ERST; a process: PML4, PDPT, PD, PT) used to take them one by one with
//! `expect()`: on a machine that is out of frames that is a kernel panic, and a
//! version that returns early instead leaks the frames it already took. This
//! takes them together or not at all.

use crate::frame::{FrameAllocator, FrameError};

impl FrameAllocator {
    /// Allocate `N` single frames (not necessarily contiguous). Either all `N`
    /// are returned, or the allocator is left exactly as it was.
    pub fn allocate_many<const N: usize>(&mut self) -> Result<[u64; N], FrameError> {
        // Checked up front, so a refusal allocates nothing: no rollback, and no
        // entry in an active allocation trace (an IOMMU domain is built from it).
        if self.free_frames() < N {
            return Err(FrameError::OutOfMemory);
        }
        let mut out = [0u64; N];
        for i in 0..N {
            match self.allocate() {
                Ok(f) => out[i] = f,
                Err(e) => {
                    // Not reachable while free_frames is exact; kept so a future
                    // accounting bug degrades to a refusal and not to a leak.
                    for f in &out[..i] {
                        let _ = self.free(*f);
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::frame::{FrameAllocator, FrameError, MemoryRegion};

    fn pool(frames: u64) -> FrameAllocator {
        FrameAllocator::from_regions(&[MemoryRegion { start: 0x10_0000, len: frames * 4096, usable: true }], 0)
    }

    #[test]
    fn allocate_many_gives_distinct_frames() {
        let mut a = pool(8);
        let f: [u64; 4] = a.allocate_many().unwrap();
        for i in 0..4 {
            for j in 0..i {
                assert_ne!(f[i], f[j]);
            }
            assert_eq!(f[i] % 4096, 0);
        }
        assert_eq!(a.free_frames(), 4);
    }

    #[test]
    fn allocate_many_refuses_without_taking_anything() {
        // Three frames left, four wanted: the xHCI/process-table case at full RAM.
        let mut a = pool(3);
        a.trace_start();
        let r: Result<[u64; 4], _> = a.allocate_many();
        assert_eq!(r, Err(FrameError::OutOfMemory));
        assert_eq!(a.free_frames(), 3, "a refused request must not consume frames");
        assert!(a.trace_take().is_empty(), "a refused request must not appear in the DMA trace");
        // What is left is still usable.
        let _: [u64; 3] = a.allocate_many().unwrap();
        assert_eq!(a.free_frames(), 0);
        let r: Result<[u64; 1], _> = a.allocate_many();
        assert_eq!(r, Err(FrameError::OutOfMemory));
    }

    #[test]
    fn allocate_many_of_zero_is_fine_on_an_empty_pool() {
        let mut a = pool(0);
        let r: [u64; 0] = a.allocate_many().unwrap();
        assert!(r.is_empty());
    }
}
