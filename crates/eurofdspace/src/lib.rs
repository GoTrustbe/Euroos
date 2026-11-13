//! **EuroFdSpace**: the containers behind the kernel's fd spaces (BUG-060).
//!
//! The syscall layer keeps its descriptor tables once per *space*: a process
//! group that owns descriptors (the kernel side, with who is in which space, is
//! `kernel/src/fdspace.rs`). This crate holds the data structures, free of
//! kernel dependencies so they are tested on the host:
//!
//! * [`Tab`]: a fixed-size table per space, [`Lists`]: a growable list per space;
//! * [`Guard`]: a held lock that dereferences to the part of one space;
//! * [`FlagSlots`]: per-descriptor status flags per space, read without a lock;
//! * [`Registry`]: which space numbers are in use, reference counted.
//! * [`ProcTab`] and [`Refs`]: the descriptor table of one process on top of
//!   a space, and the reference counts of the objects (BUG-098).
//!
//! Space [`ROOT`] always exists and lives inside the static; the other spaces
//! get heap blocks. Every allocation that happens when a space is made is
//! fallible: a process start that finds no room is refused, it does not panic.
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};

mod proc;
pub use proc::{Desc, ProcTab, Refs, TabError};

/// The launcher's space: task 0, kernel tasks and the one foreground or glibc
/// process the launcher runs at a time.
pub const ROOT: usize = 0;
/// Where a task is moved when its space is released while the task still has a
/// scheduler slot: tables that belong to nobody, cleared at every release.
pub const ORPHAN: usize = 1;
/// Upper bound on spaces (the kernel checks it against its task table).
pub const MAX_SPACES: usize = 512; // >= the task table of the kernel (512 since BUG-151)

/// Data that exists once per space.
pub trait Spaced {
    type Out: ?Sized;
    fn of(&self, s: usize) -> &Self::Out;
    fn of_mut(&mut self, s: usize) -> &mut Self::Out;
}

/// A fixed-size table per space (`N` slots). [`ROOT`]'s table is part of the
/// static; the others are heap blocks installed by `install` and taken out
/// again by `remove`. A space without a block reads and writes [`ORPHAN`]'s
/// (or, before any space was ever made, [`ROOT`]'s).
pub struct Tab<E: 'static, const N: usize> {
    root: [E; N],
    extra: Vec<Option<Box<[E]>>>,
}

impl<E: Clone + 'static, const N: usize> Tab<E, N> {
    pub const fn new(root: [E; N]) -> Self {
        Self { root, extra: Vec::new() }
    }

    /// A block of `N` copies of `fill`, or `None` when the heap has no room.
    pub fn block(fill: E) -> Option<Box<[E]>> {
        Self::block_len(N, fill)
    }

    /// A block of `len` copies of `fill` (`len <= N`): the table of a space
    /// that is smaller than the one of the root. The owner indexes a space
    /// only below the length it gave it.
    pub fn block_len(len: usize, fill: E) -> Option<Box<[E]>> {
        if len == 0 || len > N {
            return None;
        }
        let mut v: Vec<E> = Vec::new();
        v.try_reserve_exact(len).ok()?;
        v.resize(len, fill);
        Some(v.into_boxed_slice())
    }

    /// Room for space `s` in the index (so `install` cannot fail). False when
    /// the heap has no room.
    pub fn reserve(&mut self, s: usize) -> bool {
        if s == ROOT || s <= self.extra.len() {
            return true;
        }
        let need = s - self.extra.len();
        if self.extra.try_reserve(need).is_err() {
            return false;
        }
        self.extra.resize_with(s, || None);
        true
    }

    /// Give space `s` its table. `reserve(s)` must have succeeded.
    pub fn install(&mut self, s: usize, b: Box<[E]>) {
        if s != ROOT && s <= self.extra.len() && !b.is_empty() && b.len() <= N {
            self.extra[s - 1] = Some(b);
        }
    }

    /// Take the table of space `s` out (the caller drops it outside the lock).
    pub fn remove(&mut self, s: usize) -> Option<Box<[E]>> {
        if s == ROOT || s > self.extra.len() {
            return None;
        }
        self.extra[s - 1].take()
    }

    pub fn has(&self, s: usize) -> bool {
        s == ROOT || (s <= self.extra.len() && self.extra[s - 1].is_some())
    }

    /// Does any slot of ANY space satisfy `f`? For the rare question that is
    /// about the object behind a descriptor and not about one process: "does
    /// anybody still have this file open".
    pub fn any(&self, mut f: impl FnMut(&E) -> bool) -> bool {
        self.root.iter().any(&mut f) || self.extra.iter().flatten().any(|b| b.iter().any(&mut f))
    }

    fn slot(&self, s: usize) -> usize {
        if self.has(s) {
            s
        } else if self.has(ORPHAN) {
            ORPHAN
        } else {
            ROOT
        }
    }
}

