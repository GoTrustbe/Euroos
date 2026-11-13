//! Free address ranges of a bump-allocated virtual region (W21).
//!
//! The kernel hands out the demand-paged region of a process with a bump
//! pointer. Without a way back, every munmap left its addresses behind for good:
//! a YouTube renderer had used 403 of 480 GiB of addresses within minutes, and
//! V8 got ENOMEM for a 512 KiB zone segment while the region was nearly empty.
//! `FreeRanges` keeps the ranges given back, sorted and merged, and answers an
//! allocation from them (first fit, with alignment) before the bump pointer
//! moves. `no_std`, no unsafe; the kernel holds it behind its own lock.

use alloc::vec::Vec;

/// Disjoint, non-adjacent, sorted half-open ranges `[start, end)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FreeRanges {
    r: Vec<(u64, u64)>,
}

impl FreeRanges {
    pub const fn new() -> Self {
        Self { r: Vec::new() }
    }

    pub fn clear(&mut self) {
        self.r.clear();
    }

    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.r
    }

    /// Bytes in all ranges.
    pub fn total(&self) -> u64 {
        self.r.iter().map(|&(s, e)| e - s).sum()
    }

    /// Give `[start, end)` back. Overlap with ranges already free is allowed
    /// (a range unmapped twice); the result is merged with its neighbours.
    pub fn insert(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let (mut s, mut e) = (start, end);
        // First range that ends at or after `s` (could touch or overlap).
        let i = self.r.partition_point(|&(_, re)| re < s);
        let mut j = i;
        while j < self.r.len() && self.r[j].0 <= e {
            s = s.min(self.r[j].0);
            e = e.max(self.r[j].1);
            j += 1;
        }
        self.r.splice(i..j, [(s, e)]);
    }

    /// Take `[start, end)` out again (a MAP_FIXED mapping placed there).
    pub fn remove(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let i = self.r.partition_point(|&(_, re)| re <= start);
        let mut j = i;
        let mut keep: [(u64, u64); 2] = [(0, 0); 2];
        let mut nk = 0;
        while j < self.r.len() && self.r[j].0 < end {
            let (rs, re) = self.r[j];
            if rs < start {
                keep[nk] = (rs, start);
                nk += 1;
            }
            if re > end {
                keep[nk] = (end, re);
                nk += 1;
            }
            j += 1;
        }
        self.r.splice(i..j, keep[..nk].iter().copied());
    }

    /// An address for `len` bytes aligned to `align` (a power of two), taken
    /// from the lowest range that holds it; None when no range does.
    pub fn take(&mut self, len: u64, align: u64) -> Option<u64> {
        if len == 0 || !align.is_power_of_two() {
            return None;
        }
        for k in 0..self.r.len() {
            let (s, e) = self.r[k];
            let a = s.checked_add(align - 1)? & !(align - 1);
            let Some(end) = a.checked_add(len) else { continue };
            if end <= e {
                self.remove(a, end);
                return Some(a);
            }
        }
        None
    }

    /// Ranges that end at `top` shrink `top` (the bump pointer can come back
    /// down over them): returns the new top.
    pub fn lower_top(&mut self, top: u64) -> u64 {
        match self.r.last() {
            Some(&(s, e)) if e == top => {
                self.r.pop();
                s
            }
            _ => top,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_merges_and_sorts() {
        let mut f = FreeRanges::new();
        f.insert(0x5000, 0x6000);
        f.insert(0x1000, 0x2000);
        f.insert(0x2000, 0x3000); // touches
        f.insert(0x5800, 0x7000); // overlaps
        assert_eq!(f.ranges(), &[(0x1000, 0x3000), (0x5000, 0x7000)]);
        f.insert(0x3000, 0x5000); // bridges
        assert_eq!(f.ranges(), &[(0x1000, 0x7000)]);
        assert_eq!(f.total(), 0x6000);
    }

    #[test]
    fn remove_splits() {
        let mut f = FreeRanges::new();
        f.insert(0x1000, 0x9000);
        f.remove(0x3000, 0x4000);
        assert_eq!(f.ranges(), &[(0x1000, 0x3000), (0x4000, 0x9000)]);
        f.remove(0x0, 0x2000);
        f.remove(0x8000, 0xa000);
        assert_eq!(f.ranges(), &[(0x2000, 0x3000), (0x4000, 0x8000)]);
        f.remove(0x2800, 0x5000); // across two
        assert_eq!(f.ranges(), &[(0x2000, 0x2800), (0x5000, 0x8000)]);
        f.remove(0x0, 0x10000);
        assert!(f.ranges().is_empty());
    }

    #[test]
    fn take_first_fit_with_alignment() {
        let mut f = FreeRanges::new();
        f.insert(0x1000, 0x3000);
        f.insert(0x10000, 0x40000);
        assert_eq!(f.take(0x4000, 0x1000), Some(0x10000));
        assert_eq!(f.take(0x1000, 0x1000), Some(0x1000));
        assert_eq!(f.take(0x10000, 0x10000), Some(0x20000));
        assert_eq!(f.ranges(), &[(0x2000, 0x3000), (0x14000, 0x20000), (0x30000, 0x40000)]);
        assert_eq!(f.take(0x20000, 0x1000), None);
        assert_eq!(f.take(0x1000, 3), None);
    }

    #[test]
    fn lower_top() {
        let mut f = FreeRanges::new();
        f.insert(0x1000, 0x2000);
        f.insert(0x8000, 0x9000);
        assert_eq!(f.lower_top(0x9000), 0x8000);
        assert_eq!(f.lower_top(0x8000), 0x8000);
        assert_eq!(f.ranges(), &[(0x1000, 0x2000)]);
    }

    #[test]
    fn model_against_a_page_bitmap() {
        // Random insert/remove/take against a bitmap of 256 pages.
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut f = FreeRanges::new();
        let mut bits = [false; 256];
        for _ in 0..20000 {
            let a = rnd(256);
            let b = (a + 1 + rnd(16)).min(256);
            match rnd(3) {
                0 => {
                    f.insert(a * 4096, b * 4096);
                    for p in a..b { bits[p as usize] = true; }
                }
                1 => {
                    f.remove(a * 4096, b * 4096);
                    for p in a..b { bits[p as usize] = false; }
                }
                _ => {
                    let len = 1 + rnd(8);
                    let got = f.take(len * 4096, 4096);
                    let first = (0..=256 - len).find(|&p| (p..p + len).all(|q| bits[q as usize]));
                    // first fit by range = first fit by page for contiguous runs
                    assert_eq!(got, first.map(|p| p * 4096));
                    if let Some(p) = first {
                        for q in p..p + len { bits[q as usize] = false; }
                    }
                }
            }
            let mut want = alloc::vec::Vec::new();
            let mut p = 0;
            while p < 256 {
                if bits[p] {
                    let s = p;
                    while p < 256 && bits[p] { p += 1; }
                    want.push((s as u64 * 4096, p as u64 * 4096));
                } else {
                    p += 1;
                }
            }
            assert_eq!(f.ranges(), &want[..]);
        }
    }
}
