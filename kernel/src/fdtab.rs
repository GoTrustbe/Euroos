//! **Descriptor tables per process** (BUG-098, step 2 of the fd-space design).
//!
//! A fd *space* (`fdspace.rs`) owns OBJECTS: open file descriptions, pipe
//! ends, directory handles in its tables, and the sockets, eventfds and epoll
//! instances of the global classes. Every object has one number there, and
//! that number is what the handlers of the syscall layer work with.
//!
//! A *process* owns DESCRIPTORS: the numbers the program sees. Each process
//! has its own table (`eurofdspace::ProcTab`) from descriptor to object, with
//! the close-on-exec flag per descriptor. Several descriptors, in one process
//! or in several, can name one object; `eurofdspace::Refs` counts them, and
//! the object is closed when the last one goes. That is the Linux model:
//!
//! * `dup`, `dup2`, `dup3`, `F_DUPFD` add a descriptor for the same object, so
//!   offset and status flags are shared;
//! * `fork` copies the table: the child's `close` closes nothing for the
//!   parent, and what the child opens does not exist in the parent;
//! * a pipe or socket end is closed when its last descriptor is, in whatever
//!   process that is, which is when the other side sees end-of-file, `EPIPE`
//!   and `POLLHUP`;
//! * `execve` closes the close-on-exec descriptors of the calling process;
//! * `SCM_RIGHTS` carries the object: a reference travels with the message
//!   and becomes a descriptor in the receiver's table;
//! * numbers are handed out lowest first, whatever the kind of object, up to
//!   `RLIMIT_NOFILE` per process.
//!
//! [`syscall`] is the one place where descriptors become objects: it sits in
//! front of the dispatchers (the Linux one and the background one), answers
//! the calls that only concern the table (`close`, the `dup` family, the
//! descriptor flags of `fcntl`, `close_range`) itself, replaces descriptor
//! arguments by objects, and gives a new object its descriptor on the way
//! out. Where descriptors travel through the caller's memory the handler asks
//! the table itself: the two of a pipe or socket pair ([`install_pair`]), a
//! `pollfd` array ([`resolve`]), `SCM_RIGHTS` ([`scm_send`], [`scm_receive`]). An object named by a call in progress holds a reference for the
//! duration, so a `close` in another thread cannot pull it away.
//!
//! One lock (`FDT`), a leaf: taken with interrupts off, never held across a
//! call into the handlers.

use super::*;
use core::sync::atomic::AtomicU64 as A64;
use eurofdspace::{Desc, ProcTab, Refs, TabError};

const EBADF: u64 = (-9i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const EMFILE: u64 = (-24i64) as u64;
const ENOMEM: u64 = (-12i64) as u64;

/// Process ids of background processes are `BG_PROC | pid`; the processes of
/// the launcher's space are 0 (the launched program) and the main task of each
/// fork child.
const BG_PROC: u64 = 1 << 40;
/// Objects 0, 1 and 2 are the standard streams: never counted, never closed.
const STD: u64 = 3;
/// Descriptor numbers never go beyond this, whatever a process sets its limit to.
pub(super) const MAX_DESCRIPTORS: usize = 65536;
/// `RLIMIT_NOFILE` of a fresh process (soft and hard).
pub(super) const NOFILE_DEFAULT: u64 = 4096;

struct State {
    /// Sorted by process id.
    procs: alloc::vec::Vec<(u64, ProcTab)>,
    refs: Refs,
}

static FDT: Mutex<State> = Mutex::new(State { procs: alloc::vec::Vec::new(), refs: Refs::new() });

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let _g = crate::sched::IfOffGuard::new();
    let mut s = FDT.lock();
    f(&mut s)
}

impl State {
    /// The table of `proc`, made on first use with the standard streams.
    fn tab(&mut self, proc: u64) -> Option<&mut ProcTab> {
        let i = match self.procs.binary_search_by_key(&proc, |e| e.0) {
            Ok(i) => i,
            Err(i) => {
                let t = ProcTab::with_std()?;
                self.procs.try_reserve(1).ok()?;
                self.procs.insert(i, (proc, t));
                i
            }
        };
        Some(&mut self.procs[i].1)
    }

