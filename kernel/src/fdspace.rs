//! **Fd spaces**: one set of descriptor tables per process group.
//!
//! Until 2026-11 the descriptor tables of the syscall layer (`OPEN_FDS`,
//! `OPEN_DIRS`, `PIPE_FDS`, the pipes themselves, their flags and waiters) were
//! one global set. Every program launch starts with `reset_fd_table`, and that
//! wiped the descriptors of every background process alive at that moment: its
//! pipe was gone, its open file was gone, and its next `open` was handed a
//! number it still believed to be something else (BUG-060; BUG-037 had made
//! the wipe atomic, not right).
//!
//! A *space* is the unit that owns descriptors:
//!
//! * [`ROOT`] (0): the launcher (task 0), kernel tasks, and the one foreground
//!   or glibc process the launcher runs at a time, with its threads and fork
//!   children. That group keeps the model it had (shared tables, with the
//!   per-child overlays of `ring3.rs`), and `reset_fd_table` clears this space
//!   only.
//! * one space per background process started by `spawn_bg_musl` /
//!   `spawn_bg_app`. Its threads use it; a `fork` child shares it with its
//!   parent (reference counted), as the two shared the global tables before.
//!   The space is released when the last process of the group is reaped.
//!
//! The locks did not change: every table still has its one lock, taken exactly
//! where and how it was taken before. What a guard dereferences to is the
//! table of the *calling task's* space ([`current`]), which is read without a
//! lock from the scheduler's BSP mirror. Ring-3 tasks run on the BSP only; the
//! application processors run kernel work that does not touch descriptors.
//!
//! The containers (`Tab`, `Lists`, `Guard`, `FlagSlots`, `Registry`) are the
//! crate `eurofdspace`, tested on the host; this file says which task is in
//! which space.

use core::sync::atomic::{AtomicU16, Ordering};

pub use eurofdspace::{FlagSlots, Guard, Lists, Registry, Spaced, Tab, MAX_SPACES, ORPHAN, ROOT};

// One space per scheduler task is the most that can exist.
const _: () = assert!(MAX_SPACES >= crate::sched::MAX_TASKS);

static TASK_SPACE: [AtomicU16; crate::sched::MAX_TASKS] =
    [const { AtomicU16::new(0) }; crate::sched::MAX_TASKS];

/// The space of the task that is running (lock-free).
#[inline]
pub fn current() -> usize {
    of_task(crate::sched::current_lockfree())
}

/// The space of task `t` ([`ROOT`] for anything out of range).
#[inline]
pub fn of_task(t: usize) -> usize {
    if t < crate::sched::MAX_TASKS {
        TASK_SPACE[t].load(Ordering::Relaxed) as usize
    } else {
        ROOT
    }
}

/// Put task `t` in space `s`. The caller does this before the task can run.
pub fn set_task(t: usize, s: usize) {
    if t < crate::sched::MAX_TASKS && s < MAX_SPACES {
        TASK_SPACE[t].store(s as u16, Ordering::Relaxed);
    }
}

/// A scheduler slot is handed out again: its new occupant starts in [`ROOT`]
/// until a spawner says otherwise.
pub fn task_slot_reset(t: usize) {
    if t < crate::sched::MAX_TASKS {
        TASK_SPACE[t].store(ROOT as u16, Ordering::Relaxed);
    }
}

/// Move every task still pointing at `s` to [`ORPHAN`]; returns how many.
pub fn orphan_tasks_of(s: usize) -> usize {
    let mut n = 0;
    for t in TASK_SPACE.iter() {
        if t.load(Ordering::Relaxed) as usize == s {
            t.store(ORPHAN as u16, Ordering::Relaxed);
            n += 1;
        }
    }
    n
}
