//! Copy-on-write sharing of frames between address spaces (fork).
//!
//! A fork used to copy every page of the parent that held something: the
//! arena pages that are not all zeros and every committed page of the demand
//! region (23 MiB and ~100 MiB for a browser, 56 + 33 ms). With this module a
//! fork shares those frames: the entries of parent and child both become
//! read-only and carry [`COW`]; the first write of either side to such a page
//! faults, and the writer gets a copy of its own, or, when it is the last one
//! that holds the frame, the frame itself made writable again.
//!
//! [`Refs`] counts per frame how many holders it has BEYOND the first one
//! (0 = one holder, or a free frame), in one `u16` per frame of RAM. The
//! counts are atomics, so the table needs no lock and a page fault from any
//! context may use it. The page-table entries themselves are the kernel's
//! business; the decisions about them are the functions below, which the
//! kernel calls and the host tests check against a model of whole address
//! spaces (bytes per page, forks, writes, munmap, madvise, execve, exit).
//!
//! Frames that may not move (the arena of the launched process, identity
//! mapped: the kernel reaches it by address) are "pinned": their first
//! holder is that process, which never gives the frame up; when it writes a
//! shared page, the other holders get the copies instead ([`evict_plan`]).

use core::sync::atomic::{AtomicU16, Ordering};

/// Software bit in a present page-table entry: the frame is shared read-only
/// with another address space (or was, and this side has not written yet).
pub const COW: u64 = 1 << 11;
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Extra holders per frame of RAM (index = physical address / 4096).
pub struct Refs<'a> {
    counts: &'a [AtomicU16],
}

impl<'a> Refs<'a> {
    pub const fn new(counts: &'a [AtomicU16]) -> Self {
        Self { counts }
    }

    fn slot(&self, frame: u64) -> Option<&AtomicU16> {
        self.counts.get((frame >> 12) as usize)
    }

    /// Is `frame` in the table (it can be shared)?
    pub fn covers(&self, frame: u64) -> bool {
        self.slot(frame).is_some()
    }

    /// Holders beyond the first.
    pub fn extra(&self, frame: u64) -> u16 {
        self.slot(frame).map(|c| c.load(Ordering::Relaxed)).unwrap_or(0)
    }

    /// One more holder. False when the frame is not in the table or its
    /// count is at the top: the caller copies the page instead of sharing it.
    pub fn share(&self, frame: u64) -> bool {
        let Some(c) = self.slot(frame) else { return false };
        c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1).filter(|&n| n < u16::MAX))
            .is_ok()
    }

    /// One holder lets go. True when it was the only one: the caller frees
    /// the frame (unless it is pinned).
    pub fn release(&self, frame: u64) -> bool {
        let Some(c) = self.slot(frame) else { return true };
        c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1)).is_err()
    }

    /// Forget every extra holder of `frame` (its sharers were given copies).
    pub fn reset(&self, frame: u64) {
        if let Some(c) = self.slot(frame) {
            c.store(0, Ordering::Relaxed);
        }
    }

    /// Frames with more than one holder.
    pub fn shared_frames(&self) -> usize {
        self.counts.iter().filter(|c| c.load(Ordering::Relaxed) != 0).count()
    }

    /// Frames with more than one holder in [`first`, `end`) (physical
    /// addresses), each set back to one holder. For the end of a run: by then
    /// every sharer is gone and a count left over is a leak of the model.
    pub fn clear_range(&self, first: u64, end: u64) -> usize {
        let a = ((first >> 12) as usize).min(self.counts.len());
        let b = ((end >> 12) as usize).min(self.counts.len());
        let mut n = 0;
        for c in &self.counts[a..b] {
            if c.swap(0, Ordering::Relaxed) != 0 {
                n += 1;
            }
        }
        n
    }
}

/// fork, a page the parent holds: the entry both sides get from now on.
/// A private writable page becomes read-only with [`COW`] on both sides; a
/// page that is copy-on-write already gains one more holder. None: not
/// shared (the table is full for this frame); the caller copies the page.
pub fn share(pte: u64, refs: &Refs) -> Option<u64> {
    if pte & PRESENT == 0 || pte & (WRITABLE | COW) == 0 {
        return None; // not private memory (a read-only page of a file)
    }
    if !refs.share(pte & ADDR_MASK) {
        return None;
    }
    Some((pte & !WRITABLE) | COW)
}