    fn remove(&mut self, proc: u64) -> Option<ProcTab> {
        let i = self.procs.binary_search_by_key(&proc, |e| e.0).ok()?;
        Some(self.procs.remove(i).1)
    }
}

/// The reference-count key of object `obj` in fd space `space`. The socket,
/// eventfd and epoll classes are numbered once for the whole system.
fn key(space: usize, obj: u64) -> u64 {
    if obj >= GLOBAL_OBJ_BASE {
        obj
    } else {
        ((space as u64 + 1) << 32) | obj
    }
}

// ── the last descriptor events (diagnostics) ───────────────────────────────

/// What happened to the descriptor tables last: (process, what, descriptor,
/// object, system call). A program that aborts over its descriptors (chrome:
/// "FD ownership violation") gets the events of its own process printed.
const EVENTS: usize = 1024;
static EVENT_RING: Mutex<([(u64, u8, u32, u32, u16); EVENTS], usize)> = Mutex::new(([(0, 0, 0, 0, 0); EVENTS], 0));

fn note(proc: u64, what: u8, fd: u64, obj: u64, sys: u64) {
    let _g = crate::sched::IfOffGuard::new();
    let mut r = EVENT_RING.lock();
    let i = r.1 % EVENTS;
    r.0[i] = (proc + 1, what, fd as u32, obj as u32, sys as u16);
    r.1 += 1;
}

/// Print the last `max` descriptor events of the running process and what it
/// has open now.
pub fn dump_current(max: usize) {
    let proc = cur_proc();
    let rows: alloc::vec::Vec<(u8, u32, u32, u16)> = {
        let _g = crate::sched::IfOffGuard::new();
        let r = EVENT_RING.lock();
        let n = r.1.min(EVENTS);
        let mut v = alloc::vec::Vec::new();
        for k in 0..n {
            let e = r.0[(r.1 - n + k) % EVENTS];
            if e.0 == proc + 1 {
                v.push((e.1, e.2, e.3, e.4));
            }
        }
        v
    };
    let skip = rows.len().saturating_sub(max);
    let mut line = String::new();
    for (what, fd, obj, sys) in rows.into_iter().skip(skip) {
        let w = match what {
            b'A' => "new",
            b'C' => "close",
            b'D' => "dup",
            b'2' => "dup2",
            b'S' => "received",
            b'X' => "closed-at-exec",
            b'R' => "close_range",
            b'F' => "fork-copy",
            _ => "?",
        };
        line.push_str(&alloc::format!(" {w} {fd}(obj {obj:#x}{})", if sys != 0 { alloc::format!(", syscall {sys}") } else { String::new() }));
        if line.len() > 900 {
            crate::serial_println!("[fdx] process {proc}:{line}");
            line.clear();
        }
    }
    if !line.is_empty() {
        crate::serial_println!("[fdx] process {proc}:{line}");
    }
    let open = list();
    let mut l = String::new();
    for (fd, obj) in open.iter() {
        l.push_str(&alloc::format!(" {fd}={}", fd_kind(*obj)));
    }
    crate::serial_println!("[fdx] process {proc} has {} descriptors open:{l}", open.len());
}

// ── which process is running ────────────────────────────────────────────────

/// The pid of the background process each task belongs to (0: none).
static TASK_PID: [A64; crate::sched::MAX_TASKS] = [const { A64::new(0) }; crate::sched::MAX_TASKS];

/// Task `t` belongs to background process `pid`. Set by the spawner before the
/// task can run.
pub(super) fn set_task_pid(t: usize, pid: u64) {
    if t < crate::sched::MAX_TASKS {
        TASK_PID[t].store(pid, Ordering::Relaxed);
    }
}

pub(super) fn task_slot_reset(t: usize) {
    if t < crate::sched::MAX_TASKS {
        TASK_PID[t].store(0, Ordering::Relaxed);
    }
}

/// The id of a background process.
pub(super) fn bg_proc(pid: u64) -> u64 {
    BG_PROC | pid
}

/// The id of the fork child whose main task is `task` (launcher's space).
pub(super) fn child_proc(task: usize) -> u64 {
    task as u64
}

