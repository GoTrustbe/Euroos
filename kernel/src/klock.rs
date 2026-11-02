//! klock: the kernel side of EuroLock (J1 fine-grained locking).
//!
//! `crates/eurolock` provides instrumented `Mutex`/`RwLock` types with per-class
//! contention counters, a documented lock hierarchy and an order tracker. This
//! module installs the hooks that only the kernel can implement:
//!
//! - **IRQSAVE**: `Mode::Irq` locks really disable interrupts (cli) while held
//!   and restore the previous state on release.
//! - **Lock order**: one `OrderTracker` per execution context. On the BSP a
//!   context is a scheduler task (a task can be preempted while it holds a
//!   `Mode::Task` lock, and the next task must not inherit its held set); on an
//!   AP it is the core (APs run kernel jobs in one context). An inversion is
//!   counted and the first ones are printed with both classes, before the
//!   acquisition can turn into a deadlock.
//! - **Task-only check**: a `Mode::Task` lock taken with IF=0 is counted per
//!   class. Such a lock is held across long work with interrupts on, so an IF=0
//!   holder could spin forever on a preempted owner (the BUG-007 family).
//!
//! It also owns the reporting (`[lk]` lines, the `lockstat` shell command) and
//! the boot self-test of the checker itself.

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use alloc::string::String;
use alloc::vec::Vec;

use eurolock::sync::{Mutex, RwLock};
use eurolock::{LockClass, LockHooks, LockSite, Mode, OrderTracker, StatsSnapshot, ALL_CLASSES};

const MAX_CPU: usize = 8;
/// BSP contexts are scheduler task slots, AP contexts follow them.
const NCTX: usize = crate::sched::MAX_TASKS + MAX_CPU;
const NCLASS: usize = ALL_CLASSES.len();

struct Trackers(UnsafeCell<[OrderTracker; NCTX]>);
// SAFETY: each tracker is touched only by its own context (one task, or one AP
// core), always with interrupts disabled for the duration of the update, so an
// interrupt handler on the same core cannot interleave with it and no other core
// ever indexes the same slot.
unsafe impl Sync for Trackers {}
static TRACKERS: Trackers = Trackers(UnsafeCell::new([const { OrderTracker::new() }; NCTX]));

static INSTALLED: AtomicBool = AtomicBool::new(false);
static BSP_ID: AtomicUsize = AtomicUsize::new(0);

/// Order inversions seen (excluding the deliberate one of the self-test).
pub static INVERSIONS: AtomicU64 = AtomicU64::new(0);
static INVERSIONS_LOGGED: AtomicU64 = AtomicU64::new(0);
/// `Mode::Task` acquisitions with interrupts disabled, per class.
static IF0_TASK: [AtomicU64; NCLASS] = [const { AtomicU64::new(0) }; NCLASS];
static IF0_LOGGED: AtomicU64 = AtomicU64::new(0);

fn cpu() -> usize {
    crate::percpu::cpu()
}

/// The context slot of the code running right now.
fn ctx() -> usize {
    let c = cpu();
    if c == BSP_ID.load(Ordering::Relaxed) {
        crate::sched::current_lockfree().min(crate::sched::MAX_TASKS - 1)
    } else {
        crate::sched::MAX_TASKS + c
    }
}

/// Print without taking a blocking lock (a lock hook may run under any lock,
/// including the UART's own): format into a stack buffer, then `write_raw`
/// (try-lock; the line is dropped if the UART is busy on this very core).
fn raw_line(args: core::fmt::Arguments) {
    struct Buf {
        b: [u8; 200],
        n: usize,
    }
    impl Write for Buf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &c in s.as_bytes() {
                if self.n < self.b.len() {
                    self.b[self.n] = c;
                    self.n += 1;
                }
            }
            Ok(())
        }
    }
    let mut b = Buf { b: [0; 200], n: 0 };
    let _ = b.write_fmt(args);
    let _ = b.write_str("\n");
    crate::serial::write_raw(&b.b[..b.n]);
}

struct KernelHooks;