/// What a write fault on a [`COW`] entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// Not a copy-on-write entry: not this fault.
    NotCow,
    /// The writer is the only holder: the entry becomes writable, same frame.
    Reuse(u64),
    /// Others hold the frame: copy it into a fresh frame, map that writable
    /// (`fresh | flags`) and [`Refs::release`] the old one.
    Copy { old: u64, flags: u64 },
    /// The writer holds a pinned frame others share as well: the others get
    /// the copies ([`evict_plan`]), then the entry becomes writable.
    Evict,
}

/// A write to the entry `pte`. `pinned`: the frame belongs to the writer for
/// good (the identity arena of the launched process, as seen by that process).
pub fn on_write(pte: u64, refs: &Refs, pinned: bool) -> Write {
    if pte & PRESENT == 0 || pte & COW == 0 {
        return Write::NotCow;
    }
    let frame = pte & ADDR_MASK;
    let made_writable = (pte | WRITABLE) & !COW;
    if refs.extra(frame) == 0 {
        return Write::Reuse(made_writable);
    }
    if pinned {
        return Write::Evict;
    }
    Write::Copy { old: frame, flags: made_writable & !ADDR_MASK }
}

/// The entry `pte` goes away (munmap, MADV_DONTNEED, execve, exit). True:
/// the caller frees the frame (it was the last holder of a frame it owns).
/// A private page that was never shared is freed as before.
pub fn on_unmap(pte: u64, refs: &Refs) -> bool {
    if pte & PRESENT == 0 {
        return false;
    }
    if pte & COW == 0 {
        return true;
    }
    refs.release(pte & ADDR_MASK)
}

