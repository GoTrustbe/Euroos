//! Preemptive round-robin scheduler with kernel tasks (Track 3.3).
//!
//! The timer interrupt (IRQ0) jumps to `timer_switch` (assembly): it saves
//! ALL registers of the current task on its stack, calls `schedule_tick`
//! (picks the next task), sets the stack pointer to that task and restores its
//! registers + `iretq`. This is how arbitrary code is preemptively interrupted.
//!
//! Task 0 = the main thread (shell). Tasks 1..N are background counters; their
//! increasing counters prove they really run in parallel (interleaved).

use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use spin::Mutex;
use x86_64::instructions::segmentation::{Segment, CS, SS};
use x86_64::registers::model_specific::Msr;

const IA32_FS_BASE: u32 = 0xC000_0100;

// Chrome (even --single-process headless) spawns dozens of threads (thread pool,
// compositor, IO, message pumps). 48 was fine for the shell + a few glibc apps; a
// browser needs far more scheduler slots. Each slot costs one 16 KiB kernel stack.
// 512 since BUG-147 (w23) and BUG-151 (mem), the same finding twice: once the
// slots of ended fork children and their threads come back (retire_task in
// ring3.rs), a browser session still keeps up to 16 processes and their threads
// live (214 of 256 in one minimal session, 224 of 256 with 14 processes). A slot
// costs its 16 KiB kernel-task stack (STACKS) and a few words; thread kernel
// stacks (ring3.rs, MAX_THREADS = 480) past the static 224 come from the heap.
pub const MAX_TASKS: usize = 512;
const STACK_SIZE: usize = 16 * 1024;
const CONTEXT_WORDS: usize = 20; // 15 GP registers + 5 (rip,cs,rflags,rsp,ss)

/// Per-task counter (index 1..3 for the kernel background tasks).
pub static TASK_COUNTERS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// A Dead task whose slot may be reused by the NEXT spawn, set by the owner
/// after it released the task's resources and dropped it from its registries
/// (a glibc worker thread at exit). `alloc_slot` sweeps these lazily before
/// growing the table, so a process that creates and joins threads in a loop
/// no longer exhausts the 256 slots ([churn-thread], 2026-10-07).
pub static RECLAIMABLE: [core::sync::atomic::AtomicBool; MAX_TASKS] = [const { core::sync::atomic::AtomicBool::new(false) }; MAX_TASKS];

/// Task state (S2 scheduler maturity). Replaces the separate `dead`/`blocked`
/// flags with a full state machine — the basis for blocking I/O,
/// nanosleep and (S3) fork/wait/zombie reaping.
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
pub enum State {
    /// Runnable (ready to get the CPU).
    Ready,
    /// Sleeps until `ticks() >= wake`. Set back to Ready automatically by the scheduler.
    Sleeping(u64),
    /// Blocked on a wait channel (a token/address). Woken by [`wake`].
    Blocked(u64),
    /// Cleanly ended with an exit code; waits for reaping by the parent (waitpid).
    Zombie(i64),
    /// Definitively gone (reaped, or hard-terminated by a fault). Skipped
    /// forever.
    Dead,
}

struct Task {
    rsp: u64,
    /// Kernel stack for ring3->ring0 interrupts (0 = kernel task, no rsp0).
    kstack: u64,
    /// FS_BASE (IA32_FS_BASE MSR) of this task — the musl TLS pointer. Saved/
    /// restored per task on each context switch, so that preemptive musl
    /// processes each keep their own thread-local storage.
    fs_base: u64,
    /// Physical PML4 (CR3) of this task. 0 = the shared boot PML4 (kernel +
    /// kernel tasks). A separate value gives an ISOLATED address space.
    cr3: u64,
    /// Full state (see [`State`]).
    state: State,
    /// Nice value (-20..19): lower = higher priority = scheduled more often.
    nice: i8,
    /// Virtual runtime (mini-CFS): the scheduler always picks the runnable task with
    /// the SMALLEST vruntime. A higher nice makes it climb faster → fewer turns.
    vruntime: u64,
    /// Process identity (S3: waitpid/SIGCHLD). 0 = not registered as a process.
    pid: u32,
    /// Parent pid (S3: reaping + SIGCHLD routing).
    ppid: u32,
    /// Bottom (lowest address) of this task's kernel stack (0 = no guard).
    /// A CANARY lives there; on each context switch the scheduler checks it — a
    /// stack overflow (the stack grows down past this point) overwrites the
    /// canary and is thus detected instead of silently corrupting neighboring memory (S6).
    stack_bottom: u64,
    /// Per-task SYSCALL state. Because a thread can now be descheduled MID-SYSCALL
    /// (futex/epoll yield), the syscall globals must travel with the task: saved on
    /// switch-out, restored on switch-in, so a concurrent thread's syscall cannot
    /// clobber this one's user-return state. (0 when the task is not in a syscall.)
    sc_user_rsp: u64,
    sc_user_rip: u64,
    sc_saved_regs: u64,
}

/// Stack-guard canary (S6 memory hardening). Unlikely value at the bottom of each
/// kernel stack; if it changes, the stack has overflowed.
const STACK_CANARY: u64 = 0x5350_524F_5547_4421; // "SPROUGD!" — recognizable in dumps

/// Load weight per nice level (-20..19), the table of Linux: 1024 at nice 0,
/// about x1.25 per step. A task's vruntime grows by its run time x 1024 / weight
/// (S1 of docs/design/SMP-SCHEDULER.md: time, not turns).
const WEIGHTS: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916,
    9548, 7620, 6100, 4904, 3906, 3121, 2501, 1991, 1586, 1277,
    1024, 820, 655, 526, 423, 335, 272, 215, 172, 137,
    110, 87, 70, 56, 45, 36, 29, 23, 18, 15,
];

fn weight(nice: i8) -> u64 {
    WEIGHTS[(nice.clamp(-20, 19) + 20) as usize]
}

/// The desktop loop (task 0) runs at the weight of nice -10: against five busy
/// nice-0 tasks it is entitled to 65 % of the CPU (BUG-192: it got 2.5 %).
pub const DESKTOP_NICE: i8 = -10;

/// The userspace drivers (e1000d, nvmed, ...) run at the weight of nice -15
/// (BUG-231). A driver is the device's interrupt handler in ring 3: its
/// interrupt wakes it, it does a short pass and sleeps again. Under turn-based
/// vruntime it was never charged (it always slept when it left the CPU) and so
/// always ran first; charged by time at nice 0 it queued behind the desktop
/// loop (nice -10) and every CPU-bound task until the next tick, and every frame
/// of an inbound connection paid that (soak hammer p50 5.4 -> 60.9 ms). At -15
/// a woken driver is ahead of the pack again; what it takes under a flood is
/// still bounded by its weight.
pub const DRIVER_NICE: i8 = -15;

/// The driver budget (BUG-252). Nice -15 is what a driver needs to run when its
/// interrupt wakes it; it is not meant as the weight of a driver that never
/// sleeps. Under an inbound flood e1000d took 45 % of the CPU at that weight
/// (each frame it hands on is a device exit, and under QEMU's user network that
/// exit carries the host side's TCP work too) and the desktop loop's console
/// commands ran at half their rate. A driver keeps nice -15 for the first
/// `DRV_BUDGET_PCT` % of every window of `DRV_WINDOW_TICKS` ticks; past that it
/// is charged at `DRIVER_OVER_NICE` (it runs when nobody else wants the CPU)
/// for the rest of the window, and the next window starts it at the pack again
/// (the debt of the throttled stretch is not carried over: the budget is the
/// limit, not its vruntime). A driver that does a short pass per interrupt never
/// reaches the budget, and under a flood most frames still go through at once:
/// only those that come after the budget is spent wait for the next window.
/// Default 10 % of 500 ms: in 25-minute soaks with the inbound hammer, 10 % of
/// 100 ms left the hammer p50 at 8.9 ms (the throttled stretches were many and
/// short), 30 % of 100 ms the console at 4 006 commands an hour; 10 % of 500 ms
/// gave p50 5.7 ms and 5 232 commands an hour (fixed -15: 5.0 ms, 2 812).
/// `sched.drvbudget=PCT` and `sched.drvwindow=TICKS` (boot options) and the
/// shell command `drvbudget [PCT]` change it; 100 % is the old fixed weight.
pub const DRIVER_OVER_NICE: i8 = 19;
static DRV_WINDOW_TICKS: AtomicU64 = AtomicU64::new(DRV_WINDOW_DEFAULT);
const DRV_WINDOW_DEFAULT: u64 = 50;
static DRV_BUDGET_PCT: AtomicU64 = AtomicU64::new(DRV_BUDGET_DEFAULT);
const DRV_BUDGET_DEFAULT: u64 = 10;
/// The driver tasks (index + 1, 0 = free), their run time in the current window
/// (TSC), and how often a driver went over its budget.
const DRV_SLOTS: usize = 8;
static DRV_TASK: [AtomicUsize; DRV_SLOTS] = [const { AtomicUsize::new(0) }; DRV_SLOTS];
static DRV_RUN: [AtomicU64; DRV_SLOTS] = [const { AtomicU64::new(0) }; DRV_SLOTS];
static DRV_OVER: AtomicU64 = AtomicU64::new(0);
static DRV_WINDOW_START: AtomicU64 = AtomicU64::new(0);

fn drv_slot(idx: usize) -> Option<usize> {
    (0..DRV_SLOTS).find(|&k| DRV_TASK[k].load(Ordering::Relaxed) == idx + 1)
}

/// Task `idx` is a userspace driver: nice -15 within its budget (see above).
pub fn set_driver(idx: usize) {
    set_nice(idx, DRIVER_NICE);
    if drv_slot(idx).is_some() {
        return;
    }
    for k in 0..DRV_SLOTS {
        if DRV_TASK[k].compare_exchange(0, idx + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            DRV_RUN[k].store(0, Ordering::Relaxed);
            return;
        }
    }
}

/// The driver budget in percent of a window (100: none).
pub fn driver_budget() -> u64 {
    DRV_BUDGET_PCT.load(Ordering::Relaxed)
}

pub fn set_driver_budget(pct: u64) {
    DRV_BUDGET_PCT.store(pct.clamp(1, 100), Ordering::Relaxed);
}

/// The budget window in ticks.
pub fn driver_window() -> u64 {
    DRV_WINDOW_TICKS.load(Ordering::Relaxed)
}

pub fn set_driver_window(ticks: u64) {
    DRV_WINDOW_TICKS.store(ticks.clamp(5, 1000), Ordering::Relaxed);
}

/// (driver tasks, times one went over its budget).
pub fn driver_stats() -> (alloc::vec::Vec<usize>, u64) {
    let v = (0..DRV_SLOTS).filter_map(|k| DRV_TASK[k].load(Ordering::Relaxed).checked_sub(1)).collect();
    (v, DRV_OVER.load(Ordering::Relaxed))
}

/// The nice value a turn of task `idx` is charged at: its own, except for a
/// driver past its budget in this window.
/// BUG-274: the desktop loop hosts an X client's window (chrome) and spends its
/// passes serving it (events, the hosted window, compositing its frames).
static DESK_HOSTS_CLIENT: AtomicBool = AtomicBool::new(false);

/// Set by the desktop loop once a pass: does a hosted X client window exist?
pub fn desktop_hosts_client(on: bool) {
    DESK_HOSTS_CLIENT.store(on, Ordering::Relaxed);
}

fn charge_nice(s: &Scheduler, idx: usize) -> i8 {
    // BUG-274: S1 gave the desktop loop the weight of nice -10 so that its own
    // work (the shell, menus, a foreground command: BUG-192) keeps 65 % against
    // busy tasks. While it hosts an X client the busy tasks are mostly that
    // client, and the loop's passes are work for it: at 9x the client's weight
    // the loop took 47 % of the CPU while chrome waited for its turn, and key to
    // screen went from 320 to 400 ms. Then it is charged like its client.
    if idx == 0 && DESK_HOSTS_CLIENT.load(Ordering::Relaxed) {
        return 0;
    }
    let n = s.tasks[idx].nice;
    if n >= DRIVER_OVER_NICE {
        return n;
    }
    let Some(k) = drv_slot(idx) else { return n };
    let pct = DRV_BUDGET_PCT.load(Ordering::Relaxed);
    if pct >= 100 {
        return n;
    }
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    if DRV_RUN[k].load(Ordering::Relaxed) > per * DRV_WINDOW_TICKS.load(Ordering::Relaxed) * pct / 100 {
        DRIVER_OVER_NICE
    } else {
        n
    }
}

