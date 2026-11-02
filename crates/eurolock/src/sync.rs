//! Instrumented spinlocks (J1 fine-grained locking).
//!
//! `Mutex` and `RwLock` here are thin wrappers around the `spin` locks that add
//! three things the kernel needs to replace its global `IF=0` critical sections
//! with fine-grained locks safely:
//!
//! 1. **Contention statistics** per lock class (always) and per named lock site
//!    (optional), so "which lock hurts" is measured, not guessed.
//! 2. **The IRQSAVE rule as a type-level choice.** A lock is created in one of
//!    three [`Mode`]s:
//!    - [`Mode::Irq`]: interrupts are disabled for as long as the lock is held
//!      (and while waiting for it). Mandatory for every lock that can be reached
//!      from an `IF=0` context: interrupt handlers, and syscalls (which enter with
//!      IF cleared by FMASK). This is the rule that cost four debugging campaigns
//!      (BUG-007/010/011): a lock held with IF=1 can have its holder parked by the
//!      timer while an `IF=0` spinner on the same core waits forever.
//!    - [`Mode::Task`]: interrupts stay as they are. For long-held locks (a whole
//!      filesystem operation with disk I/O) that are only ever taken from task
//!      context. The kernel hook counts every acquisition that happens with
//!      IF=0 anyway, so a violation of "task context only" is visible.
//!    - [`Mode::Plain`]: statistics only, no ordering checks. Used to measure the
//!      legacy global locks without changing their behaviour.
//! 3. **Lock-order checking.** Before waiting for a `Task`/`Irq` lock the
//!    installed [`LockHooks`] is told which class is about to be taken; the kernel
//!    feeds that into a per-context [`crate::OrderTracker`] and reports an
//!    inversion before it can ever become a real deadlock.
//!
//! The hooks are installed once by the kernel ([`install_hooks`]). Without hooks
//! (host tests, early boot) the locks are plain instrumented spinlocks.
//! No `unsafe` here: the unsafe parts (cli/sti, per-CPU state) live in the kernel.

use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{LockClass, LockStats, StatsSnapshot, ALL_CLASSES};

/// How a lock interacts with interrupts and the order checker (see module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Statistics only (legacy locks being measured).
    Plain,
    /// Task context only; interrupts untouched; order-checked.
    Task,
    /// Interrupts disabled while held; order-checked. IRQ/syscall-reachable locks.
    Irq,
}

/// Kernel-provided callbacks. All methods must be callable from any context,
/// including with interrupts disabled, and must not take any `eurolock` lock.
pub trait LockHooks: Sync {
    /// Disable interrupts on this core; return whether they were enabled.
    fn irq_save(&self) -> bool;
    /// Re-enable interrupts if `was_enabled`.
    fn irq_restore(&self, was_enabled: bool);
    /// A lock of `class` is about to be taken (`trying` = via `try_lock`, which
    /// cannot deadlock, so an order violation is not reportable) or was just
    /// taken. Called before waiting for a blocking acquisition.
    fn on_acquire(&self, class: LockClass, mode: Mode, trying: bool);
    /// The lock of `class` was released.
    fn on_release(&self, class: LockClass, mode: Mode);
    /// The index of the current CPU (0..SHARDS), for per-CPU sharded counters
    /// and replicated tables. Default: everything on shard 0.
    fn cpu(&self) -> usize {
        0
    }
    /// A cheap monotonic clock (the kernel: TSC) for lock hold times. Default 0
    /// = hold times are not measured.
    fn now(&self) -> u64 {
        0
    }
}

#[inline]
fn now() -> u64 {
    match hooks() {
        Some(h) => h.now(),
        None => 0,
    }
}

#[inline]
fn record_hold(class: LockClass, site: Option<&'static LockSite>, t0: u64) {
    if t0 == 0 {
        return;
    }
    let t = now().saturating_sub(t0);
    CLASS_STATS[class.index()].record_hold(t);
    if let Some(s) = site {
        s.stats.record_hold(t);
    }
}

static HOOKS: spin::Once<&'static dyn LockHooks> = spin::Once::new();

/// Install the kernel hooks (once; later calls are ignored).
pub fn install_hooks(h: &'static dyn LockHooks) {
    HOOKS.call_once(|| h);
}

#[inline]
fn hooks() -> Option<&'static dyn LockHooks> {
    HOOKS.get().copied()
}

