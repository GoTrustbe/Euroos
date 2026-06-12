//! Swap-subsysteem-kern (plan J3): onder geheugendruk worden anonieme pagina's
//! naar een swap-partitie/-bestand geschreven en bij toegang weer ingelezen.
//!
//! Deze module is de architectuur-onafhankelijke kern: een **swap-slot-allocator**
//! (welke swap-blokken vrij/bezet zijn) + een **CLOCK (second-chance) page-
//! replacement**-policy (welke frame als slachtoffer wordt uitgeswapt). De
//! daadwerkelijke page-I/O + PTE-manipulatie is kernel-werk dat hierop bouwt. Pure
//! `no_std`-logica → volledig host-getest, los van enig geheugen.

use alloc::vec;
use alloc::vec::Vec;

/// Beheert de bezetting van een swap-gebied met `n` slots (elk = één pagina).
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

    /// Reserveer een vrij swap-slot; `None` als het gebied vol is.
    pub fn alloc(&mut self) -> Option<usize> {
        let s = self.used.iter().position(|&u| !u)?;
        self.used[s] = true;
        self.used_count += 1;
        Some(s)
    }

    /// Geef een swap-slot vrij (de pagina is weer ingelezen).
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

/// Eén beheerd frame in de CLOCK-policy: een fysiek frame-adres + referentie-bit.
struct Page {
    frame: u64,
    referenced: bool,
}

/// CLOCK / second-chance page-replacement. Frames staan in een ring; de "wijzer"
/// draait rond: een frame met gezette referentie-bit krijgt een tweede kans (bit
/// gewist), een frame met gewiste bit wordt het slachtoffer. Zo benadert CLOCK LRU
/// tegen O(1)-kosten en zonder per-toegang-boekhouding.
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

    /// Neem een nieuw frame in beheer (binnengehaald → referentie-bit gezet).
    pub fn insert(&mut self, frame: u64) {
        self.pages.push(Page { frame, referenced: true });
    }

    /// Markeer een frame als recent gebruikt (zet z'n referentie-bit).
    pub fn touch(&mut self, frame: u64) {
        if let Some(p) = self.pages.iter_mut().find(|p| p.frame == frame) {
            p.referenced = true;
        }
    }

    /// Aantal beheerde frames.
    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Kies + verwijder een slachtoffer-frame om uit te swappen. Geeft het frame-
    /// adres, of `None` als er niets beheerd wordt.
    pub fn evict(&mut self) -> Option<u64> {
        if self.pages.is_empty() {
            return None;
        }
        // Draai rond tot een frame met gewiste referentie-bit (max 2× de ring).
        for _ in 0..self.pages.len() * 2 {
            if self.hand >= self.pages.len() {
                self.hand = 0;
            }
            if self.pages[self.hand].referenced {
                self.pages[self.hand].referenced = false; // tweede kans
                self.hand += 1;
            } else {
                let victim = self.pages.remove(self.hand);
                if self.hand >= self.pages.len() {
                    self.hand = 0;
                }
                return Some(victim.frame);
            }
        }
        // Iedereen had z'n bit gezet → na één ronde zijn ze gewist; neem de huidige.
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
        // hergebruik het vrijgegeven slot
        assert_eq!(a.alloc().unwrap(), s0);
    }

    #[test]
    fn swap_exhaustion() {
        let mut a = SwapArea::new(2);
        assert!(a.alloc().is_some());
        assert!(a.alloc().is_some());
        assert_eq!(a.alloc(), None); // vol
        assert_eq!(a.free_count(), 0);
    }

    #[test]
    fn clock_evicts_unreferenced_first() {
        let mut c = Clock::new();
        c.insert(0x1000);
        c.insert(0x2000);
        c.insert(0x3000);
        // Alle drie hebben hun bit gezet (insert). Eerste evict geeft iedereen een
        // tweede kans (wist bits) en swapt dan het eerste frame uit.
        let v = c.evict().unwrap();
        assert_eq!(v, 0x1000);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn clock_second_chance_for_touched() {
        let mut c = Clock::new();
        c.insert(0x1000);
        c.insert(0x2000);
        // Wis ieders bit door een volledige eerste ronde te forceren: evict 0x1000.
        assert_eq!(c.evict().unwrap(), 0x1000);
        // Nu staat alleen 0x2000 (bit gewist). Touch het → tweede kans.
        c.insert(0x3000); // bit gezet
        c.touch(0x2000); // bit gezet
        // Beide bits gezet → eerste ronde wist ze, slachtoffer = de huidige hand-positie.
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