/// A turn of `idx` of `cycles` ends: count it against a driver's budget, and
/// start a new window when this one is over (a driver that ran past its budget
/// starts it at the pack, not behind it).
fn drv_account(s: &mut Scheduler, idx: usize, cycles: u64) {
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let start = DRV_WINDOW_START.load(Ordering::Relaxed);
    let win = DRV_WINDOW_TICKS.load(Ordering::Relaxed);
    if now.wrapping_sub(start) >= win {
        DRV_WINDOW_START.store(now, Ordering::Relaxed);
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        let cap = per * win * DRV_BUDGET_PCT.load(Ordering::Relaxed) / 100;
        for k in 0..DRV_SLOTS {
            let was = DRV_RUN[k].swap(0, Ordering::Relaxed);
            if was > cap {
                if let Some(t) = DRV_TASK[k].load(Ordering::Relaxed).checked_sub(1) {
                    if t < s.count && s.tasks[t].vruntime > s.min_vr {
                        s.tasks[t].vruntime = s.min_vr;
                    }
                }
            }
        }
    }
    if let Some(k) = drv_slot(idx) {
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        let cap = per * win * DRV_BUDGET_PCT.load(Ordering::Relaxed) / 100;
        let before = DRV_RUN[k].fetch_add(cycles, Ordering::Relaxed);
        if before <= cap && before + cycles > cap {
            DRV_OVER.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// vruntime is in weighted nanoseconds. A task that waited gets ahead of the
/// pack by half a slice at most: `max(vruntime, min_vr - WAKE_FLOOR)`.
const WAKE_FLOOR: u64 = 5_000_000;

/// TSC cycles of run time at `nice` as weighted nanoseconds of vruntime.
fn vr_charge(cycles: u64, nice: i8) -> u64 {
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1) as u128;
    // 10 ms per tick: ns = cycles * 10^7 / per.
    let ns = cycles as u128 * 10_000_000 / per;
    (ns * 1024 / weight(nice) as u128).min(u64::MAX as u128 / 4) as u64
}

/// Place a task that waited: never more than WAKE_FLOOR ahead of the pack.
fn place_woken(s: &mut Scheduler, idx: usize) {
    let floor = s.min_vr.saturating_sub(WAKE_FLOOR);
    if s.tasks[idx].vruntime < floor {
        s.tasks[idx].vruntime = floor;
    }
}

/// Block the CURRENT task on the generic futex wait channel (FUTEX_WAIT).
pub fn block_current() {
    block_on(0);
}

/// Set to ask the timer tick for a full task census (see `census_trylock`).
pub static CENSUS_REQUEST: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// A census of EVERY task, taken from INTERRUPT context: index, state and cr3.
/// cr3 is what makes it useful - it names the address space, so the threads of
/// one chrome process group together and a renderer whose raster workers never
/// run is visible as such.
///
/// Every lock here is a TRY-lock and nothing allocates. A diagnostic must never
/// be able to wedge the machine it is diagnosing: the futex dump that once ran
/// from the CDP pump took ring3 spinlocks with interrupts enabled and froze the
/// guest it was meant to explain.
fn sysn(t: usize) -> u64 {
    if t < MAX_TASKS { crate::ring3::SYSCALLS_PER_TASK[t].load(Ordering::Relaxed) } else { 0 }
}

fn cpun(t: usize) -> u64 {
    crate::ring3::task_ticks(t)
}

pub fn census_trylock() {
    let Some(s) = SCHED.try_lock() else {
        crate::serial_println!("[census] scheduler lock busy, skipped this tick");
        return;
    };
    crate::serial_println!("[census] {} tasks, current={}", s.count, s.current);
    // Where the desktop loop (task 0) was when it last lost the CPU: its saved
    // interrupt frame has the RIP at +120 (see schedule_tick). Not when it is
    // the task that is running now.
    if s.current != 0 && s.count > 0 && s.tasks[0].rsp > 0x1000 {
        let (rip, cs) = unsafe { (*((s.tasks[0].rsp + 120) as *const u64), *((s.tasks[0].rsp + 128) as *const u64)) };
        crate::serial_println!("[census] t0 (desktop loop) was preempted at rip={rip:#018x} cs={cs:#x} vruntime={} (current t{}: {}, min {})",
            s.tasks[0].vruntime, s.current, s.tasks[s.current].vruntime, s.min_vr);
    }
    for i in 0..s.count {
        let t = &s.tasks[i];
        match t.state {
            State::Ready => crate::serial_println!("[census] t{i} cr3={:#x} sys={} cpu={} Ready", t.cr3, sysn(i), cpun(i)),
            State::Sleeping(w) => {
                crate::serial_println!("[census] t{i} cr3={:#x} sys={} cpu={} Sleeping(until {w})", t.cr3, sysn(i), cpun(i))
            }
            State::Blocked(c) => {
                crate::serial_println!("[census] t{i} cr3={:#x} sys={} cpu={} Blocked(chan={c:#x})", t.cr3, sysn(i), cpun(i))
            }
            ref other => crate::serial_println!("[census] t{i} cr3={:#x} sys={} cpu={} {other:?}", t.cr3, sysn(i), cpun(i)),
        }
    }
}

/// BUG-157: the census of the NMI probe, WITHOUT the scheduler lock. The probe
/// is sent to a machine that stands still, and the place it stood in s4 was
/// schedule_core itself, which holds SCHED: `census_trylock` printed nothing
/// ("the ready list was empty" was the absence of a list). This reads the
/// table through the lock (a racy read of a table that no longer changes),
/// allocates nothing and takes no lock. Every task that is not Dead: state,
/// address space, syscalls and ticks so far, its last syscall, and for a task
/// that is not running the rip, cs and rflags it will resume with (an IF=0
/// there, on a Ready task, is a task that yields with interrupts off).
static SCHED_PEEK: AtomicU64 = AtomicU64::new(0);

pub fn census_nolock() {
    let peek = SCHED_PEEK.load(Ordering::Relaxed);
    if peek == 0 {
        return;
    }
    // SAFETY: see above; plain integers, bounds checked below.
    let s: &Scheduler = unsafe { &*(peek as *const Scheduler) };
    let count = s.count.min(MAX_TASKS);
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    crate::serial_println!(
        "[nmi-tasks] {} slots, current={} (mirror {}), SCHED {}, ticks={now}, min_vr={}, t0 wants={} idle={} last-turn-tick={}, sched-skips={}",
        s.count, s.current, CURRENT_MIRROR.load(Ordering::Relaxed),
        if SCHED.is_locked() { "HELD" } else { "free" }, s.min_vr,
        T0_WANTS.load(Ordering::Relaxed), T0_IDLE.load(Ordering::Relaxed),
        T0_TURN_TICK.load(Ordering::Relaxed), SCHED_SKIPS.load(Ordering::Relaxed));
    // A line each for the tasks that could run: the current one, the Ready
    // ones, the sleepers whose time has come (24 lines at most: the probe is
    // also sent to a machine that runs, and a second of printing in an NMI
    // was a second without the timer).
    let (mut ready, mut sleeping, mut blocked, mut zombie, mut dead, mut lines) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for i in 0..count {
        let t = &s.tasks[i];
        let st = t.state;
        let runnable = match st {
            State::Ready => { ready += 1; true }
            State::Sleeping(w) => { sleeping += 1; w <= now }
            State::Blocked(_) => { blocked += 1; false }
            State::Zombie(_) => { zombie += 1; false }
            State::Dead => { dead += 1; false }
        };
        if !(runnable || (i == s.current && st != State::Dead)) || lines >= 24 {
            continue;
        }
        lines += 1;
        let (sn, sa, sr) = crate::ring3::last_syscall(i);
        // The frame a task that is not running resumes from: 15 registers,
        // then rip, cs, rflags (schedule_tick). Only for a pointer into the
        // kernel stacks (static, or kernel heap: both below the user regions).
        let rsp = t.rsp;
        let frame = if i != s.current && rsp > 0x10_0000 && rsp < (1u64 << 39) && rsp & 7 == 0 {
            unsafe { Some((*((rsp + 120) as *const u64), *((rsp + 128) as *const u64), *((rsp + 136) as *const u64))) }
        } else {
            None
        };
        crate::serial_print!("[nmi-tasks] t{i} ");
        match st {
            State::Ready => crate::serial_print!("Ready"),
            State::Sleeping(w) => crate::serial_print!("Sleeping(due since {w})"),
            State::Blocked(c) => crate::serial_print!("Blocked({c:#x})"),
            State::Zombie(c) => crate::serial_print!("Zombie({c})"),
            State::Dead => {}
        }
        crate::serial_print!(" pid={} cr3={:#x} vr={} sys={} cpu={} last={sn}(a1={sa:#x})->{sr:#x}",
            t.pid, t.cr3, t.vruntime, sysn(i), cpun(i));
        match frame {
            Some((rip, cs, fl)) => crate::serial_print!(" resumes rip={rip:#x} cs={cs:#x} IF={}", (fl >> 9) & 1),
            None => crate::serial_print!(" {}", if i == s.current { "RUNNING" } else { "(no frame)" }),
        }
        crate::ring3::thread_name_print(i);
        crate::serial_println!();
    }
    // The others in short: tN:B (blocked), tN:S (sleeping), tN:Z (zombie).
    let mut col = 0;
    for i in 0..count {
        let c = match s.tasks[i].state {
            State::Blocked(_) => "B",
            State::Sleeping(w) if w > now => "S",
            State::Zombie(_) => "Z",
            _ => continue,
        };
        if col == 0 {
            crate::serial_print!("[nmi-tasks]");
        }
        crate::serial_print!(" t{i}:{c}");
        col += 1;
        if col == 20 {
            crate::serial_println!();
            col = 0;
        }
    }
    if col != 0 {
        crate::serial_println!();
    }
    crate::serial_println!("[nmi-tasks] {ready} Ready, {sleeping} Sleeping, {blocked} Blocked, {zombie} Zombie, {dead} Dead");
}

/// Every task as (index, cr3, state), copied out under the lock so a census can
/// print it without holding the scheduler.
pub fn snapshot_tasks() -> alloc::vec::Vec<(usize, u64, State)> {
    let _g = IfOffGuard::new();
    let s = SCHED.lock();
    (0..s.count).map(|i| (i, s.tasks[i].cr3, s.tasks[i].state)).collect()
}

/// Diagnostic: summarise every live task's state (Ready/Sleeping/Blocked/Zombie).
/// Used by the glibc launcher's stall detector to see a many-thread deadlock.
pub fn dump_states() {
    let s = SCHED.lock();
    let mut ready = 0usize;
    let mut blocked = 0usize;
    let mut sleeping = 0usize;
    for i in 0..s.count {
        match s.tasks[i].state {
            State::Ready => {
                ready += 1;
                crate::serial_println!("[stall]   task {i}: Ready (cr3={:#x})", s.tasks[i].cr3);
            }
            State::Sleeping(w) => {
                sleeping += 1;
                crate::serial_println!("[stall]   task {i}: Sleeping(until {w})");
            }
            State::Blocked(c) => {
                blocked += 1;
                crate::serial_println!("[stall]   task {i}: Blocked(chan={c:#x})");
            }
            ref other => {
                // Name the states the arms above skip (Running, Dead, ...): three
                // tasks fell outside every counted state during the fp1 stall and
                // the dump could not say WHAT they were — that unnamed state was
                // the whole diagnosis.
                crate::serial_println!("[stall]   task {i}: {other:?}");
            }
        }
    }
    crate::serial_println!("[stall] summary: {ready} Ready, {blocked} Blocked, {sleeping} Sleeping (of {} tasks); current={}",
        s.count, s.current);
}

/// Block the CURRENT task on wait channel `chan` (an address/token). The scheduler
/// skips it until [`wake`]`(chan, ..)`. Basis for futex, pipes, waitpid.
pub fn block_on(chan: u64) {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Blocked(chan);
}

// -- Wake preemption (S2 of docs/design/SMP-SCHEDULER.md) --
//
// A wake used to wait for the end of the waker's tick (up to 10 ms), or, with
// the W23 hand-off `wakeyield`, the waker yielded inside its system call and
// waited a round of thirty ready threads for its next turn (harness median
// 483 -> 1102 ms; removed). Now a wake whose task is at least 1 ms of vruntime
// behind the running one sends IPI_RESCHED to the BSP: from a system call the
// IPI fires at `sysret`, the waker keeps its place in vruntime order and is
// only preempted, and the woken task runs at once. DESK_KICK stays: after the
// hosted window drew, the next switch goes to the desktop loop.
static RESCHED: AtomicBool = AtomicBool::new(false);
/// The desktop loop has something to put on the screen (the hosted window
/// drew): the next switch goes to it even if it had its turn this tick.
static DESK_KICK: AtomicBool = AtomicBool::new(false);
/// When each task was last woken (TSC; 0 = not waiting for the CPU).
static WOKEN_AT: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// Wake-to-run delays: under 0.1, 1, 5, 10, 20, 50, 100 ms, and longer.
pub static WAKE_HIST: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub const WAKE_HIST_MS: [&str; 8] = ["<0.1", "<1", "<5", "<10", "<20", "<50", "<100", ">=100"];

/// A woken task preempts the running one when its vruntime is at least this far
/// below the running task's (weighted ns): 1 ms.
const WAKE_PREEMPT: u64 = 1_000_000;

/// Nice value of task `idx` (None: no such task).
pub fn nice_of(idx: usize) -> Option<i8> {
    let s = SCHED.lock();
    (idx < s.count).then(|| s.tasks[idx].nice)
}

fn woken(s: &Scheduler, idx: usize) {
    let now = rdtsc();
    WOKEN_AT[idx].store(now, Ordering::Relaxed);
    let cur = s.current;
    if idx == cur || cur >= s.count {
        return;
    }
    // The running task's vruntime including its turn so far. The desktop loop in
    // its idle halt always gives way.
    let want = if cur == 0 && T0_IDLE.load(Ordering::Relaxed) {
        true
    } else {
        let live = s.tasks[cur].vruntime
            .wrapping_add(vr_charge(now.saturating_sub(EXEC_START.load(Ordering::Relaxed)), charge_nice(s, cur)));
        s.tasks[idx].vruntime.saturating_add(WAKE_PREEMPT) < live
    };
    if want {
        RESCHED.store(true, Ordering::Relaxed);
    }
}

/// After a wake (the scheduler lock dropped): send the reschedule IPI if the
/// wake asked for preemption. From a system call it fires at `sysret`.
fn kick_after_wake() {
    if RESCHED.swap(false, Ordering::Relaxed) {
        resched_ipi();
    }
}

fn ran(idx: usize) {
    let w = WOKEN_AT[idx].swap(0, Ordering::Relaxed);
    if w == 0 {
        return;
    }
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    let us = unsafe { core::arch::x86_64::_rdtsc() }.saturating_sub(w) * 10_000 / per;
    let b = match us {
        0..=99 => 0,
        100..=999 => 1,
        1000..=4999 => 2,
        5000..=9999 => 3,
        10000..=19999 => 4,
        20000..=49999 => 5,
        50000..=99999 => 6,
        _ => 7,
    };
    WAKE_HIST[b].fetch_add(1, Ordering::Relaxed);
}

/// Did the running system call wake someone it should hand the CPU to? The
/// dispatch epilogue then sends the reschedule IPI (the wake functions already
/// did when they could; this is the backstop).
pub fn take_resched() -> bool {
    RESCHED.swap(false, Ordering::Relaxed)
}

/// The hosted window drew: the desktop loop is next (see DESK_KICK).
pub fn want_desktop() {
    DESK_KICK.store(true, Ordering::Relaxed);
}

/// S3: the entry stack of AP `cpu` (TSS.rsp0 there): slot 0 of its scheduler
/// stacks, which the idle context never used (it runs on the AP boot stack) and
/// which `ap_setup` registered with `percpu`.
pub fn ap_entry_stack_top(cpu: usize) -> u64 {
    let base = unsafe { core::ptr::addr_of!(AP_STACKS2[cpu % MAX_CPU][0]) as u64 };
    (base + STACK_SIZE as u64) & !0xF
}

/// S3: save the FPU state of task `idx` (an AP switching it out).
pub(crate) unsafe fn fpu_save_task(idx: usize) {
    let areas = core::ptr::addr_of_mut!(FPU_AREAS) as *mut FpuArea;
    let p = (*areas.add(idx)).0.as_mut_ptr();
    if AVX_ENABLED.load(Ordering::Relaxed) {
        core::arch::asm!("xsave [{}]", in(reg) p, in("eax") 7u32, in("edx") 0u32, options(nostack, preserves_flags));
    } else {
        core::arch::asm!("fxsave [{}]", in(reg) p, options(nostack, preserves_flags));
    }
    FPU_VALID[idx].store(true, Ordering::Release);
}

/// S3: load the FPU state of task `idx` (an AP switching it in).
pub(crate) unsafe fn fpu_restore_task(idx: usize) {
    let areas = core::ptr::addr_of_mut!(FPU_AREAS) as *mut FpuArea;
    let src = if FPU_VALID[idx].load(Ordering::Acquire) {
        (*areas.add(idx)).0.as_ptr()
    } else {
        core::ptr::addr_of!(FPU_TEMPLATE) as *const u8
    };
    if AVX_ENABLED.load(Ordering::Relaxed) {
        core::arch::asm!("xrstor [{}]", in(reg) src, in("eax") 7u32, in("edx") 0u32, options(nostack, preserves_flags));
    } else {
        core::arch::asm!("fxrstor [{}]", in(reg) src, options(nostack, preserves_flags));
    }
}

/// S3: may task `i` of the table leave for an AP now? A `Ready` ring-3 task
/// whose saved frame is a user-mode frame at the top of its own kernel stack.
fn ap_target(s: &Scheduler, i: usize) -> Option<usize> {
    let t = &s.tasks[i];
    if i == 0 || t.cr3 == 0 || t.kstack == 0 || t.rsp != t.kstack - (CONTEXT_WORDS as u64) * 8 {
        return None;
    }
    let cs = unsafe { ((t.rsp + 16 * 8) as *const u64).read_volatile() };
    crate::apuser::target(i, cs & 3 == 3, t.cr3)
}

/// S3: take back what the APs sent home (scheduler lock held).
fn drain_locked(s: &mut Scheduler) {
    if !crate::apuser::inbox_pending() {
        return;
    }
    crate::apuser::drain_inbox(|i, r, f, tsc| {
        s.tasks[i].rsp = r;
        s.tasks[i].fs_base = f;
        let c = vr_charge(tsc, s.tasks[i].nice);
        s.tasks[i].vruntime = s.tasks[i].vruntime.wrapping_add(c);
        place_woken(s, i);
    });
}

/// S4: task `idx` is on an AP: bring it home before its state changes (kill,
/// exit, a park, a reclaimed slot). Scheduler lock held; an AP never takes it.
fn bring_home(s: &mut Scheduler, idx: usize) {
    if idx < MAX_TASKS && !crate::apuser::on_bsp(idx) {
        crate::apuser::evict_wait(idx);
        drain_locked(s);
    }
}

/// S4: while this lives, no task of address space `cr3` runs on an AP (fork
/// copies it). Every task of it that is on an AP is brought home first.
pub struct HoldMm(());
impl HoldMm {
    pub fn new(cr3: u64) -> Self {
        let mut s = SCHED.lock();
        if crate::apuser::hold_cr3(cr3) > 0 {
            drain_locked(&mut s);
        }
        HoldMm(())
    }
}
impl Drop for HoldMm {
    fn drop(&mut self) {
        crate::apuser::release_hold();
    }
}

/// Unblock task `idx` (FUTEX_WAKE on a specific task).
pub fn unblock(idx: usize) {
    {
    let mut s = SCHED.lock();
    if idx < s.count && matches!(s.tasks[idx].state, State::Blocked(c) if c != PARKED) {
        s.tasks[idx].state = State::Ready;
        place_woken(&mut s, idx);
        woken(&s, idx);
    }
    }
    kick_after_wake();
}

/// Wake task `idx` only if it is Sleeping (a timed sleep), never from Blocked:
/// the wake of an epoll_wait/poll waiter (11f-A). The waiter sleeps with
/// `sleep_ticks`; between registering and sleeping it may block in something
/// else (a demand fault on the user buffer, a disk read), and waking THAT wait
/// would end it before its event (an `unblock_any` there broke chrome's
/// navigation in two of three runs).
/// `still(idx)` is checked under the scheduler lock: the waiter's registration
/// is cleared before any other sleep of that task can begin.
pub fn wake_sleeping(idx: usize, still: impl Fn(usize) -> bool) {
    {
        let mut s = SCHED.lock();
        if idx < s.count && matches!(s.tasks[idx].state, State::Sleeping(_)) && still(idx) {
            s.tasks[idx].state = State::Ready;
            place_woken(&mut s, idx);
            woken(&s, idx);
        }
    }
    kick_after_wake();
}

/// Wake task `idx` from EITHER Blocked or Sleeping -> Ready. A timed futex waiter is
/// parked as Sleeping(deadline) so it auto-wakes on timeout; an explicit FUTEX_WAKE
/// must be able to wake it EARLY too, which plain `unblock` (Blocked-only) cannot.
pub fn unblock_any(idx: usize) {
    {
    let mut s = SCHED.lock();
    if idx < s.count && matches!(s.tasks[idx].state, State::Blocked(c) if c != PARKED) || matches!(s.tasks[idx].state, State::Sleeping(_)) {
        s.tasks[idx].state = State::Ready;
        place_woken(&mut s, idx);
        woken(&s, idx);
    }
    }
    kick_after_wake();
}

/// [`unblock_any`] for interrupt context: never waits for the scheduler lock.
/// False when the lock was busy (the caller keeps its own "work pending" flag;
/// the woken task's next look finds it).
pub fn try_unblock_any(idx: usize) -> bool {
    let Some(mut s) = SCHED.try_lock() else { return false };
    if idx < s.count && matches!(s.tasks[idx].state, State::Blocked(c) if c != PARKED) || matches!(s.tasks[idx].state, State::Sleeping(_)) {
        s.tasks[idx].state = State::Ready;
        place_woken(&mut s, idx);
        woken(&s, idx);
    }
    drop(s);
    kick_after_wake();
    true
}

/// Start a kernel task on the BSP: `entry` runs on its own 16 KiB stack (with
/// the overflow canary) in the boot address space, with interrupts on, under
/// the same mini-CFS as every other task. For kernel services that must keep
/// running while task 0 is busy (the network worker). Never returns: the task
/// lives as long as the kernel. None when the task table is full.
pub fn spawn_kernel(entry: extern "C" fn() -> !, nice: i8) -> Option<usize> {
    let _g = IfOffGuard::new();
    let mut s = SCHED.lock();
    let i = alloc_slot(&mut s)?;
    FPU_VALID[i].store(false, Ordering::Relaxed);
    s.tasks[i] = EMPTY_TASK;
    s.tasks[i].rsp = init_stack(i, entry);
    s.tasks[i].stack_bottom = task_stack_bottom(i);
    s.tasks[i].nice = nice;
    s.tasks[i].vruntime = s.min_vr;
    crate::apuser::on_spawn(i, false);
    s.tasks[i].state = State::Ready;
    Some(i)
}

/// Tickless-idle helper: when NO task other than `current` is runnable, return the
/// earliest Sleeping deadline among the others (so the caller can fast-forward the
/// clock straight to it instead of busy-spinning through idle ticks). Returns:
/// - `Some(deadline)`  — everyone else is Sleeping; the soonest wakes at `deadline`.
/// - `None`            — some other task is Ready (real work to do), OR the only
///                       non-current tasks are Blocked with no timeout (a genuine
///                       wait/deadlock, not idle) — do not fast-forward.
/// Used to make chrome's multi-second timed futex waits elapse in wall-time instead
/// of ~60x-slower busy-spun guest ticks under TCG.
pub fn idle_next_deadline(current: usize) -> Option<u64> {
    let s = SCHED.lock();
    let mut earliest: Option<u64> = None;
    let mut any_blocked = false;
    for i in 0..s.count {
        if i == current {
            continue;
        }
        match s.tasks[i].state {
            State::Ready if crate::apuser::on_bsp(i) => return None, // real work is runnable now
            State::Sleeping(w) => earliest = Some(earliest.map_or(w, |e| e.min(w))),
            State::Blocked(_) => any_blocked = true,
            _ => {}
        }
    }
    // Only fast-forward toward a real timed wakeup. If the only non-idle tasks are
    // Blocked-with-no-deadline, that is a genuine wait (or deadlock) — don't skip.
    let _ = any_blocked;
    earliest
}

/// Wake up to `n` tasks blocked on wait channel `chan`. Returns the number of
/// woken tasks.
pub fn wake(chan: u64, n: usize) -> usize {
    let mut s = SCHED.lock();
    let count = s.count;
    let mut woken = 0;
    for i in 0..count {
        if woken >= n {
            break;
        }
        if s.tasks[i].state == State::Blocked(chan) {
            s.tasks[i].state = State::Ready;
            place_woken(&mut s, i);
            woken += 1;
            self::woken(&s, i);
        }
    }
    drop(s);
    kick_after_wake();
    woken
}

/// Make the CURRENT task sleep `n` ticks (100 Hz → ~10 ms/tick). Real timed wait:
/// the scheduler skips it until the wake time. (The caller then yields the CPU
/// at the next timer tick.)
pub fn sleep_ticks(n: u64) {
    let wake = crate::interrupts::ticks() + n;
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Sleeping(wake);
}

/// Mark the CURRENT task as ZOMBIE with an exit code (just-exited process). Waits
/// for reaping by [`reap`]/[`take_zombie_child`] (waitpid). Returns the index.
pub fn exit_current(code: i64) -> usize {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Zombie(code);
    cur
}

/// Reap zombie task `idx`: return its exit code and set it to Dead.
pub fn reap(idx: usize) -> Option<i64> {
    let mut s = SCHED.lock();
    if idx < s.count {
        if let State::Zombie(code) = s.tasks[idx].state {
            s.tasks[idx].state = State::Dead;
            return Some(code);
        }
    }
    None
}

/// Find a zombie child of parent `ppid` and reap it: (pid, exit code). For waitpid.
pub fn take_zombie_child(ppid: u32) -> Option<(u32, i64)> {
    let mut s = SCHED.lock();
    let count = s.count;
    for i in 0..count {
        if s.tasks[i].ppid == ppid {
            if let State::Zombie(code) = s.tasks[i].state {
                s.tasks[i].state = State::Dead;
                return Some((s.tasks[i].pid, code));
            }
        }
    }
    None
}

/// Set pid/ppid of task `idx` (by the process layer at spawn/fork).
pub fn set_ident(idx: usize, pid: u32, ppid: u32) {
    let mut s = SCHED.lock();
    if idx < s.count {
        s.tasks[idx].pid = pid;
        s.tasks[idx].ppid = ppid;
    }
}

/// Set the nice value (priority) of task `idx` (-20..19).
pub fn set_nice(idx: usize, nice: i8) {
    let mut s = SCHED.lock();
    if idx < s.count {
        s.tasks[idx].nice = nice.clamp(-20, 19);
    }
}

/// Mark the CURRENT task as terminated (by the fault handler). Returns the index.
pub fn mark_current_dead() -> usize {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Dead;
    cur
}

/// Mark a specific task as terminated (e.g. `kill <pid>` from the shell).
pub fn mark_dead(idx: usize) {
    let mut s = SCHED.lock();
    bring_home(&mut s, idx); // S4: never mark a task dead that an AP still runs
    if idx < s.count {
        s.tasks[idx].state = State::Dead;
    }
}

/// Is task `idx` Dead (terminated, awaiting reclaim)? Used to reclaim leaked
/// per-thread resources (e.g. kernel-stack slots) whose owner faulted without
/// running the clean exit path.
/// A task's scheduler state, printable — for the futex lost-wake trap.
pub fn state_of(idx: usize) -> Option<&'static str> {
    let s = SCHED.try_lock()?;
    if idx >= s.count {
        return None;
    }
    Some(match s.tasks[idx].state {
        State::Ready => "Ready",
        State::Blocked(_) => "Blocked",
        State::Sleeping(_) => "Sleeping",
        State::Zombie(_) => "Zombie",
        State::Dead => "Dead",
    })
}

/// The task whose saved FS_BASE equals `tcb` — on x86_64 pthread_t IS the TCB
/// address IS fs_base, so glibc's _IO_lock_t owner pointer names a task directly.
pub fn task_by_fs_base(tcb: u64) -> Option<usize> {
    let s = SCHED.try_lock()?;
    (0..s.count).find(|&i| s.tasks[i].fs_base == tcb)
}

pub fn is_dead(idx: usize) -> bool {
    let s = SCHED.lock();
    idx < s.count && s.tasks[idx].state == State::Dead
}


/// The shared boot PML4 (kernel address space). Set by main after `paging::init`.
static BOOT_PML4: AtomicU64 = AtomicU64::new(0);

pub fn set_boot_pml4(p: u64) {
    BOOT_PML4.store(p, Ordering::Relaxed);
}

pub fn boot_pml4() -> u64 {
    BOOT_PML4.load(Ordering::Relaxed)
}


/// A pristine, unused task slot (used to reset a slot before it is recycled).
const EMPTY_TASK: Task = Task { rsp: 0, kstack: 0, fs_base: 0, cr3: 0, state: State::Dead, nice: 0, vruntime: 0, pid: 0, ppid: 0, stack_bottom: 0, sc_user_rsp: 0, sc_user_rip: 0, sc_saved_regs: 0 };

struct Scheduler {
    tasks: [Task; MAX_TASKS],
    count: usize,
    current: usize,
    /// Slots of fully-finished tasks (resources freed, no BgProc) available for
    /// reuse — so the OS can run unbounded programs without exhausting the table.
    free_slots: alloc::vec::Vec<usize>,
    /// The smallest vruntime among runnable tasks at the last pick: where a new
    /// or woken task is placed, so nobody arrives far behind the pack and holds
    /// the CPU until it catches up (CFS's min_vruntime).
    min_vr: u64,
}

static SITE_SCHED: eurolock::LockSite = eurolock::LockSite::new("sched.SCHED (BSP scheduler)", eurolock::LockClass::Sched);
// J1: measured legacy global lock (Mode::Plain = statistics only, behaviour unchanged).
static SCHED: eurolock::sync::Mutex<Scheduler> =
    eurolock::sync::Mutex::new_site(&SITE_SCHED, eurolock::Mode::Plain, Scheduler {
    tasks: [const {
        Task { rsp: 0, kstack: 0, fs_base: 0, cr3: 0, state: State::Ready, nice: 0, vruntime: 0, pid: 0, ppid: 0, stack_bottom: 0, sc_user_rsp: 0, sc_user_rip: 0, sc_saved_regs: 0 }
    }; MAX_TASKS],
    count: 1,
    current: 0,
    min_vr: 0,
    free_slots: alloc::vec::Vec::new(),
});

// Stacks for the background tasks (task 0 uses the existing kernel stack).
static mut STACKS: [[u8; STACK_SIZE]; MAX_TASKS] = [[0; STACK_SIZE]; MAX_TASKS];

// ── Per-task FPU / SSE (XMM) state ──────────────────────────────────────────
// The context switch previously saved GP regs + FS_BASE + syscall state, but NOT
// the x87/SSE register file. Real userland (glibc memcpy/strlen, chrome's Skia
// rasterizer, any -msse code) keeps live data in XMM0-15/MXCSR; without saving it
// on switch, two preemptively-scheduled threads silently clobber each other's XMM
// state. That corruption manifests as wild pointers / rip=0 / GP faults — the
// "worker threads die" wall on the chrome multi-process path. Fix: FXSAVE the
// outgoing task's FPU/SSE state and FXRSTOR the incoming task's on every switch.
// (FXSAVE = x87 + SSE, 512 bytes; sufficient because AVX/XCR0 is not enabled, so
// there is no YMM/ZMM state to preserve. When AVX lands this must become XSAVE.)
// 64-byte aligned + large enough for an XSAVE area with x87+SSE+AVX (legacy 512 +
// header 64 + YMM_Hi 256 = 832 B); 1 KiB gives margin. 64-byte alignment satisfies
// both XSAVE (needs 64) and FXSAVE (needs 16). When AVX is off only the first 512 B
// (the FXSAVE image) are used.
#[repr(C, align(64))]
struct FpuArea([u8; 1024]);
static mut FPU_AREAS: [FpuArea; MAX_TASKS] = [const { FpuArea([0; 1024]) }; MAX_TASKS];
// A clean save image (FNINIT state) copied into a slot the first time a task is
// switched to, so its first restore loads a valid, zeroed FPU rather than garbage.
static mut FPU_TEMPLATE: FpuArea = FpuArea([0; 1024]);
static FPU_VALID: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];
// True once AVX is enabled (CR4.OSXSAVE + XCR0[x87|SSE|AVX]) — the context switch
// then uses XSAVE/XRSTOR (preserving YMM) instead of FXSAVE/FXRSTOR. Stays false on
// CPUs without XSAVE/AVX (e.g. the default qemu64 the public image boots on), so
// that path is byte-for-byte the verified SSE-only behavior.
static AVX_ENABLED: AtomicBool = AtomicBool::new(false);