/// The process the running task belongs to.
pub(super) fn cur_proc() -> u64 {
    let cur = crate::sched::current();
    if fdspace::of_task(cur) == fdspace::ROOT {
        fork_child_owner(cur).unwrap_or(0) as u64
    } else if cur < crate::sched::MAX_TASKS {
        BG_PROC | TASK_PID[cur].load(Ordering::Relaxed)
    } else {
        BG_PROC
    }
}

/// The soft `RLIMIT_NOFILE` of the running process.
fn limit() -> usize {
    let key = proc_key(crate::sched::current());
    match proc_limits(key).get(eurosandbox::rlimits::RLIMIT_NOFILE) {
        Ok((cur, _)) => (cur as usize).min(MAX_DESCRIPTORS),
        Err(_) => NOFILE_DEFAULT as usize,
    }
}

// ── references held by calls in progress ────────────────────────────────────

/// Per task: the keys of the objects its current system call was handed
/// (0 = none), and the process that task belongs to (id + 1; 0 = none). Read
/// when a process is ended from outside while a task of it is in a call.
static HOLDS: [[A64; 2]; crate::sched::MAX_TASKS] = [const { [const { A64::new(0) }; 2] }; crate::sched::MAX_TASKS];
static HOLD_PROC: [A64; crate::sched::MAX_TASKS] = [const { A64::new(0) }; crate::sched::MAX_TASKS];

/// The object number inside a key (see [`key`]).
fn key_obj(k: u64) -> u64 {
    k & 0xffff_ffff
}

// ── the calls that only concern the table ───────────────────────────────────

type Inner<'a> = &'a mut dyn FnMut(u64, u64, u64, u64, u64, u64) -> u64;

fn as_fd(v: u64) -> Option<usize> {
    let fd = v as u32 as i32;
    if fd < 0 {
        None
    } else {
        Some(fd as usize)
    }
}

/// The object descriptor `fd` of the running process names.
pub(super) fn resolve(fd: u64) -> Option<u64> {
    let fd = as_fd(fd)?;
    let proc = cur_proc();
    with(|s| s.tab(proc).and_then(|t| t.get(fd)).map(|d| d.obj as u64))
}

/// Is `fd` an open descriptor of the running process?
pub(super) fn is_open(fd: u64) -> bool {
    resolve(fd).is_some()
}

/// Drop one reference to `obj`; the last one closes it through `close`.
fn unref(space: usize, obj: u64, close: &mut dyn FnMut(u64)) {
    if obj < STD {
        return;
    }
    if with(|s| s.refs.dec(key(space, obj))) {
        close(obj);
    }
}

/// Drop one reference to `obj` (a descriptor that was in flight and is not
/// delivered); the last one closes the object.
pub(super) fn unref_obj(obj: u64) {
    unref(fdspace::current(), obj, &mut |o| {
        let _ = close_fd_now(o);
    });
}

fn sys_close(proc: u64, space: usize, fd: u64, inner: Inner) -> u64 {
    let Some(fd) = as_fd(fd) else { return EBADF };
    let Some(d) = with(|s| s.tab(proc).and_then(|t| t.take(fd))) else { return EBADF };
    note(proc, b'C', fd as u64, d.obj as u64, 3);
    unref(space, d.obj as u64, &mut |o| {
        let _ = inner(3, o, 0, 0, 0, 0);
    });
    0
}

/// A new descriptor at the lowest free number at or above `from` for the
/// object `old` names.
fn sys_dup(proc: u64, space: usize, old: u64, from: u64, cloexec: bool) -> u64 {
    let Some(old) = as_fd(old) else { return EBADF };
    let lim = limit();
    if from as usize >= lim {
        return EINVAL;
    }
    with(|s| {
        let Some(t) = s.tab(proc) else { return ENOMEM };
        let Some(d) = t.get(old) else { return EBADF };
        match t.install(from as usize, lim, Desc { obj: d.obj, cloexec }) {
            Ok(fd) => {
                if d.obj as u64 >= STD && !s.refs.inc(key(space, d.obj as u64)) {
                    if let Some(t) = s.tab(proc) {
                        t.take(fd);
                    }
                    return ENOMEM;
                }
                note(proc, b'D', fd as u64, d.obj as u64, old as u64);
                fd as u64
            }
            Err(TabError::Full) => EMFILE,
            Err(TabError::NoMem) => ENOMEM,
        }
    })
}