/// The current CPU's shard index (0 without hooks).
#[inline]
pub fn cpu_shard() -> usize {
    match hooks() {
        Some(h) => h.cpu() % crate::SHARDS,
        None => 0,
    }
}

// ── Per-class statistics ────────────────────────────────────────────────────

const NCLASS: usize = ALL_CLASSES.len();
static CLASS_STATS: [LockStats; NCLASS] = [const { LockStats::new() }; NCLASS];

/// The aggregated statistics of every lock of `class`.
pub fn class_stats(class: LockClass) -> &'static LockStats {
    &CLASS_STATS[class.index()]
}

/// Snapshot of every class that saw at least one acquisition or failed try.
pub fn class_snapshots() -> impl Iterator<Item = (LockClass, StatsSnapshot)> {
    ALL_CLASSES.iter().map(|&c| (c, class_stats(c).snapshot())).filter(|(_, s)| s.acquires + s.contended > 0)
}

// ── Named lock sites ────────────────────────────────────────────────────────

/// A named lock (a specific static, or "all per-port locks"), with its own
/// counters on top of the per-class ones. Registered on first use, so a report
/// can list every site that was ever taken.
pub struct LockSite {
    pub name: &'static str,
    pub class: LockClass,
    pub stats: LockStats,
    registered: AtomicBool,
}

impl LockSite {
    pub const fn new(name: &'static str, class: LockClass) -> Self {
        LockSite { name, class, stats: LockStats::new(), registered: AtomicBool::new(false) }
    }
}

const MAX_SITES: usize = 64;
static SITES: [spin::Once<&'static LockSite>; MAX_SITES] = [const { spin::Once::new() }; MAX_SITES];
static NSITES: AtomicUsize = AtomicUsize::new(0);

/// Register a site for reporting (done automatically on first acquisition of a
/// lock that uses it; call this for sites fed by hand via `stats`).
pub fn register_site(site: &'static LockSite) {
    register(site)
}

fn register(site: &'static LockSite) {
    if site.registered.load(Ordering::Relaxed) || site.registered.swap(true, Ordering::AcqRel) {
        return;
    }
    let i = NSITES.fetch_add(1, Ordering::AcqRel);
    if i < MAX_SITES {
        SITES[i].call_once(|| site);
    }
}

/// Every registered site (in registration order).
pub fn sites() -> impl Iterator<Item = &'static LockSite> {
    let n = NSITES.load(Ordering::Acquire).min(MAX_SITES);
    SITES[..n].iter().filter_map(|o| o.get().copied())
}

#[inline]
fn record(class: LockClass, site: Option<&'static LockSite>, spins: u64) {
    CLASS_STATS[class.index()].record_acquire(spins);
    if let Some(s) = site {
        register(s);
        s.stats.record_acquire(spins);
    }
}

#[inline]
fn record_try_failed(class: LockClass, site: Option<&'static LockSite>) {
    CLASS_STATS[class.index()].record_try_failed();
    if let Some(s) = site {
        register(s);
        s.stats.record_try_failed();
    }
}

/// Entry bookkeeping shared by every lock kind: returns the saved IF state.
#[inline]
fn enter(class: LockClass, mode: Mode, trying: bool) -> Option<bool> {
    let h = hooks()?;
    let saved = if mode == Mode::Irq { Some(h.irq_save()) } else { None };
    if mode != Mode::Plain {
        h.on_acquire(class, mode, trying);
    }
    saved
}

#[inline]
fn leave(class: LockClass, mode: Mode, saved: Option<bool>) {
    if let Some(h) = hooks() {
        if mode != Mode::Plain {
            h.on_release(class, mode);
        }
        if let Some(was) = saved {
            h.irq_restore(was);
        }
    }
}

/// Undo `enter` for a failed `try_*` (nothing was acquired).
#[inline]
fn abort(class: LockClass, mode: Mode, saved: Option<bool>) {
    leave(class, mode, saved)
}

// ── Mutex ───────────────────────────────────────────────────────────────────

/// An instrumented spin mutex (see module docs).
pub struct Mutex<T: ?Sized> {
    class: LockClass,
    mode: Mode,
    site: Option<&'static LockSite>,
    inner: spin::Mutex<T>,
}