/// RAII interrupt-off guard: disables interrupts on creation and restores the
/// PREVIOUS state on drop. For ring3's spinlock helpers: a lock held with IF=1
/// can have its holder parked by the timer forever, freezing every later IF=0
/// spinner (the chrome-MP freeze family).
pub struct IfOffGuard(bool);
impl IfOffGuard {
    pub fn new() -> Self {
        let was = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        IfOffGuard(was)
    }
}
impl Drop for IfOffGuard {
    fn drop(&mut self) {
        if self.0 {
            x86_64::instructions::interrupts::enable();
        }
    }
}

/// Whether AVX was enabled at boot (for the System panel / diagnostics).
pub fn avx_enabled() -> bool {
    AVX_ENABLED.load(Ordering::Relaxed)
}

/// Enable AVX if the CPU supports it, then capture a clean FPU save template. Call
/// once at boot before preemptive scheduling starts.
///
/// AVX matters for real userland: chrome's SwiftShader software GL (and much SIMD
/// code) uses AVX2, which #UDs unless the OS sets CR4.OSXSAVE and enables the AVX
/// state bit in XCR0. Gated on CPUID so the lean image on plain qemu64 (no AVX) is
/// unaffected. Enabling AVX also means the switch must preserve YMM (XSAVE), which
/// is why FpuArea is XSAVE-sized above.
pub fn fpu_init() {
    // CPUID.1:ECX bit 26 = XSAVE, bit 28 = AVX.
    let c1 = unsafe { core::arch::x86_64::__cpuid(1) };
    let has_xsave = c1.ecx & (1 << 26) != 0;
    let has_avx = c1.ecx & (1 << 28) != 0;
    if has_xsave && has_avx {
        unsafe {
            // CR4.OSXSAVE (bit 18): allow XSETBV/XGETBV and OS-managed extended state.
            let mut cr4: u64;
            core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
            cr4 |= 1 << 18;
            core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack, preserves_flags));
            // XCR0 |= x87(0) | SSE(1) | AVX(2) = 0x7. ECX=0 selects XCR0.
            core::arch::asm!("xsetbv", in("ecx") 0u32, in("eax") 0x7u32, in("edx") 0u32,
                options(nomem, nostack, preserves_flags));
        }
        AVX_ENABLED.store(true, Ordering::Relaxed);
    }
    // Capture the clean template in whichever format the switch will use.
    unsafe {
        let p = core::ptr::addr_of_mut!(FPU_TEMPLATE) as *mut u8;
        if AVX_ENABLED.load(Ordering::Relaxed) {
            core::arch::asm!("fninit", "xsave [{}]", in(reg) p, in("eax") 7u32, in("edx") 0u32,
                options(nostack, preserves_flags));
        } else {
            core::arch::asm!("fninit", "fxsave [{}]", in(reg) p, options(nostack, preserves_flags));
        }
    }
    crate::serial_println!("[fpu] context-switch FPU save: {} (AVX {})",
        if AVX_ENABLED.load(Ordering::Relaxed) { "XSAVE (x87+SSE+AVX/YMM)" } else { "FXSAVE (x87+SSE)" },
        if AVX_ENABLED.load(Ordering::Relaxed) { "ON" } else { "off" });
}

/// Save the outgoing task's FPU state and restore the incoming task's. Uses XSAVE
/// (YMM-preserving) when AVX is enabled, else FXSAVE. Only the switch path calls
/// this, with `out != inc`.
#[inline]
unsafe fn fpu_switch(out: usize, inc: usize) {
    let areas = core::ptr::addr_of_mut!(FPU_AREAS) as *mut FpuArea;
    let out_p = (*areas.add(out)).0.as_mut_ptr();
    let valid = FPU_VALID[inc].load(Ordering::Relaxed);
    let src = if valid {
        (*areas.add(inc)).0.as_ptr()
    } else {
        core::ptr::addr_of!(FPU_TEMPLATE) as *const u8 // first run: clean template
    };
    if AVX_ENABLED.load(Ordering::Relaxed) {
        core::arch::asm!("xsave [{}]", in(reg) out_p, in("eax") 7u32, in("edx") 0u32, options(nostack, preserves_flags));
        core::arch::asm!("xrstor [{}]", in(reg) src, in("eax") 7u32, in("edx") 0u32, options(nostack, preserves_flags));
    } else {
        core::arch::asm!("fxsave [{}]", in(reg) out_p, options(nostack, preserves_flags));
        core::arch::asm!("fxrstor [{}]", in(reg) src, options(nostack, preserves_flags));
    }
    FPU_VALID[out].store(true, Ordering::Relaxed);
    FPU_VALID[inc].store(true, Ordering::Relaxed);
}

/// G1: a GUARDED kernel stack top per task slot (0 = not set → fall back on the
/// BSS `STACKS`). main.rs fills these before `init()` from the guarded-stack pool, so
/// that a kernel-task overflow faults on an unmapped guard page (→ hardware #PF,
/// the fault handler terminates only that task) instead of silently smashing the neighbor stack.
static TASK_GUARDED_TOP: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Set the guarded stack top for task slot `idx` (call before [`init`]).
pub fn set_task_guarded_stack(idx: usize, top: u64) {
    if idx < MAX_TASKS {
        TASK_GUARDED_TOP[idx].store(top, Ordering::Release);
    }
}

