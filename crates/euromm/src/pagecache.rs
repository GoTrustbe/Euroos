//! The page cache as a data structure: (file, page) -> frame, with recency and
//! a write state per page, and the choice of what to give back under memory
//! pressure. No hardware in here: the kernel tells it which frames are mapped
//! and which were referenced since the last pass (it reads that from the page
//! tables), and gets back the list of pages to unmap and free.
//!
//! The rules (docs/design/MEMORY-RECLAIM.md):
//!
//! * only a CLEAN page can be evicted. A DIRTY page has to be written back
//!   first ([`PageCache::begin_writeback`], [`PageCache::end_writeback`]); a
//!   page that is being written, or that was written to again while it was
//!   being written, stays;
//! * among the clean pages, the ones nobody maps go first, oldest first; then
//!   the ones that are mapped but were not referenced since the last pass,
//!   oldest first; a page that was referenced is not evicted;
//! * "oldest" is the last time the cache was asked for the page (a fault that
//!   mapped it), a logical clock that only moves forward.
//!
//! Host-tested against a brute-force model (tests below).

use alloc::vec::Vec;

/// Write state of a cached page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageState {
    /// Same bytes as the backing store: can be dropped and read again.
    Clean,
    /// Newer than the backing store: must be written before it can go.
    Dirty,
    /// Being written. `again`: it was written to once more meanwhile.
    Writeback { again: bool },
}

/// What the page tables say about a cached frame at this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Use {
    /// No process maps it.
    Unmapped,
    /// Mapped, not referenced since the last pass.
    Idle,
    /// Mapped and referenced since the last pass.
    Hot,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    key: u64,
    frame: u64,
    stamp: u64,
    state: PageState,
}

/// (file, page) -> frame. Sorted by key; lookups are binary searches.
#[derive(Debug, Default)]
pub struct PageCache {
    e: Vec<Entry>,
    clock: u64,
    evicted: u64,
}

impl PageCache {
    pub const fn new() -> Self {
        Self { e: Vec::new(), clock: 0, evicted: 0 }
    }

    pub fn len(&self) -> usize {
        self.e.len()
    }

    pub fn is_empty(&self) -> bool {
        self.e.is_empty()
    }

    /// Pages evicted since the cache was made.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Pages that are not clean (dirty or being written).
    pub fn unclean(&self) -> usize {
        self.e.iter().filter(|x| x.state != PageState::Clean).count()
    }

    fn find(&self, key: u64) -> Result<usize, usize> {
        self.e.binary_search_by_key(&key, |x| x.key)
    }

    /// The frame of `key`, and the page becomes the most recently used.
    pub fn get(&mut self, key: u64) -> Option<u64> {
        let i = self.find(key).ok()?;
        self.clock += 1;
        self.e[i].stamp = self.clock;
        Some(self.e[i].frame)
    }

    /// The frame and state of `key` without touching its recency.
    pub fn peek(&self, key: u64) -> Option<(u64, PageState)> {
        self.find(key).ok().map(|i| (self.e[i].frame, self.e[i].state))
    }

    /// Add a clean page. False (and nothing changes) when the key is there
    /// already or the list cannot grow: the caller keeps the frame.
    pub fn insert(&mut self, key: u64, frame: u64) -> bool {
        match self.find(key) {
            Ok(_) => false,
            Err(i) => {
                if self.e.try_reserve(1).is_err() {
                    return false;
                }
                self.clock += 1;
                self.e.insert(i, Entry { key, frame, stamp: self.clock, state: PageState::Clean });
                true
            }
        }
    }

    /// The page was written to. False when it is not cached.
    pub fn mark_dirty(&mut self, key: u64) -> bool {
        let Ok(i) = self.find(key) else { return false };
        self.e[i].state = match self.e[i].state {
            PageState::Clean | PageState::Dirty => PageState::Dirty,
            PageState::Writeback { .. } => PageState::Writeback { again: true },
        };
        true
    }

    /// The oldest dirty page, now marked as being written: write its frame to
    /// the backing store, then call [`Self::end_writeback`].
    pub fn begin_writeback(&mut self) -> Option<(u64, u64)> {
        let i = (0..self.e.len())
            .filter(|&i| self.e[i].state == PageState::Dirty)
            .min_by_key(|&i| self.e[i].stamp)?;
        self.e[i].state = PageState::Writeback { again: false };
        Some((self.e[i].key, self.e[i].frame))
    }

    /// The write of `key` ended. It is clean when the write succeeded and the
    /// page was not written to meanwhile; dirty again otherwise.
    pub fn end_writeback(&mut self, key: u64, ok: bool) {
        if let Ok(i) = self.find(key) {
            if let PageState::Writeback { again } = self.e[i].state {
                self.e[i].state = if ok && !again { PageState::Clean } else { PageState::Dirty };
            }
        }
    }