fn sys_dup2(proc: u64, space: usize, old: u64, new: u64, cloexec: bool, inner: Inner) -> u64 {
    let (Some(old), Some(new)) = (as_fd(old), as_fd(new)) else { return EBADF };
    let lim = limit();
    let r = with(|s| {
        let Some(t) = s.tab(proc) else { return Err(ENOMEM) };
        let Some(d) = t.get(old) else { return Err(EBADF) };
        if old == new {
            return Ok(None);
        }
        if d.obj as u64 >= STD && !s.refs.inc(key(space, d.obj as u64)) {
            return Err(ENOMEM);
        }
        let Some(t) = s.tab(proc) else { return Err(ENOMEM) };
        match t.put(new, lim, Desc { obj: d.obj, cloexec }) {
            Ok(was) => Ok(was),
            Err(e) => {
                if d.obj as u64 >= STD {
                    s.refs.dec(key(space, d.obj as u64));
                }
                Err(match e {
                    TabError::Full => EBADF, // newfd out of the allowed range
                    TabError::NoMem => ENOMEM,
                })
            }
        }
    });
    match r {
        Err(e) => e,
        Ok(was) => {
            note(proc, b'2', new as u64, 0, old as u64);
            if let Some(w) = was {
                unref(space, w.obj as u64, &mut |o| {
                    let _ = inner(3, o, 0, 0, 0, 0);
                });
            }
            new as u64
        }
    }
}

fn sys_close_range(proc: u64, space: usize, first: u64, last: u64, flags: u64, inner: Inner) -> u64 {
    const UNSHARE: u64 = 2;
    const CLOEXEC: u64 = 4;
    let (first, last) = (first as u32 as usize, last as u32 as usize);
    if flags & !(UNSHARE | CLOEXEC) != 0 || first > last {
        return EINVAL;
    }
    let mut gone: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    let ok = with(|s| {
        let Some(t) = s.tab(proc) else { return false };
        if flags & CLOEXEC != 0 {
            let top = t.top();
            for fd in first..=last.min(top) {
                t.set_cloexec(fd, true);
            }
            return true;
        }
        let n = t.iter().filter(|&(fd, _)| fd >= first && fd <= last).count();
        if gone.try_reserve(n).is_err() {
            return false;
        }
        t.close_where(|fd, _| fd >= first && fd <= last, |d| gone.push(d.obj));
        gone.retain(|&o| o as u64 >= STD && s.refs.dec(key(space, o as u64)));
        true
    });
    if !ok {
        return ENOMEM;
    }
    note(proc, b'R', first as u64, last.min(0xffff_ffff) as u64, flags);
    for o in gone {
        let _ = inner(3, o as u64, 0, 0, 0, 0);
    }
    0
}

// ── new objects get a descriptor ────────────────────────────────────────────

/// Give the freshly made object `obj` a descriptor in the running process
/// (one reference). On failure the object is closed and the errno returned.
fn adopt(proc: u64, space: usize, obj: u64, sys: u64, inner: Inner) -> u64 {
    // The handlers mark a close-on-exec object in the old per-owner list
    // (`fd_set_cloexec`); the flag moves to the descriptor here.
    let cloexec = fd_is_cloexec(obj);
    if cloexec {
        fd_set_cloexec(obj, false);
    }
    let lim = limit();
    let r = with(|s| {
        let Some(t) = s.tab(proc) else { return Err(ENOMEM) };
        match t.install(0, lim, Desc { obj: obj as u32, cloexec }) {
            Ok(fd) => {
                if obj >= STD && !s.refs.inc(key(space, obj)) {
                    if let Some(t) = s.tab(proc) {
                        t.take(fd);
                    }
                    return Err(ENOMEM);
                }
                Ok(fd as u64)
            }
            Err(TabError::Full) => Err(EMFILE),
            Err(TabError::NoMem) => Err(ENOMEM),
        }
    });
    match r {
        Ok(fd) => {
            note(proc, b'A', fd, obj, sys);
            fd
        }
        Err(e) => {
            if obj >= STD {
                let _ = inner(3, obj, 0, 0, 0, 0);
            }
            e
        }
    }
}