global_asm!(
    ".global timer_switch",
    "timer_switch:",
    // BUG-032: an interrupt does not clear RFLAGS.DF, and the interrupted code may
    // be inside a backward copy (`std; rep movsq; cld`: compiler_builtins memmove,
    // or ring 3). Rust code assumes DF=0, so the scheduler's own `rep movsq` ran
    // backwards and read a stale stack word as its SCHED guard pointer. The
    // interrupted DF comes back with its RFLAGS on iretq.
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",   // arg1 = current stack pointer (full saved context)
    "and rsp, -16",   // 16-align for the Rust call
    "call schedule_tick",
    "mov rsp, rax",   // switch to the next task's stack
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

// Cooperative-yield stub: identical to `timer_switch`, but its Rust callee
// (`yield_tick`) does NOT send an APIC EOI (this is a software interrupt, not a
// hardware IRQ). Invoked via `int YIELD_VECTOR` from `yield_now` after a task
// has blocked/slept itself, so the switch happens now instead of at the next tick.
global_asm!(
    ".global yield_switch",
    "yield_switch:",
    // BUG-032: an interrupt does not clear RFLAGS.DF, and the interrupted code may
    // be inside a backward copy (`std; rep movsq; cld`: compiler_builtins memmove,
    // or ring 3). Rust code assumes DF=0, so the scheduler's own `rep movsq` ran
    // backwards and read a stale stack word as its SCHED guard pointer. The
    // interrupted DF comes back with its RFLAGS on iretq.
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call yield_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

// S2: the reschedule IPI (IPI_RESCHED_VECTOR). Identical to `yield_switch`; its
// Rust callee sends the EOI (a hardware interrupt) and switches like a tick that
// does not count time: the woken task with the least vruntime runs now instead
// of at the next tick.
global_asm!(
    ".global resched_switch",
    "resched_switch:",
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call resched_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

extern "C" {
    fn timer_switch();
    fn yield_switch();
    fn resched_switch();
}

/// Address of the reschedule-IPI stub (IPI_RESCHED_VECTOR in the IDT).
pub fn resched_stub_addr() -> u64 {
    resched_switch as usize as u64
}

/// Reschedule IPIs taken.
pub static RESCHED_IPIS: AtomicU64 = AtomicU64::new(0);

/// S8: windows in which a tick or a reschedule IPI must not switch tasks (a
/// system call with IF=0 that waits for a disk completion with interrupts on),
/// and a reschedule IPI that came in such a window.
static NO_PREEMPT: AtomicU64 = AtomicU64::new(0);
static RESCHED_DEFERRED: AtomicBool = AtomicBool::new(false);

/// Guard of a no-preemption window (BSP only, interrupts off when made).
pub struct NoPreempt(());
impl NoPreempt {
    pub fn new() -> Self {
        NO_PREEMPT.fetch_add(1, Ordering::Relaxed);
        NoPreempt(())
    }
}
impl Drop for NoPreempt {
    fn drop(&mut self) {
        if NO_PREEMPT.fetch_sub(1, Ordering::Relaxed) == 1 && RESCHED_DEFERRED.swap(false, Ordering::Relaxed) {
            resched_ipi();
        }
    }
}

#[no_mangle]
pub extern "sysv64" fn resched_tick(rsp: u64) -> u64 {
    note_df(rsp);
    crate::apic::eoi();
    RESCHED_IPIS.fetch_add(1, Ordering::Relaxed);
    schedule_core(rsp, false, false)
}

/// Ask the BSP to reschedule: a fixed IPI on IPI_RESCHED_VECTOR. From a system
/// call (IF=0) it stays pending until `sysret`: the waker finishes its call and
/// is preempted on its first user instruction (no yield inside the call).
pub fn resched_ipi() {
    crate::apic::send_ipi(crate::percpu::bsp_apic_id(), crate::interrupts::IPI_RESCHED_VECTOR);
}

pub fn stub_addr() -> u64 {
    timer_switch as usize as u64
}

/// Address of the cooperative-yield stub (registered on `YIELD_VECTOR` in the IDT).
pub fn yield_stub_addr() -> u64 {
    yield_switch as usize as u64
}

/// Cooperative yield: switch to another runnable task RIGHT NOW. The caller must
/// already have moved the current task off Ready (block_current / sleep_ticks);
/// otherwise it just round-robins. Must hold NO locks (SCHED especially). Safe
/// from a syscall (ring-0) context: the software interrupt saves this task's
/// kernel context so it resumes exactly here when scheduled again.
/// Is any OTHER task Ready to run right now? The launcher's wait loop asks this to
/// choose between handing the CPU over (someone wants it) and a real `hlt` (nobody
/// does — and under TCG a halted vCPU parks the host thread, which is the difference
/// between an idle guest costing nothing and costing a full core).
pub fn any_other_ready() -> bool {
    let s = match SCHED.try_lock() {
        Some(g) => g,
        None => return true, // contended: assume someone is runnable
    };
    let cur = s.current;
    (0..s.count).any(|i| i != cur && s.tasks[i].state == State::Ready && crate::apuser::on_bsp(i))
}

#[track_caller]
pub fn yield_now() {
    if current_lockfree() == 0 {
        note_t0_yield(core::panic::Location::caller());
    }
    // SAFETY: YIELD_VECTOR is wired to `yield_switch` in the IDT (interrupts::init).
    // No options: the software interrupt pushes a frame (uses the stack) and the
    // context switch reads/writes scheduler memory; the switch preserves all GP
    // registers, so the compiler's default caller-saved assumptions are correct.
    unsafe { core::arch::asm!("int {v}", v = const crate::interrupts::YIELD_VECTOR) };
}

/// Called by the assembly stub: save the current rsp, pick the next
/// task (round-robin) and return its rsp.
///
/// NOTE: explicit `sysv64` — the UEFI target turns `extern "C"` into the Win64
/// ABI (argument in RCX), but our stub provides the argument in RDI (SysV).
/// BUG-007 diagnostic: timer ticks that found SCHED already held (in task context) and
/// were safely skipped instead of deadlocking. Nonzero ⇒ the deadlock window was hit.
pub static SCHED_SKIPS: AtomicU64 = AtomicU64::new(0);

/// BUG-032: context switches entered while the interrupted code had RFLAGS.DF set
/// (read from its interrupt frame), and those in which the Rust handler itself
/// still saw DF set (must stay 0: the stubs clear it). For the `[bug032]` self-test.
pub static DF_INTERRUPTED: AtomicU64 = AtomicU64::new(0);
pub static DF_IN_HANDLER: AtomicU64 = AtomicU64::new(0);
const RFLAGS_DF: u64 = 1 << 10;

#[inline(always)]
fn note_df(rsp: u64) {
    let f: u64;
    // SAFETY: reads RFLAGS; `rsp` is the stub's frame: 15 GPRs, RIP, CS, RFLAGS.
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) f, options(preserves_flags)) };
    if f & RFLAGS_DF != 0 {
        DF_IN_HANDLER.fetch_add(1, Ordering::Relaxed);
    }
    if unsafe { *((rsp + 136) as *const u64) } & RFLAGS_DF != 0 {
        DF_INTERRUPTED.fetch_add(1, Ordering::Relaxed);
    }
}

/// BUG-032 regression self-test: run across timer ticks and one cooperative yield
/// with RFLAGS.DF set (pure assembly: Rust code must never run with DF=1), then
/// check that every context switch in that window saw DF set in the interrupted
/// frame, that the Rust handler never saw it (the stubs clear it), that iretq gave
/// the interrupted code its DF back, and that SYSCALL entry clears DF (FMASK).
/// Before the fix the handler ran with DF=1 and the scheduler's `rep movsq` copied
/// its lock guard backwards (the sched.rs:755 panic, `current` read through a stale
/// stack word).
pub fn df_selftest() {
    let was_on = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::enable();
    let (i0, h0) = (DF_INTERRUPTED.load(Ordering::Relaxed), DF_IN_HANDLER.load(Ordering::Relaxed));
    let ticks = core::ptr::addr_of!(crate::interrupts::TICKS) as *const u64;
    let target = crate::interrupts::TICKS.load(Ordering::Relaxed) + 3;
    let f_after: u64;
    // SAFETY: only `cmp`/`pause`/`int`/flag instructions run between std and cld;
    // the yield vector is the scheduler's own stub (round-robins when nobody blocked).
    unsafe {
        core::arch::asm!(
            "std",
            "2:",
            "pause",
            "cmp [{t}], {n}",
            "jb 2b",
            "int {v}",
            "pushfq",
            "pop {f}",
            "cld",
            v = const crate::interrupts::YIELD_VECTOR,
            t = in(reg) ticks,
            n = in(reg) target,
            f = out(reg) f_after,
        );
    }
    if !was_on {
        x86_64::instructions::interrupts::disable();
    }
    let interrupted = DF_INTERRUPTED.load(Ordering::Relaxed) - i0;
    let in_handler = DF_IN_HANDLER.load(Ordering::Relaxed) - h0;
    let kept = f_after & RFLAGS_DF != 0;
    let fmask = unsafe { x86_64::registers::model_specific::Msr::new(0xC000_0084).read() };
    let fmask_df = fmask & RFLAGS_DF != 0;
    let ok = interrupted >= 2 && in_handler == 0 && kept && fmask_df;
    crate::serial_println!(
        "[bug032] DF set across context switches: {interrupted} switch(es) interrupted code with DF=1, \
         handler saw DF=1 in {in_handler}, DF back after iretq={kept}, SYSCALL clears DF (FMASK {fmask:#x})={fmask_df} → {}",
        if ok { "OK ✓" } else { "FAILED" }
    );
}
static SCHED_LOG_CTR: AtomicU64 = AtomicU64::new(0);

/// The desktop loop (task 0) left the CPU while it still had something to poll
/// (a yield, or preempted at work), not halted in an idle machine.
static T0_WANTS: AtomicBool = AtomicBool::new(false);
/// The desktop loop yielded in this tick: no second pass before the next tick
/// (unless the hosted window drew, DESK_KICK).
static T0_YIELDED: AtomicBool = AtomicBool::new(false);
/// The desktop loop is in its idle halt (set right before the hlt, cleared
/// after it). A tick that finds it so was not work (see idle_tick).
static T0_IDLE: AtomicBool = AtomicBool::new(false);

/// The desktop loop's idle wait: halt until the next interrupt, and let the
/// scheduler know that the time is idle, not work.
pub fn desktop_idle_hlt() {
    T0_IDLE.store(true, Ordering::Relaxed);
    let t0 = rdtsc();
    let tick0 = crate::interrupts::TICKS.load(Ordering::Relaxed);
    T0_HLT_START.store(t0, Ordering::Relaxed);
    x86_64::instructions::hlt();
    let st = T0_HLT_START.swap(0, Ordering::Relaxed);
    if st != 0 {
        T0_HLT_ACC.fetch_add(rdtsc().saturating_sub(st), Ordering::Relaxed);
    }
    // 11f-A: what ended the idle halt, the timer tick or another interrupt.
    let d = rdtsc().saturating_sub(t0);
    // Only while a key is on its way to the screen (delivery to first paint).
    if crate::xserver::keylat::in_flight() {
        let k = if crate::interrupts::TICKS.load(Ordering::Relaxed) != tick0 { 0 } else { 1 };
        IDLE_BY[k].fetch_add(d, Ordering::Relaxed);
        IDLE_BY_N[k].fetch_add(1, Ordering::Relaxed);
    }
    T0_IDLE.store(false, Ordering::Relaxed);
}
/// Idle halts of the BSP ended by the timer tick [0] or another interrupt [1]:
/// TSC and count (`keylat` prints them, `keylat reset` clears them). Ended by
/// the tick means nothing woke a waiting thread before it: a timed wait,
/// which the 10 ms tick rounds up.
/// 11f-A: sleepers the tick woke while a key was on its way, by the system
/// call they slept in: epoll_wait/epoll_pwait, poll/ppoll/select/pselect,
/// futex, nanosleep/clock_nanosleep, other (kernel tasks included).
pub static TICK_WAKES: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
pub const TICK_WAKE_NAMES: [&str; 5] = ["epoll", "poll/select", "futex", "nanosleep", "other"];
fn tick_wake_kind(sys: u64) -> usize {
    match sys {
        232 | 281 | 441 => 0,
        7 | 271 | 23 | 270 => 1,
        202 => 2,
        35 | 230 => 3,
        _ => 4,
    }
}
pub static IDLE_BY: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
pub static IDLE_BY_N: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// The tick in which the desktop loop was last given the CPU by the scheduler.
static T0_TURN_TICK: AtomicU64 = AtomicU64::new(0);
/// Turns the desktop loop got because a tick ended another task's turn.
pub static T0_TICK_TURNS: AtomicU64 = AtomicU64::new(0);

pub static TRACE_SCHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// TSC at the previous timer interrupt, ticks the clock had to add to stay on real
/// time, and how many late ticks have been logged.
static LAST_TICK_TSC: AtomicU64 = AtomicU64::new(0);
/// Timer ticks that landed while each task was current (lock-free; read by the
/// DevTools heartbeat for the [cpu] line).
pub static TICKS_PER_TASK: [AtomicU64; MAX_TASKS] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; MAX_TASKS]
};
pub static LOST_TICKS: AtomicU64 = AtomicU64::new(0);
/// BUG-400 (`[clock-drift]`, `uptime`): timer interrupts taken on the BSP, and
/// the ticks they added to the tick count (the rest of the count is forced
/// advances from task context).
pub static TICK_IRQS: AtomicU64 = AtomicU64::new(0);
pub static TICK_ADDED: AtomicU64 = AtomicU64::new(0);
/// BUG-400: the tick count's share of real time, the periods of the time
/// base (`timebase`) the timer interrupts have counted. `TICKS` is this plus
/// the forced advances from task context.
static REAL_TICKS: AtomicU64 = AtomicU64::new(0);

/// The ticks of real time counted so far (the rest of `TICKS` was forced).
pub fn real_ticks() -> u64 {
    REAL_TICKS.load(Ordering::Relaxed)
}

// -- S0 measurement (docs/design/SMP-SCHEDULER.md) --
// Run time per task on the TSC (a short turn right after a tick is never seen by
// the tick ledger), the ticks split by ring, switches, and where the desktop loop
// gives the CPU away. All lock-free; read by `sched-stat` and `hogtest`.
/// TSC cycles each task ran, charged at every switch away from it (task 0's
/// idle halt not included).
pub static RUN_TSC: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// TSC at which the running task's turn began (or was last charged).
static EXEC_START: AtomicU64 = AtomicU64::new(0);
/// The running task has an address space of its own (a ring-3 task).
static CUR_USER: AtomicBool = AtomicBool::new(false);
/// Ticks that found each task in ring 3, and in ring 0 (task 0's idle halt apart).
pub static UTICKS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
pub static KTICKS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
pub static IDLE_TICKS: AtomicU64 = AtomicU64::new(0);
/// Times each task was given the CPU.
pub static SWITCHES_IN: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// TSC the desktop loop spent halted in all; the start of a halt in progress;
/// halted time not yet taken off its run time.
pub static T0_IDLE_TSC: AtomicU64 = AtomicU64::new(0);
static T0_HLT_START: AtomicU64 = AtomicU64::new(0);
static T0_HLT_ACC: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    // SAFETY: rdtsc has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Idle time of the desktop loop since the last charge, up to `now`.
fn t0_idle_take(now: u64) -> u64 {
    let mut idle = T0_HLT_ACC.swap(0, Ordering::Relaxed);
    let st = T0_HLT_START.load(Ordering::Relaxed);
    if st != 0 {
        idle += now.saturating_sub(st);
        T0_HLT_START.store(now, Ordering::Relaxed);
    }
    T0_IDLE_TSC.fetch_add(idle, Ordering::Relaxed);
    idle
}

/// Run time of task `idx` so far, the running task's turn included.
pub fn run_tsc_now(idx: usize) -> u64 {
    let base = RUN_TSC[idx].load(Ordering::Relaxed);
    if current_lockfree() == idx {
        base + rdtsc().saturating_sub(EXEC_START.load(Ordering::Relaxed))
    } else {
        base
    }
}

/// Bumped each time a slot gets a new task: `hogtest` tells the processes
/// started in its window from the tasks that were there before.
pub static SPAWN_GEN: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Kernel time of ring-3 tasks on the TSC (system calls and demand faults). A
/// system call runs with IF=0, so no tick ever samples it: the tick ledger
/// cannot see this share. KSTART: when the task's current stretch in a system
/// call began (0 = not in one, 1 = in one but switched away).
pub static KTSC: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
static KSTART: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// A system call of task `t` begins / ends (ring3::linux_dispatch).
pub fn syscall_enter(t: usize) {
    if t < MAX_TASKS {
        KSTART[t].store(rdtsc().max(2), Ordering::Relaxed);
    }
}
pub fn syscall_exit(t: usize) {
    if t < MAX_TASKS {
        let st = KSTART[t].swap(0, Ordering::Relaxed);
        if st > 1 {
            KTSC[t].fetch_add(rdtsc().saturating_sub(st), Ordering::Relaxed);
        }
    }
}
/// A demand fault of task `t` outside a system call took `cycles`.
pub fn fault_time(t: usize, cycles: u64) {
    if t < MAX_TASKS && KSTART[t].load(Ordering::Relaxed) == 0 {
        KTSC[t].fetch_add(cycles, Ordering::Relaxed);
    }
}

/// Is the running task a ring-3 task (lock-free, for the tick)?
pub fn task_is_user(idx: usize) -> bool {
    idx == current_lockfree() && CUR_USER.load(Ordering::Relaxed)
}

/// Where the desktop loop yielded (`#[track_caller]` of `yield_now`): the
/// address of the `Location` and a count, 16 sites.
static T0_YIELD_KEY: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static T0_YIELD_N: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