impl<T> Mutex<T> {
    /// A lock of `class` in `mode`, counted only per class.
    pub const fn new(class: LockClass, mode: Mode, value: T) -> Self {
        Mutex { class, mode, site: None, inner: spin::Mutex::new(value) }
    }
    /// A lock that also counts into a named `site` (its class is the site's).
    pub const fn new_site(site: &'static LockSite, mode: Mode, value: T) -> Self {
        // The class is read from the site at run time: a const initializer may not
        // read a field of a static that has interior mutability.
        Mutex { class: LockClass::TestInner, mode, site: Some(site), inner: spin::Mutex::new(value) }
    }
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    #[inline]
    fn cls(&self) -> LockClass {
        match self.site {
            Some(s) => s.class,
            None => self.class,
        }
    }
    pub fn class(&self) -> LockClass {
        self.cls()
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Acquire, spinning (and counting the wait) while another holder has it.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let class = self.cls();
        let saved = enter(class, self.mode, false);
        let mut spins = 0u64;
        let g = loop {
            if let Some(g) = self.inner.try_lock() {
                break g;
            }
            while self.inner.is_locked() {
                spins += 1;
                core::hint::spin_loop();
            }
        };
        record(class, self.site, spins);
        MutexGuard { g: Some(g), class, mode: self.mode, saved, site: self.site, t0: now() }
    }

    /// Acquire only if free right now.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let class = self.cls();
        let saved = enter(class, self.mode, true);
        match self.inner.try_lock() {
            Some(g) => {
                record(class, self.site, 0);
                Some(MutexGuard { g: Some(g), class, mode: self.mode, saved, site: self.site, t0: now() })
            }
            None => {
                record_try_failed(class, self.site);
                abort(class, self.mode, saved);
                None
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// Exclusive access without locking (the borrow checker proves exclusivity).
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

pub struct MutexGuard<'a, T: ?Sized> {
    site: Option<&'static LockSite>,
    t0: u64,
    g: Option<spin::MutexGuard<'a, T>>,
    class: LockClass,
    mode: Mode,
    saved: Option<bool>,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.g.as_ref().expect("guard alive")
    }
}
impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.g.as_mut().expect("guard alive")
    }
}
impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // Release the lock FIRST, then the bookkeeping, then interrupts.
        drop(self.g.take());
        record_hold(self.class, self.site, self.t0);
        leave(self.class, self.mode, self.saved);
    }
}

// ── RwLock ──────────────────────────────────────────────────────────────────

/// An instrumented reader-writer spinlock. For read-mostly tables (mount table,
/// channel table): lookups run in parallel, only (un)registration serialises.
pub struct RwLock<T: ?Sized> {
    class: LockClass,
    mode: Mode,
    site: Option<&'static LockSite>,
    inner: spin::RwLock<T>,
}

impl<T> RwLock<T> {
    pub const fn new(class: LockClass, mode: Mode, value: T) -> Self {
        RwLock { class, mode, site: None, inner: spin::RwLock::new(value) }
    }
    pub const fn new_site(site: &'static LockSite, mode: Mode, value: T) -> Self {
        RwLock { class: LockClass::TestInner, mode, site: Some(site), inner: spin::RwLock::new(value) }
    }
}

impl<T: ?Sized> RwLock<T> {
    #[inline]
    fn cls(&self) -> LockClass {
        match self.site {
            Some(s) => s.class,
            None => self.class,
        }
    }
    pub fn read(&self) -> ReadGuard<'_, T> {
        let class = self.cls();
        let saved = enter(class, self.mode, false);
        let mut spins = 0u64;
        let g = loop {
            if let Some(g) = self.inner.try_read() {
                break g;
            }
            spins += 1;
            core::hint::spin_loop();
        };
        record(class, self.site, spins);
        ReadGuard { g: Some(g), class, mode: self.mode, saved, site: self.site, t0: now() }
    }

    pub fn write(&self) -> WriteGuard<'_, T> {
        let class = self.cls();
        let saved = enter(class, self.mode, false);
        let mut spins = 0u64;
        let g = loop {
            if let Some(g) = self.inner.try_write() {
                break g;
            }
            spins += 1;
            core::hint::spin_loop();
        };
        record(class, self.site, spins);
        WriteGuard { g: Some(g), class, mode: self.mode, saved, site: self.site, t0: now() }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

