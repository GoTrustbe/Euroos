//! IrqLock: a spin lock that is held with interrupts off, wherever it is taken (BUG-390).
//!
//! Ring-0 code runs on the BSP only, and system calls and page faults run with
//! interrupts off. A spin lock that such code takes must never be held by a
//! task the timer can preempt: once a task with interrupts off spins on it, the
//! holder never runs again and the machine stops (the BUG-007 family; BUG-390:
//! the desktop loop held DEMAND_FILE_MAPS with interrupts on in its heartbeat
//! census, the browser's main thread then spun in `child_mem_swap`). An
//! `IrqLock` turns interrupts off before it takes the lock and restores the
//! previous state after it is released, so no caller can get that wrong.
//!
//! A wait that is still not over after two seconds is said once per lock on the
//! serial console (lock, waiting task and place, last holder and place), so a
//! hang that is left (a holder that yields while holding, a lock taken twice)
//! names itself. The record of the holder is two relaxed stores per acquisition.

use core::ops::{Deref, DerefMut};
use core::panic::Location;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Waits of two seconds or longer on any `IrqLock` since boot.
pub static LONG_WAITS: AtomicU64 = AtomicU64::new(0);

pub struct IrqLock<T> {
    m: spin::Mutex<T>,
    name: &'static str,
    holder: AtomicUsize,
    site: AtomicUsize,
    said: AtomicBool,
}

pub struct IrqLockGuard<'a, T> {
    // Declaration order is drop order: the lock is released before the
    // interrupt state is restored.
    g: spin::MutexGuard<'a, T>,
    _irq: crate::sched::IfOffGuard,
}

impl<T> IrqLock<T> {
    pub const fn new(v: T, name: &'static str) -> Self {
        IrqLock {
            m: spin::Mutex::new(v),
            name,
            holder: AtomicUsize::new(usize::MAX),
            site: AtomicUsize::new(0),
            said: AtomicBool::new(false),
        }
    }

    #[track_caller]
    pub fn lock(&self) -> IrqLockGuard<'_, T> {
        let irq = crate::sched::IfOffGuard::new();
        let here = Location::caller();
        let g = match self.m.try_lock() {
            Some(g) => g,
            None => self.wait(here),
        };
        self.note(here);
        IrqLockGuard { g, _irq: irq }
    }

    #[track_caller]
    pub fn try_lock(&self) -> Option<IrqLockGuard<'_, T>> {
        let irq = crate::sched::IfOffGuard::new();
        let g = self.m.try_lock()?;
        self.note(Location::caller());
        Some(IrqLockGuard { g, _irq: irq })
    }

    fn note(&self, here: &'static Location<'static>) {
        self.holder.store(crate::sched::current_lockfree(), Ordering::Relaxed);
        self.site.store(here as *const Location<'static> as usize, Ordering::Relaxed);
    }

    #[cold]
    fn wait(&self, here: &'static Location<'static>) -> spin::MutexGuard<'_, T> {
        let t0 = unsafe { core::arch::x86_64::_rdtsc() };
        let limit = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1_000_000) * 200; // ~2 s
        let mut long = false;
        loop {
            if let Some(g) = self.m.try_lock() {
                return g;
            }
            core::hint::spin_loop();
            if !long && unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0) > limit {
                long = true;
                LONG_WAITS.fetch_add(1, Ordering::Relaxed);
                if !self.said.swap(true, Ordering::Relaxed) {
                    self.say(here);
                }
            }
        }
    }

    #[cold]
    fn say(&self, here: &'static Location<'static>) {
        let me = crate::sched::current_lockfree();
        let holder = self.holder.load(Ordering::Relaxed);
        let site = self.site.load(Ordering::Relaxed);
        let (hf, hl) = if site == 0 {
            ("?", 0)
        } else {
            // SAFETY: only `note` stores here, always the address of a `&'static Location`.
            let l = unsafe { &*(site as *const Location<'static>) };
            (l.file(), l.line())
        };
        let line = alloc::format!(
            "[irqlock] {} not free after 2 s: task {me} waits at {}:{}; last taken by task {holder}{} at {hf}:{hl}\n",
            self.name, here.file(), here.line(), if holder == me { " (this task)" } else { "" });
        crate::serial::write_raw(line.as_bytes());
    }
}

impl<T> Deref for IrqLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.g
    }
}

impl<T> DerefMut for IrqLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.g
    }
}
