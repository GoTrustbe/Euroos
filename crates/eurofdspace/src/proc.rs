//! One descriptor table per process (BUG-098).
//!
//! A *descriptor* is a number in one process. What it names is an *object*:
//! an open file description, a pipe end, a socket, an eventfd, an epoll
//! instance. Several descriptors, in one process or in several, can name the
//! same object (`dup`, `fork`, `SCM_RIGHTS`); the object is closed when the
//! last of them is.
//!
//! * [`ProcTab`]: the table of one process: number -> ([`Desc`]) object and
//!   the close-on-exec flag, which belongs to the descriptor.
//! * [`Refs`]: how many descriptors (and descriptors in flight, and system
//!   calls in progress) name each object.
//!
//! Every allocation is fallible: a table that cannot grow says so.

use alloc::vec::Vec;

/// What one descriptor is: the object it names and its close-on-exec flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Desc {
    pub obj: u32,
    pub cloexec: bool,
}

/// Why a descriptor could not be put in a table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabError {
    /// No number below the limit is free (`EMFILE`).
    Full,
    /// The table could not grow (`ENOMEM`).
    NoMem,
}

/// The descriptor table of one process. Numbers are handed out lowest first.
pub struct ProcTab {
    slots: Vec<Option<Desc>>,
    open: usize,
    /// No number below this one is free.
    hint: usize,
}

impl Default for ProcTab {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcTab {
    pub const fn new() -> Self {
        Self { slots: Vec::new(), open: 0, hint: 0 }
    }

    /// A table with descriptors 0, 1 and 2 naming objects 0, 1 and 2 (the
    /// standard streams). None when the heap has no room.
    pub fn with_std() -> Option<Self> {
        let mut t = Self::new();
        for fd in 0..3 {
            t.put(fd, usize::MAX, Desc { obj: fd as u32, cloexec: false }).ok()?;
        }
        Some(t)
    }

    pub fn get(&self, fd: usize) -> Option<Desc> {
        self.slots.get(fd).copied().flatten()
    }

    /// How many descriptors are open.
    pub fn open(&self) -> usize {
        self.open
    }

    /// One past the highest open number (0 for an empty table).
    pub fn top(&self) -> usize {
        self.slots.iter().rposition(|s| s.is_some()).map_or(0, |i| i + 1)
    }

    /// The lowest free number at or above `from`.
    pub fn lowest_free(&self, from: usize) -> usize {
        let start = from.max(self.hint);
        (start..self.slots.len()).find(|&i| self.slots[i].is_none()).unwrap_or(self.slots.len().max(start))
    }

    fn grow(&mut self, fd: usize) -> Result<(), TabError> {
        if fd >= self.slots.len() {
            let need = fd + 1 - self.slots.len();
            self.slots.try_reserve(need).map_err(|_| TabError::NoMem)?;
            self.slots.resize(fd + 1, None);
        }
        Ok(())
    }

    /// Give `d` the lowest free number in `from..limit`.
    pub fn install(&mut self, from: usize, limit: usize, d: Desc) -> Result<usize, TabError> {
        let fd = self.lowest_free(from);
        if fd >= limit {
            return Err(TabError::Full);
        }
        self.grow(fd)?;
        self.slots[fd] = Some(d);
        self.open += 1;
        if fd == self.hint {
            self.hint = fd + 1;
        }
        Ok(fd)
    }

    /// Put `d` at exactly `fd` (`dup2`). Returns what was there.
    pub fn put(&mut self, fd: usize, limit: usize, d: Desc) -> Result<Option<Desc>, TabError> {
        if fd >= limit {
            return Err(TabError::Full);
        }
        self.grow(fd)?;
        let old = self.slots[fd].replace(d);
        if old.is_none() {
            self.open += 1;
        }
        Ok(old)
    }

    /// Close number `fd`: what it named.
    pub fn take(&mut self, fd: usize) -> Option<Desc> {
        let d = self.slots.get_mut(fd)?.take()?;
        self.open -= 1;
        if fd < self.hint {
            self.hint = fd;
        }
        Some(d)
    }

    /// Set or clear close-on-exec on `fd`. False when `fd` is not open.
    pub fn set_cloexec(&mut self, fd: usize, on: bool) -> bool {
        match self.slots.get_mut(fd) {
            Some(Some(d)) => {
                d.cloexec = on;
                true
            }
            _ => false,
        }
    }

    /// The copy a fork child starts with. None when the heap has no room.
    pub fn try_clone(&self) -> Option<Self> {
        let top = self.top();
        let mut slots = Vec::new();
        slots.try_reserve_exact(top).ok()?;
        slots.extend_from_slice(&self.slots[..top]);
        Some(Self { slots, open: self.open, hint: self.hint.min(top) })
    }

    /// Every open descriptor, lowest number first.
    pub fn iter(&self) -> impl Iterator<Item = (usize, Desc)> + '_ {
        self.slots.iter().enumerate().filter_map(|(i, s)| s.map(|d| (i, d)))
    }

