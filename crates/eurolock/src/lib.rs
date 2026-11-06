//! **EuroLock** — sovereign lock-order monitoring (Sprint 5 / J1).
//!
//! Deadlocks arise when two paths take the same two locks in the OPPOSITE
//! order (A→B vs. B→A). The classic defense is a **global total
//! order** over all locks: you may only take a lock while you hold exclusively
//! locks of a LOWER rank. This crate fixes that ranking (the J1
//! audit of the EuroOS kernel) and provides a runtime detector that reports an
//! order INVERSION immediately — before it ever leads to a real hang.
//!
//! The `OrderTracker` is meant to be per-CPU (each core keeps its own held
//! stack). Pure `no_std` logic, no `unsafe`, no allocation — host-tested.
//!
//! J1 (fine-grained locking) adds [`LockStats`] contention counters and the
//! instrumented [`sync::Mutex`] / [`sync::RwLock`] that every new fine-grained
//! kernel lock is built on: per-class statistics, the IRQSAVE rule as a
//! [`Mode`], and order checking through kernel-installed [`LockHooks`].

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod sync;
pub use sync::{LockHooks, LockSite, Mode};

/// The lock classes of the kernel, in the MANDATORY acquisition order (low rank =
/// outermost = taken first). The rank is the single source of truth; new locks
/// get a place in this hierarchy. Derived from the J1 lock inventory
/// (`docs/SPRINT-PLAN-FINE-LOCKING.md`): coarse/long-held at the top (scheduler),
/// leaves that are taken from everywhere at the bottom (audit log, serial, heap).
///
/// Fine-grained (per-instance) locks share ONE class per kind: all per-mount locks
/// are `Fs`, all per-channel locks `IpcChannel`, and so on. Two instances of the
/// same class may never be nested (equal rank = inversion), which is exactly the
/// rule the code follows: no path holds two mounts, two channels or two
/// connections at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockClass {
    /// BSP scheduler (coarsest, longest held) — always first.
    Sched,
    /// Per-CPU run queue / kernel work queue of one core (J1 per-CPU scheduling).
    RunQueue,
    /// Per-process tables (FILES / OPEN_FDS / PIPES / background processes).
    Process,
    /// VFS mount table (read-mostly; held only to route a path, never across I/O).
    FsMountTable,
    /// One mounted filesystem (per-mount lock, held across that FS operation).
    Fs,
    /// Block cache of the root device (taken inside an FS operation).
    BlockCache,
    /// Encryption vault + master key (sometimes during FS ops).
    Vault,
    /// Network stack globals (config, port queues, socket-table allocation).
    Net,
    /// One network socket (per-socket state).
    NetSocket,
    /// AF_UNIX listener table (bind / connect / accept).
    UnixTable,
    /// One AF_UNIX connection (its two byte FIFOs).
    UnixConn,
    /// EuroIPC channel table (register / lookup only). Inside the network
    /// classes: with the userspace NIC driver every frame the netstack sends
    /// or receives is a EuroIPC message, taken while a socket is held. No IPC
    /// lock is ever held while another class is taken (leaf, above the heap).
    IpcTable,
    /// One EuroIPC channel (per-port message queue).
    IpcChannel,
    /// Agent capabilities / registry.
    Agent,
    /// Firewall rules.
    Firewall,
    /// Audit log (leaf, taken from nearly everywhere, so almost last).
    Audit,
    /// Serial port / UART (logging).
    Serial,
    /// Kernel heap (innermost leaf: any path may allocate while holding anything).
    Heap,
    /// Reserved for self-tests that deliberately provoke an inversion.
    TestOuter,
    /// Reserved for self-tests that deliberately provoke an inversion.
    TestInner,
}

