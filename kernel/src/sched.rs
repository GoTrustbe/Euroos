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
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use x86_64::instructions::segmentation::{Segment, CS, SS};
use x86_64::registers::model_specific::Msr;

const IA32_FS_BASE: u32 = 0xC000_0100;

const MAX_TASKS: usize = 48;
const STACK_SIZE: usize = 16 * 1024;
const CONTEXT_WORDS: usize = 20; // 15 GP registers + 5 (rip,cs,rflags,rsp,ss)

/// Per-task counter (index 1..3 for the kernel background tasks).
pub static TASK_COUNTERS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Task state (S2 scheduler maturity). Replaces the separate `dead`/`blocked`
/// flags with a full state machine — the basis for blocking I/O,
/// nanosleep and (S3) fork/wait/zombie reaping.
#[derive(Clone, Copy, PartialEq, Eq)]
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
}

/// Stack-guard canary (S6 memory hardening). Unlikely value at the bottom of each
/// kernel stack; if it changes, the stack has overflowed.
const STACK_CANARY: u64 = 0x5350_524F_5547_4421; // "SPROUGD!" — recognizable in dumps

/// vruntime step per turn, weighted on nice (-20..19 -> 64..2560). Equal nice =
/// equal step = fair round-robin (behavior-preserving relative to the old scheduler).
fn vstep(nice: i8) -> u64 {
    ((nice as i64) + 21) as u64 * 64
}

/// Block the CURRENT task on the generic futex wait channel (FUTEX_WAIT).
pub fn block_current() {
    block_on(0);
}

/// Block the CURRENT task on wait channel `chan` (an address/token). The scheduler
/// skips it until [`wake`]`(chan, ..)`. Basis for futex, pipes, waitpid.
pub fn block_on(chan: u64) {
    let mut s = SCHED.lock();
    let cur = s.current;
    s.tasks[cur].state = State::Blocked(chan);
}

/// Unblock task `idx` (FUTEX_WAKE on a specific task).
pub fn unblock(idx: usize) {
    let mut s = SCHED.lock();
    if idx < s.count && matches!(s.tasks[idx].state, State::Blocked(_)) {
        s.tasks[idx].state = State::Ready;
    }
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
            woken += 1;
        }
    }
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
    if idx < s.count {
        s.tasks[idx].state = State::Dead;
    }
}

/// The shared boot PML4 (kernel address space). Set by main after `paging::init`.
static BOOT_PML4: AtomicU64 = AtomicU64::new(0);

pub fn set_boot_pml4(p: u64) {
    BOOT_PML4.store(p, Ordering::Relaxed);
}

pub fn boot_pml4() -> u64 {
    BOOT_PML4.load(Ordering::Relaxed)
}


struct Scheduler {
    tasks: [Task; MAX_TASKS],
    count: usize,
    current: usize,
}

static SCHED: Mutex<Scheduler> = Mutex::new(Scheduler {
    tasks: [const {
        Task { rsp: 0, kstack: 0, fs_base: 0, cr3: 0, state: State::Ready, nice: 0, vruntime: 0, pid: 0, ppid: 0, stack_bottom: 0 }
    }; MAX_TASKS],
    count: 1,
    current: 0,
});

// Stacks for the background tasks (task 0 uses the existing kernel stack).
static mut STACKS: [[u8; STACK_SIZE]; MAX_TASKS] = [[0; STACK_SIZE]; MAX_TASKS];

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

extern "C" {
    fn timer_switch();
}

pub fn stub_addr() -> u64 {
    timer_switch as usize as u64
}