/// `pipe`, `pipe2`, `socketpair`: the two objects the handler just made
/// become descriptors of the running process, lowest numbers first. Called by
/// the handler itself (it has the caller's memory in reach for the `int[2]`).
/// Err(errno): nothing was installed and the handler closes the two objects.
pub(super) fn install_pair(a: u64, b: u64) -> Result<(u64, u64), u64> {
    let (proc, space) = (cur_proc(), fdspace::current());
    let mut flag = |obj: u64| {
        let c = fd_is_cloexec(obj);
        if c {
            fd_set_cloexec(obj, false);
        }
        c
    };
    let (ca, cb) = (flag(a), flag(b));
    let lim = limit();
    let err = |e: TabError| match e {
        TabError::Full => EMFILE,
        TabError::NoMem => ENOMEM,
    };
    with(|s| {
        let t = s.tab(proc).ok_or(ENOMEM)?;
        let fa = t.install(0, lim, Desc { obj: a as u32, cloexec: ca }).map_err(err)?;
        let fb = match t.install(0, lim, Desc { obj: b as u32, cloexec: cb }) {
            Ok(fd) => fd,
            Err(e) => {
                t.take(fa);
                return Err(err(e));
            }
        };
        let ok_a = s.refs.inc(key(space, a));
        let ok_b = ok_a && s.refs.inc(key(space, b));
        if !ok_b {
            if ok_a {
                s.refs.dec(key(space, a));
            }
            if let Some(t) = s.tab(proc) {
                t.take(fa);
                t.take(fb);
            }
            return Err(ENOMEM);
        }
        note(proc, b'A', fa as u64, a, 293);
        note(proc, b'A', fb as u64, b, 293);
        Ok((fa as u64, fb as u64))
    })
}

/// The kernel made object `obj` for process `proc` before it runs (the
/// DevTools pipes of the browser): it is descriptor `fd` there.
pub(super) fn preopen(proc: u64, space: usize, fd: usize, obj: u64) {
    with(|s| {
        let Some(t) = s.tab(proc) else { return };
        if let Ok(was) = t.put(fd, MAX_DESCRIPTORS, Desc { obj: obj as u32, cloexec: false }) {
            if obj >= STD {
                s.refs.inc(key(space, obj));
            }
            if let Some(w) = was {
                if w.obj as u64 >= STD {
                    s.refs.dec(key(space, w.obj as u64));
                }
            }
        }
    });
}

// ── SCM_RIGHTS ──────────────────────────────────────────────────────────────

/// `sendmsg` with `SCM_RIGHTS`: the object behind descriptor `fd` of the
/// sender, with one reference taken for the message in flight.
pub(super) fn scm_send(fd: i32) -> Option<u64> {
    if fd < 0 {
        return None;
    }
    let (proc, space) = (cur_proc(), fdspace::current());
    with(|s| {
        let d = s.tab(proc)?.get(fd as usize)?;
        let obj = d.obj as u64;
        if obj >= STD && !s.refs.inc(key(space, obj)) {
            return None;
        }
        Some(obj)
    })
}

/// `recvmsg`: the object that travelled becomes a descriptor of the
/// receiver; the reference of the message is now the descriptor's. None when
/// the receiver's table is full (the reference is dropped).
pub(super) fn scm_receive(obj: u64, cloexec: bool) -> Option<u64> {
    let proc = cur_proc();
    let lim = limit();
    let r = with(|s| s.tab(proc).and_then(|t| t.install(0, lim, Desc { obj: obj as u32, cloexec }).ok()));
    match r {
        Some(fd) => note(proc, b'S', fd as u64, obj, 47),
        None => unref_obj(obj),
    }
    r.map(|fd| fd as u64)
}

// ── fork, exec, exit ────────────────────────────────────────────────────────