/// Every class, in declaration (= rank) order.
pub const ALL_CLASSES: &[LockClass] = &[
    LockClass::Sched,
    LockClass::RunQueue,
    LockClass::Process,
    LockClass::FsMountTable,
    LockClass::Fs,
    LockClass::BlockCache,
    LockClass::Vault,
    LockClass::Net,
    LockClass::NetSocket,
    LockClass::UnixTable,
    LockClass::UnixConn,
    LockClass::IpcTable,
    LockClass::IpcChannel,
    LockClass::Agent,
    LockClass::Firewall,
    LockClass::Audit,
    LockClass::Serial,
    LockClass::Heap,
    LockClass::TestOuter,
    LockClass::TestInner,
];

impl LockClass {
    /// The rank in the global total order (strictly increasing = acquisition order).
    pub const fn rank(self) -> u16 {
        match self {
            LockClass::Sched => 10,
            LockClass::RunQueue => 15,
            LockClass::Process => 20,
            LockClass::FsMountTable => 28,
            LockClass::Fs => 30,
            LockClass::BlockCache => 35,
            LockClass::Vault => 40,
            LockClass::Net => 50,
            LockClass::NetSocket => 52,
            LockClass::UnixTable => 54,
            LockClass::UnixConn => 56,
            LockClass::IpcTable => 57,
            LockClass::IpcChannel => 58,
            LockClass::Agent => 60,
            LockClass::Firewall => 70,
            LockClass::Audit => 80,
            LockClass::Serial => 90,
            LockClass::Heap => 95,
            LockClass::TestOuter => 200,
            LockClass::TestInner => 201,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            LockClass::Sched => "Sched",
            LockClass::RunQueue => "RunQueue",
            LockClass::Process => "Process",
            LockClass::FsMountTable => "FsMountTable",
            LockClass::Fs => "Fs",
            LockClass::BlockCache => "BlockCache",
            LockClass::Vault => "Vault",
            LockClass::Net => "Net",
            LockClass::NetSocket => "NetSocket",
            LockClass::UnixTable => "UnixTable",
            LockClass::UnixConn => "UnixConn",
            LockClass::IpcTable => "IpcTable",
            LockClass::IpcChannel => "IpcChannel",
            LockClass::Agent => "Agent",
            LockClass::Firewall => "Firewall",
            LockClass::Audit => "Audit",
            LockClass::Serial => "Serial",
            LockClass::Heap => "Heap",
            LockClass::TestOuter => "TestOuter",
            LockClass::TestInner => "TestInner",
        }
    }
    /// Dense index (0..ALL_CLASSES.len()), for per-class counter tables.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// A detected order inversion: while `holding` was held, an attempt was made to
/// take `acquiring` — but it has a ≤ rank, so this path can form a deadlock with
/// another path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inversion {
    pub holding: LockClass,
    pub acquiring: LockClass,
}

/// Maximum lock nesting depth that we track (well above any real path).
const MAX_DEPTH: usize = 16;

/// Keeps the stack of HELD lock classes per CPU and checks that each
/// new acquisition respects the global order.
pub struct OrderTracker {
    held: [Option<LockClass>; MAX_DEPTH],
    depth: usize,
}

impl Default for OrderTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderTracker {
    pub const fn new() -> Self {
        Self { held: [None; MAX_DEPTH], depth: 0 }
    }

    /// Register a lock acquisition. Returns `Err(Inversion)` if a lock with
    /// a ≥ rank is already held (order violation → potential deadlock).
    /// On an inversion the lock is NOT pushed onto the stack (the caller decides
    /// whether to panic, log, or continue).
    pub fn acquire(&mut self, c: LockClass) -> Result<(), Inversion> {
        for i in 0..self.depth {
            let h = self.held[i].expect("held[..depth] is always Some");
            if h.rank() >= c.rank() {
                return Err(Inversion { holding: h, acquiring: c });
            }
        }
        if self.depth < MAX_DEPTH {
            self.held[self.depth] = Some(c);
            self.depth += 1;
        }
        Ok(())
    }