/// The holder of a pinned frame writes it: of the entries of the other
/// address spaces at the same address (`others`), the ones that map `frame`
/// each get a copy. Returns their positions in `others`; after the copies the
/// count of `frame` is [`Refs::reset`].
pub fn evict_plan(frame: u64, others: &[u64]) -> impl Iterator<Item = usize> + '_ {
    others.iter().enumerate()
        .filter(move |(_, &e)| e & PRESENT != 0 && e & ADDR_MASK == frame)
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    fn table(n: usize) -> Vec<AtomicU16> {
        (0..n).map(|_| AtomicU16::new(0)).collect()
    }

    #[test]
    fn counts_holders_beyond_the_first() {
        let t = table(8);
        let r = Refs::new(&t);
        let f = 3 << 12;
        assert!(r.release(f), "one holder: it is the last");
        assert!(r.share(f) && r.share(f));
        assert_eq!(r.extra(f), 2);
        assert!(!r.release(f) && !r.release(f));
        assert!(r.release(f));
        assert_eq!(r.extra(f), 0);
        assert!(!r.share(64 << 12), "outside the table: copy instead");
        assert!(r.release(64 << 12));
    }

    #[test]
    fn a_full_count_is_refused_not_wrapped() {
        let t = table(2);
        t[1].store(u16::MAX - 1, Ordering::Relaxed);
        let r = Refs::new(&t);
        assert!(!r.share(1 << 12));
        assert_eq!(r.extra(1 << 12), u16::MAX - 1);
        assert_eq!(share(0x1003, &r), None);
    }

    #[test]
    fn decisions_on_entries() {
        let t = table(16);
        let r = Refs::new(&t);
        let pte = (5 << 12) | PRESENT | WRITABLE | 4;
        let s = share(pte, &r).unwrap();
        assert_eq!(s, (5 << 12) | PRESENT | COW | 4);
        assert_eq!(share((6 << 12) | PRESENT, &r), None, "a read-only page of a file is not private");
        assert_eq!(on_write(s, &r, false), Write::Copy { old: 5 << 12, flags: PRESENT | WRITABLE | 4 });
        assert_eq!(on_write(s, &r, true), Write::Evict);
        assert!(!on_unmap(s, &r));
        assert_eq!(on_write(s, &r, false), Write::Reuse((5 << 12) | PRESENT | WRITABLE | 4));
        assert_eq!(on_write(pte, &r, false), Write::NotCow);
        assert!(on_unmap(s, &r));
        assert!(on_unmap(pte, &r));
        assert!(!on_unmap(0, &r));
        let others = [0, s, (7 << 12) | 1, s & !PRESENT, s];
        assert_eq!(evict_plan(5 << 12, &others).collect::<Vec<_>>(), [1, 4]);
    }

    #[test]
    fn clear_range_counts_leftovers() {
        let t = table(16);
        let r = Refs::new(&t);
        r.share(2 << 12);
        r.share(9 << 12);
        r.share(9 << 12);
        assert_eq!(r.shared_frames(), 2);
        assert_eq!(r.clear_range(0, 8 << 12), 1);
        assert_eq!(r.clear_range(0, 1 << 30), 1);
        assert_eq!(r.shared_frames(), 0);
    }

    // The model: address spaces made of entries, against address spaces made of bytes.

    /// The kernel side in small: frames of one word, page tables as maps from
    /// page number to entry, a free list, and the same decisions as the kernel.
    struct Machine<'a> {
        mem: Vec<u64>,
        free: alloc::collections::BTreeSet<u64>,
        spaces: Vec<Option<BTreeMap<u64, u64>>>,
        refs: Refs<'a>,
        /// Process 0 is the launched process: its frames are pinned (identity).
        pinned: Vec<u64>,
    }

    const FLAGS: u64 = PRESENT | WRITABLE | 4;

    impl<'a> Machine<'a> {
        fn alloc(&mut self) -> u64 {
            let f = self.free.pop_last().expect("the model never runs out");
            self.mem[(f >> 12) as usize] = 0;
            f
        }
        fn dealloc(&mut self, f: u64) {
            assert!(!self.free.contains(&f), "double free of {f:#x}");
            assert!(!self.pinned.contains(&f), "a pinned frame is never freed");
            assert_eq!(self.refs.extra(f), 0, "a frame freed with holders left");
            self.free.insert(f);
        }
        fn pte(&self, p: usize, page: u64) -> Option<u64> {
            self.spaces[p].as_ref().unwrap().get(&page).copied()
        }
        fn set(&mut self, p: usize, page: u64, e: u64) {
            self.spaces[p].as_mut().unwrap().insert(page, e);
        }
        fn read(&self, p: usize, page: u64) -> u64 {
            match self.pte(p, page) {
                Some(e) => self.mem[(e >> 12) as usize],
                None => 0,
            }
        }
        fn copy_frame(&mut self, from: u64) -> u64 {
            let f = self.alloc();
            self.mem[(f >> 12) as usize] = self.mem[(from >> 12) as usize];
            f
        }
        fn write(&mut self, p: usize, page: u64, v: u64) {
            let e = match self.pte(p, page) {
                Some(e) => e,
                None => {
                    // The launched process has its arena in place (pinned frames).
                    let f = if p == 0 { self.pinned[page as usize] } else { self.alloc() };
                    self.set(p, page, f | FLAGS);
                    f | FLAGS
                }
            };
            let e = match on_write(e, &self.refs, p == 0) {
                Write::NotCow => e,
                Write::Reuse(n) => {
                    self.set(p, page, n);
                    n
                }
                Write::Copy { old, flags } => {
                    let n = self.copy_frame(old) | flags;
                    self.set(p, page, n);
                    if self.refs.release(old) && !self.pinned.contains(&old) {
                        self.dealloc(old);
                    }
                    n
                }
                Write::Evict => {
                    let frame = e & ADDR_MASK;
                    let others: Vec<(usize, u64)> = (1..self.spaces.len())
                        .filter(|&q| self.spaces[q].is_some())
                        .map(|q| (q, self.pte(q, page).unwrap_or(0)))
                        .collect();
                    let ents: Vec<u64> = others.iter().map(|o| o.1).collect();
                    let plan: Vec<usize> = evict_plan(frame, &ents).collect();
                    for i in plan {
                        let (q, oe) = others[i];
                        let f = self.copy_frame(frame);
                        self.set(q, page, f | ((oe | WRITABLE) & !COW & !ADDR_MASK));
                    }
                    self.refs.reset(frame);
                    let n = (e | WRITABLE) & !COW;
                    self.set(p, page, n);
                    n
                }
            };
            assert!(e & WRITABLE != 0);
            self.mem[(e >> 12) as usize] = v;
        }
        fn unmap(&mut self, p: usize, page: u64) {
            if let Some(e) = self.spaces[p].as_mut().unwrap().remove(&page) {
                let f = e & ADDR_MASK;
                if on_unmap(e, &self.refs) && !self.pinned.contains(&f) {
                    self.dealloc(f);
                }
            }
        }
        fn fork(&mut self, p: usize) -> usize {
            let pages: Vec<(u64, u64)> = self.spaces[p].as_ref().unwrap().iter().map(|(&k, &v)| (k, v)).collect();
            let mut child = BTreeMap::new();
            for (page, e) in pages {
                match share(e, &self.refs) {
                    Some(n) => {
                        self.set(p, page, n);
                        child.insert(page, n);
                    }
                    None => {
                        let f = self.copy_frame(e & ADDR_MASK);
                        child.insert(page, f | FLAGS);
                    }
                }
            }
            self.spaces.push(Some(child));
            self.spaces.len() - 1
        }
        fn exit(&mut self, p: usize) {
            let pages: Vec<u64> = self.spaces[p].as_ref().unwrap().keys().copied().collect();
            for page in pages {
                self.unmap(p, page);
            }
            self.spaces[p] = None;
        }
        /// Every frame: holders seen in the tables = 1 + extra, none is free
        /// while mapped, a shared frame is writable nowhere.
        fn check(&self) {
            let mut seen: BTreeMap<u64, u16> = BTreeMap::new();
            let free = &self.free;
            for s in self.spaces.iter().flatten() {
                for &e in s.values() {
                    *seen.entry(e & ADDR_MASK).or_default() += 1;
                    assert!(!free.contains(&(e & ADDR_MASK)), "a mapped frame is free");
                    assert!(e & WRITABLE == 0 || e & COW == 0);
                }
            }
            let writable: alloc::collections::BTreeSet<u64> = self.spaces.iter().flatten()
                .flat_map(|s| s.values()).filter(|&&e| e & WRITABLE != 0).map(|&e| e & ADDR_MASK).collect();
            for (&f, &n) in seen.iter() {
                assert_eq!(self.refs.extra(f) + 1, n, "frame {f:#x}");
                assert!(n == 1 || !writable.contains(&f), "a shared frame is writable somewhere");
            }
        }
    }

    /// Random forks, writes, reads, munmaps, execves and exits on all sides,
    /// against a model in which every process has bytes of its own.
    #[test]
    fn model_of_forks_and_writes() {
        const PAGES: u64 = 12;
        const FRAMES: usize = 4096;
        let t = table(FRAMES);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..40u64 {
            let mut m = Machine {
                mem: alloc::vec![0; FRAMES],
                free: ((PAGES + 1) as usize..FRAMES).map(|f| (f as u64) << 12).collect(),
                spaces: alloc::vec![Some(BTreeMap::new())],
                refs: Refs::new(&t),
                pinned: (0..PAGES).map(|i| (i + 1) << 12).collect(),
            };
            let start_free = m.free.len();
            // The model: per process, the value of each page.
            let mut model: Vec<Option<BTreeMap<u64, u64>>> = alloc::vec![Some(BTreeMap::new())];
            for step in 0..600u64 {
                let live: Vec<usize> = (0..model.len()).filter(|&i| model[i].is_some()).collect();
                let p = live[rnd(live.len() as u64) as usize];
                let page = rnd(PAGES);
                match rnd(16) {
                    0..=5 => {
                        let v = (round << 32) | (step << 8) | p as u64 | 1;
                        m.write(p, page, v);
                        model[p].as_mut().unwrap().insert(page, v);
                    }
                    6..=8 => {
                        let want = model[p].as_ref().unwrap().get(&page).copied().unwrap_or(0);
                        assert_eq!(m.read(p, page), want, "round {round} step {step} process {p} page {page}");
                    }
                    9 if live.len() < 9 => {
                        let c = m.fork(p);
                        let copy = model[p].clone();
                        model.push(copy);
                        assert_eq!(c, model.len() - 1);
                    }
                    10 if p != 0 => {
                        // munmap or MADV_DONTNEED: the page reads zeros again.
                        m.unmap(p, page);
                        model[p].as_mut().unwrap().remove(&page);
                    }
                    11 if p != 0 => {
                        // execve: everything goes, the new image starts empty.
                        m.exit(p);
                        m.spaces[p] = Some(BTreeMap::new());
                        model[p] = Some(BTreeMap::new());
                    }
                    12 if p != 0 => {
                        m.exit(p);
                        model[p] = None;
                    }
                    _ => {}
                }
                m.check();
            }
            for (p, pages) in model.iter().enumerate() {
                for (&page, &v) in pages.iter().flatten() {
                    assert_eq!(m.read(p, page), v);
                }
            }
            for p in 1..model.len() {
                if model[p].is_some() {
                    m.exit(p);
                }
            }
            m.check();
            // Every frame back, every count 0: the launched process alone is left.
            assert_eq!(m.free.len(), start_free, "round {round}: frames lost");
            assert_eq!(m.refs.shared_frames(), 0, "round {round}: counts left over");
        }
    }
}