impl<E: Clone + 'static, const N: usize> Spaced for Tab<E, N> {
    type Out = [E];
    fn of(&self, s: usize) -> &[E] {
        match self.slot(s) {
            ROOT => &self.root,
            k => self.extra[k - 1].as_deref().unwrap_or(&self.root),
        }
    }
    fn of_mut(&mut self, s: usize) -> &mut [E] {
        match self.slot(s) {
            ROOT => &mut self.root,
            k => match self.extra[k - 1].as_deref_mut() {
                Some(t) => t,
                None => &mut self.root,
            },
        }
    }
}

/// A growable list per space. Lists start empty and cost nothing until used.
pub struct Lists<E> {
    root: Vec<E>,
    extra: Vec<Vec<E>>,
    empty: Vec<E>,
}

impl<E> Lists<E> {
    pub const fn new() -> Self {
        Self { root: Vec::new(), extra: Vec::new(), empty: Vec::new() }
    }

    /// Drop the list of space `s`; the caller drops the returned storage
    /// outside the lock.
    pub fn remove(&mut self, s: usize) -> Vec<E> {
        if s == ROOT || s > self.extra.len() {
            return Vec::new();
        }
        core::mem::take(&mut self.extra[s - 1])
    }
}

impl<E> Spaced for Lists<E> {
    type Out = Vec<E>;
    fn of(&self, s: usize) -> &Vec<E> {
        if s == ROOT {
            &self.root
        } else {
            self.extra.get(s - 1).unwrap_or(&self.empty)
        }
    }
    fn of_mut(&mut self, s: usize) -> &mut Vec<E> {
        if s == ROOT {
            return &mut self.root;
        }
        if s > self.extra.len() {
            self.extra.resize_with(s, Vec::new);
        }
        &mut self.extra[s - 1]
    }
}

/// A held lock that dereferences to the data of one space.
pub struct Guard<G> {
    g: G,
    s: usize,
}

impl<G> Guard<G> {
    pub fn new(g: G, s: usize) -> Self {
        Self { g, s }
    }
}

impl<G, D> Deref for Guard<G>
where
    G: Deref<Target = D>,
    D: Spaced + 'static,
{
    type Target = D::Out;
    fn deref(&self) -> &D::Out {
        self.g.of(self.s)
    }
}

impl<G, D> DerefMut for Guard<G>
where
    G: DerefMut<Target = D>,
    D: Spaced + 'static,
{
    fn deref_mut(&mut self) -> &mut D::Out {
        let s = self.s;
        self.g.of_mut(s)
    }
}

/// Per-descriptor status flags of one space (what `fcntl(F_GETFL)` reports).
pub struct Flags<const N: usize> {
    pub nonblock: [AtomicBool; N],
    pub accmode: [AtomicU8; N],
}

impl<const N: usize> Flags<N> {
    pub fn reset(&self) {
        for b in self.nonblock.iter() {
            b.store(false, Ordering::Relaxed);
        }
        for a in self.accmode.iter() {
            a.store(2, Ordering::Relaxed);
        }
    }
}

/// The flag blocks of the spaces other than [`ROOT`] (null = none). Read
/// without a lock on every `fcntl`; written when a space is made or released.
pub struct FlagSlots<const N: usize>([AtomicPtr<Flags<N>>; MAX_SPACES]);

impl<const N: usize> FlagSlots<N> {
    pub const fn new() -> Self {
        Self([const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_SPACES])
    }

    /// The flags of space `s`, if it has a block of its own.
    pub fn get(&self, s: usize) -> Option<&Flags<N>> {
        if s == ROOT || s >= MAX_SPACES {
            return None;
        }
        let p = self.0[s].load(Ordering::Acquire);
        // SAFETY: a block is published by `install` and withdrawn by `remove`,
        // which the owner calls only when no task of the space can run again
        // (the space is released at reap time; stragglers were moved to ORPHAN,
        // whose block is never withdrawn).
        unsafe { p.as_ref() }
    }

    /// Give space `s` a fresh block. False when the heap has no room.
    pub fn install(&self, s: usize) -> bool {
        if s == ROOT || s >= MAX_SPACES {
            return false;
        }
        let layout = core::alloc::Layout::new::<Flags<N>>();
        // SAFETY: Flags is plain atomics; all-zero bytes are a valid value
        // (nonblock = false), and `reset` sets the access modes to their default.
        let p = unsafe { alloc::alloc::alloc_zeroed(layout) } as *mut Flags<N>;
        if p.is_null() {
            return false;
        }
        unsafe { (*p).reset() };
        let old = self.0[s].swap(p, Ordering::AcqRel);
        if !old.is_null() {
            // SAFETY: `old` came from this allocator with this layout.
            unsafe { alloc::alloc::dealloc(old as *mut u8, layout) };
        }
        true
    }