pub struct ReadGuard<'a, T: ?Sized> {
    site: Option<&'static LockSite>,
    t0: u64,
    g: Option<spin::RwLockReadGuard<'a, T>>,
    class: LockClass,
    mode: Mode,
    saved: Option<bool>,
}
impl<T: ?Sized> Deref for ReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.g.as_ref().expect("guard alive")
    }
}
impl<T: ?Sized> Drop for ReadGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.g.take());
        record_hold(self.class, self.site, self.t0);
        leave(self.class, self.mode, self.saved);
    }
}

pub struct WriteGuard<'a, T: ?Sized> {
    site: Option<&'static LockSite>,
    t0: u64,
    g: Option<spin::RwLockWriteGuard<'a, T>>,
    class: LockClass,
    mode: Mode,
    saved: Option<bool>,
}
impl<T: ?Sized> Deref for WriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.g.as_ref().expect("guard alive")
    }
}
impl<T: ?Sized> DerefMut for WriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.g.as_mut().expect("guard alive")
    }
}
impl<T: ?Sized> Drop for WriteGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.g.take());
        record_hold(self.class, self.site, self.t0);
        leave(self.class, self.mode, self.saved);
    }
}


// ── ReadMostly: a per-CPU replicated reader-writer lock ─────────────────────

/// A read-mostly table, replicated once per CPU shard.
///
/// Even an uncontended `RwLock::read` is an atomic read-modify-write on ONE
/// shared lock word, so every lookup in a shared table (which channel is port
/// 501? which mount owns /var?) bounces that cache line between all cores. On
/// four cores that alone kept the per-channel IPC path near x1.7 (measured).
///
/// `ReadMostly<T>` keeps one copy of `T` per shard, each behind its own
/// cache-line-aligned `spin::RwLock`. A reader locks only its own CPU's copy
/// (no line shared with other readers). A writer takes every shard's write lock
/// in index order (so writers cannot deadlock each other), applies the same
/// update to every copy, then releases them all: once `write` returns, every
/// subsequent read on any core sees the update. Updates must therefore be
/// deterministic (the closure runs once per copy); tables of `Arc`s make the
/// copies cheap. The whole replica set is ONE lock for the order checker.
pub struct ReadMostly<T> {
    class: LockClass,
    mode: Mode,
    site: Option<&'static LockSite>,
    shards: [Replica<T>; crate::SHARDS],
}

/// One cache-line-aligned copy of a [`ReadMostly`] table.
#[repr(align(64))]
pub struct Replica<T>(spin::RwLock<T>);

impl<T> Replica<T> {
    pub const fn new(v: T) -> Self {
        Replica(spin::RwLock::new(v))
    }
}

impl<T> ReadMostly<T> {
    /// `copies` = one identical initial replica per shard, e.g.
    /// `[const { Replica::new(Vec::new()) }; SHARDS]` (keeps this `const`).
    pub const fn new_site(site: &'static LockSite, mode: Mode, copies: [Replica<T>; crate::SHARDS]) -> Self {
        ReadMostly { class: LockClass::TestInner, mode, site: Some(site), shards: copies }
    }

    #[inline]
    fn cls(&self) -> LockClass {
        match self.site {
            Some(s) => s.class,
            None => self.class,
        }
    }