/// The child gets a copy of the parent's table; every object in it gains a
/// reference. False when the heap has no room (the fork is refused).
pub(super) fn fork(parent: u64, child: u64, space: usize, close: &mut dyn FnMut(u64)) -> bool {
    // A table left under the child's id belongs to a process that is gone.
    exit(child, space, close);
    with(|s| {
        let Some(copy) = s.tab(parent).and_then(|t| t.try_clone()) else { return false };
        // Count first, so a failure half-way can be undone exactly.
        let mut done = 0usize;
        let mut ok = true;
        for (_, d) in copy.iter() {
            if d.obj as u64 >= STD {
                if !s.refs.inc(key(space, d.obj as u64)) {
                    ok = false;
                    break;
                }
                done += 1;
            }
        }
        if ok {
            let at = match s.procs.binary_search_by_key(&child, |e| e.0) {
                Ok(_) => return false,
                Err(i) => i,
            };
            if s.procs.try_reserve(1).is_ok() {
                let n = copy.open();
                s.procs.insert(at, (child, copy));
                note(child, b'F', n as u64, 0, 57);
                return true;
            }
        }
        for (_, d) in copy.iter().filter(|(_, d)| d.obj as u64 >= STD).take(done) {
            s.refs.dec(key(space, d.obj as u64));
        }
        false
    })
}

/// `execve`: the close-on-exec descriptors of `proc` are closed. Returns how
/// many.
pub(super) fn exec(proc: u64, space: usize, close: &mut dyn FnMut(u64)) -> usize {
    let mut gone: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    let mut n = 0usize;
    with(|s| {
        let Some(t) = s.tab(proc) else { return };
        let want = t.iter().filter(|(_, d)| d.cloexec).count();
        if gone.try_reserve(want).is_err() {
            return;
        }
        t.close_where(|_, d| d.cloexec, |d| gone.push(d.obj));
        n = gone.len();
        note(proc, b'X', n as u64, 0, 59);
        gone.retain(|&o| o as u64 >= STD && s.refs.dec(key(space, o as u64)));
    });
    for o in gone {
        close(o as u64);
    }
    n
}

/// The process is gone: every descriptor it had is closed, and the
/// references its tasks held for calls in progress are dropped. Objects
/// whose last reference that was are closed through `close`.
pub(super) fn exit(proc: u64, space: usize, close: &mut dyn FnMut(u64)) {
    let (slots, mut last) = with(|s| {
        let slots = s.remove(proc).map(|mut t| t.take_all()).unwrap_or_default();
        let mut last: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
        for d in slots.iter().flatten() {
            let obj = d.obj as u64;
            if obj >= STD && s.refs.dec(key(space, obj)) && last.try_reserve(1).is_ok() {
                last.push(obj);
            }
        }
        for t in 0..crate::sched::MAX_TASKS {
            if HOLD_PROC[t].load(Ordering::Relaxed) == proc + 1 {
                HOLD_PROC[t].store(0, Ordering::Relaxed);
                for h in HOLDS[t].iter() {
                    let k = h.swap(0, Ordering::Relaxed);
                    if k != 0 && s.refs.dec(k) && last.try_reserve(1).is_ok() {
                        last.push(key_obj(k));
                    }
                }
            }
        }
        (slots, last)
    });
    drop(slots);
    for o in last.drain(..) {
        close(o);
    }
}

/// The launcher's space is wiped for a new program (`reset_fd_table`): the
/// tables of its processes go, and with them every reference to an object of
/// that space. Sockets, eventfds and epoll instances a process of the old run
/// left open are returned: the caller closes them.
pub(super) fn reset_root() -> alloc::vec::Vec<u64> {
    with(|s| {
        let mut left: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
        let mut i = 0;
        while i < s.procs.len() {
            if s.procs[i].0 < BG_PROC {
                let (_, mut t) = s.procs.remove(i);
                for d in t.take_all().into_iter().flatten() {
                    let obj = d.obj as u64;
                    if obj >= GLOBAL_OBJ_BASE && s.refs.dec(key(fdspace::ROOT, obj)) && left.try_reserve(1).is_ok() {
                        left.push(obj);
                    }
                }
            } else {
                i += 1;
            }
        }
        s.refs.forget(|k| k >> 32 == fdspace::ROOT as u64 + 1);
        left
    })
}