    /// Every frame the cache holds (any state), in key order.
    pub fn frames(&self) -> impl Iterator<Item = u64> + '_ {
        self.e.iter().map(|x| x.frame)
    }

    /// Take up to `want` pages out of the cache and return them as
    /// (key, frame, was mapped): the caller unmaps the mapped ones and frees
    /// the frames. `usage(frame)` is asked once per clean page. Order: see the
    /// module text. Fewer than `want` come back when there are no more pages
    /// that may go.
    pub fn evict(&mut self, want: usize, usage: &mut dyn FnMut(u64) -> Use) -> Vec<(u64, u64, bool)> {
        let mut out: Vec<(u64, u64, bool)> = Vec::new();
        if want == 0 {
            return out;
        }
        // (tier, stamp, key) of every page that may go.
        let mut cand: Vec<(Use, u64, u64)> = Vec::new();
        if cand.try_reserve(self.e.len()).is_err() {
            return out; // no memory for the list: evict nothing rather than fail
        }
        for x in self.e.iter() {
            if x.state != PageState::Clean {
                continue;
            }
            let u = usage(x.frame);
            if u != Use::Hot {
                cand.push((u, x.stamp, x.key));
            }
        }
        cand.sort_unstable();
        cand.truncate(want);
        if out.try_reserve(cand.len()).is_err() {
            return out;
        }
        // Remove in one pass over the sorted entries.
        let mut gone: Vec<u64> = Vec::new();
        if gone.try_reserve(cand.len()).is_err() {
            return out;
        }
        gone.extend(cand.iter().map(|c| c.2));
        gone.sort_unstable();
        for &(u, _, key) in cand.iter() {
            if let Ok(i) = self.find(key) {
                out.push((key, self.e[i].frame, u == Use::Idle));
            }
        }
        self.e.retain(|x| gone.binary_search(&x.key).is_err());
        self.evicted += out.len() as u64;
        out
    }


    /// Empty the cache; the frames come back for the caller to free.
    pub fn drain(&mut self) -> impl Iterator<Item = u64> {
        core::mem::take(&mut self.e).into_iter().map(|x| x.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_insert_and_recency() {
        let mut c = PageCache::new();
        assert!(c.insert(5, 0x5000));
        assert!(c.insert(1, 0x1000));
        assert!(c.insert(9, 0x9000));
        assert!(!c.insert(5, 0x7000), "a key is cached once");
        assert_eq!(c.get(5), Some(0x5000));
        assert_eq!(c.get(2), None);
        // 1 is now the oldest, then 9, then 5.
        let v = c.evict(2, &mut |_| Use::Unmapped);
        assert_eq!(v, vec![(1, 0x1000, false), (9, 0x9000, false)]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.evicted(), 2);
    }

    #[test]
    fn unmapped_before_idle_and_hot_never() {
        let mut c = PageCache::new();
        for k in 0..6u64 {
            c.insert(k, 0x1000 * (k + 1));
        }
        // frames 0x1000,0x2000: hot; 0x3000,0x4000: idle; 0x5000,0x6000: unmapped
        let mut usage = |f: u64| match f {
            0x1000 | 0x2000 => Use::Hot,
            0x3000 | 0x4000 => Use::Idle,
            _ => Use::Unmapped,
        };
        let v = c.evict(3, &mut usage);
        assert_eq!(v, vec![(4, 0x5000, false), (5, 0x6000, false), (2, 0x3000, true)]);
        let v = c.evict(10, &mut usage);
        assert_eq!(v, vec![(3, 0x4000, true)]);
        assert_eq!(c.len(), 2, "the two hot pages stay");
    }

    #[test]
    fn dirty_pages_wait_for_their_writeback() {
        let mut c = PageCache::new();
        c.insert(1, 0x1000);
        c.insert(2, 0x2000);
        c.insert(3, 0x3000);
        assert!(c.mark_dirty(1));
        assert!(c.mark_dirty(2));
        assert!(!c.mark_dirty(7));
        assert_eq!(c.unclean(), 2);
        // Only the clean page can go.
        assert_eq!(c.evict(10, &mut |_| Use::Unmapped), vec![(3, 0x3000, false)]);
        // Write 1 back: the oldest dirty page first.
        assert_eq!(c.begin_writeback(), Some((1, 0x1000)));
        assert!(c.evict(10, &mut |_| Use::Unmapped).is_empty(), "a page being written stays");
        c.end_writeback(1, true);
        assert_eq!(c.evict(10, &mut |_| Use::Unmapped), vec![(1, 0x1000, false)]);
        // 2 is written to again while it is being written: dirty afterwards.
        assert_eq!(c.begin_writeback(), Some((2, 0x2000)));
        c.mark_dirty(2);
        c.end_writeback(2, true);
        assert_eq!(c.peek(2), Some((0x2000, PageState::Dirty)));
        // A failed write leaves it dirty too.
        assert_eq!(c.begin_writeback(), Some((2, 0x2000)));
        c.end_writeback(2, false);
        assert_eq!(c.peek(2), Some((0x2000, PageState::Dirty)));
        assert!(c.evict(10, &mut |_| Use::Unmapped).is_empty());
        assert_eq!(c.begin_writeback(), Some((2, 0x2000)));
        c.end_writeback(2, true);
        assert_eq!(c.evict(10, &mut |_| Use::Unmapped).len(), 1);
        assert!(c.is_empty());
    }

    /// The brute-force model: a plain list, the victim found by looking at
    /// every page each time.
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct M {
        key: u64,
        frame: u64,
        stamp: u64,
        state: PageState,
    }

    #[test]
    fn model_random_operations() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut c = PageCache::new();
        let mut m: Vec<M> = Vec::new();
        let mut clock = 0u64;
        let mut next_frame = 0x10_0000u64;
        let mut freed: Vec<u64> = Vec::new();
        for step in 0..30000 {
            let key = rnd(96);
            match rnd(10) {
                0..=2 => {
                    let had = m.iter().any(|x| x.key == key);
                    let ok = c.insert(key, next_frame);
                    assert_eq!(ok, !had);
                    if ok {
                        clock += 1;
                        m.push(M { key, frame: next_frame, stamp: clock, state: PageState::Clean });
                        next_frame += 0x1000;
                    }
                }
                3..=4 => {
                    let want = m.iter_mut().find(|x| x.key == key).map(|x| {
                        clock += 1;
                        x.stamp = clock;
                        x.frame
                    });
                    assert_eq!(c.get(key), want);
                }
                5 => {
                    let hit = m.iter_mut().find(|x| x.key == key).map(|x| {
                        x.state = match x.state {
                            PageState::Writeback { .. } => PageState::Writeback { again: true },
                            _ => PageState::Dirty,
                        };
                    });
                    assert_eq!(c.mark_dirty(key), hit.is_some());
                }
                6 => {
                    let want = m.iter_mut().filter(|x| x.state == PageState::Dirty).min_by_key(|x| x.stamp).map(|x| {
                        x.state = PageState::Writeback { again: false };
                        (x.key, x.frame)
                    });
                    assert_eq!(c.begin_writeback(), want);
                }
                7 => {
                    let ok = rnd(4) != 0;
                    if let Some(x) = m.iter_mut().find(|x| x.key == key) {
                        if let PageState::Writeback { again } = x.state {
                            x.state = if ok && !again { PageState::Clean } else { PageState::Dirty };
                        }
                    }
                    c.end_writeback(key, ok);
                }
                _ => {
                    // The page tables of this step: a function of the frame.
                    let salt = rnd(1 << 20);
                    let usage = |f: u64| match ((f >> 12) ^ salt) % 4 {
                        0 => Use::Hot,
                        1 => Use::Idle,
                        _ => Use::Unmapped,
                    };
                    let want_n = rnd(12) as usize;
                    let mut want: Vec<(u64, u64, bool)> = Vec::new();
                    for _ in 0..want_n {
                        let best = m.iter().enumerate()
                            .filter(|(_, x)| x.state == PageState::Clean && usage(x.frame) != Use::Hot)
                            .min_by_key(|(_, x)| (usage(x.frame), x.stamp))
                            .map(|(i, _)| i);
                        match best {
                            Some(i) => {
                                let x = m.swap_remove(i);
                                want.push((x.key, x.frame, usage(x.frame) == Use::Idle));
                            }
                            None => break,
                        }
                    }
                    let got = c.evict(want_n, &mut |f| usage(f));
                    assert_eq!(got, want, "step {step}");
                    for (_, f, _) in got {
                        assert!(!freed.contains(&f), "frame {f:#x} evicted twice");
                        freed.push(f);
                    }
                }
            }
            assert_eq!(c.len(), m.len());
            assert_eq!(c.unclean(), m.iter().filter(|x| x.state != PageState::Clean).count());
            if step % 512 == 0 {
                for x in m.iter() {
                    assert_eq!(c.peek(x.key), Some((x.frame, x.state)));
                }
            }
        }
        // Whatever is left comes out exactly once.
        let mut left: Vec<u64> = c.drain().collect();
        left.sort_unstable();
        let mut want: Vec<u64> = m.iter().map(|x| x.frame).collect();
        want.sort_unstable();
        assert_eq!(left, want);
        assert!(left.iter().all(|f| !freed.contains(f)));
    }
}