    /// Withdraw and free the block of space `s`.
    pub fn remove(&self, s: usize) {
        if s == ROOT || s == ORPHAN || s >= MAX_SPACES {
            return;
        }
        let old = self.0[s].swap(core::ptr::null_mut(), Ordering::AcqRel);
        if !old.is_null() {
            // SAFETY: as in `install`.
            unsafe { alloc::alloc::dealloc(old as *mut u8, core::alloc::Layout::new::<Flags<N>>()) };
        }
    }
}

/// Which space numbers are in use, and by how many processes.
pub struct Registry {
    refs: [u16; MAX_SPACES],
}

impl Registry {
    pub const fn new() -> Self {
        Self { refs: [0; MAX_SPACES] }
    }

    /// A free space number (never [`ROOT`] or [`ORPHAN`]) with one reference.
    pub fn take(&mut self) -> Option<usize> {
        let s = (ORPHAN + 1..MAX_SPACES).find(|&s| self.refs[s] == 0)?;
        self.refs[s] = 1;
        Some(s)
    }

    /// One more process uses `s` (a fork child). False when `s` is not in use.
    pub fn retain(&mut self, s: usize) -> bool {
        if s <= ORPHAN || s >= MAX_SPACES || self.refs[s] == 0 || self.refs[s] == u16::MAX {
            return false;
        }
        self.refs[s] += 1;
        true
    }

    /// One process less. True when that was the last one: the caller frees
    /// the tables.
    pub fn release(&mut self, s: usize) -> bool {
        if s <= ORPHAN || s >= MAX_SPACES || self.refs[s] == 0 {
            return false;
        }
        self.refs[s] -= 1;
        self.refs[s] == 0
    }

