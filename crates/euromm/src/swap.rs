//! Swap subsystem core (plan J3): under memory pressure, anonymous pages are
//! written to a swap partition/file and read back in on access.
//!
//! This module is the architecture-independent core: a **swap-slot allocator**
//! (which swap blocks are free/used) + a **CLOCK (second-chance) page-
//! replacement** policy (which frame is chosen as victim to swap out). The
//! actual page I/O + PTE manipulation is kernel work that builds on this. Pure
//! `no_std` logic → fully host-tested, independent of any memory.

use alloc::vec;
use alloc::vec::Vec;

/// Manages the occupancy of a swap area with `n` slots (each = one page).
pub struct SwapArea {
    used: Vec<bool>,
    used_count: usize,
}

impl SwapArea {
    pub fn new(slots: usize) -> Self {
        SwapArea {
            used: vec![false; slots],
            used_count: 0,
        }
    }

    /// Reserve a free swap slot; `None` if the area is full.
    pub fn alloc(&mut self) -> Option<usize> {
        let s = self.used.iter().position(|&u| !u)?;
        self.used[s] = true;
        self.used_count += 1;
        Some(s)
    }

    /// Free a swap slot (the page has been read back in).
    pub fn free(&mut self, slot: usize) {
        if slot < self.used.len() && self.used[slot] {
            self.used[slot] = false;
            self.used_count -= 1;
        }
    }

    pub fn capacity(&self) -> usize {
        self.used.len()
    }
    pub fn used(&self) -> usize {
        self.used_count
    }
    pub fn free_count(&self) -> usize {
        self.used.len() - self.used_count
    }
}

/// One managed frame in the CLOCK policy: a physical frame address + reference bit.
struct Page {
    frame: u64,
    referenced: bool,
}

/// CLOCK / second-chance page replacement. Frames sit in a ring; the "hand"
/// rotates around: a frame with its reference bit set gets a second chance (bit
/// cleared), a frame with a cleared bit becomes the victim. This way CLOCK
/// approximates LRU at O(1) cost and without per-access bookkeeping.
pub struct Clock {
    pages: Vec<Page>,
    hand: usize,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    pub fn new() -> Self {
        Clock { pages: Vec::new(), hand: 0 }
    }

    /// Take a new frame under management (brought in → reference bit set).
    pub fn insert(&mut self, frame: u64) {
        self.pages.push(Page { frame, referenced: true });
    }

    /// Mark a frame as recently used (set its reference bit).
    pub fn touch(&mut self, frame: u64) {
        if let Some(p) = self.pages.iter_mut().find(|p| p.frame == frame) {
            p.referenced = true;
        }
    }

    /// Number of managed frames.
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Pick + remove a victim frame to swap out. Returns the frame address, or
    /// `None` if nothing is being managed.
    pub fn evict(&mut self) -> Option<u64> {
        if self.pages.is_empty() {
            return None;
        }
        // Rotate around until a frame with a cleared reference bit (max 2× the ring).
        for _ in 0..self.pages.len() * 2 {
            if self.hand >= self.pages.len() {
                self.hand = 0;
            }
            if self.pages[self.hand].referenced {
                self.pages[self.hand].referenced = false; // second chance
                self.hand += 1;
            } else {
                let victim = self.pages.remove(self.hand);
                if self.hand >= self.pages.len() {
                    self.hand = 0;
                }
                return Some(victim.frame);
            }
        }
        // Everyone had their bit set → after one round they are cleared; take the current one.
        let idx = self.hand % self.pages.len();
        let victim = self.pages.remove(idx);
        Some(victim.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_alloc_and_free() {
        let mut a = SwapArea::new(4);
        assert_eq!(a.free_count(), 4);
        let s0 = a.alloc().unwrap();
        let s1 = a.alloc().unwrap();
        assert_ne!(s0, s1);
        assert_eq!(a.used(), 2);
        a.free(s0);
        assert_eq!(a.used(), 1);
        // reuse the freed slot
        assert_eq!(a.alloc().unwrap(), s0);
    }

    #[test]
    fn swap_exhaustion() {
        let mut a = SwapArea::new(2);
        assert!(a.alloc().is_some());
        assert!(a.alloc().is_some());
        assert_eq!(a.alloc(), None); // full
        assert_eq!(a.free_count(), 0);
    }

    #[test]
    fn clock_evicts_unreferenced_first() {
        let mut c = Clock::new();
        c.insert(0x1000);
        c.insert(0x2000);
        c.insert(0x3000);
        // All three have their bit set (insert). The first evict gives everyone a
        // second chance (clears bits) and then swaps out the first frame.
        let v = c.evict().unwrap();
        assert_eq!(v, 0x1000);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn clock_second_chance_for_touched() {
        let mut c = Clock::new();
        c.insert(0x1000);
        c.insert(0x2000);
        // Clear everyone's bit by forcing a full first round: evict 0x1000.
        assert_eq!(c.evict().unwrap(), 0x1000);
        // Now only 0x2000 remains (bit cleared). Touch it → second chance.
        c.insert(0x3000); // bit set
        c.touch(0x2000); // bit set
        // Both bits set → the first round clears them, victim = the current hand position.
        let v = c.evict().unwrap();
        assert!(v == 0x2000 || v == 0x3000);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn clock_empty_evict_is_none() {
        let mut c = Clock::new();
        assert_eq!(c.evict(), None);
    }
}