    /// Close every descriptor for which `pick` says so (exec: the
    /// close-on-exec ones; `close_range`: a range of numbers). The objects they
    /// named are handed to `gone`, one call per descriptor.
    pub fn close_where(&mut self, mut pick: impl FnMut(usize, Desc) -> bool, mut gone: impl FnMut(Desc)) {
        for fd in 0..self.slots.len() {
            if let Some(d) = self.slots[fd] {
                if pick(fd, d) {
                    self.slots[fd] = None;
                    self.open -= 1;
                    if fd < self.hint {
                        self.hint = fd;
                    }
                    gone(d);
                }
            }
        }
    }

    /// Empty the table (process exit); the storage is handed back so the
    /// caller walks and drops it outside its lock.
    pub fn take_all(&mut self) -> Vec<Option<Desc>> {
        self.open = 0;
        self.hint = 0;
        core::mem::take(&mut self.slots)
    }
}

/// Reference counts per object. A key is whatever identifies an object for
/// the caller (the kernel uses fd space and object number).
pub struct Refs {
    /// Sorted by key.
    v: Vec<(u64, u32)>,
}

impl Default for Refs {
    fn default() -> Self {
        Self::new()
    }
}

impl Refs {
    pub const fn new() -> Self {
        Self { v: Vec::new() }
    }

    pub fn get(&self, key: u64) -> u32 {
        match self.v.binary_search_by_key(&key, |e| e.0) {
            Ok(i) => self.v[i].1,
            Err(_) => 0,
        }
    }

    /// One more reference. False when the heap has no room for a new entry.
    pub fn inc(&mut self, key: u64) -> bool {
        match self.v.binary_search_by_key(&key, |e| e.0) {
            Ok(i) => {
                self.v[i].1 += 1;
                true
            }
            Err(i) => {
                if self.v.try_reserve(1).is_err() {
                    return false;
                }
                self.v.insert(i, (key, 1));
                true
            }
        }
    }