/// Called by the assembly stub: save the current rsp, pick the next
/// task (round-robin) and return its rsp.
///
/// NOTE: explicit `sysv64` — the UEFI target turns `extern "C"` into the Win64
/// ABI (argument in RCX), but our stub provides the argument in RDI (SysV).
/// BUG-007 diagnostic: timer ticks that found SCHED already held (in task context) and
/// were safely skipped instead of deadlocking. Nonzero ⇒ the deadlock window was hit.
pub static SCHED_SKIPS: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub extern "sysv64" fn schedule_tick(rsp: u64) -> u64 {
    crate::interrupts::TICKS.fetch_add(1, Ordering::Relaxed);
    crate::interrupts::send_timer_eoi();
    // BUG-007: the timer must NEVER block on SCHED. Task-context code (the desktop loop,
    // syscalls, supervise/reap) holds SCHED.lock() with interrupts ENABLED; a blocking
    // acquire here would deadlock the core — this handler spins with interrupts off while
    // the lock holder, the very task we just preempted, can never run to release it
    // (total silence, no fault). So TRY the lock and, if it's held, skip this preemption
    // tick: the holder frees it within microseconds and the next tick schedules normally.
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
    s.tasks[cur].rsp = rsp;
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
    // 1. Wake sleepers: Sleeping -> Ready as soon as their wake time is reached.
    let now = crate::interrupts::TICKS.load(Ordering::Relaxed);
    for i in 0..s.count {
        if let State::Sleeping(w) = s.tasks[i].state {
            if now >= w {
                s.tasks[i].state = State::Ready;
            }
        }
    }
    // 2. Let the outgoing task (if it's still runnable) climb its vruntime,
    //    weighted on nice. Higher nice -> larger step -> chosen less often.
    if s.tasks[cur].state == State::Ready {
        let step = vstep(s.tasks[cur].nice);
        s.tasks[cur].vruntime = s.tasks[cur].vruntime.wrapping_add(step);
    }
    // 3. Mini-CFS: pick the runnable task with the SMALLEST vruntime. Start the scan at
    //    cur+1 with strict '<', so that equal-vruntime tasks rotate round-robin
    //    (behavior-preserving with equal nice). Task 0 is always Ready -> fallback.
    let mut best = cur;
    let mut bestv = u64::MAX;
    let mut found = false;
    for k in 1..=s.count {
        let i = (cur + k) % s.count;
        if s.tasks[i].state == State::Ready && s.tasks[i].vruntime < bestv {
            bestv = s.tasks[i].vruntime;
            best = i;
            found = true;
        }
    }
    if !found {
        best = if s.tasks[cur].state == State::Ready { cur } else { 0 };
    }
    s.current = best;
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
    let mut s = SCHED.lock();
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
    s.current = 0;
}

/// Add a **ring-3** task to the round-robin. `kstack_top` is the kernel
/// stack the CPU switches to on an interrupt from ring 3 (TSS.rsp0);
/// there we build the initial ring-3 context so that the first switch jumps
/// there via `iretq`.
/// The index of the currently running task (used by the syscall layer to
/// distinguish a scheduled background task from a synchronous foreground exec).
pub fn current() -> usize {
    SCHED.lock().current
}

/// Number of scheduler tasks (for the live system panel).
pub fn task_count() -> usize {
    SCHED.lock().count
}

/// Pick a free task slot (grows the table). Returns None if the table is full.
/// NB: no reuse of DEAD slots yet — that requires the associated BgProc
/// to already be reaped (otherwise a new process shares the slot with a zombie). With
/// MAX_TASKS=48 there is plenty for the current workload; slot recycling = later.
fn alloc_slot(s: &mut Scheduler) -> Option<usize> {
    if s.count < MAX_TASKS {
        let i = s.count;
        s.count += 1;
        return Some(i);
    }
    None
}

pub fn spawn_user(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64) -> usize {
    let mut s = SCHED.lock();
    let idx = alloc_slot(&mut s).expect("scheduler task table full");
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
    s.tasks[idx].state = State::Ready;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime; // start fairly at equal level
    s.tasks[idx].nice = 0;
    idx
}

/// Make a THREAD task: resume in ring 3 at `rip`/`rsp` with rax=0 (the child
/// sees clone() return 0), SHARES the address space `cr3` with its process, but
/// has its OWN kernel stack and FS_BASE (TLS). For the `clone` syscall.
#[allow(clippy::too_many_arguments)]
pub fn spawn_thread(rip: u64, rsp: u64, cs: u64, ss: u64, kstack_top: u64, cr3: u64, fs_base: u64, saved_regs: u64) -> usize {
    let mut s = SCHED.lock();
    let idx = alloc_slot(&mut s).expect("scheduler task table full");
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
    s.tasks[idx].state = State::Ready;
    s.tasks[idx].vruntime = s.tasks[s.current].vruntime;
    s.tasks[idx].nice = 0;
    idx
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

// Equal workload (one `busy()`) — the difference in counters now comes PURELY from
// the nice priority (S2), not from different loop durations.
extern "C" fn task_a() -> ! {
    loop {
        TASK_COUNTERS[1].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_b() -> ! {
    loop {
        TASK_COUNTERS[2].fetch_add(1, Ordering::Relaxed);
        busy();
    }
}
extern "C" fn task_c() -> ! {
    loop {
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
    crate::apic::eoi();
    let cpu = this_cpu();
    let mut s = AP_SCHED[cpu].lock();
    if s.count == 0 {
        return rsp;
    }
    let cur = s.current;
    s.tasks[cur].rsp = rsp;
    let n = (cur + 1) % s.count;
    s.current = n;
    s.tasks[n].rsp
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
