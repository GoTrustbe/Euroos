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

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// The lock classes of the kernel, in the MANDATORY acquisition order (low rank =
/// outermost = taken first). The rank is the single source of truth; new locks
/// get a place in this hierarchy. Derived from the J1 lock inventory:
/// coarse/long-held at the top (scheduler), leaves that are taken from-everywhere
/// at the bottom (audit log, serial).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockClass {
    /// Scheduler (coarsest, longest held) — always first.
    Sched,
    /// Per-process tables (FILES / OPEN_FDS / PIPES / background processes).
    Process,
    /// File system / VFS.
    Fs,
    /// Encryption vault + master key (sometimes during FS ops).
    Vault,
    /// Network stack.
    Net,
    /// Agent capabilities / registry.
    Agent,
    /// Firewall rules.
    Firewall,
    /// Audit log (leaf — taken from nearly everywhere, so almost last).
    Audit,
    /// Serial port / UART (innermost leaf — logging, always last).
    Serial,
}

impl LockClass {
    /// The rank in the global total order (strictly increasing = acquisition order).
    pub const fn rank(self) -> u16 {
        match self {
            LockClass::Sched => 10,
            LockClass::Process => 20,
            LockClass::Fs => 30,
            LockClass::Vault => 40,
            LockClass::Net => 50,
            LockClass::Agent => 60,
            LockClass::Firewall => 70,
            LockClass::Audit => 80,
            LockClass::Serial => 90,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            LockClass::Sched => "Sched",
            LockClass::Process => "Process",
            LockClass::Fs => "Fs",
            LockClass::Vault => "Vault",
            LockClass::Net => "Net",
            LockClass::Agent => "Agent",
            LockClass::Firewall => "Firewall",
            LockClass::Audit => "Audit",
            LockClass::Serial => "Serial",
        }
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

#[cfg(test)]
mod tests {
    use super::LockClass::*;
    use super::*;

    const ALL: &[LockClass] = &[Sched, Process, Fs, Vault, Net, Agent, Firewall, Audit, Serial];

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
}