    pub fn in_use(&self) -> usize {
        self.refs.iter().filter(|&&r| r != 0).count()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::sync::Mutex;

    type T = Tab<Option<(usize, usize)>, 8>;

    fn tab_with(spaces: &[usize]) -> T {
        let mut t = T::new([None; 8]);
        for &s in spaces {
            assert!(t.reserve(s));
            t.install(s, T::block(None).unwrap());
        }
        t
    }

    #[test]
    fn any_looks_in_every_space() {
        // BUG-091: "does anybody still have file 12 open" is asked of all spaces.
        let mut t = tab_with(&[ORPHAN, 2, 3]);
        assert!(!t.any(|e| matches!(e, Some((12, _)))));
        t.of_mut(3)[4] = Some((12, 0));
        assert!(t.any(|e| matches!(e, Some((12, _)))));
        assert!(!t.any(|e| matches!(e, Some((13, _)))));
        t.of_mut(ROOT)[1] = Some((13, 9));
        assert!(t.any(|e| matches!(e, Some((13, _)))));
        let _ = t.remove(3);
        assert!(!t.any(|e| matches!(e, Some((12, _)))));
    }

    #[test]
    fn a_launch_reset_of_root_leaves_the_other_spaces_alone() {
        // BUG-060 in miniature: the launcher clears ITS table.
        let mut t = tab_with(&[ORPHAN, 2, 3]);
        t.of_mut(ROOT)[3] = Some((7, 0));
        t.of_mut(2)[3] = Some((11, 5));
        t.of_mut(3)[4] = Some((12, 0));
        t.of_mut(ROOT).fill(None); // reset_fd_table
        assert_eq!(t.of(ROOT)[3], None);
        assert_eq!(t.of(2)[3], Some((11, 5)));
        assert_eq!(t.of(3)[4], Some((12, 0)));
        assert_eq!(t.of(3)[3], None, "spaces 2 and 3 use the same fd numbers independently");
    }

    #[test]
    fn a_space_without_a_table_falls_back_to_orphan_then_root() {
        let mut t = T::new([None; 8]);
        t.of_mut(ROOT)[1] = Some((1, 1));
        // nothing installed at all: ROOT
        assert_eq!(t.of(5)[1], Some((1, 1)));
        assert!(t.reserve(ORPHAN));
        t.install(ORPHAN, T::block(None).unwrap());
        // ORPHAN exists: a missing space reads and writes it, never ROOT
        assert_eq!(t.of(5)[1], None);
        t.of_mut(5)[2] = Some((9, 9));
        assert_eq!(t.of(ORPHAN)[2], Some((9, 9)));
        assert_eq!(t.of(ROOT)[2], None);
    }

    #[test]
    fn install_remove_and_reuse_of_a_space_number() {
        let mut t = tab_with(&[ORPHAN, 4]);
        assert!(t.has(4) && !t.has(3) && t.has(ROOT));
        t.of_mut(4)[0] = Some((3, 3));
        let old = t.remove(4).expect("the table comes out");
        assert_eq!(old[0], Some((3, 3)));
        assert!(!t.has(4));
        assert!(t.remove(4).is_none());
        assert!(t.remove(ROOT).is_none(), "ROOT is never taken out");
        // the number is used again: a fresh table, nothing of the old one
        t.install(4, T::block(None).unwrap());
        assert_eq!(t.of(4)[0], None);
        // a block longer than the root's is not installed; a shorter one is
        // (the table of a small space), and has its own length
        let mut u = T::new([None; 8]);
        assert!(u.reserve(2));
        u.install(2, alloc::vec![None; 9].into_boxed_slice());
        assert!(!u.has(2));
        u.install(2, T::block_len(4, None).unwrap());
        assert!(u.has(2));
        assert_eq!(u.of(2).len(), 4);
        assert_eq!(u.of(ROOT).len(), 8);
        assert!(T::block_len(0, None).is_none() && T::block_len(9, None).is_none());
        // install without reserve is a no-op, not a panic
        let mut v = T::new([None; 8]);
        v.install(6, T::block(None).unwrap());
        assert!(!v.has(6));
    }

    #[test]
    fn lists_are_per_space_and_cost_nothing_until_written() {
        let mut l: Lists<u32> = Lists::new();
        assert!(l.of(7).is_empty());
        l.of_mut(ROOT).push(1);
        l.of_mut(3).push(30);
        l.of_mut(3).push(31);
        assert_eq!(l.of(ROOT).as_slice(), &[1]);
        assert_eq!(l.of(3).as_slice(), &[30, 31]);
        assert!(l.of(2).is_empty() && l.of(4).is_empty());
        l.of_mut(ROOT).clear(); // the launcher's reset
        assert_eq!(l.of(3).len(), 2);
        assert_eq!(l.remove(3), alloc::vec![30, 31]);
        assert!(l.of(3).is_empty());
        assert!(l.remove(ROOT).is_empty() && l.remove(99).is_empty());
    }

    #[test]
    fn a_guard_dereferences_to_its_space() {
        let m = Mutex::new(tab_with(&[ORPHAN, 2]));
        {
            let mut g = Guard::new(m.lock().unwrap(), 2);
            g[5] = Some((1, 2));
            assert_eq!(g.len(), 8);
        }
        {
            let g = Guard::new(m.lock().unwrap(), ROOT);
            assert_eq!(g[5], None);
        }
        let g = Guard::new(m.lock().unwrap(), 2);
        assert_eq!(g.get(5).copied().flatten(), Some((1, 2)));
    }

    #[test]
    fn registry_counts_references() {
        let mut r = Registry::new();
        let a = r.take().unwrap();
        let b = r.take().unwrap();
        assert!(a > ORPHAN && b > ORPHAN && a != b);
        assert_eq!(r.in_use(), 2);
        assert!(r.retain(a)); // a fork child
        assert!(!r.release(a), "the parent is reaped: the child still uses the space");
        assert!(r.release(a), "the last one");
        assert!(!r.release(a), "released twice is refused");
        assert!(!r.retain(a), "a released space cannot be shared");
        assert!(!r.retain(ROOT) && !r.release(ROOT) && !r.release(ORPHAN));
        assert_eq!(r.in_use(), 1);
        assert_eq!(r.take(), Some(a), "the number is handed out again");
    }

    #[test]
    fn registry_refuses_when_full() {
        let mut r = Registry::new();
        let mut n = 0;
        while r.take().is_some() {
            n += 1;
        }
        assert_eq!(n, MAX_SPACES - 2);
        assert!(r.take().is_none());
    }

    #[test]
    fn flags_default_to_blocking_read_write_and_are_per_space() {
        static F: FlagSlots<16> = FlagSlots::new();
        assert!(F.get(ROOT).is_none() && F.get(2).is_none());
        assert!(F.install(2) && F.install(3));
        let f2 = F.get(2).unwrap();
        assert_eq!(f2.accmode[4].load(Ordering::Relaxed), 2);
        assert!(!f2.nonblock[4].load(Ordering::Relaxed));
        f2.accmode[4].store(0, Ordering::Relaxed);
        f2.nonblock[4].store(true, Ordering::Relaxed);
        assert_eq!(F.get(3).unwrap().accmode[4].load(Ordering::Relaxed), 2);
        f2.reset();
        assert_eq!(f2.accmode[4].load(Ordering::Relaxed), 2);
        F.remove(2);
        assert!(F.get(2).is_none());
        F.remove(2); // twice is fine
        assert!(!F.install(ROOT) && !F.install(MAX_SPACES));
        F.remove(3);
    }
}