fn note_t0_yield(loc: &'static core::panic::Location<'static>) {
    let key = loc as *const _ as u64;
    for i in 0..16 {
        let k = T0_YIELD_KEY[i].load(Ordering::Relaxed);
        if k == key || (k == 0 && T0_YIELD_KEY[i].compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed).is_ok()) {
            T0_YIELD_N[i].fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
}

/// The yield sites of the desktop loop so far: (key, count).
pub fn t0_yield_sites() -> alloc::vec::Vec<(u64, u64)> {
    (0..16)
        .map(|i| (T0_YIELD_KEY[i].load(Ordering::Relaxed), T0_YIELD_N[i].load(Ordering::Relaxed)))
        .filter(|x| x.0 != 0)
        .collect()
}

/// `file:line` of a yield site key.
pub fn site_name(key: u64) -> alloc::string::String {
    // SAFETY: keys are addresses of `&'static Location` stored by note_t0_yield.
    let loc = unsafe { &*(key as *const core::panic::Location<'static>) };
    alloc::format!("{}:{}", loc.file(), loc.line())
}

/// Stop a Ready task until [`start_parked`] (the scheduler tests' hogs). Not
/// for the running task.
pub fn park(idx: usize) {
    let mut s = SCHED.lock();
    bring_home(&mut s, idx);
    if idx < s.count && idx != s.current && s.tasks[idx].state == State::Ready {
        s.tasks[idx].state = State::Blocked(PARKED);
    }
}
/// TSC at the timer interrupt that was taken last (BUG-182).
pub fn last_tick_tsc() -> u64 {
    LAST_TICK_TSC.load(Ordering::Relaxed)
}
static LATE_TICKS_LOGGED: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub extern "sysv64" fn schedule_tick(rsp: u64) -> u64 {
    note_df(rsp);
    // A late tick: the periodic LAPIC timer has one pending bit, so a window with
    // interrupts off longer than a period swallows every period but one. The clock
    // is kept on real time from the TSC, and the first late ticks say WHERE the
    // window ended (the interrupted rip, the task and its last syscall, the demand
    // faults served meanwhile): run 35 on the NUC lost two thirds of its ticks with
    // the vCPU idle whenever anyone looked, and the desktop clock fell behind by
    // minutes while the timer itself was fine.
    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let prev_tsc = LAST_TICK_TSC.swap(now_tsc, Ordering::Relaxed);
    // S0: [if0-window], the cause of a late tick (steal, disk wait, faults, syscall).
    crate::schedstat::on_tick(prev_tsc, now_tsc, rsp);
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed);
    let mut add = 1u64;
    if prev_tsc != 0 && per != 0 {
        let gap = now_tsc.wrapping_sub(prev_tsc);
        if gap > per * 2 {
            let lost = (gap / per - 1).min(10_000);
            add += lost;
            LOST_TICKS.fetch_add(lost, Ordering::Relaxed);
            let n = LATE_TICKS_LOGGED.fetch_add(1, Ordering::Relaxed);
            // BUG-182: a window of 200 ms or more is always said (the first
            // 64): the one-second window of that bug came after the 24 lines
            // below were used up, and nothing named where it ended.
            static LONG_LOGGED: AtomicU64 = AtomicU64::new(0);
            let long = lost >= 20 && LONG_LOGGED.fetch_add(1, Ordering::Relaxed) < 64;
            if n < 24 || (n + 1) % 128 == 0 || long {
                let (rip, cs) = unsafe { (*((rsp + 120) as *const u64), *((rsp + 128) as *const u64)) };
                let cur = current();
                let (sn, sa1, _) = crate::ring3::last_syscall(cur);
                let (fc, fcyc) = crate::ring3::fault_counters();
                crate::serial_println!(
                    "[tick-late] #{} +{lost} ticks ({} Mcycles) rip={rip:#x} cs={cs:#x} task {cur} last-syscall={sn}(a1={sa1:#x}) faults-total={fc} ({} Mcycles) lost-total={}",
                    n + 1, gap / 1_000_000, fcyc / 1_000_000, LOST_TICKS.load(Ordering::Relaxed)
                );
            }
        }
    }
    // BUG-400: the tick count follows the time base, not the interrupts. The
    // late-tick rule above made up a coalesced period only when the gap was
    // over two periods, and the LAPIC period is as good as one 10 ms PIT
    // window (+747 ppm long under KVM: the clock lost 0.7 s an hour). Now each
    // interrupt adds the 10 ms periods the time base passed since the last one
    // (on a grid half a period off the interrupts, so jitter does not flip it):
    // 1 as a rule, 0 when the LAPIC runs early, more after a lost interrupt.
    // The rule above still logs the late ones.
    if per != 0 {
        if !crate::timebase::running() {
            let t = crate::interrupts::TICKS.load(Ordering::Relaxed) + add;
            crate::timebase::start(now_tsc, t * 10_000_000, per);
            REAL_TICKS.store(t, Ordering::Relaxed);
        } else {
            crate::timebase::on_tick(now_tsc, per);
            let due = (crate::timebase::ns_at(now_tsc) + 5_000_000) / 10_000_000;
            let done = REAL_TICKS.load(Ordering::Relaxed);
            add = due.saturating_sub(done);
            REAL_TICKS.store(done.max(due), Ordering::Relaxed);
        }
    }
    crate::interrupts::TICKS.fetch_add(add, Ordering::Relaxed);
    TICK_IRQS.fetch_add(1, Ordering::Relaxed);
    TICK_ADDED.fetch_add(add, Ordering::Relaxed);
    // Who was running when this tick landed: the CPU ledger the [cpu] heartbeat
    // line reads (the browser's download crawled at 8 KB/s in run 61 with the
    // data queued unread: was the reader starved, and by whom?).
    {
        let c = current_lockfree();
        if c < TICKS_PER_TASK.len() {
            TICKS_PER_TASK[c].fetch_add(add, Ordering::Relaxed);
        }
    }
    crate::interrupts::send_timer_eoi();
    if CENSUS_REQUEST.swap(false, Ordering::Relaxed) {
        census_trylock();
    }
    // 3G-2: the independent deadman check — runs even while the main loop is busy;
    // trips if the loop stopped petting the watchdog (a hang).
    crate::watchdog::tick_check();
    // Service the USB host controller on EVERY timer tick (100 Hz), independent of
    // task scheduling. A full-screen app (the DOOM port) can starve the desktop
    // loop down to a few Hz — far below the ~125 Hz an emulated USB keyboard's
    // interrupt endpoint expects — so keystrokes were being dropped. Polling here
    // keeps the endpoint serviced + re-armed regardless of load. IRQ-safe: the
    // `POLLING` guard bails if a task-context poll is mid-flight.
    crate::xhci::poll();
    crate::virtio_input::poll(); // virtio-tablet / virtio-keyboard (polled)
    // Refresh the vDSO clock page (userspace clock_gettime reads it, no syscall).
    crate::ring3::vdso_tick();
    // Sample WHERE the preempted thread was executing, if it was in ring 3. The stub
    // pushed 15 registers on top of the CPU's interrupt frame, so the interrupted RIP
    // sits at rsp+120 and its CS at rsp+128. Costs two loads per tick and is the only
    // way to see a thread that spins in user space without ever making a syscall.
    unsafe {
        let rip = *((rsp + 120) as *const u64);
        let cs = *((rsp + 128) as *const u64);
        let c = current_lockfree();
        if cs & 3 == 3 {
            crate::ring3::sample_user_rip(current(), rip);
            if c < MAX_TASKS {
                UTICKS[c].fetch_add(1, Ordering::Relaxed);
            }
        } else {
            crate::ring3::sample_kernel_rip(rip);
            if c == 0 && T0_IDLE.load(Ordering::Relaxed) {
                IDLE_TICKS.fetch_add(1, Ordering::Relaxed);
            } else if c < MAX_TASKS {
                KTICKS[c].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    // BUG-007: the timer must NEVER block on SCHED. Task-context code (the desktop loop,
    // syscalls, supervise/reap) holds SCHED.lock() with interrupts ENABLED; a blocking
    // acquire here would deadlock the core — this handler spins with interrupts off while
    // the lock holder, the very task we just preempted, can never run to release it
    // (total silence, no fault). So TRY the lock and, if it's held, skip this preemption
    // tick: the holder frees it within microseconds and the next tick schedules normally.
    schedule_core(rsp, false, true)
}

/// Cooperative YIELD: a task that just blocked/slept itself calls this to switch
/// away IMMEDIATELY instead of running (uselessly) until the next timer tick.
/// Entered via a dedicated software-interrupt vector (see `yield_switch`), so it
/// runs the SAME context switch as the timer, but WITHOUT the timer prelude
/// (no TICKS bump, no EOI, no watchdog/USB poll). Called from `yield_now`.
#[no_mangle]
pub extern "sysv64" fn yield_tick(rsp: u64) -> u64 {
    note_df(rsp);
    schedule_core(rsp, true, false)
}

extern "C" {
    /// The first byte of the loaded kernel image (PE/COFF, set by the linker).
    static __ImageBase: u8;
}
static IMAGE_LO: AtomicU64 = AtomicU64::new(0);
static IMAGE_HI: AtomicU64 = AtomicU64::new(0);

/// Is `a` inside the loaded kernel image (headers, code, data: all mapped in
/// every address space)? The bounds come from the image's own PE header,
/// read once; an unreadable header answers false for every address.
pub fn in_kernel_image(a: u64) -> bool {
    let mut lo = IMAGE_LO.load(Ordering::Relaxed);
    if lo == 0 {
        // SAFETY: __ImageBase is the start of the loaded image; its DOS and PE
        // headers are part of it.
        unsafe {
            let base = core::ptr::addr_of!(__ImageBase) as u64;
            let mz = (base as *const u16).read_unaligned();
            let lfanew = ((base + 0x3c) as *const u32).read_unaligned() as u64;
            let ok = mz == 0x5a4d && lfanew < 0x1000
                && ((base + lfanew) as *const u32).read_unaligned() == 0x0000_4550;
            // PE32+: SizeOfImage at offset 56 of the optional header, which follows
            // the 4-byte signature and the 20-byte file header.
            let size = if ok { ((base + lfanew + 24 + 56) as *const u32).read_unaligned() as u64 } else { 0 };
            IMAGE_HI.store(base + size, Ordering::Relaxed);
            IMAGE_LO.store(base.max(1), Ordering::Relaxed);
            lo = base.max(1);
        }
    }
    a >= lo && a < IMAGE_HI.load(Ordering::Relaxed)
}

/// The shared scheduling core: save the outgoing rsp, pick the next runnable task
/// (mini-CFS) and return its rsp. Used by both the timer tick and a cooperative
/// yield. Does NO EOI — the caller owns interrupt acknowledgement.
fn schedule_core(rsp: u64, via_yield: bool, tick: bool) -> u64 {
    // S8: a system call that waits for the disk with interrupts on keeps the CPU
    // (the timer still ran its prelude). A reschedule asked meanwhile is sent
    // again when the window ends.
    if !via_yield && NO_PREEMPT.load(Ordering::Relaxed) != 0 {
        if !tick {
            RESCHED_DEFERRED.store(true, Ordering::Relaxed);
        }
        return rsp;
    }
    let mut s = match SCHED.try_lock() {
        Some(g) => g,
        None => {
            if SCHED_SKIPS.fetch_add(1, Ordering::Relaxed) == 0 {
                crate::serial_println!(
                    "[sched-guard] preemption tick skipped — SCHED held in task context (BUG-007 deadlock averted)"
                );
            }
            return rsp; // keep running the current task; do not switch this tick
        }
    };
    let cur = s.current;
    // BUG-032: `current` once held eight bytes of kernel machine code here, and the
    // index below panicked with nothing but the bad value. Check it first (one
    // compare, the bounds check it replaces was there anyway) and dump what lies
    // around the field, so a recurrence says what was written over it.
    if cur >= MAX_TASKS || cur >= s.count {
        let count = s.count;
        let pc = core::ptr::addr_of!(s.current) as u64;
        drop(s);
        sched_state_corrupt(cur, count, pc, rsp);
    }
    s.tasks[cur].rsp = rsp;
    // S0: charge the outgoing task's turn on the TSC (task 0's idle halt apart).
    let now_tsc = rdtsc();
    let mut ran_tsc = now_tsc.saturating_sub(EXEC_START.swap(now_tsc, Ordering::Relaxed));
    if cur == 0 {
        ran_tsc = ran_tsc.saturating_sub(t0_idle_take(now_tsc));
    }
    RUN_TSC[cur].fetch_add(ran_tsc, Ordering::Relaxed);
    // Kernel time: a task switched away inside a system call stops the clock.
    let ks = KSTART[cur].load(Ordering::Relaxed);
    if ks > 1 {
        KTSC[cur].fetch_add(now_tsc.saturating_sub(ks), Ordering::Relaxed);
        KSTART[cur].store(1, Ordering::Relaxed);
    }
    // Save the OUTGOING task's in-flight syscall state (it may be mid-syscall, having
    // yielded from futex/epoll) so a concurrent thread's syscall cannot clobber its
    // user-return state; restored when this task is scheduled again.
    let (u_rsp, u_rip, s_regs) = crate::ring3::get_syscall_globals();
    s.tasks[cur].sc_user_rsp = u_rsp;
    s.tasks[cur].sc_user_rip = u_rip;
    s.tasks[cur].sc_saved_regs = s_regs;
    // S6 stack guard: is the canary of the just-run task still intact? If not,
    // its kernel stack overflowed -> stop with a clear diagnosis instead of
    // silently letting neighboring memory get corrupted.
    let sb = s.tasks[cur].stack_bottom;
    if sb != 0 && unsafe { (sb as *const u64).read_volatile() } != STACK_CANARY {
        panic!("STACK-OVERFLOW: kernel task {cur} overflowed its stack (guard canary @ {sb:#x} overwritten)");
    }
    // Save the FS_BASE (musl TLS pointer) of the outgoing task and restore that
    // of the incoming one — this way each preemptive musl process keeps its own TLS.
    let mut fs = Msr::new(IA32_FS_BASE);
    s.tasks[cur].fs_base = unsafe { fs.read() };
    // S3: tasks the APs sent home (a system call, a fault, an eviction): their
    // saved user frame and TLS come back into the table, their AP time is charged.
    drain_locked(&mut s);
    // 1. Wake sleepers: Sleeping -> Ready as soon as their wake time is reached.
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    let in_flight = crate::xserver::keylat::in_flight();
    for i in 0..s.count {
        if let State::Sleeping(w) = s.tasks[i].state {
            if now >= w {
                s.tasks[i].state = State::Ready;
                place_woken(&mut s, i);
                if in_flight {
                    TICK_WAKES[tick_wake_kind(crate::ring3::last_syscall(i).0)].fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    // 2. Let the outgoing task (if it's still runnable) climb its vruntime,
    //    weighted on nice. Higher nice -> larger step -> chosen less often.
    //    Not for a tick that found task 0 halted: idle time is not work, and
    //    charging it made the desktop loop the most expensive task on the
    //    machine after ten idle minutes. A renderer thread created then, at its
    //    creator's low vruntime, won every tick for 380 s (run 71: 38090 of
    //    39664 ticks) while the desktop loop, the heartbeats and every other
    //    thread waited for it to catch up.
    // Idle is what the desktop loop says it is: it sets T0_IDLE around its hlt.
    // This used to read the byte before the interrupted rip and call the tick
    // idle when that byte was 0xf4 (hlt). Two bugs in one read:
    // - W23 / the TCG watchdog trips: 0xf4 is also -12 as a displacement or
    //   an immediate (lea -0xc(%rbp), ...), common in the soft-float code the
    //   loop draws with. A tick there counted the busy loop as idle, cleared
    //   T0_WANTS, and the loop lost its turn at every tick to five busy
    //   self-test processes: one pass of 25-57 s, "the loop meanwhile: Ready".
    // - BUG-136: the read touched whatever the frame said (a frame of another
    //   layout, a user rip), with SCHED held; a page fault there asks the
    //   scheduler who runs and waits for its own lock.
    // S1: every turn is charged by its length on the TSC (`ran_tsc` above, task
    // 0's idle halt already taken off), weighted on nice, whether the task stays
    // Ready, blocks, sleeps or yields: a thread that yields after 50 us pays 50 us,
    // not a turn. The desktop loop never banks idle time: when it leaves the CPU
    // it is placed like a task that waited (at most half a slice ahead).
    // BUG-252: a driver is charged at nice -15 within its budget, at
    // DRIVER_OVER_NICE past it (the turn that crosses it counts as within).
    let cn = charge_nice(&s, cur);
    drv_account(&mut s, cur, ran_tsc);
    let charge = vr_charge(ran_tsc, cn);
    s.tasks[cur].vruntime = s.tasks[cur].vruntime.wrapping_add(charge);
    if cur == 0 {
        place_woken(&mut s, 0);
        if via_yield {
            T0_YIELDED.store(true, Ordering::Relaxed);
        }
    }
    if tick {
        // A new tick: the desktop loop may have a pass again.
        T0_YIELDED.store(false, Ordering::Relaxed);
    }
    if tick {
        deskwatch::tick_sample(cur, rsp, s.tasks[0].state);
    }
    // 3. Mini-CFS: pick the runnable task with the SMALLEST vruntime. Start the scan at
    //    cur+1 with strict '<', so that equal-vruntime tasks rotate round-robin
    //    (behavior-preserving with equal nice). Task 0 is always Ready -> fallback.
    //
    //    The desktop loop (task 0) is the exception (BUG-127). It is what the
    //    user looks at and types into, and one pass of it is short:
    //    - a timer tick that ends ANOTHER task's turn gives the CPU to the
    //      desktop loop first, when it has asked for more (it yielded or was
    //      preempted; not when it halted idle). It runs one pass and hands on,
    //      so the next task gets the rest of the tick. The desktop is then never
    //      further away than one tick, however many tasks are ready, and a busy
    //      desktop loop gets at most every other tick.
    //    - within one tick it gets no second turn from a task that yields or
    //      blocks: threads that pass a futex back and forth must not pay for a
    //      desktop pass every time.
    //    - when it yields, the CPU goes to someone else if anyone is ready.
    let mut best = cur;
    let mut bestv = u64::MAX;
    let mut found = false;
    let t0_out = T0_YIELDED.load(Ordering::Relaxed);
    // S3: Ready tasks in user context that go to an AP instead (offloaded at the
    // end of this switch, after the outgoing task's FPU state is saved).
    let ap_mode = crate::apuser::mode() != crate::apuser::OFF;
    let mut leave = [(0u16, 0u8); 16];
    let mut nleave = 0usize;
    for k in 1..=s.count {
        let i = (cur + k) % s.count;
        if i == 0 && t0_out {
            continue;
        }
        if s.tasks[i].state == State::Ready && !crate::apuser::on_bsp(i) {
            continue; // an AP owns it
        }
        if ap_mode && nleave < leave.len() && s.tasks[i].state == State::Ready {
            if let Some(c) = ap_target(&s, i) {
                leave[nleave] = (i as u16, c as u8);
                nleave += 1;
                continue;
            }
        }
        if s.tasks[i].state == State::Ready && s.tasks[i].vruntime < bestv {
            bestv = s.tasks[i].vruntime;
            best = i;
            found = true;
        }
    }
    if found && bestv > s.min_vr {
        s.min_vr = bestv;
    }
    // The hosted window drew (want_desktop): at the next yield of any task the
    // desktop loop puts it on the screen, not after the next tick.
    if !tick && cur != 0 && s.tasks[0].state == State::Ready && DESK_KICK.swap(false, Ordering::Relaxed) {
        best = 0;
        found = true;
    }
    if best == 0 && cur != 0 {
        T0_TURN_TICK.store(now, Ordering::Relaxed);
        if tick {
            T0_TICK_TURNS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if !found {
        let cur_leaves = leave[..nleave].iter().any(|&(t, _)| t as usize == cur);
        best = if s.tasks[cur].state == State::Ready && !cur_leaves && crate::apuser::on_bsp(cur) { cur } else { 0 };
    }
    crate::apuser::picked_on_bsp(best);
    if best != cur {
        ran(best);
    }
    // Wedge diagnostic (gated): rate-limited log of what the scheduler picks — fires
    // from the scheduler itself, so it works even when the launcher task is starved.
    if crate::ring3::STALL_DIAG.load(Ordering::Relaxed) {
        let n = SCHED_LOG_CTR.fetch_add(1, Ordering::Relaxed);
        if n % 30000 == 0 {
            let mut rdy = 0usize;
            let mut blk = 0usize;
            for i in 0..s.count {
                match s.tasks[i].state {
                    State::Ready => rdy += 1,
                    State::Blocked(_) => blk += 1,
                    _ => {}
                }
            }
            crate::serial_println!("[sched] #{n} cur={cur} -> next={best} found={found} | {rdy} Ready {blk} Blocked (via_yield={via_yield})");
        }
    }
    let _ = via_yield;
    // Save the outgoing task's x87/SSE (XMM) register file and restore the incoming
    // task's, so preemptively-scheduled threads don't clobber each other's FP/XMM
    // state (the multithreaded-corruption fix). Only when the task actually changes.
    if best != cur {
        unsafe { fpu_switch(cur, best) };
    }
    if KSTART[best].load(Ordering::Relaxed) == 1 {
        KSTART[best].store(now_tsc.max(2), Ordering::Relaxed);
    }
    if best != cur {
        SWITCHES_IN[best].fetch_add(1, Ordering::Relaxed);
        if best == 0 && T0_HLT_START.load(Ordering::Relaxed) != 0 {
            T0_HLT_START.store(now_tsc, Ordering::Relaxed);
        }
    }
    CUR_USER.store(s.tasks[best].cr3 != 0, Ordering::Relaxed);
    s.current = best;
    CURRENT_MIRROR.store(best as u64, Ordering::Relaxed);
    let next = s.current;
    unsafe { fs.write(s.tasks[next].fs_base) };
    // Switch address space (CR3) if the incoming task has its own PML4.
    // Every PML4 maps the kernel identically (supervisor), so this code + all
    // kernel stacks stay valid across the switch; only the USER-visible
    // user frames differ per process -> memory isolation.
    let boot = BOOT_PML4.load(Ordering::Relaxed);
    let cur_cr3 = if s.tasks[cur].cr3 != 0 { s.tasks[cur].cr3 } else { boot };
    let next_cr3 = if s.tasks[next].cr3 != 0 { s.tasks[next].cr3 } else { boot };
    if next_cr3 != 0 && next_cr3 != cur_cr3 {
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) next_cr3, options(nostack, preserves_flags)) };
    }
    // For a ring-3 task: set TSS.rsp0 to ITS kernel stack, so that its
    // interrupt frames don't collide with those of other ring-3 processes.
    let ks = s.tasks[next].kstack;
    if ks != 0 {
        crate::gdt::set_rsp0(ks);
    }
    // Restore the INCOMING task's syscall state + point the syscall stack at its own
    // kstack, so its (possibly mid-syscall) execution resumes on its own stack.
    crate::ring3::set_syscall_globals(
        s.tasks[next].sc_user_rsp,
        s.tasks[next].sc_user_rip,
        s.tasks[next].sc_saved_regs,
        ks,
    );
    // S3: hand the leaving tasks to their APs. The outgoing task's FPU state was
    // saved by fpu_switch above (it is not `next`: it was no pick candidate).
    for &(t, c) in &leave[..nleave] {
        let t = t as usize;
        if t == next {
            continue;
        }
        let tk = &s.tasks[t];
        crate::apuser::offload(t, c as usize, tk.rsp, tk.fs_base, tk.cr3, tk.kstack);
    }
    s.tasks[next].rsp
}

/// Make a fresh task stack with an initial context that jumps to `entry` via
/// `iretq` (with interrupts on).
fn init_stack(index: usize, entry: extern "C" fn() -> !) -> u64 {
    let cs = CS::get_reg().0 as u64;
    let ss = SS::get_reg().0 as u64;
    // G1: use the GUARDED stack if it's set for this slot (overflow → guard
    // #PF), otherwise the BSS `STACKS`. For the guarded stack `base` (lowest address)
    // is the first stack page; the unmapped guard page lies directly below it.
    let guarded = TASK_GUARDED_TOP[index].load(Ordering::Acquire);
    let (base, top) = if guarded != 0 {
        let top = guarded & !0xF;
        ((top - STACK_SIZE as u64) as *mut u8, top)
    } else {
        // SAFETY: each STACKS[index] is used by exactly one task.
        let b = unsafe { core::ptr::addr_of_mut!(STACKS[index]) as *mut u8 };
        (b, (b as u64 + STACK_SIZE as u64) & !0xF)
    };
    // S6: write the stack-guard canary at the bottom (lowest address) of the stack.
    unsafe { (base as *mut u64).write(STACK_CANARY) };
    let ctx = top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    unsafe {
        for i in 0..15 {
            words.add(i).write(0); // r15..rax = 0
        }
        words.add(15).write(entry as usize as u64); // rip
        words.add(16).write(cs); // cs
        words.add(17).write(0x202); // rflags (IF=1)
        words.add(18).write(top); // rsp for the task
        words.add(19).write(ss); // ss
    }
    ctx
}

/// The top of the scheduler-task kernel stack that contains `addr`, or `None` when `addr`
/// is on no such stack (boot stack, AP stacks, IST stacks, a ring-3 address). Lock-free:
/// the NMI probe calls it (BUG-202) and the scheduler lock may be what is wedged.
pub fn task_stack_top_containing(addr: u64) -> Option<u64> {
    for i in 0..MAX_TASKS {
        let g = TASK_GUARDED_TOP[i].load(Ordering::Acquire);
        if g != 0 {
            let top = g & !0xF;
            if addr >= top - STACK_SIZE as u64 && addr <= top {
                return Some(top);
            }
        }
    }
    // SAFETY: only the address of the static is taken.
    let base = unsafe { core::ptr::addr_of!(STACKS) as u64 };
    let end = base + (MAX_TASKS * STACK_SIZE) as u64;
    if addr >= base && addr < end {
        let slot = (addr - base) / STACK_SIZE as u64;
        return Some((base + (slot + 1) * STACK_SIZE as u64) & !0xF);
    }
    None
}

/// The stack bottom (canary location / lowest address) of task slot `index` —
/// guarded base if set, otherwise the BSS `STACKS`. Must match `init_stack`.
fn task_stack_bottom(index: usize) -> u64 {
    let guarded = TASK_GUARDED_TOP[index].load(Ordering::Acquire);
    if guarded != 0 {
        (guarded & !0xF) - STACK_SIZE as u64
    } else {
        unsafe { core::ptr::addr_of_mut!(STACKS[index]) as u64 }
    }
}

/// Start the background tasks and activate round-robin scheduling.
pub fn init() {
    // Where the table lives, for the census that takes no lock (BUG-157).
    SCHED_PEEK.store(&*SCHED.lock() as *const Scheduler as u64, Ordering::Relaxed);
    let mut s = SCHED.lock();
    s.tasks[0].nice = DESKTOP_NICE;
    // The kernel demo tasks (S2 priority a/b/c, the S2 sleeper, the G1 guarded-stack
    // overflow) are dev self-tests: they prove the mini-CFS priority ordering and
    // kernel-stack-overflow recovery. task_a/b/c busy-spin (until they park ~2.5 s in)
    // and task_overflow deliberately faults — neither belongs in a shipping image, so
    // spawn them only under `selftest`. The public image starts with just the boot
    // task (slot 0); real processes fill slots 1+ as they are spawned.
    if cfg!(feature = "selftest") {
        s.tasks[1].rsp = init_stack(1, task_a);
        s.tasks[2].rsp = init_stack(2, task_b);
        s.tasks[3].rsp = init_stack(3, task_c);
        s.tasks[4].rsp = init_stack(4, task_sleeper);
        // G1 self-test: a task on a GUARDED stack that intentionally overflows its
        // stack. The guard page catches it as a hardware #PF; the fault handler
        // terminates ONLY this task and the kernel keeps running (proof of recovery).
        s.tasks[5].rsp = init_stack(5, task_overflow);
        // S6: register the stack bottom (canary location) for the overflow watchdog.
        for i in 1..=5 {
            s.tasks[i].stack_bottom = task_stack_bottom(i);
        }
        // S2 priority demo: a/b/c do EQUAL workload but get different
        // nice — their counters show that the mini-CFS schedules high priority more often.
        s.tasks[1].nice = -10; // high priority  -> most turns
        s.tasks[2].nice = 0; //   normal
        s.tasks[3].nice = 10; //  low priority  -> fewest turns
        s.count = 6;
    } else {
        s.count = 1;
    }
    s.current = 0;
    CURRENT_MIRROR.store(0, Ordering::Relaxed);
    // BUG-032: the hardware write watch (memwatch.rs) guards these two fields.
    crate::memwatch::set_targets(
        core::ptr::addr_of!(s.current) as u64,
        core::ptr::addr_of!(s.count) as u64,
        MAX_TASKS,
    );
    drop(s);
    crate::memwatch::arm_this_cpu();
}

/// BUG-032: `schedule_core` found a `current` that is no task index. Name what is
/// around the field (the scheduler's tail and the statics behind it), the core,
/// the address space and the interrupted code, then stop: the state is corrupt.
#[cold]
#[inline(never)]
fn sched_state_corrupt(cur: usize, count: usize, pc: u64, rsp: u64) -> ! {
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    // SAFETY: `rsp` is the interrupt frame the stub built (15 GPRs, then RIP, CS).
    let (rip, cs, rflags) =
        unsafe { (*((rsp + 120) as *const u64), *((rsp + 128) as *const u64), *((rsp + 136) as *const u64)) };
    crate::serial_println!(
        "\n[bug032] SCHED.current is no task index: current={cur:#018x} (bytes {:02x?}) count={count} \
         @ {pc:#x} (SCHED.current is @ {:#x}); cpu={} cr3={cr3:#x}; interrupted rip={rip:#x} cs={cs:#x} rflags={rflags:#x}; \
         write watch {} core(s), {} writes checked",
        (cur as u64).to_le_bytes(),
        crate::memwatch::current_field(),
        crate::apic::lapic_id(),
        crate::memwatch::armed_cpus(),
        crate::memwatch::HITS.load(Ordering::Relaxed)
    );
    dump_corrupt_neighbourhood(pc);
    panic!("BUG-032: SCHED.current={cur:#x} (count {count}) — scheduler state overwritten");
}

/// Hex dump of the 32 words around `addr` (an address inside the static scheduler
/// state), for the BUG-032 reports. Reads only: no lock, no allocation.
pub fn dump_corrupt_neighbourhood(addr: u64) {
    let base = (addr & !7).wrapping_sub(16 * 8);
    for row in 0..8u64 {
        let a = base + row * 32;
        // SAFETY: the static scheduler and its neighbours in .data are always mapped.
        let w: [u64; 4] = core::array::from_fn(|k| unsafe { ((a + k as u64 * 8) as *const u64).read_volatile() });
        crate::serial_println!("[bug032]   {a:#018x}: {:016x} {:016x} {:016x} {:016x}{}",
            w[0], w[1], w[2], w[3], if addr >= a && addr < a + 32 { "   <- field" } else { "" });
    }
}

/// Add a **ring-3** task to the round-robin. `kstack_top` is the kernel
/// stack the CPU switches to on an interrupt from ring 3 (TSS.rsp0);
/// there we build the initial ring-3 context so that the first switch jumps
/// there via `iretq`.
/// The index of the currently running task (used by the syscall layer to
/// distinguish a scheduled background task from a synchronous foreground exec).
/// Lock-free mirror of `SCHED.current`, updated at every switch. Readers that
/// may run with the scheduler lock held (capability checks happen on nearly
/// every syscall path) MUST use this: `current()` takes SCHED.lock(), and one
/// caller holding it wedged the whole machine on boot - serial dead, NMI
/// blocked on the same lock, 100% CPU.
static CURRENT_MIRROR: AtomicU64 = AtomicU64::new(0);

/// The running task, without taking any lock.
pub fn current_lockfree() -> usize {
    CURRENT_MIRROR.load(Ordering::Relaxed) as usize
}

pub fn current() -> usize {
    SCHED.lock().current
}

/// The current task's recorded (cr3, kstack) — so a foreground excursion can save
/// them, install its own, and restore them afterwards. Needed when the boot task
/// runs a program that BLOCKS (a threaded glibc process joining its workers): the
/// preemptive switch must resume the task with the right address space + rsp0.
pub fn current_cr3_kstack() -> (u64, u64) {
    let s = SCHED.lock();
    let cur = s.current;
    (s.tasks[cur].cr3, s.tasks[cur].kstack)
}
pub fn set_current_cr3_kstack(cr3: u64, kstack: u64) {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].cr3 = cr3;
    s.tasks[cur].kstack = kstack;
}

/// Number of scheduler tasks (for the live system panel).
pub fn task_count() -> usize {
    SCHED.lock().count
}

/// The scheduler table (BUG-140): slots handed out so far (high-water), slots
/// whose task lives (not Dead), and Dead slots a spawn may take again (on the
/// free list or marked RECLAIMABLE). Dead slots that are neither are held.
pub fn slot_census() -> (usize, usize, usize) {
    let s = SCHED.lock();
    let live = (0..s.count).filter(|&i| s.tasks[i].state != State::Dead).count();
    let reusable = (0..s.count)
        .filter(|&i| s.tasks[i].state == State::Dead
            && (s.free_slots.contains(&i) || RECLAIMABLE[i].load(Ordering::Relaxed)))
        .count();
    (s.count, live, reusable)
}

/// Tasks that are not Dead (the table holds MAX_TASKS).
pub fn live_tasks() -> usize {
    slot_census().1
}

/// Pick a task slot: first reuse a RECLAIMED slot (a finished task whose resources
/// were freed and which has no pending BgProc — see [`reclaim_task`]), otherwise
/// grow the table. Returns None only if the table is full AND nothing is reclaimable.
/// A reused slot is reset to a pristine [`EMPTY_TASK`] so no stale field (pid,
/// stack_bottom, …) leaks from its previous occupant.
fn alloc_slot(s: &mut Scheduler) -> Option<usize> {
    let i = alloc_slot_raw(s)?;
    // Whichever way the slot was found (free list, lazy sweep, a new one), it is
    // taken now: a RECLAIMABLE mark left from its previous occupant must not let
    // the sweep hand it out again while the new task lives or after it dies
    // (BUG-140 marks the slots of ended fork-child tasks).
    RECLAIMABLE[i].store(false, Ordering::Relaxed);
    // The new occupant starts in the launcher's fd space; a spawner that gives
    // it another one does so before the task can run (ring3/fdspace.rs).
    crate::ring3::fd_space_slot_reset(i);
    SPAWN_GEN[i].fetch_add(1, Ordering::Relaxed);
    KSTART[i].store(0, Ordering::Relaxed);
    if let Some(k) = drv_slot(i) {
        DRV_TASK[k].store(0, Ordering::Relaxed);
    }
    Some(i)
}

fn alloc_slot_raw(s: &mut Scheduler) -> Option<usize> {
    while let Some(i) = s.free_slots.pop() {
        // Defensive: only reuse a slot that is genuinely Dead and not the current
        // task (should always hold, since reclaim_task enforces it).
        if i < s.count && i != s.current && s.tasks[i].state == State::Dead {
            s.tasks[i] = EMPTY_TASK;
            return Some(i);
        }
    }
    // Lazy reclaim: a finished worker thread flagged RECLAIMABLE (resources
    // released, registries cleaned) whose slot nobody pushed to free_slots yet,
    // because a task cannot reclaim itself while current.
    for i in 0..s.count {
        if i != s.current && s.tasks[i].state == State::Dead
            && RECLAIMABLE[i].swap(false, Ordering::Relaxed)
        {
            s.tasks[i] = EMPTY_TASK;
            FPU_VALID[i].store(false, Ordering::Relaxed);
            return Some(i);
        }
    }
    if s.count < MAX_TASKS {
        let i = s.count;
        s.count += 1;
        return Some(i);
    }
    None
}

/// Offer a finished task's slot for reuse. Safe ONLY for tasks whose resources are
/// already released (kernel stack, address space) and which have NO associated
/// BgProc/zombie awaiting waitpid — i.e. glibc run_glibc tasks. The task must be
/// Dead and not current. Idempotent (won't double-list a slot).
pub fn reclaim_task(idx: usize) {
    let mut s = SCHED.lock();
    bring_home(&mut s, idx);
    if idx < s.count && idx != s.current && s.tasks[idx].state == State::Dead && !s.free_slots.contains(&idx) {
        s.free_slots.push(idx);
        // A reused slot must start with a clean FPU/SSE state, not the dead task's:
        // invalidate so its first switch-in restores the FNINIT template.
        FPU_VALID[idx].store(false, Ordering::Relaxed);
    }
}

pub fn spawn_user(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64) -> usize {
    spawn_user_in(rip, rsp, cs, ss, kstack_top, cr3, State::Ready)
}

/// BUG-040: the wait channel of a task that exists but must not run yet. No code
/// wakes it but [`start_parked`]; `unblock`/`unblock_any` skip it.
const PARKED: u64 = 0xB040_9A4B_ED00_0001;

/// Like [`spawn_user`], but the task stays parked (never scheduled) until
/// [`start_parked`]. For a launcher that has to publish who the task is (the
/// glibc main task, its caps and kernel-stack owner) before the task's first
/// syscall can look: with `spawn_user` a timer tick in between ran the program
/// against the previous run's globals (BUG-040).
pub fn spawn_user_parked(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64) -> usize {
    spawn_user_in(rip, rsp, cs, ss, kstack_top, cr3, State::Blocked(PARKED))
}

/// Let a task made by [`spawn_user_parked`] run. No-op for any other state.
pub fn start_parked(idx: usize) {
    let mut s = SCHED.lock();
    if idx < s.count && s.tasks[idx].state == State::Blocked(PARKED) {
        s.tasks[idx].state = State::Ready;
        place_woken(&mut s, idx);
    }
}

/// Is `idx` still parked (made by [`spawn_user_parked`], not started)?
pub fn is_parked(idx: usize) -> bool {
    let s = SCHED.lock();
    idx < s.count && s.tasks[idx].state == State::Blocked(PARKED)
}

fn spawn_user_in(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64, initial: State) -> usize {
    let mut s = SCHED.lock();
    // Return a sentinel instead of panicking when the table is full: a program that
    // spawns too many threads (chrome) must get -EAGAIN, never crash the kernel.
    let idx = match alloc_slot(&mut s) {
        Some(i) => i,
        None => return usize::MAX,
    };
    let ctx = kstack_top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    // SAFETY: kstack_top is a valid, exclusive kernel stack for this task.
    unsafe {
        for i in 0..15 {
            words.add(i).write(0);
        }
        words.add(15).write(rip);
        words.add(16).write(cs); // ring-3 code selector (RPL 3)
        words.add(17).write(0x202); // rflags (IF=1)
        words.add(18).write(rsp); // user stack top
        words.add(19).write(ss); // ring-3 data selector (RPL 3)
    }
    s.tasks[idx].rsp = ctx;
    s.tasks[idx].kstack = kstack_top; // own interrupt stack for this task
    // cr3 MUST be set before the task becomes Ready (BUG-007): otherwise a timer-driven
    // preemption can pick this runnable task while its cr3 is still 0, and the scheduler
    // (see the `else boot` fallback in `switch`) would run its ring-3 code on the boot
    // PML4, where the user arena is supervisor-only -> fault/hang. Mirrors spawn_thread.
    s.tasks[idx].cr3 = cr3;
    crate::apuser::on_spawn(idx, spawn_migratable());
    s.tasks[idx].state = initial;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime.max(s.min_vr); // at the pack, never behind it
    s.tasks[idx].nice = 0;
    idx
}

/// Make a THREAD task: resume in ring 3 at `rip`/`rsp` with rax=0 (the child
/// sees clone() return 0), SHARES the address space `cr3` with its process, but
/// has its OWN kernel stack and FS_BASE (TLS). For the `clone` syscall.
#[allow(clippy::too_many_arguments)]
pub fn spawn_thread(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64, fs_base: u64, saved_regs: u64) -> usize {
    let mut s = SCHED.lock();
    // Sentinel (not panic) when full: the clone syscall turns this into -EAGAIN.
    let idx = match alloc_slot(&mut s) {
        Some(i) => i,
        None => return usize::MAX,
    };
    let ctx = kstack_top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    // SAFETY: kstack_top is an exclusive kernel stack for this thread.
    // The child INHERITS the registers of the parent (only rax=0). musl's __clone
    // expects this: in the child it does `call *%r9` with r9 = thread function.
    // The saved register block (syscall_entry) is laid out as:
    //   [0]=r15 [1]=r14 [2]=r13 [3]=r12 [4]=r10 [5]=r9 [6]=r8 [7]=rdx [8]=rsi
    //   [9]=rdi [10]=rbp [11]=rbx [12]=r11 [13]=rcx
    // The context (timer_switch pop order) is r15,r14,r13,r12,r11,r10,r9,r8,
    //   rbp,rdi,rsi,rdx,rcx,rbx,rax.
    let sr = saved_regs as *const u64;
    unsafe {
        words.add(0).write(sr.add(0).read()); // r15
        words.add(1).write(sr.add(1).read()); // r14
        words.add(2).write(sr.add(2).read()); // r13
        words.add(3).write(sr.add(3).read()); // r12
        words.add(4).write(sr.add(12).read()); // r11
        words.add(5).write(sr.add(4).read()); // r10
        words.add(6).write(sr.add(5).read()); // r9  (musl: thread function!)
        words.add(7).write(sr.add(6).read()); // r8
        words.add(8).write(sr.add(10).read()); // rbp
        words.add(9).write(sr.add(9).read()); // rdi
        words.add(10).write(sr.add(8).read()); // rsi
        words.add(11).write(sr.add(7).read()); // rdx
        words.add(12).write(sr.add(13).read()); // rcx
        words.add(13).write(sr.add(11).read()); // rbx
        words.add(14).write(0); // rax = 0 (the child sees clone() return 0)
        words.add(15).write(rip);
        words.add(16).write(cs);
        words.add(17).write(0x202); // IF=1
        words.add(18).write(rsp); // child stack (passed by clone)
        words.add(19).write(ss);
    }
    s.tasks[idx].rsp = ctx;
    s.tasks[idx].kstack = kstack_top;
    s.tasks[idx].cr3 = cr3; // SHARED address space with the process
    s.tasks[idx].fs_base = fs_base; // own TLS
    crate::apuser::on_spawn(idx, spawn_migratable());
    s.tasks[idx].state = State::Ready;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime.max(s.min_vr);
    s.tasks[idx].nice = 0;
    idx
}

/// S3: tasks spawned while this is false never leave the BSP (the musl and
/// EuroOS background paths, drivers, the demo counters). The glibc launchers
/// and clone() set it around their spawn (`MigratableSpawn`).
static SPAWN_MIGRATABLE: AtomicBool = AtomicBool::new(false);
fn spawn_migratable() -> bool {
    SPAWN_MIGRATABLE.load(Ordering::Relaxed)
}
/// Guard: tasks spawned while it lives are glibc tasks that may run on an AP.
pub struct MigratableSpawn(bool);
impl MigratableSpawn {
    pub fn new() -> Self {
        MigratableSpawn(SPAWN_MIGRATABLE.swap(true, Ordering::Relaxed))
    }
}
impl Drop for MigratableSpawn {
    fn drop(&mut self) {
        SPAWN_MIGRATABLE.store(self.0, Ordering::Relaxed);
    }
}

/// S6 self-test: overwrite the stack canary of a task (simulates an overflow)
/// so that the scheduler watchdog detects it on the next switch.
#[doc(hidden)]
pub fn debug_corrupt_stack_canary(task: usize) {
    let s = SCHED.lock();
    if task < s.count && s.tasks[task].stack_bottom != 0 {
        unsafe { (s.tasks[task].stack_bottom as *mut u64).write(0xBADC0DE) };
    }
}

fn busy() {
    for _ in 0..150_000 {
        core::hint::spin_loop();
    }
}

/// Once the S2 priority self-test has reported its counters, the demo tasks
/// task_a/b/c have served their purpose. They are infinite busy-loops, so left
/// running they steal ~half the CPU from real work (the glibc/pthreads/Chromium
/// path crawls behind them). PARK them: they then sleep instead of spinning,
/// freeing the CPU for scheduled user processes. Flipped by task_sleeper.
pub static DEMO_PARK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A parked demo task: sleep in long stretches (yielding the CPU) forever, so the
/// self-test tasks stop competing with real work after they've reported.
fn demo_park_forever() -> ! {
    loop {
        sleep_ticks(1000); // ~10 s asleep per cycle
        x86_64::instructions::hlt(); // yield now; the next tick keeps skipping us
    }
}

// Equal workload (one `busy()`) — the difference in counters now comes PURELY from
// the nice priority (S2), not from different loop durations. After the self-test
// reports, the task parks (see DEMO_PARK) so it stops stealing CPU from real work.
extern "C" fn task_a() -> ! {
    loop {
        if DEMO_PARK.load(Ordering::Relaxed) {
            demo_park_forever();
        }
        TASK_COUNTERS[1].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_b() -> ! {
    loop {
        if DEMO_PARK.load(Ordering::Relaxed) {
            demo_park_forever();
        }
        TASK_COUNTERS[2].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_c() -> ! {
    loop {
        if DEMO_PARK.load(Ordering::Relaxed) {
            demo_park_forever();
        }
        TASK_COUNTERS[3].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}

/// G1 self-test: intentionally overflow its own (GUARDED) kernel stack. Each
/// recursion level consumes ~256 B; after ~60 levels the stack crosses the unmapped
/// guard page → hardware #PF. The page-fault handler (on its OWN IST stack)
/// recognizes the guard address, terminates ONLY this task, and the scheduler/desktop
/// keeps running — proof of recovery from a real kernel-stack overflow.
extern "C" fn task_overflow() -> ! {
    #[inline(never)]
    fn recurse(depth: u64) -> u64 {
        // `black_box` + use-after-recursion prevents tail-call/elimination, so
        // the stack REALLY grows per level (no loop optimization).
        let mut buf = [0u8; 256];
        buf[0] = depth as u8;
        let buf = core::hint::black_box(buf);
        let deeper = recurse(core::hint::black_box(depth).wrapping_add(1));
        core::hint::black_box(buf[0] as u64).wrapping_add(deeper)
    }
    crate::kinfo!("G1 sched-selftest: task 5 intentionally overflows its guarded kernel stack...");
    let _ = core::hint::black_box(recurse(0));
    loop {
        x86_64::instructions::hlt(); // unreachable: the guard #PF terminates this task
    }
}

/// S2 self-test: proves that `sleep_ticks` does REAL timed waiting (the task yields
/// the CPU instead of busy-looping) and reports the priority counters.
extern "C" fn task_sleeper() -> ! {
    let mut woke = 0u64;
    loop {
        woke += 1;
        TASK_COUNTERS[4].fetch_add(1, Ordering::Relaxed);
        let t = crate::interrupts::TICKS.load(Ordering::Relaxed);
        if woke <= 4 {
            crate::kinfo!("S2 sched-selftest: sleeper woke #{woke} @ tick {t} (~0.5s/cycle)");
        }
        if woke == 5 {
            let a = TASK_COUNTERS[1].load(Ordering::Relaxed);
            let b = TASK_COUNTERS[2].load(Ordering::Relaxed);
            let c = TASK_COUNTERS[3].load(Ordering::Relaxed);
            crate::kinfo!("S2 prio-selftest: counters nice(-10/0/+10) = {a}/{b}/{c}, sleeper={}", TASK_COUNTERS[4].load(Ordering::Relaxed));
            // The priority demo has proven its point; stop the busy-loops from
            // stealing CPU so real user processes (glibc/pthreads) get the core.
            DEMO_PARK.store(true, Ordering::Relaxed);
        }
        sleep_ticks(50); // sleep ~0.5 s
        x86_64::instructions::hlt(); // yield CPU; the next tick switches away (Sleeping)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// SMP: per-CPU scheduler for the application processors (Run 1).
//
// Each AP has its OWN run queue + context switch, separate from the global BSP
// scheduler above. This way each core independently runs its own (pinned to that
// core) kernel tasks — real per-CPU scheduling, without touching the proven BSP
// scheduler (desktop/network/ring-3). No cross-CPU lock contention:
// an AP only touches its own `AP_SCHED[cpu]`. Task migration/IPIs = Run 2.
// ════════════════════════════════════════════════════════════════════════════

const MAX_CPU: usize = 8;
const AP_NTASK: usize = 4; // idle + 2 workers + room for 1 balanced task

/// Current core index (LAPIC id, masked onto the per-CPU tables).
pub fn this_cpu() -> usize {
    (crate::apic::lapic_id() & 7) as usize
}

#[derive(Clone, Copy)]
struct ApTask {
    rsp: u64,
}

struct ApSched {
    tasks: [ApTask; AP_NTASK],
    count: usize,
    current: usize,
}

static AP_SCHED: [Mutex<ApSched>; MAX_CPU] =
    [const { Mutex::new(ApSched { tasks: [ApTask { rsp: 0 }; AP_NTASK], count: 0, current: 0 }) }; MAX_CPU];

// Worker stacks per core (slot 0 = idle, uses the AP main stack; 1..2 = workers).
static mut AP_STACKS2: [[[u8; STACK_SIZE]; AP_NTASK]; MAX_CPU] =
    [[[0; STACK_SIZE]; AP_NTASK]; MAX_CPU];

/// Per-core work counters — prove that each AP runs/interleaves its own tasks.
pub static AP_WORK_A: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static AP_WORK_B: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
/// Counter of the dynamically BALANCED task (Run 2: load-balancing).
pub static AP_WORK_C: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];

global_asm!(
    ".global ap_timer_switch",
    "ap_timer_switch:",
    // BUG-032: an interrupt does not clear RFLAGS.DF, and the interrupted code may
    // be inside a backward copy (`std; rep movsq; cld`: compiler_builtins memmove,
    // or ring 3). Rust code assumes DF=0, so the scheduler's own `rep movsq` ran
    // backwards and read a stale stack word as its SCHED guard pointer. The
    // interrupted DF comes back with its RFLAGS on iretq.
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call ap_schedule_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

extern "C" {
    fn ap_timer_switch();
}

/// Address of the AP context-switch stub (for the IDT of the AP timer, vector 0x41).
pub fn ap_stub_addr() -> u64 {
    ap_timer_switch as usize as u64
}

/// Per-CPU round-robin: save the current rsp of this core and return the next
/// task rsp. Only touches `AP_SCHED[cpu]` (single-writer → no contention).
#[no_mangle]
pub extern "sysv64" fn ap_schedule_tick(rsp: u64) -> u64 {
    note_df(rsp);
    crate::apic::eoi();
    let cpu = this_cpu();
    if AP_PARKED[cpu].load(Ordering::Acquire) {
        return crate::apuser::ap_tick(cpu, rsp);
    }
    let mut s = AP_SCHED[cpu].lock();
    if s.count == 0 {
        return rsp;
    }
    let cur = s.current;
    s.tasks[cur].rsp = rsp;
    // J1: once the [smp] self-test has read the demo workers' counters they are
    // parked: drop them from this core's queue and stay on slot 0, the idle task
    // that runs the per-CPU kwork loop (`kwork::ap_idle_loop`). Their stacks are
    // simply abandoned (infinite loops, nothing to unwind). From then on the
    // core sleeps in hlt when it has no work instead of spinning forever.
    if AP_PARK.load(Ordering::Acquire) && s.count > 1 {
        s.count = 1;
        s.current = 0;
        AP_PARKED[cpu].store(true, Ordering::Release);
        return s.tasks[0].rsp;
    }
    let n = (cur + 1) % s.count;
    s.current = n;
    // NOTE (J1): this used to store `n` into CURRENT_MIRROR, the BSP scheduler's
    // lock-free "current task" mirror, so every AP tick overwrote the BSP's
    // current-task index with an AP slot number (0..3). The AP run queue keeps
    // its own `current`; the mirror is BSP-only.
    s.tasks[n].rsp
}

/// J1: park the AP demo workers (see `ap_schedule_tick`).
static AP_PARK: AtomicBool = AtomicBool::new(false);
static AP_PARKED: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];

/// True once AP `cpu` dropped its demo workers and runs only its idle/kwork
/// loop (and, with smpuser, ring-3 tasks through `apuser::ap_tick`).
pub fn ap_parked(cpu: usize) -> bool {
    AP_PARKED[cpu % MAX_CPU].load(Ordering::Acquire)
}

/// Park the demo workers on every listed AP; returns how many cores confirmed
/// (waits up to ~0.5 s). Called by `smp::init` after its counters were reported.
pub fn ap_park_demo_workers(cpus: &[usize]) -> usize {
    AP_PARK.store(true, Ordering::Release);
    for _ in 0..500 {
        if cpus.iter().all(|&c| AP_PARKED[c].load(Ordering::Acquire)) {
            break;
        }
        crate::apic::busy_wait_us(1_000);
    }
    cpus.iter().filter(|&&c| AP_PARKED[c].load(Ordering::Acquire)).count()
}

fn ap_init_stack(cpu: usize, slot: usize, entry: extern "C" fn() -> !) -> u64 {
    let cs = CS::get_reg().0 as u64;
    let ss = SS::get_reg().0 as u64;
    let base = unsafe { core::ptr::addr_of_mut!(AP_STACKS2[cpu][slot]) as *mut u8 };
    let top = (base as u64 + STACK_SIZE as u64) & !0xF;
    let ctx = top - (CONTEXT_WORDS as u64) * 8;
    let words = ctx as *mut u64;
    unsafe {
        for i in 0..15 {
            words.add(i).write(0);
        }
        words.add(15).write(entry as usize as u64); // rip
        words.add(16).write(cs);
        words.add(17).write(0x202); // IF=1
        words.add(18).write(top);
        words.add(19).write(ss);
    }
    ctx
}

/// Set up the per-CPU run queue of this core: idle (slot 0, filled on the first
/// tick = the running ap_main hlt loop) + two worker tasks. Call on the AP
/// itself, after `gdt::init_ap`, before the timer turns on.
pub fn ap_setup(cpu: usize) {
    // J1: the worker stacks of this core identify it (percpu.rs), registered
    // before its timer can switch to them.
    let base = unsafe { core::ptr::addr_of!(AP_STACKS2[cpu]) as u64 };
    crate::percpu::register_stack(base, base + (AP_NTASK * STACK_SIZE) as u64, cpu);
    let mut s = AP_SCHED[cpu].lock();
    s.tasks[0].rsp = 0; // idle — rsp comes at the first preemption
    s.tasks[1].rsp = ap_init_stack(cpu, 1, ap_worker_a);
    s.tasks[2].rsp = ap_init_stack(cpu, 2, ap_worker_b);
    s.count = 3;
    s.current = 0;
}

extern "C" fn ap_worker_a() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_A[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

extern "C" fn ap_worker_b() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_B[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

extern "C" fn ap_worker_c() -> ! {
    let c = this_cpu();
    loop {
        AP_WORK_C[c].fetch_add(1, Ordering::Relaxed);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
    }
}

/// Current run-queue length of core `cpu` (for load-balancing decisions).
pub fn ap_load(cpu: usize) -> usize {
    AP_SCHED[cpu].lock().count
}

/// Load-balancing: dynamically place an extra kernel task on core `cpu`. Safe
/// from the BSP while the AP is running (the per-CPU Mutex serializes with its
/// scheduler tick). The AP picks it up on the next round. False = queue full.
pub fn ap_enqueue_worker(cpu: usize) -> bool {
    let mut s = AP_SCHED[cpu].lock();
    if s.count >= AP_NTASK {
        return false;
    }
    let slot = s.count;
    s.tasks[slot].rsp = ap_init_stack(cpu, slot, ap_worker_c);
    s.count += 1;
    true
}

/// The desktop loop's own stopwatch (W22): how long one pass of the loop takes,
/// by part, and how long the loop then waits for the CPU. The time between the
/// starts of two passes is how long a key, a click or a console line can lie
/// unread; more than 100 ms of it is said on the serial console, with where it
/// went. Only task 0 calls `mark`.
pub mod deskwatch {
    use core::sync::atomic::{AtomicU64, Ordering};

    const PARTS: usize = 6;
    static MARKS: [AtomicU64; PARTS] = [const { AtomicU64::new(0) }; PARTS];
    static READY_AT_END: AtomicU64 = AtomicU64::new(0);
    static MAX_GAP_MS: AtomicU64 = AtomicU64::new(0);
    static OVER: AtomicU64 = AtomicU64::new(0);
    static OVER_TOTAL: AtomicU64 = AtomicU64::new(0);
    static MAX_GAP_TOTAL_MS: AtomicU64 = AtomicU64::new(0);
    pub const LIMIT_MS: u64 = 100;
    /// The longest named step of the pass in progress (see [`step`]): its
    /// length in ms and its name (pointer and length of a `&'static str`).
    static STEP_AT: AtomicU64 = AtomicU64::new(0);
    static LONG_MS: AtomicU64 = AtomicU64::new(0);
    static LONG_PTR: AtomicU64 = AtomicU64::new(0);
    static LONG_LEN: AtomicU64 = AtomicU64::new(0);

    /// The work since the last `step` or `mark` was `name`. A long pass then
    /// names its longest step in its `[desk]` line: "keys and drawing 57241 ms"
    /// (the TCG watchdog trip of chrome-2026-11b) said where in 1100 lines of
    /// the loop, not what.
    pub fn step(name: &'static str) {
        cpu_step(name.as_ptr() as u64, name.len() as u64);
        let t = now();
        let d = ms(STEP_AT.swap(t, Ordering::Relaxed), t);
        if d > LONG_MS.load(Ordering::Relaxed) {
            LONG_MS.store(d, Ordering::Relaxed);
            LONG_PTR.store(name.as_ptr() as u64, Ordering::Relaxed);
            LONG_LEN.store(name.len() as u64, Ordering::Relaxed);
        }
    }

    // Where a long pass spends its time: every timer tick after the pass has
    // run for a second samples who has the CPU. For the desktop loop itself its
    // interrupted rip (and, once, the kernel return addresses on its stack);
    // for anyone else the task number. Printed with the `[desk]` line of a
    // pass over a second (the TCG watchdog trips of W23, task 3).
    const NS: usize = 6;
    static S_RIP: [AtomicU64; NS] = [const { AtomicU64::new(0) }; NS];
    static S_RIPN: [AtomicU64; NS] = [const { AtomicU64::new(0) }; NS];
    static S_TASK: [AtomicU64; NS] = [const { AtomicU64::new(u64::MAX) }; NS];
    static S_TASKN: [AtomicU64; NS] = [const { AtomicU64::new(0) }; NS];
    static S_STACK: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];
    static S_STACK_SET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    /// The state of the desktop loop at the ticks another task had: Ready,
    /// Sleeping, Blocked (and the last channel it was blocked on).
    static S_T0: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
    static S_T0_CHAN: AtomicU64 = AtomicU64::new(0);

    fn bump(keys: &[AtomicU64; NS], counts: &[AtomicU64; NS], key: u64, empty: u64) {
        for i in 0..NS {
            let k = keys[i].load(Ordering::Relaxed);
            if k == key {
                counts[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
            if k == empty {
                keys[i].store(key, Ordering::Relaxed);
                counts[i].store(1, Ordering::Relaxed);
                return;
            }
        }
    }

    /// Timer tick (scheduler lock held: atomics only). `frame` is the address of
    /// the interrupted context of `cur` (15 registers, then the iret frame).
    pub fn tick_sample(cur: usize, frame: u64, t0: super::State) {
        let start = MARKS[0].load(Ordering::Relaxed);
        let end = MARKS[PARTS - 1].load(Ordering::Relaxed);
        if start == 0 || end >= start || ms(start, now()) < 1000 {
            return; // no pass in progress, or not a long one (yet)
        }
        if cur == 0 {
            // SAFETY: the timer entry pushed this frame on the task's kernel stack.
            let (rip, cs, rsp) = unsafe {
                (*((frame + 120) as *const u64), *((frame + 128) as *const u64), *((frame + 144) as *const u64))
            };
            if cs & 3 != 0 {
                return;
            }
            bump(&S_RIP, &S_RIPN, rip, 0);
            if !S_STACK_SET.swap(true, Ordering::Relaxed) {
                // Return addresses into the kernel image above the interrupted rsp
                // (a kernel stack: mapped, ours).
                let mut n = 0;
                let mut p = rsp & !7;
                let stop = p + 0x600;
                while p < stop && n < S_STACK.len() {
                    let v = unsafe { (p as *const u64).read_volatile() };
                    if super::in_kernel_image(v) {
                        S_STACK[n].store(v, Ordering::Relaxed);
                        n += 1;
                    }
                    p += 8;
                }
            }
        } else {
            bump(&S_TASK, &S_TASKN, cur as u64, u64::MAX);
            match t0 {
                super::State::Ready => { S_T0[0].fetch_add(1, Ordering::Relaxed); }
                super::State::Sleeping(_) => { S_T0[1].fetch_add(1, Ordering::Relaxed); }
                super::State::Blocked(c) => {
                    S_T0[2].fetch_add(1, Ordering::Relaxed);
                    S_T0_CHAN.store(c, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    fn samples() -> alloc::string::String {
        let mut s = alloc::string::String::new();
        let mut any = false;
        for i in 0..NS {
            let n = S_RIPN[i].load(Ordering::Relaxed);
            if n > 0 {
                s.push_str(&alloc::format!("{}{:#x} x{n}", if any { ", " } else { "; ticks of the loop at rip " }, S_RIP[i].load(Ordering::Relaxed)));
                any = true;
            }
        }
        let mut other = false;
        for i in 0..NS {
            let n = S_TASKN[i].load(Ordering::Relaxed);
            if n > 0 {
                s.push_str(&alloc::format!("{}t{} x{n}", if other { ", " } else { "; ticks of other tasks: " }, S_TASK[i].load(Ordering::Relaxed)));
                other = true;
            }
        }
        if other {
            s.push_str(&alloc::format!("; the loop meanwhile: Ready x{}, Sleeping x{}, Blocked x{} (chan {:#x})",
                S_T0[0].load(Ordering::Relaxed), S_T0[1].load(Ordering::Relaxed), S_T0[2].load(Ordering::Relaxed),
                S_T0_CHAN.load(Ordering::Relaxed)));
        }
        if S_STACK_SET.load(Ordering::Relaxed) {
            s.push_str(&alloc::format!("; anchor deskwatch::mark @ {:#x}", mark as usize as u64));
            s.push_str("; loop stack:");
            for a in S_STACK.iter() {
                let v = a.load(Ordering::Relaxed);
                if v != 0 {
                    s.push_str(&alloc::format!(" {v:#x}"));
                }
            }
        }
        s
    }

    fn samples_reset() {
        for i in 0..NS {
            S_RIP[i].store(0, Ordering::Relaxed);
            S_RIPN[i].store(0, Ordering::Relaxed);
            S_TASK[i].store(u64::MAX, Ordering::Relaxed);
            S_TASKN[i].store(0, Ordering::Relaxed);
        }
        for a in S_STACK.iter() {
            a.store(0, Ordering::Relaxed);
        }
        for a in S_T0.iter() {
            a.store(0, Ordering::Relaxed);
        }
        S_STACK_SET.store(false, Ordering::Relaxed);
    }

    fn longest_step() -> Option<(&'static str, u64)> {
        let (p, l) = (LONG_PTR.load(Ordering::Relaxed), LONG_LEN.load(Ordering::Relaxed));
        if p == 0 {
            return None;
        }
        // SAFETY: p/l were taken from a `&'static str` by `step`.
        let s = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(p as *const u8, l as usize)) };
        Some((s, LONG_MS.load(Ordering::Relaxed)))
    }

    fn now() -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    fn ms(from: u64, to: u64) -> u64 {
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        to.saturating_sub(from) * 10 / per
    }

    /// Part `i` of the pass begins. 0 is the start of a pass: the previous pass
    /// and the wait after it are judged here. PARTS-1 is the end of the pass.
    // On-CPU time of the desktop loop per part and per named step (11f-A),
    // from its own run time on the TSC: a pass's wall time in `[desk]` also
    // counts the turns of the tasks that preempted it.
    const NSTEP: usize = 40;
    static CPU_PART: [AtomicU64; PARTS] = [const { AtomicU64::new(0) }; PARTS];
    static CPU_PREV: AtomicU64 = AtomicU64::new(PARTS as u64 - 1);
    static CPU_LAST: AtomicU64 = AtomicU64::new(0);
    static CPU_STEP_LAST: AtomicU64 = AtomicU64::new(0);
    static CPU_PASSES: AtomicU64 = AtomicU64::new(0);
    static CPU_SINCE: AtomicU64 = AtomicU64::new(0);
    static STEP_KEY: [AtomicU64; NSTEP] = [const { AtomicU64::new(0) }; NSTEP];
    static STEP_LEN: [AtomicU64; NSTEP] = [const { AtomicU64::new(0) }; NSTEP];
    static STEP_CPU: [AtomicU64; NSTEP] = [const { AtomicU64::new(0) }; NSTEP];

    fn cpu_step(ptr: u64, len: u64) {
        let r = super::run_tsc_now(0);
        let d = r.saturating_sub(CPU_STEP_LAST.swap(r, Ordering::Relaxed));
        for k in 0..NSTEP {
            let key = STEP_KEY[k].load(Ordering::Relaxed);
            if key == ptr || key == 0 {
                if key == 0 {
                    STEP_KEY[k].store(ptr, Ordering::Relaxed);
                    STEP_LEN[k].store(len, Ordering::Relaxed);
                }
                STEP_CPU[k].fetch_add(d, Ordering::Relaxed);
                return;
            }
        }
    }

    fn cpu_mark(i: usize) {
        let r = super::run_tsc_now(0);
        let d = r.saturating_sub(CPU_LAST.swap(r, Ordering::Relaxed));
        let prev = CPU_PREV.swap(i as u64, Ordering::Relaxed) as usize;
        CPU_PART[prev.min(PARTS - 1)].fetch_add(d, Ordering::Relaxed);
        CPU_STEP_LAST.store(r, Ordering::Relaxed);
        if i == 0 {
            CPU_PASSES.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Forget the on-CPU ledger (with `keylat reset`).
    pub fn cpu_reset() {
        for a in CPU_PART.iter().chain(STEP_CPU.iter()) {
            a.store(0, Ordering::Relaxed);
        }
        CPU_PASSES.store(0, Ordering::Relaxed);
        CPU_SINCE.store(now(), Ordering::Relaxed);
    }

    /// The on-CPU ledger of the desktop loop since `cpu_reset`.
    pub fn cpu_lines() -> alloc::vec::Vec<alloc::string::String> {
        let since = CPU_SINCE.load(Ordering::Relaxed);
        let wall = if since == 0 { 0 } else { ms(since, now()) };
        let passes = CPU_PASSES.load(Ordering::Relaxed).max(1);
        let p: [u64; PARTS] = core::array::from_fn(|k| ms(0, CPU_PART[k].load(Ordering::Relaxed)));
        let total: u64 = p.iter().sum();
        let mut out = alloc::vec![alloc::format!(
            "[desk-cpu] desktop loop on the CPU {total} ms of {wall} ms in {passes} passes ({} us a pass): network and DevTools {}, devices and windows {}, menus {}, keys and drawing {}, hosted window and console {}, between passes {}",
            total * 1000 / passes, p[0], p[1], p[2], p[3], p[4], p[5])];
        let mut v: alloc::vec::Vec<(u64, usize)> = (0..NSTEP)
            .filter(|&k| STEP_KEY[k].load(Ordering::Relaxed) != 0)
            .map(|k| (STEP_CPU[k].load(Ordering::Relaxed), k))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        let mut l = alloc::string::String::from("[desk-cpu] steps:");
        for (c, k) in v.iter().take(8) {
            // SAFETY: key and length were taken from a static str by `step`.
            let name = unsafe {
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                    STEP_KEY[*k].load(Ordering::Relaxed) as *const u8, STEP_LEN[*k].load(Ordering::Relaxed) as usize))
            };
            l.push_str(&alloc::format!(" {name} {} ms;", ms(0, *c)));
        }
        out.push(l);
        out
    }

    pub fn mark(i: usize) {
        cpu_mark(i);
        let t = now();
        if i == 0 {
            let m: [u64; PARTS] = core::array::from_fn(|k| MARKS[k].load(Ordering::Relaxed));
            if m[0] != 0 && m[PARTS - 1] >= m[0] {
                let gap = ms(m[0], t);
                MAX_GAP_MS.fetch_max(gap, Ordering::Relaxed);
                MAX_GAP_TOTAL_MS.fetch_max(gap, Ordering::Relaxed);
                if gap > LIMIT_MS {
                    OVER.fetch_add(1, Ordering::Relaxed);
                    let n = OVER_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
                    if n <= 60 || n % 50 == 0 {
                        let mut step = match longest_step() {
                            Some((name, d)) => alloc::format!("; longest step: {name} {d} ms"),
                            None => alloc::string::String::new(),
                        };
                        if gap > 1000 {
                            step.push_str(&samples());
                        }
                        crate::serial_println!(
                            "[desk] #{n} the desktop loop was away {gap} ms: its pass took {} ms (network and DevTools {}, devices and windows {}, menus {}, keys and drawing {}, hosted window and console {}), then {} ms without the CPU ({} other tasks were ready){step}",
                            ms(m[0], m[PARTS - 1]), ms(m[0], m[1]), ms(m[1], m[2]), ms(m[2], m[3]), ms(m[3], m[4]),
                            ms(m[4], m[PARTS - 1]), ms(m[PARTS - 1], t), READY_AT_END.load(Ordering::Relaxed));
                    }
                }
            }
        }
        if i < PARTS {
            MARKS[i].store(t, Ordering::Relaxed);
        }
        STEP_AT.store(t, Ordering::Relaxed);
        if i == 0 {
            LONG_MS.store(0, Ordering::Relaxed);
            LONG_PTR.store(0, Ordering::Relaxed);
            samples_reset();
        }
        if i == PARTS - 1 {
            let ready = match super::SCHED.try_lock() {
                Some(s) => (1..s.count).filter(|&k| s.tasks[k].state == super::State::Ready).count() as u64,
                None => 0,
            };
            READY_AT_END.store(ready, Ordering::Relaxed);
        }
    }

    /// (longest gap in ms, gaps over the limit) since the last call, and the
    /// same two over the whole run.
    pub fn take() -> (u64, u64, u64, u64) {
        (MAX_GAP_MS.swap(0, Ordering::Relaxed), OVER.swap(0, Ordering::Relaxed),
         MAX_GAP_TOTAL_MS.load(Ordering::Relaxed), OVER_TOTAL.load(Ordering::Relaxed))
    }
}