/// A background fd space was released: the references to its objects go
/// with its tables.
pub(super) fn space_released(space: usize) {
    with(|s| {
        s.refs.forget(|k| k >> 32 == space as u64 + 1);
    });
}

// ── diagnostics ─────────────────────────────────────────────────────────────

/// The descriptor numbers of the running process, lowest first
/// (`/proc/self/fd`).
pub(super) fn list() -> alloc::vec::Vec<(usize, u64)> {
    let proc = cur_proc();
    with(|s| {
        let mut v = alloc::vec::Vec::new();
        if let Some(t) = s.tab(proc) {
            if v.try_reserve(t.open()).is_ok() {
                v.extend(t.iter().map(|(fd, d)| (fd, d.obj as u64)));
            }
        }
        v
    })
}

/// (processes with a table, descriptors in all of them, objects with a
/// reference, objects of the launcher's space below the global classes).
pub fn census() -> (usize, usize, usize, usize) {
    with(|s| {
        let d = s.procs.iter().map(|(_, t)| t.open()).sum();
        let root = s.refs.count(|k| k >> 32 == fdspace::ROOT as u64 + 1);
        (s.procs.len(), d, s.refs.len(), root)
    })
}

/// One line per process: id, open descriptors, highest number.
pub fn census_lines() -> alloc::vec::Vec<String> {
    let rows: alloc::vec::Vec<(u64, usize, usize)> =
        with(|s| s.procs.iter().map(|(p, t)| (*p, t.open(), t.top())).collect());
    rows.into_iter()
        .map(|(p, open, top)| {
            if p >= BG_PROC {
                alloc::format!("background pid {}: {open} descriptors, highest {}", p & (BG_PROC - 1), top as i64 - 1)
            } else if p == 0 {
                alloc::format!("main process: {open} descriptors, highest {}", top as i64 - 1)
            } else {
                alloc::format!("fork child (task {p}): {open} descriptors, highest {}", top as i64 - 1)
            }
        })
        .collect()
}

// ── the gate ────────────────────────────────────────────────────────────────

/// Which arguments of system call `num` are descriptors: (plain, directory).
/// Bit i = argument i+1. A plain descriptor must be open (`EBADF`); a
/// directory descriptor may be `AT_FDCWD` or unused (absolute path).
fn fd_args(num: u64) -> (u8, u8) {
    match num {
        0 | 1 | 5 | 8 | 16 | 17 | 18 | 19 | 20 | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 49 | 50 | 51 | 52 | 54 | 55 | 72
        | 73 | 74 | 75 | 77 | 78 | 81 | 91 | 93 | 138 | 187 | 190 | 193 | 196 | 199 | 217 | 221 | 232 | 254 | 255 | 277
        | 278 | 281 | 285 | 286 | 287 | 288 | 295 | 296 | 299 | 306 | 307 | 327 | 328 | 441 => (0b00001, 0),
        40 | 276 => (0b00011, 0),          // sendfile(out, in), tee(in, out)
        233 | 275 | 326 => (0b00101, 0),   // epoll_ctl(ep, op, fd), splice / copy_file_range(in, off, out)
        257 | 258 | 259 | 260 | 261 | 262 | 263 | 267 | 268 | 269 | 280 | 332 | 437 | 439 => (0, 0b00001),
        264 | 265 | 316 => (0, 0b00101),   // renameat, linkat, renameat2
        266 => (0, 0b00010),               // symlinkat(target, dirfd, path)
        _ => (0, 0),
    }
}

/// Does system call `num` return a new object number on success?
fn returns_obj(num: u64) -> bool {
    matches!(num, 2 | 85 | 257 | 437 | 41 | 43 | 288 | 213 | 291 | 284 | 290 | 253 | 294 | 282 | 289 | 283 | 319)
}