    /// Record `c` as held WITHOUT the order check: used after an inversion was
    /// reported, so the matching `release` stays balanced.
    pub fn push_unchecked(&mut self, c: LockClass) {
        if self.depth < MAX_DEPTH {
            self.held[self.depth] = Some(c);
            self.depth += 1;
        }
    }

    /// Release a lock (remove the topmost matching class from the stack).
    pub fn release(&mut self, c: LockClass) {
        for i in (0..self.depth).rev() {
            if self.held[i] == Some(c) {
                for j in i..self.depth - 1 {
                    self.held[j] = self.held[j + 1];
                }
                self.depth -= 1;
                self.held[self.depth] = None;
                return;
            }
        }
    }

    /// Number of locks currently held.
    pub fn depth(&self) -> usize {
        self.depth
    }
}

/// Check that the `LockClass` ranks form a strict total order (no two
/// classes with the same rank). Usable as a compile/boot sanity check.
pub fn ranks_are_total_order(classes: &[LockClass]) -> bool {
    let mut i = 0;
    while i < classes.len() {
        let mut j = i + 1;
        while j < classes.len() {
            if classes[i].rank() == classes[j].rank() {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

// ── Contention statistics ────────────────────────────────────────────────────
//
// "Measure, don't guess": every instrumented lock (or every lock of one class)
// feeds a `LockStats`. Pure atomics, no allocation, safe to update from any core
// and from interrupt context. Relaxed ordering is enough: these are counters,
// nothing synchronises through them.

use core::sync::atomic::{AtomicU64, Ordering};

/// Number of per-CPU shards (counters, replicated tables). Matches the kernel's
/// 8-core limit; with fewer cores the extra shards stay unused.
pub const SHARDS: usize = 8;

/// One cache line of counters (one shard). 64-byte aligned so two cores never
/// write the same line: a shared counter that every core increments on every
/// lock acquisition is itself a scalability bottleneck (measured: it held the
/// fine-grained ipc/unix paths below x2 on four cores).
#[repr(align(64))]
struct Shard {
    acquires: AtomicU64,
    contended: AtomicU64,
    spins: AtomicU64,
    max_spins: AtomicU64,
    try_failed: AtomicU64,
    hold_total: AtomicU64,
    hold_max: AtomicU64,
}

impl Shard {
    const fn new() -> Self {
        Shard {
            acquires: AtomicU64::new(0),
            contended: AtomicU64::new(0),
            spins: AtomicU64::new(0),
            max_spins: AtomicU64::new(0),
            try_failed: AtomicU64::new(0),
            hold_total: AtomicU64::new(0),
            hold_max: AtomicU64::new(0),
        }
    }
}

/// Counters for one lock (or one class of per-instance locks), sharded per CPU
/// (the shard is chosen by the installed hooks; without hooks everything lands
/// in shard 0). A snapshot sums the shards.
pub struct LockStats {
    shards: [Shard; SHARDS],
}

/// A copy of the counters, for reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub acquires: u64,
    pub contended: u64,
    pub spins: u64,
    pub max_spins: u64,
    pub try_failed: u64,
    /// Total / longest time the lock was HELD, in the hooks' clock units (TSC
    /// cycles in the kernel; 0 without a clock). For an `Irq`-mode lock (or any
    /// lock taken inside a syscall) the hold time is an interrupts-off window.
    pub hold_total: u64,
    pub hold_max: u64,
}

impl StatsSnapshot {
    /// Contended acquisitions per million acquisitions (avoids floats in no_std).
    pub fn contention_ppm(&self) -> u64 {
        if self.acquires == 0 {
            0
        } else {
            self.contended.saturating_mul(1_000_000) / self.acquires
        }
    }
    /// Difference `self - earlier` (for before/after measurements); `max_spins`
    /// keeps the later value.
    pub fn since(&self, earlier: &StatsSnapshot) -> StatsSnapshot {
        StatsSnapshot {
            acquires: self.acquires.saturating_sub(earlier.acquires),
            contended: self.contended.saturating_sub(earlier.contended),
            spins: self.spins.saturating_sub(earlier.spins),
            max_spins: self.max_spins,
            try_failed: self.try_failed.saturating_sub(earlier.try_failed),
            hold_total: self.hold_total.saturating_sub(earlier.hold_total),
            hold_max: self.hold_max,
        }
    }
    /// Add another snapshot (aggregate several locks of one class).
    pub fn add(&mut self, o: &StatsSnapshot) {
        self.acquires += o.acquires;
        self.contended += o.contended;
        self.spins += o.spins;
        self.max_spins = self.max_spins.max(o.max_spins);
        self.try_failed += o.try_failed;
        self.hold_total += o.hold_total;
        self.hold_max = self.hold_max.max(o.hold_max);
    }
}

impl Default for LockStats {
    fn default() -> Self {
        Self::new()
    }
}

impl LockStats {
    pub const fn new() -> Self {
        Self { shards: [const { Shard::new() }; SHARDS] }
    }

    #[inline]
    fn shard(&self) -> &Shard {
        &self.shards[sync::cpu_shard() % SHARDS]
    }

    /// Record one successful acquisition that waited `spins` iterations
    /// (0 = the lock was free on the first try).
    pub fn record_acquire(&self, spins: u64) {
        let s = self.shard();
        s.acquires.fetch_add(1, Ordering::Relaxed);
        if spins > 0 {
            s.contended.fetch_add(1, Ordering::Relaxed);
            s.spins.fetch_add(spins, Ordering::Relaxed);
            s.max_spins.fetch_max(spins, Ordering::Relaxed);
        }
    }

    /// Record how long one acquisition held the lock.
    pub fn record_hold(&self, t: u64) {
        let s = self.shard();
        s.hold_total.fetch_add(t, Ordering::Relaxed);
        s.hold_max.fetch_max(t, Ordering::Relaxed);
    }

    /// Record a `try_lock` that found the lock held.
    pub fn record_try_failed(&self) {
        let s = self.shard();
        s.contended.fetch_add(1, Ordering::Relaxed);
        s.try_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let mut out = StatsSnapshot::default();
        for s in &self.shards {
            out.add(&StatsSnapshot {
                acquires: s.acquires.load(Ordering::Relaxed),
                contended: s.contended.load(Ordering::Relaxed),
                spins: s.spins.load(Ordering::Relaxed),
                max_spins: s.max_spins.load(Ordering::Relaxed),
                try_failed: s.try_failed.load(Ordering::Relaxed),
                hold_total: s.hold_total.load(Ordering::Relaxed),
                hold_max: s.hold_max.load(Ordering::Relaxed),
            });
        }
        out
    }

    pub fn reset(&self) {
        for s in &self.shards {
            s.acquires.store(0, Ordering::Relaxed);
            s.contended.store(0, Ordering::Relaxed);
            s.spins.store(0, Ordering::Relaxed);
            s.max_spins.store(0, Ordering::Relaxed);
            s.try_failed.store(0, Ordering::Relaxed);
            s.hold_total.store(0, Ordering::Relaxed);
            s.hold_max.store(0, Ordering::Relaxed);
        }
    }
}

/// Rank `entries` (name, snapshot) by contention cost: total spins first, then
/// contended count, then acquisitions. The report order of the `[lk]` self-test
/// and the `lockstat` shell command. Sorts in place, no allocation.
pub fn rank_by_cost<T>(entries: &mut [(T, StatsSnapshot)]) {
    entries.sort_unstable_by(|a, b| {
        b.1.spins
            .cmp(&a.1.spins)
            .then(b.1.contended.cmp(&a.1.contended))
            .then(b.1.acquires.cmp(&a.1.acquires))
    });
}

#[cfg(test)]
mod tests {
    use super::LockClass::*;
    use super::*;

    const ALL: &[LockClass] = ALL_CLASSES;

    #[test]
    fn hierarchy_is_a_strict_total_order() {
        assert!(ranks_are_total_order(ALL));
        // And strictly increasing as declared.
        for w in ALL.windows(2) {
            assert!(w[0].rank() < w[1].rank());
        }
    }

    #[test]
    fn ascending_acquisition_is_allowed() {
        let mut t = OrderTracker::new();
        assert_eq!(t.acquire(Sched), Ok(()));
        assert_eq!(t.acquire(Fs), Ok(())); // coarse → fine = OK
        assert_eq!(t.acquire(Audit), Ok(()));
        assert_eq!(t.depth(), 3);
        t.release(Audit);
        t.release(Fs);
        t.release(Sched);
        assert_eq!(t.depth(), 0);
    }

    #[test]
    fn descending_acquisition_is_flagged() {
        let mut t = OrderTracker::new();
        assert_eq!(t.acquire(Fs), Ok(()));
        // Hold Fs and then take Sched (lower rank) = inversion.
        assert_eq!(t.acquire(Sched), Err(Inversion { holding: Fs, acquiring: Sched }));
        // The rejected lock is NOT on the stack.
        assert_eq!(t.depth(), 1);
    }

    #[test]
    fn reacquiring_same_class_is_flagged() {
        let mut t = OrderTracker::new();
        assert!(t.acquire(Net).is_ok());
        // The same class again (equal rank) = not allowed (no recursion).
        assert_eq!(t.acquire(Net), Err(Inversion { holding: Net, acquiring: Net }));
    }

    #[test]
    fn classic_ab_ba_deadlock_is_caught() {
        // Path 1 takes Fs→Vault (OK). Path 2 (fresh, other CPU) takes Vault→Fs: the
        // second acquisition is an inversion and is reported — exactly the A→B/B→A
        // pattern that would otherwise lead to a deadlock.
        let mut p1 = OrderTracker::new();
        assert!(p1.acquire(Fs).is_ok());
        assert!(p1.acquire(Vault).is_ok());

        let mut p2 = OrderTracker::new();
        assert!(p2.acquire(Vault).is_ok());
        assert_eq!(p2.acquire(Fs), Err(Inversion { holding: Vault, acquiring: Fs }));
    }

    #[test]
    fn release_middle_then_continue() {
        let mut t = OrderTracker::new();
        t.acquire(Sched).unwrap();
        t.acquire(Net).unwrap();
        t.release(Sched); // releasing a non-topmost lock
        assert_eq!(t.depth(), 1);
        // With only Net held, a higher rank is still allowed.
        assert!(t.acquire(Audit).is_ok());
    }

    #[test]
    fn push_unchecked_keeps_release_balanced_after_an_inversion() {
        let mut t = OrderTracker::new();
        t.acquire(Fs).unwrap();
        assert!(t.acquire(Sched).is_err());
        t.push_unchecked(Sched);
        assert_eq!(t.depth(), 2);
        t.release(Sched);
        t.release(Fs);
        assert_eq!(t.depth(), 0);
    }

    #[test]
    fn class_index_matches_table_position() {
        for (i, c) in ALL_CLASSES.iter().enumerate() {
            assert_eq!(c.index(), i, "{}", c.name());
        }
    }

    #[test]
    fn fine_grained_classes_sit_inside_their_subsystem() {
        // Route through the table, then take the instance: table < instance.
        assert!(FsMountTable.rank() < Fs.rank());
        assert!(Fs.rank() < BlockCache.rank());
        assert!(IpcTable.rank() < IpcChannel.rank());
        assert!(Net.rank() < NetSocket.rank());
        assert!(UnixTable.rank() < UnixConn.rank());
        // The userspace NIC driver: a socket send/receive is a EuroIPC message,
        // so the IPC classes sit inside the network classes.
        assert!(NetSocket.rank() < IpcTable.rank());
        assert!(Net.rank() < IpcChannel.rank());
        // The heap is the innermost leaf: allocating under any lock is legal.
        for c in ALL_CLASSES {
            if !matches!(c, Heap | TestOuter | TestInner) {
                assert!(c.rank() < Heap.rank(), "{}", c.name());
            }
        }
    }

    #[test]
    fn two_instances_of_one_class_may_not_nest() {
        // Two per-mount locks at once would let two CPUs take them in opposite
        // orders; the checker refuses the second one.
        let mut t = OrderTracker::new();
        assert!(t.acquire(Fs).is_ok());
        assert_eq!(t.acquire(Fs), Err(Inversion { holding: Fs, acquiring: Fs }));
        // Allocation under a mount lock is fine.
        assert!(t.acquire(Heap).is_ok());
    }

    #[test]
    fn stats_count_contention() {
        let s = LockStats::new();
        s.record_acquire(0);
        s.record_acquire(0);
        s.record_acquire(40);
        s.record_acquire(10);
        s.record_try_failed();
        let snap = s.snapshot();
        assert_eq!(snap.acquires, 4);
        assert_eq!(snap.contended, 3);
        assert_eq!(snap.spins, 50);
        assert_eq!(snap.max_spins, 40);
        assert_eq!(snap.try_failed, 1);
        assert_eq!(snap.contention_ppm(), 750_000);
        s.reset();
        assert_eq!(s.snapshot(), StatsSnapshot::default());
        assert_eq!(StatsSnapshot::default().contention_ppm(), 0);
    }

    #[test]
    fn stats_since_and_add() {
        let s = LockStats::new();
        s.record_acquire(5);
        let a = s.snapshot();
        s.record_acquire(0);
        s.record_acquire(7);
        let d = s.snapshot().since(&a);
        assert_eq!(d.acquires, 2);
        assert_eq!(d.contended, 1);
        assert_eq!(d.spins, 7);
        let mut sum = StatsSnapshot::default();
        sum.add(&d);
        sum.add(&a);
        assert_eq!(sum.acquires, 3);
        assert_eq!(sum.max_spins, 7);
    }

    #[test]
    fn stats_are_exact_under_real_threads() {
        // Four OS threads hammer one counter set: atomics must not lose updates.
        let s = std::sync::Arc::new(LockStats::new());
        let hs: std::vec::Vec<_> = (0..4)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for i in 0..9_999u64 {
                        s.record_acquire(i % 3);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let snap = s.snapshot();
        assert_eq!(snap.acquires, 4 * 9_999);
        assert_eq!(snap.contended, 4 * 6_666);
        assert_eq!(snap.spins, 4 * 3_333 * 3);
        assert_eq!(snap.max_spins, 2);
    }

    #[test]
    fn hold_times_sum_and_max() {
        let s = LockStats::new();
        s.record_hold(10);
        s.record_hold(250);
        s.record_hold(40);
        let a = s.snapshot();
        assert_eq!((a.hold_total, a.hold_max), (300, 250));
        s.record_hold(5);
        let d = s.snapshot().since(&a);
        assert_eq!((d.hold_total, d.hold_max), (5, 250));
    }

    #[test]
    fn ranking_puts_the_costliest_first() {
        let mut v = [
            ("cheap", StatsSnapshot { acquires: 1000, contended: 1, spins: 3, max_spins: 3, try_failed: 0, hold_total: 0, hold_max: 0 }),
            ("hot", StatsSnapshot { acquires: 10, contended: 9, spins: 9000, max_spins: 2000, try_failed: 0, hold_total: 0, hold_max: 0 }),
            ("idle", StatsSnapshot::default()),
        ];
        rank_by_cost(&mut v);
        assert_eq!(v[0].0, "hot");
        assert_eq!(v[1].0, "cheap");
        assert_eq!(v[2].0, "idle");
    }
}