impl LockHooks for KernelHooks {
    #[inline]
    fn irq_save(&self) -> bool {
        let was = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        was
    }
    #[inline]
    fn irq_restore(&self, was_enabled: bool) {
        if was_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
    fn on_acquire(&self, class: LockClass, mode: Mode, trying: bool) {
        let if_on = x86_64::instructions::interrupts::are_enabled();
        if mode == Mode::Task && !if_on {
            IF0_TASK[class.index()].fetch_add(1, Ordering::Relaxed);
            if IF0_LOGGED.fetch_add(1, Ordering::Relaxed) < 8 {
                raw_line(format_args!(
                    "[lk] WARNING: task-only lock {} taken with IF=0 (cpu {} ctx {})",
                    class.name(),
                    cpu(),
                    ctx()
                ));
            }
        }
        // Update this context's tracker with interrupts off (see `Trackers`).
        x86_64::instructions::interrupts::disable();
        let c = ctx();
        // SAFETY: see `Trackers`: own slot, interrupts off.
        let t = unsafe { &mut (*TRACKERS.0.get())[c] };
        let r = t.acquire(class);
        if let Err(inv) = r {
            // `acquire` does not record a refused class; record it anyway so the
            // matching release stays balanced.
            t.push_unchecked(class);
            if !trying && !is_selftest_pair(inv.holding, inv.acquiring) {
                INVERSIONS.fetch_add(1, Ordering::Relaxed);
                if INVERSIONS_LOGGED.fetch_add(1, Ordering::Relaxed) < 16 {
                    raw_line(format_args!(
                        "[lk] LOCK ORDER INVERSION: holding {} (rank {}) while acquiring {} (rank {}) on cpu {} ctx {}",
                        inv.holding.name(),
                        inv.holding.rank(),
                        inv.acquiring.name(),
                        inv.acquiring.rank(),
                        cpu(),
                        c
                    ));
                }
            }
        }
        if if_on {
            x86_64::instructions::interrupts::enable();
        }
    }
    #[inline]
    fn cpu(&self) -> usize {
        cpu()
    }
    #[inline]
    fn now(&self) -> u64 {
        // SAFETY: rdtsc has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    fn on_release(&self, class: LockClass, _mode: Mode) {
        let if_on = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        // SAFETY: see `Trackers`.
        unsafe { (*TRACKERS.0.get())[ctx()].release(class) };
        if if_on {
            x86_64::instructions::interrupts::enable();
        }
    }
}

static HOOKS: KernelHooks = KernelHooks;

/// Deliberate inversions of the boot self-test are counted separately.
static SELFTEST_INVERSIONS: AtomicU64 = AtomicU64::new(0);
fn is_selftest_pair(h: LockClass, a: LockClass) -> bool {
    if matches!(h, LockClass::TestOuter | LockClass::TestInner) && matches!(a, LockClass::TestOuter | LockClass::TestInner) {
        SELFTEST_INVERSIONS.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Install the hooks. Call after the Local APIC is up (`lapic_id` must work)
/// and before the APs start taking locks.
pub fn init() {
    let bsp = (crate::apic::lapic_id() & (MAX_CPU as u32 - 1)) as usize;
    crate::percpu::set_bsp(bsp);
    BSP_ID.store(bsp, Ordering::Relaxed);
    eurolock::sync::register_site(&SITE_HEAP);
    // The per-CPU heap caches may now index by Local APIC id.
    crate::allocator::percpu_ready();
    eurolock::sync::install_hooks(&HOOKS);
    INSTALLED.store(true, Ordering::Release);
    crate::serial_println!(
        "[lk] EuroLock hooks installed: IRQSAVE for Irq-mode locks, per-context order tracking ({} contexts), {} lock classes",
        NCTX,
        NCLASS
    );
}

// ── Legacy-lock measurement sites ────────────────────────────────────────────
// The global locks that existed before J1, instrumented in `Mode::Plain`
// (statistics only, behaviour unchanged) so the inventory is measured.

pub static SITE_HEAP: LockSite = LockSite::new("heap (linked-list allocator)", LockClass::Heap);

// ── Reporting ────────────────────────────────────────────────────────────────

/// Snapshot of every registered site.
pub fn site_snapshots() -> Vec<(&'static str, StatsSnapshot)> {
    eurolock::sync::sites().map(|s| (s.name, s.stats.snapshot())).collect()
}

fn fmt_row(name: &str, s: &StatsSnapshot) -> String {
    // Hold times in microseconds of guest TSC.
    let per_us = (crate::apic::TSC_PER_TICK.load(Ordering::Relaxed) / 10_000).max(1);
    alloc::format!(
        "{name}: acq={} contended={} ({}.{:02}%) spins={} max-wait={} try-fail={} hold-max={}us hold-avg={}us",
        s.acquires,
        s.contended,
        s.contention_ppm() / 10_000,
        (s.contention_ppm() / 100) % 100,
        s.spins,
        s.max_spins,
        s.try_failed,
        s.hold_max / per_us,
        s.hold_total / s.acquires.max(1) / per_us
    )
}

/// Lines for the `lockstat` shell command / `[lk]` report: sites, ranked by cost.
pub fn report_lines(top: usize) -> Vec<String> {
    let mut sites = site_snapshots();
    eurolock::rank_by_cost(&mut sites);
    let mut out = Vec::new();
    for (n, s) in sites.iter().take(top) {
        out.push(fmt_row(n, s));
    }
    let mut classes: Vec<(LockClass, StatsSnapshot)> = eurolock::sync::class_snapshots().collect();
    eurolock::rank_by_cost(&mut classes);
    for (c, s) in classes.iter().take(top) {
        out.push(fmt_row(&alloc::format!("class {}", c.name()), s));
    }
    let if0: u64 = IF0_TASK.iter().map(|a| a.load(Ordering::Relaxed)).sum();
    out.push(alloc::format!(
        "heap per-CPU caches: hits={} refills={} flushes={} parked={} KiB",
        crate::allocator::PCPU_HITS.iter().map(|c| c.0.load(Ordering::Relaxed)).sum::<u64>(),
        crate::allocator::PCPU_REFILLS.load(Ordering::Relaxed),
        crate::allocator::PCPU_FLUSHES.load(Ordering::Relaxed),
        crate::allocator::percpu_cached_bytes() / 1024
    ));
    out.push(alloc::format!(
        "order inversions={} (self-test provoked={}) task-lock-with-IF0={}",
        INVERSIONS.load(Ordering::Relaxed),
        SELFTEST_INVERSIONS.load(Ordering::Relaxed),
        if0
    ));
    out
}

/// Print the report with a tag.
pub fn report(tag: &str, top: usize) {
    for l in report_lines(top) {
        crate::serial_println!("[lk] {tag} {l}");
    }
}

/// Snapshot of all sites, for before/after deltas.
pub fn snapshot_all() -> Vec<(&'static str, StatsSnapshot)> {
    site_snapshots()
}

/// Print `now - before` per site (only sites that moved), ranked by cost.
pub fn report_delta(tag: &str, before: &[(&'static str, StatsSnapshot)], top: usize) {
    let mut d: Vec<(&'static str, StatsSnapshot)> = site_snapshots()
        .into_iter()
        .map(|(n, s)| {
            let b = before.iter().find(|(bn, _)| *bn == n).map(|x| x.1).unwrap_or_default();
            (n, s.since(&b))
        })
        .filter(|(_, s)| s.acquires + s.contended > 0)
        .collect();
    eurolock::rank_by_cost(&mut d);
    for (n, s) in d.iter().take(top) {
        crate::serial_println!("[lk] {tag} {}", fmt_row(n, s));
    }
}

/// Print the sites ranked by their longest single hold (the IF=0 windows of
/// Irq-mode locks and of locks held inside syscalls show up here).
pub fn report_hold(tag: &str, top: usize) {
    let mut sites = site_snapshots();
    sites.sort_unstable_by(|a, b| b.1.hold_max.cmp(&a.1.hold_max));
    for (n, s) in sites.iter().take(top) {
        crate::serial_println!("[lk] {tag} longest-held {}", fmt_row(n, s));
    }
}

pub fn inversions() -> u64 {
    INVERSIONS.load(Ordering::Relaxed)
}

pub fn if0_violations() -> u64 {
    IF0_TASK.iter().map(|a| a.load(Ordering::Relaxed)).sum()
}

// ── Self-test ───────────────────────────────────────────────────────────────

static ST_OUTER: LockSite = LockSite::new("selftest.outer", LockClass::TestOuter);
static ST_INNER: LockSite = LockSite::new("selftest.inner", LockClass::TestInner);
static ST_A: Mutex<u64> = Mutex::new_site(&ST_OUTER, Mode::Irq, 0);
static ST_B: Mutex<u64> = Mutex::new_site(&ST_INNER, Mode::Irq, 0);
static ST_T: Mutex<u64> = Mutex::new(LockClass::TestInner, Mode::Task, 0);
static ST_RW: RwLock<u64> = RwLock::new(LockClass::TestOuter, Mode::Task, 0);

/// `[lk]` boot self-test of the checker itself:
/// 1. an Irq-mode lock really runs with IF=0 and restores IF=1 after;
/// 2. the correct order (outer→inner) is silent;
/// 3. a deliberate inversion (inner→outer) is detected (and kept apart from the
///    real-inversion counter);
/// 4. a Task-mode lock leaves IF alone; try_lock on a held lock fails and counts.
pub fn selftest() {
    if !INSTALLED.load(Ordering::Acquire) {
        crate::serial_println!("[lk] self-test skipped: hooks not installed");
        return;
    }
    let was_on = x86_64::instructions::interrupts::are_enabled();
    // 1
    let irq_ok = {
        let mut g = ST_A.lock();
        *g += 1;
        !x86_64::instructions::interrupts::are_enabled()
    } && x86_64::instructions::interrupts::are_enabled() == was_on;
    // 2
    let real_before = INVERSIONS.load(Ordering::Relaxed);
    let prov_before = SELFTEST_INVERSIONS.load(Ordering::Relaxed);
    {
        let _a = ST_A.lock();
        let _b = ST_B.lock();
    }
    let ordered_silent = SELFTEST_INVERSIONS.load(Ordering::Relaxed) == prov_before;
    // 3
    {
        let _b = ST_B.lock();
        let _a = ST_A.lock(); // inverted on purpose
    }
    let detected = SELFTEST_INVERSIONS.load(Ordering::Relaxed) == prov_before + 1
        && INVERSIONS.load(Ordering::Relaxed) == real_before;
    // 4
    let task_if = {
        let _t = ST_T.lock();
        x86_64::instructions::interrupts::are_enabled() == was_on
    };
    let try_ok = {
        let _t = ST_T.lock();
        ST_T.try_lock().is_none()
    } && ST_T.try_lock().is_some();
    let rw_ok = {
        let r1 = ST_RW.read();
        let r2 = ST_RW.read();
        *r1 + *r2 == 0
    } && {
        *ST_RW.write() += 1;
        *ST_RW.read() == 1
    };
    // The tracker of this context is balanced again.
    let balanced = {
        x86_64::instructions::interrupts::disable();
        // SAFETY: see `Trackers`.
        let d = unsafe { (*TRACKERS.0.get())[ctx()].depth() };
        if was_on {
            x86_64::instructions::interrupts::enable();
        }
        d == 0
    };
    let ok = irq_ok && ordered_silent && detected && task_if && try_ok && rw_ok && balanced;
    crate::serial_println!(
        "[lk] checker self-test: irqsave={} correct-order-silent={} deliberate-inversion-detected={} task-mode-keeps-IF={} try_lock={} rwlock={} tracker-balanced={} -> {}",
        irq_ok,
        ordered_silent,
        detected,
        task_if,
        try_ok,
        rw_ok,
        balanced,
        if ok { "OK ✓" } else { "FAILED" }
    );
}

/// `lockstat` shell command.
pub fn shell(args: &str) -> Vec<String> {
    let top = args.trim().parse::<usize>().unwrap_or(12);
    let mut v = alloc::vec![String::from("EuroLock contention (ranked by spin cost):")];
    v.extend(report_lines(top));
    v
}