/// A system call of the running task. `inner` is the dispatcher behind the
/// gate: it sees object numbers where the program wrote descriptors.
pub(super) fn syscall(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, inner: Inner) -> u64 {
    let proc = cur_proc();
    let space = fdspace::current();
    match num {
        3 => return sys_close(proc, space, a1, inner),
        32 => return sys_dup(proc, space, a1, 0, false),
        33 => return sys_dup2(proc, space, a1, a2, false, inner),
        292 => {
            // dup3(old, new, flags): only O_CLOEXEC is a flag, and old == new is an error.
            if a3 & !0x8_0000 != 0 || (a1 as u32) == (a2 as u32) {
                return EINVAL;
            }
            return sys_dup2(proc, space, a1, a2, a3 & 0x8_0000 != 0, inner);
        }
        72 => match a2 {
            0 => return sys_dup(proc, space, a1, a3, false),    // F_DUPFD
            1030 => return sys_dup(proc, space, a1, a3, true),  // F_DUPFD_CLOEXEC
            1 => {
                // F_GETFD
                let Some(fd) = as_fd(a1) else { return EBADF };
                return with(|s| s.tab(proc).and_then(|t| t.get(fd)).map_or(EBADF, |d| u64::from(d.cloexec)));
            }
            2 => {
                // F_SETFD
                let Some(fd) = as_fd(a1) else { return EBADF };
                return with(|s| s.tab(proc).map_or(EBADF, |t| if t.set_cloexec(fd, a3 & 1 != 0) { 0 } else { EBADF }));
            }
            _ => {}
        },
        436 => return sys_close_range(proc, space, a1, a2, a3, inner),
        _ => {}
    }
    let (mut plain, dirs) = fd_args(num);
    // mmap(addr, len, prot, flags, fd, off): the fd counts unless MAP_ANONYMOUS.
    if num == 9 && a4 & 0x20 == 0 {
        plain = 0b10000;
    }
    let mut args = [a1, a2, a3, a4, a5];
    let mut held = [0u64; 2];
    if plain | dirs != 0 {
        let bad = with(|s| {
            let mut n = 0;
            for i in 0..5 {
                let bit = 1u8 << i;
                if (plain | dirs) & bit == 0 {
                    continue;
                }
                let obj = as_fd(args[i]).and_then(|fd| s.tab(proc).and_then(|t| t.get(fd))).map(|d| d.obj as u64);
                match obj {
                    Some(obj) => {
                        if obj >= STD && n < held.len() && s.refs.inc(key(space, obj)) {
                            held[n] = key(space, obj);
                            n += 1;
                        }
                        args[i] = obj;
                    }
                    None if plain & bit != 0 => {
                        for h in held.iter_mut().take(n) {
                            s.refs.dec(*h); // never the last: the table still has it
                            *h = 0;
                        }
                        return true;
                    }
                    // Not a descriptor of this process: AT_FDCWD and friends stay
                    // as they are, a stale number becomes "no directory".
                    None => {
                        if as_fd(args[i]).is_some() {
                            args[i] = u32::MAX as u64;
                        }
                    }
                }
            }
            false
        });
        if bad {
            return EBADF;
        }
    }
    let cur = crate::sched::current();
    let track = held[0] != 0 && cur < crate::sched::MAX_TASKS;
    if track {
        HOLDS[cur][0].store(held[0], Ordering::Relaxed);
        HOLDS[cur][1].store(held[1], Ordering::Relaxed);
        HOLD_PROC[cur].store(proc + 1, Ordering::Relaxed);
    }
    let r = inner(num, args[0], args[1], args[2], args[3], args[4]);
    if held[0] != 0 {
        // Give the references back, unless the process was ended meanwhile and
        // `exit` already did (this task then does not get here, but be exact).
        let mine = !track || HOLD_PROC[cur].swap(0, Ordering::Relaxed) == proc + 1;
        if mine {
            for (i, h) in held.iter().enumerate() {
                if *h == 0 {
                    continue;
                }
                if track {
                    HOLDS[cur][i].store(0, Ordering::Relaxed);
                }
                if with(|s| s.refs.dec(*h)) {
                    let _ = inner(3, key_obj(*h), 0, 0, 0, 0);
                }
            }
        }
    }
    if (r as i64) < 0 {
        return r;
    }
    if returns_obj(num) {
        adopt(proc, space, r, num, inner)
    } else {
        r
    }
}