    /// Run `f` on this CPU's copy, under that copy's read lock only.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let class = self.cls();
        let saved = enter(class, self.mode, false);
        let l = &self.shards[cpu_shard() % crate::SHARDS].0;
        let mut spins = 0u64;
        let g = loop {
            if let Some(g) = l.try_read() {
                break g;
            }
            spins += 1;
            core::hint::spin_loop();
        };
        record(class, self.site, spins);
        let r = f(&g);
        drop(g);
        leave(class, self.mode, saved);
        r
    }

    /// Apply `f` to every copy (all write locks held at once); returns the
    /// result of the last application.
    pub fn write<R>(&self, mut f: impl FnMut(&mut T) -> R) -> R {
        let class = self.cls();
        let saved = enter(class, self.mode, false);
        let mut spins = 0u64;
        let mut guards: [Option<spin::RwLockWriteGuard<'_, T>>; crate::SHARDS] = [const { None }; crate::SHARDS];
        for (i, s) in self.shards.iter().enumerate() {
            guards[i] = Some(loop {
                if let Some(g) = s.0.try_write() {
                    break g;
                }
                spins += 1;
                core::hint::spin_loop();
            });
        }
        record(class, self.site, spins);
        let mut r = None;
        for g in guards.iter_mut() {
            r = Some(f(g.as_mut().expect("all write guards held")));
        }
        drop(guards);
        leave(class, self.mode, saved);
        r.expect("SHARDS > 0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Inversion, OrderTracker};
    use std::cell::RefCell;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::vec::Vec;

    // A host stand-in for the kernel hooks: a per-THREAD order tracker (a host
    // thread plays the role of a kernel context) plus an IF flag per thread.
    std::thread_local! {
        static TRACK: RefCell<OrderTracker> = const { RefCell::new(OrderTracker::new()) };
        static IF: RefCell<bool> = const { RefCell::new(true) };
        static INVERSIONS: RefCell<Vec<Inversion>> = const { RefCell::new(Vec::new()) };
    }
    static IRQ_SAVES: AtomicU64 = AtomicU64::new(0);

    struct TestHooks;
    impl LockHooks for TestHooks {
        fn irq_save(&self) -> bool {
            IRQ_SAVES.fetch_add(1, Ordering::Relaxed);
            IF.with(|f| core::mem::replace(&mut *f.borrow_mut(), false))
        }
        fn irq_restore(&self, was: bool) {
            if was {
                IF.with(|f| *f.borrow_mut() = true);
            }
        }
        fn on_acquire(&self, class: LockClass, _mode: Mode, trying: bool) {
            TRACK.with(|t| {
                let mut t = t.borrow_mut();
                match t.acquire(class) {
                    Ok(()) => {}
                    Err(inv) => {
                        if !trying {
                            INVERSIONS.with(|v| v.borrow_mut().push(inv));
                        }
                    }
                }
            });
        }
        fn on_release(&self, class: LockClass, _mode: Mode) {
            TRACK.with(|t| t.borrow_mut().release(class));
        }
        fn cpu(&self) -> usize {
            CPU.with(|c| *c)
        }
    }
    static NEXT_CPU: AtomicU64 = AtomicU64::new(0);
    std::thread_local! {
        static CPU: usize = (NEXT_CPU.fetch_add(1, Ordering::Relaxed) % 8) as usize;
    }
    static TEST_HOOKS: TestHooks = TestHooks;

    fn setup() {
        install_hooks(&TEST_HOOKS);
    }
    fn if_enabled() -> bool {
        IF.with(|f| *f.borrow())
    }
    fn inversions() -> Vec<Inversion> {
        INVERSIONS.with(|v| v.borrow().clone())
    }

    #[test]
    fn irq_mode_disables_interrupts_while_held_and_restores() {
        setup();
        let m = Mutex::new(LockClass::IpcChannel, Mode::Irq, 5u32);
        assert!(if_enabled());
        {
            let mut g = m.lock();
            *g += 1;
            assert!(!if_enabled(), "IF must be 0 while an Irq lock is held");
        }
        assert!(if_enabled(), "IF restored after release");
        assert_eq!(*m.lock(), 6);
    }

    #[test]
    fn irq_mode_nesting_restores_only_at_the_outermost() {
        setup();
        let a = Mutex::new(LockClass::IpcTable, Mode::Irq, ());
        let b = Mutex::new(LockClass::IpcChannel, Mode::Irq, ());
        let ga = a.lock();
        let gb = b.lock();
        drop(gb);
        assert!(!if_enabled(), "inner release must not re-enable while the outer is held");
        drop(ga);
        assert!(if_enabled());
    }

    #[test]
    fn task_mode_leaves_interrupts_alone() {
        setup();
        let m = Mutex::new(LockClass::Fs, Mode::Task, ());
        let _g = m.lock();
        assert!(if_enabled());
    }

    #[test]
    fn order_violation_is_reported_before_waiting() {
        setup();
        let outer = Mutex::new(LockClass::TestOuter, Mode::Task, ());
        let inner = Mutex::new(LockClass::TestInner, Mode::Task, ());
        {
            let _i = inner.lock();
            let _o = outer.lock(); // inverted: rank(TestOuter) < rank(TestInner)
        }
        let inv = inversions();
        assert_eq!(inv, std::vec![Inversion { holding: LockClass::TestInner, acquiring: LockClass::TestOuter }]);
        // The correct order is silent.
        let before = inversions().len();
        {
            let _o = outer.lock();
            let _i = inner.lock();
        }
        assert_eq!(inversions().len(), before);
    }

    #[test]
    fn try_lock_never_reports_an_inversion_and_counts_failures() {
        setup();
        static SITE: LockSite = LockSite::new("test.try", LockClass::TestInner);
        let outer = Mutex::new(LockClass::TestOuter, Mode::Task, ());
        let inner = Mutex::new_site(&SITE, Mode::Task, ());
        let gi = inner.lock();
        let before = inversions().len();
        let o = outer.try_lock();
        assert!(o.is_some());
        assert_eq!(inversions().len(), before, "a try_lock cannot deadlock");
        drop(o);
        // The held lock refuses a try and counts it.
        assert!(inner.try_lock().is_none());
        drop(gi);
        let s = SITE.stats.snapshot();
        assert_eq!(s.try_failed, 1);
        assert_eq!(s.acquires, 1);
        assert!(sites().any(|x| x.name == "test.try"));
    }

    #[test]
    fn plain_mode_is_not_order_checked() {
        setup();
        let outer = Mutex::new(LockClass::TestOuter, Mode::Plain, ());
        let inner = Mutex::new(LockClass::TestInner, Mode::Plain, ());
        let before = inversions().len();
        let _i = inner.lock();
        let _o = outer.lock();
        assert_eq!(inversions().len(), before);
    }

    #[test]
    fn mutex_is_exclusive_and_counts_contention_under_threads() {
        setup();
        static SITE: LockSite = LockSite::new("test.hot", LockClass::TestOuter);
        let m = Arc::new(Mutex::new_site(&SITE, Mode::Irq, 0u64));
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let m = m.clone();
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        let mut g = m.lock();
                        // A non-atomic read-modify-write: only exclusion keeps it exact.
                        let v = *g;
                        core::hint::black_box(&v);
                        *g = v + 1;
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(*m.lock(), 80_000);
        let s = SITE.stats.snapshot();
        assert_eq!(s.acquires, 80_001);
    }

    #[test]
    fn readmostly_writes_reach_every_replica_and_reads_stay_consistent() {
        setup();
        static SITE: LockSite = LockSite::new("test.readmostly", LockClass::IpcTable);
        let t = Arc::new(ReadMostly::new_site(&SITE, Mode::Irq, [const { Replica::new(Vec::<u32>::new()) }; crate::SHARDS]));
        // Writers append 0..N in order on one thread; readers on 6 threads (6
        // different shards) must always see a prefix 0..k (never a gap, never
        // a value out of order), and IF must be restored.
        let n = 3_000u32;
        let w = {
            let t = t.clone();
            std::thread::spawn(move || {
                for i in 0..n {
                    t.write(|v| v.push(i));
                }
            })
        };
        let readers: Vec<_> = (0..6)
            .map(|_| {
                let t = t.clone();
                std::thread::spawn(move || {
                    let mut last = 0usize;
                    loop {
                        let (len, ok) = t.read(|v| (v.len(), v.iter().enumerate().all(|(i, &x)| x == i as u32)));
                        assert!(ok, "replica must be a gap-free prefix");
                        assert!(len >= last, "a reader never goes back in time on its own shard");
                        last = len;
                        assert!(if_enabled());
                        if len == n as usize {
                            break;
                        }
                    }
                })
            })
            .collect();
        w.join().unwrap();
        for r in readers {
            r.join().unwrap();
        }
        // After write returned, every shard has the full table.
        for s in 0..crate::SHARDS {
            assert_eq!(t.shards[s].0.read().len(), n as usize);
        }
        let st = SITE.stats.snapshot();
        assert!(st.acquires >= n as u64);
    }

    #[test]
    fn rwlock_readers_share_and_writer_excludes() {
        setup();
        let l = Arc::new(RwLock::new(LockClass::FsMountTable, Mode::Task, Vec::<u32>::new()));
        {
            let r1 = l.read();
            let r2 = l.read(); // two readers at once
            assert_eq!(r1.len() + r2.len(), 0);
        }
        let hs: Vec<_> = (0..4)
            .map(|t| {
                let l = l.clone();
                std::thread::spawn(move || {
                    for i in 0..2_000u32 {
                        if i % 10 == 0 {
                            l.write().push(t);
                        } else {
                            let r = l.read();
                            core::hint::black_box(r.len());
                        }
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(l.read().len(), 4 * 200);
    }
}