    /// One reference less. True when that was the last one (the caller
    /// closes the object). False for a key that has none.
    pub fn dec(&mut self, key: u64) -> bool {
        match self.v.binary_search_by_key(&key, |e| e.0) {
            Ok(i) => {
                self.v[i].1 -= 1;
                if self.v[i].1 == 0 {
                    self.v.remove(i);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Forget every key for which `f` holds (a space that is wiped as a
    /// whole); returns how many.
    pub fn forget(&mut self, mut f: impl FnMut(u64) -> bool) -> usize {
        let n = self.v.len();
        self.v.retain(|e| !f(e.0));
        n - self.v.len()
    }

    /// Objects with at least one reference.
    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    /// How many keys satisfy `f`.
    pub fn count(&self, mut f: impl FnMut(u64) -> bool) -> usize {
        self.v.iter().filter(|e| f(e.0)).count()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec as SVec;

    fn d(obj: u32) -> Desc {
        Desc { obj, cloexec: false }
    }

    #[test]
    fn numbers_are_lowest_first_and_reused() {
        let mut t = ProcTab::with_std().unwrap();
        assert_eq!(t.install(0, 4096, d(10)), Ok(3));
        assert_eq!(t.install(0, 4096, d(11)), Ok(4));
        assert_eq!(t.install(0, 4096, d(12)), Ok(5));
        assert_eq!(t.take(4), Some(d(11)));
        assert_eq!(t.take(4), None);
        assert_eq!(t.install(0, 4096, d(13)), Ok(4));
        assert_eq!(t.install(0, 4096, d(14)), Ok(6));
        assert_eq!(t.take(0), Some(d(0)));
        assert_eq!(t.install(0, 4096, d(15)), Ok(0));
        assert_eq!(t.open(), 7);
    }

    #[test]
    fn dupfd_starts_at_its_argument() {
        let mut t = ProcTab::with_std().unwrap();
        assert_eq!(t.install(50, 4096, d(9)), Ok(50));
        assert_eq!(t.install(50, 4096, d(9)), Ok(51));
        // The numbers below stay free and are still handed out lowest first.
        assert_eq!(t.install(0, 4096, d(9)), Ok(3));
        assert_eq!(t.install(10, 4096, d(9)), Ok(10));
        assert_eq!(t.install(0, 4096, d(9)), Ok(4));
        assert_eq!(t.top(), 52);
    }

    #[test]
    fn the_limit_is_emfile() {
        let mut t = ProcTab::with_std().unwrap();
        for want in 3..8 {
            assert_eq!(t.install(0, 8, d(1)), Ok(want));
        }
        assert_eq!(t.install(0, 8, d(1)), Err(TabError::Full));
        assert_eq!(t.put(8, 8, d(1)), Err(TabError::Full));
        assert_eq!(t.install(9, 8, d(1)), Err(TabError::Full));
        assert_eq!(t.take(5), Some(d(1)));
        assert_eq!(t.install(0, 8, d(2)), Ok(5));
    }

    #[test]
    fn dup2_replaces_and_reports_what_was_there() {
        let mut t = ProcTab::with_std().unwrap();
        assert_eq!(t.put(7, 4096, d(20)), Ok(None));
        assert_eq!(t.put(7, 4096, d(21)), Ok(Some(d(20))));
        assert_eq!(t.get(7), Some(d(21)));
        assert_eq!(t.get(6), None);
        assert_eq!(t.open(), 4);
        // The hole below 7 is still found.
        assert_eq!(t.install(0, 4096, d(22)), Ok(3));
    }

    #[test]
    fn cloexec_belongs_to_the_descriptor() {
        let mut t = ProcTab::with_std().unwrap();
        let a = t.install(0, 4096, Desc { obj: 30, cloexec: true }).unwrap();
        let b = t.install(0, 4096, d(30)).unwrap();
        assert!(t.get(a).unwrap().cloexec);
        assert!(!t.get(b).unwrap().cloexec);
        assert!(t.set_cloexec(b, true));
        assert!(t.set_cloexec(a, false));
        assert!(!t.set_cloexec(99, true));
        let mut gone = SVec::new();
        t.close_where(|_, d| d.cloexec, |d| gone.push(d.obj));
        assert_eq!(gone, [30]);
        assert_eq!(t.get(b), None);
        assert_eq!(t.get(a), Some(d(30)));
        assert_eq!(t.install(0, 4096, d(31)), Ok(b));
    }

    #[test]
    fn a_fork_copy_is_independent() {
        let mut p = ProcTab::with_std().unwrap();
        p.install(0, 4096, d(40)).unwrap();
        p.install(0, 4096, Desc { obj: 41, cloexec: true }).unwrap();
        let mut c = p.try_clone().unwrap();
        assert_eq!(c.get(3), Some(d(40)));
        assert_eq!(c.get(4), Some(Desc { obj: 41, cloexec: true }));
        assert_eq!(c.take(3), Some(d(40)));
        assert_eq!(c.install(0, 4096, d(42)), Ok(3));
        assert_eq!(p.get(3), Some(d(40)));
        assert_eq!(p.install(0, 4096, d(43)), Ok(5));
        assert_eq!(c.get(5), None);
        assert_eq!(c.open(), 5);
    }

    #[test]
    fn exit_hands_back_everything() {
        let mut t = ProcTab::with_std().unwrap();
        t.install(0, 4096, d(50)).unwrap();
        t.put(9, 4096, d(51)).unwrap();
        let all: SVec<u32> = t.take_all().into_iter().flatten().map(|d| d.obj).collect();
        assert_eq!(all, [0, 1, 2, 50, 51]);
        assert_eq!(t.open(), 0);
        assert_eq!(t.install(0, 4096, d(52)), Ok(0));
    }

    #[test]
    fn close_range_closes_a_range_of_numbers() {
        let mut t = ProcTab::with_std().unwrap();
        for i in 0..10 {
            t.install(0, 4096, d(60 + i)).unwrap();
        }
        let mut gone = SVec::new();
        t.close_where(|fd, _| (5..=8).contains(&fd), |d| gone.push(d.obj));
        assert_eq!(gone, [62, 63, 64, 65]);
        assert_eq!(t.open(), 9);
        assert_eq!(t.lowest_free(0), 5);
        assert_eq!(t.lowest_free(6), 6);
        assert_eq!(t.lowest_free(9), 13);
    }

    #[test]
    fn four_thousand_descriptors() {
        let mut t = ProcTab::with_std().unwrap();
        for want in 3..4096 {
            assert_eq!(t.install(0, 4096, d(1)), Ok(want));
        }
        assert_eq!(t.install(0, 4096, d(1)), Err(TabError::Full));
        assert_eq!(t.open(), 4096);
        assert_eq!(t.take(2000), Some(d(1)));
        assert_eq!(t.install(0, 4096, d(2)), Ok(2000));
    }

    #[test]
    fn the_last_reference_closes() {
        let mut r = Refs::new();
        assert!(r.inc(7));
        assert!(r.inc(7));
        assert!(r.inc(3));
        assert_eq!(r.get(7), 2);
        assert!(!r.dec(7));
        assert!(r.dec(7));
        assert_eq!(r.get(7), 0);
        assert!(!r.dec(7)); // no references: nothing to close twice
        assert!(r.dec(3));
        assert!(r.is_empty());
    }

    #[test]
    fn a_wiped_space_forgets_its_keys() {
        let mut r = Refs::new();
        for k in [1u64, 2, 3, (5 << 32) | 1, (5 << 32) | 2] {
            r.inc(k);
        }
        assert_eq!(r.count(|k| k >> 32 == 5), 2);
        assert_eq!(r.forget(|k| k >> 32 == 5), 2);
        assert_eq!(r.len(), 3);
        assert_eq!(r.get((5 << 32) | 1), 0);
        assert_eq!(r.get(2), 1);
    }
}
