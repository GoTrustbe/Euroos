//! Ring 3 on the application processors: steps S3 to S6 of
//! docs/design/SMP-SCHEDULER.md (asymmetric SMP: the kernel stays on the BSP,
//! user mode runs on every CPU).
//!
//! A ring-3 task whose saved context is *user mode* may run on an AP. Whenever
//! it needs the kernel (a system call, a page fault, any exception) the AP saves
//! its user context and sends the task back to the BSP, which re-executes the
//! same instruction on the unchanged path: the Linux ABI layer, the swapped
//! globals, the drivers and every interrupt handler keep their invariant "kernel
//! code runs on one CPU". The APs run with `EFER.SCE = 0`, so `syscall` in ring 3
//! raises #UD with rip at the instruction; that is bounced like any fault.
//!
//! Ownership (design 3.1): while `RUNCTX[i].cpu == k` (an AP), CPU k owns the
//! task's saved frame, `fs_base`, FPU area and AP run time; the BSP sees the task
//! `Ready`, never picks it and touches none of them. An AP never changes
//! `Task.state` and never takes the scheduler lock: its only way back is
//! `INBOX[k]` plus a reschedule IPI to the BSP. The per-AP run queue is a bitmap
//! of task slots (atomics, no lock): whoever clears a task's bit owns it.
//!
//! The AP keeps its frames on its own entry stack (`AP_TSS[k].rsp0`, a stack
//! registered with `percpu`, so `percpu::cpu()` and the `[lk]` order contexts stay
//! right); a task's kernel stack holds only its saved 20-word frame at the top.
//!
//! Modes (`smpuser=off|test|on`, boot option and shell command): `off` places
//! nothing (the APs behave as before), `test` places only tasks pinned by a test
//! (`[ap-user]`, `[tlb-smp]`, `[kill-smp]`, `[cow-smp]`, `[spread]`), `on` places
//! every migratable glibc task (S5).

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::sched::MAX_TASKS;

pub const MAX_CPU: usize = 8;
const W: usize = MAX_TASKS / 64;
/// `RunCtx.cpu` of a task the BSP owns.
pub const BSP: u8 = 0xFF;
const IDLE: u16 = 0xFFFF;
const FRAME_WORDS: usize = 20;
const IA32_FS_BASE: u32 = 0xC000_0100;

pub const OFF: u8 = 0;
pub const TEST: u8 = 1;
pub const ON: u8 = 2;
static MODE: AtomicU8 = AtomicU8::new(OFF);

pub fn mode() -> u8 {
    MODE.load(Ordering::Relaxed)
}
pub fn mode_name(m: u8) -> &'static str {
    match m {
        TEST => "test",
        ON => "on",
        _ => "off",
    }
}
pub fn parse_mode(v: &str) -> Option<u8> {
    match v.trim() {
        "off" => Some(OFF),
        "test" => Some(TEST),
        "on" => Some(ON),
        _ => None,
    }
}
/// Set the mode. Going to `off` sends every task home at its next AP tick.
pub fn set_mode(m: u8) {
    MODE.store(m, Ordering::Relaxed);
}

/// Task may run on an AP at all (a glibc process or thread with its own address
/// space and kernel stack; not task 0, kernel tasks, the musl background path).
pub const F_MIGRATABLE: u32 = 1;
/// Bounced (or kicked): resume on the BSP once, then it may leave again.
pub const F_NEEDS_BSP: u32 = 2;
/// The BSP wants the task back (S4: kill, exit, exec, fork, signal, munmap).
pub const F_EVICT: u32 = 4;
/// Stay on the BSP (a task that bounces more than it computes, S5).
pub const F_STAY: u32 = 8;

pub struct RunCtx {
    pub rsp: AtomicU64,
    pub fs_base: AtomicU64,
    pub cr3: AtomicU64,
    pub kstack: AtomicU64,
    /// BSP, or the AP that owns the task (queued or running).
    pub cpu: AtomicU8,
    /// `test` mode: the AP the task is pinned to (BSP = none).
    pub pin: AtomicU8,
    pub last: AtomicU8,
    pub flags: AtomicU32,
    /// AP run time (TSC) not yet charged to the task's vruntime.
    pub ap_tsc: AtomicU64,
    pub bounces: AtomicU64,
    /// AP user time (TSC) since the task last came to the BSP (S5: bounce rate).
    pub run_since: AtomicU64,
}
const RC0: RunCtx = RunCtx {
    rsp: AtomicU64::new(0),
    fs_base: AtomicU64::new(0),
    cr3: AtomicU64::new(0),
    kstack: AtomicU64::new(0),
    cpu: AtomicU8::new(BSP),
    pin: AtomicU8::new(BSP),
    last: AtomicU8::new(BSP),
    flags: AtomicU32::new(0),
    ap_tsc: AtomicU64::new(0),
    bounces: AtomicU64::new(0),
    run_since: AtomicU64::new(0),
};
pub static RUNCTX: [RunCtx; MAX_TASKS] = [RC0; MAX_TASKS];

static READY: [[AtomicU64; W]; MAX_CPU] = [const { [const { AtomicU64::new(0) }; W] }; MAX_CPU];
pub static NR: [AtomicU32; MAX_CPU] = [const { AtomicU32::new(0) }; MAX_CPU];
pub static CUR_ON: [AtomicU16; MAX_CPU] = [const { AtomicU16::new(IDLE) }; MAX_CPU];
pub static CUR_CR3: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static IDLE_RSP: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static SLICE_AT: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static RR: [AtomicU16; MAX_CPU] = [const { AtomicU16::new(0) }; MAX_CPU];
static USER_OK: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];
static APIC_OF: [AtomicU8; MAX_CPU] = [const { AtomicU8::new(0) }; MAX_CPU];
static INBOX: [[AtomicU64; W]; MAX_CPU] = [const { [const { AtomicU64::new(0) }; W] }; MAX_CPU];
static INBOX_ANY: AtomicBool = AtomicBool::new(false);

// Per-AP counters (`smpstat`, the step markers).
pub static ST_IN: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_BOUNCE_SC: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_BOUNCE_PF: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_BOUNCE_OTHER: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_EVICT: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_USER_TSC: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_TICKS: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_IDLE_TICKS: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static ST_OFFLOADS: AtomicU64 = AtomicU64::new(0);
pub static ST_DRAINED: AtomicU64 = AtomicU64::new(0);

#[inline]
fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The Local APIC id of AP `cpu`.
pub fn apic_of(cpu: usize) -> u8 {
    APIC_OF[cpu % MAX_CPU].load(Ordering::Relaxed)
}

/// APs that can run ring 3 (their `ap_init` ran).
pub fn user_aps() -> alloc::vec::Vec<usize> {
    (0..MAX_CPU).filter(|&c| USER_OK[c].load(Ordering::Acquire)).collect()
}

// ── Task life cycle (BSP, scheduler lock held by the caller) ─────────────────

/// A task slot got a new task: forget everything about the previous one.
pub fn on_spawn(idx: usize, migratable: bool) {
    let rc = &RUNCTX[idx];
    rc.cpu.store(BSP, Ordering::Relaxed);
    rc.flags.store(if migratable { F_MIGRATABLE } else { 0 }, Ordering::Relaxed);
    rc.ap_tsc.store(0, Ordering::Relaxed);
    rc.bounces.store(0, Ordering::Relaxed);
    rc.run_since.store(0, Ordering::Relaxed);
    rc.last.store(BSP, Ordering::Relaxed);
    let pin = PIN_PLAN.take_one(migratable);
    rc.pin.store(pin, Ordering::Relaxed);
}

/// Is the task the BSP's (not queued on or running on an AP)?
#[inline]
pub fn on_bsp(idx: usize) -> bool {
    RUNCTX[idx].cpu.load(Ordering::Acquire) == BSP
}

/// The AP a `Ready` task in user context should go to, or None (stay).
/// `user_frame`: the task's saved frame has a ring-3 CS.
pub fn target(idx: usize, user_frame: bool, cr3: u64) -> Option<usize> {
    let m = mode();
    if m == OFF || !user_frame || held(cr3) {
        return None;
    }
    let rc = &RUNCTX[idx];
    let f = rc.flags.load(Ordering::Relaxed);
    if f & F_MIGRATABLE == 0 || f & (F_NEEDS_BSP | F_STAY) != 0 {
        return None;
    }
    let pin = rc.pin.load(Ordering::Relaxed);
    if pin != BSP {
        let p = pin as usize;
        return (p < MAX_CPU && USER_OK[p].load(Ordering::Relaxed)).then_some(p);
    }
    // S5 places the tasks nobody pinned (smpuser=on).
    None
}

/// Hand a task to AP `cpu` (BSP, under the scheduler lock, after the task's FPU
/// state was saved). From here on the AP owns it. The BSP may still be on the
/// task's kernel stack (the outgoing task), but below the saved frame, and never
/// reads that frame again; the AP reads and writes only those 20 words.
pub fn offload(idx: usize, cpu: usize, rsp: u64, fs_base: u64, cr3: u64, kstack: u64) {
    let rc = &RUNCTX[idx];
    debug_assert!(rsp == kstack - (FRAME_WORDS as u64) * 8);
    rc.rsp.store(rsp, Ordering::Relaxed);
    rc.fs_base.store(fs_base, Ordering::Relaxed);
    rc.cr3.store(cr3, Ordering::Relaxed);
    rc.kstack.store(kstack, Ordering::Relaxed);
    rc.cpu.store(cpu as u8, Ordering::Release);
    READY[cpu][idx / 64].fetch_or(1 << (idx % 64), Ordering::SeqCst);
    NR[cpu].fetch_add(1, Ordering::SeqCst);
    ST_OFFLOADS.fetch_add(1, Ordering::Relaxed);
    if CUR_ON[cpu].load(Ordering::SeqCst) == IDLE {
        send_resched(cpu);
    }
}

fn send_resched(cpu: usize) {
    crate::apic::send_ipi(APIC_OF[cpu].load(Ordering::Relaxed), crate::interrupts::IPI_RESCHED_VECTOR);
}

/// Did an AP send tasks home? (cheap check before `drain_inbox`)
#[inline]
pub fn inbox_pending() -> bool {
    INBOX_ANY.load(Ordering::Acquire)
}

/// Take every task the APs sent home (BSP, scheduler lock held): `f(idx, rsp,
/// fs_base, ap_tsc)` restores them into the task table.
pub fn drain_inbox(mut f: impl FnMut(usize, u64, u64, u64)) {
    if !INBOX_ANY.swap(false, Ordering::AcqRel) {
        return;
    }
    for c in 0..MAX_CPU {
        for w in 0..W {
            let mut v = INBOX[c][w].swap(0, Ordering::AcqRel);
            while v != 0 {
                let b = v.trailing_zeros() as usize;
                v &= v - 1;
                let idx = w * 64 + b;
                let rc = &RUNCTX[idx];
                let tsc = rc.ap_tsc.swap(0, Ordering::Relaxed);
                f(idx, rc.rsp.load(Ordering::Relaxed), rc.fs_base.load(Ordering::Relaxed), tsc);
                rc.flags.fetch_and(!F_EVICT, Ordering::Relaxed);
                rc.cpu.store(BSP, Ordering::Release);
                ST_DRAINED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// The BSP picked the task: a bounce is served once, after that it may leave.
#[inline]
pub fn picked_on_bsp(idx: usize) {
    let rc = &RUNCTX[idx];
    if rc.flags.load(Ordering::Relaxed) & F_NEEDS_BSP != 0 {
        rc.flags.fetch_and(!F_NEEDS_BSP, Ordering::Relaxed);
    }
}

/// System-call epilogue on the BSP: should the caller leave for an AP at its
/// first user instruction (a self reschedule IPI)?
pub fn wants_ap(idx: usize) -> bool {
    if mode() == OFF || idx >= MAX_TASKS {
        return false;
    }
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    target(idx, true, cr3).is_some()
}

// ── Test pinning ─────────────────────────────────────────────────────────────

/// The migratable tasks spawned while a plan is armed are pinned to its APs in
/// turn (the step tests).
struct PinPlan {
    cpus: [AtomicU8; 16],
    n: AtomicU8,
    next: AtomicU32,
    armed: AtomicBool,
    /// After the last entry, start again at entry 1 (entry 0 is the first task only).
    cycle: AtomicBool,
}
static PIN_PLAN: PinPlan = PinPlan {
    cpus: [const { AtomicU8::new(BSP) }; 16],
    n: AtomicU8::new(0),
    next: AtomicU32::new(0),
    armed: AtomicBool::new(false),
    cycle: AtomicBool::new(false),
};
impl PinPlan {
    fn take_one(&self, migratable: bool) -> u8 {
        if !migratable || !self.armed.load(Ordering::Acquire) {
            return BSP;
        }
        let n = self.n.load(Ordering::Relaxed) as u32;
        if n == 0 {
            return BSP;
        }
        let k = self.next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            if !self.cycle.load(Ordering::Relaxed) || n < 2 {
                return BSP;
            }
            return self.cpus[(1 + (k - 1) % (n - 1)) as usize].load(Ordering::Relaxed);
        }
        self.cpus[k as usize].load(Ordering::Relaxed)
    }
}
/// Pin the next migratable tasks spawned to `cpus`, one each, in order (BSP:
/// that one stays on the BSP; empty: stop). Tasks after the last entry stay.
pub fn pin_plan(cpus: &[usize]) {
    pin_plan_ex(cpus, false);
}
/// The same, and every further migratable task goes to entries 1.. in turn.
pub fn pin_cycle(cpus: &[usize]) {
    pin_plan_ex(cpus, true);
}
fn pin_plan_ex(cpus: &[usize], cycle: bool) {
    PIN_PLAN.armed.store(false, Ordering::Release);
    PIN_PLAN.cycle.store(cycle, Ordering::Relaxed);
    for (i, &c) in cpus.iter().take(16).enumerate() {
        PIN_PLAN.cpus[i].store(if c >= MAX_CPU { BSP } else { c as u8 }, Ordering::Relaxed);
    }
    PIN_PLAN.n.store(cpus.len().min(16) as u8, Ordering::Relaxed);
    PIN_PLAN.next.store(0, Ordering::Relaxed);
    PIN_PLAN.armed.store(!cpus.is_empty(), Ordering::Release);
}

// ── AP side ──────────────────────────────────────────────────────────────────

const WHY_TICK: u8 = 0;
const WHY_RESCHED: u8 = 1;
const WHY_PING: u8 = 2;

unsafe fn frame_word(rsp: u64, i: usize) -> u64 {
    ((rsp + i as u64 * 8) as *const u64).read_volatile()
}

/// Take the next queued task of `cpu` (round robin over the slots).
fn take_next(cpu: usize) -> Option<usize> {
    if NR[cpu].load(Ordering::SeqCst) == 0 {
        return None;
    }
    let start = RR[cpu].load(Ordering::Relaxed) as usize % MAX_TASKS;
    let (w0, b0) = (start / 64, start % 64);
    for k in 0..=W {
        let w = (w0 + k) % W;
        let mask = if k == 0 {
            !0u64 << b0
        } else if k == W {
            if b0 == 0 { 0 } else { (1u64 << b0) - 1 }
        } else {
            !0u64
        };
        loop {
            let v = READY[cpu][w].load(Ordering::SeqCst) & mask;
            if v == 0 {
                break;
            }
            let b = v.trailing_zeros() as usize;
            let bit = 1u64 << b;
            if READY[cpu][w].fetch_and(!bit, Ordering::SeqCst) & bit != 0 {
                NR[cpu].fetch_sub(1, Ordering::SeqCst);
                let idx = w * 64 + b;
                RR[cpu].store(((idx + 1) % MAX_TASKS) as u16, Ordering::Relaxed);
                return Some(idx);
            }
        }
    }
    None
}

fn requeue(cpu: usize, idx: usize) {
    READY[cpu][idx / 64].fetch_or(1 << (idx % 64), Ordering::SeqCst);
    NR[cpu].fetch_add(1, Ordering::SeqCst);
}

/// Run task `idx` on this AP: its address space, TLS and FPU state; returns the
/// frame to `iretq` from (on the task's kernel stack).
fn switch_in(cpu: usize, idx: usize, now: u64) -> u64 {
    let rc = &RUNCTX[idx];
    CUR_ON[cpu].store(idx as u16, Ordering::SeqCst);
    let cr3 = rc.cr3.load(Ordering::Relaxed);
    CUR_CR3[cpu].store(cr3, Ordering::SeqCst);
    // Always written, also when it is the address space this AP had: a CR3 load
    // drops every non-global TLB entry (no PCID), and the BSP may have changed
    // the tables while the task was away.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
        x86_64::registers::model_specific::Msr::new(IA32_FS_BASE).write(rc.fs_base.load(Ordering::Relaxed));
        crate::sched::fpu_restore_task(idx);
    }
    SLICE_AT[cpu].store(now, Ordering::Relaxed);
    rc.last.store(cpu as u8, Ordering::Relaxed);
    ST_IN[cpu].fetch_add(1, Ordering::Relaxed);
    rc.rsp.load(Ordering::Relaxed)
}

/// Save the user context of task `idx` (its frame at `rsp` on this AP's entry
/// or IST stack) into the task: the frame to the top of its kernel stack, TLS,
/// FPU, run time.
fn save_out(cpu: usize, idx: usize, rsp: u64, now: u64) {
    let rc = &RUNCTX[idx];
    let dst = rc.kstack.load(Ordering::Relaxed) - (FRAME_WORDS as u64) * 8;
    if dst != rsp {
        unsafe { core::ptr::copy_nonoverlapping(rsp as *const u64, dst as *mut u64, FRAME_WORDS) };
    }
    rc.rsp.store(dst, Ordering::Relaxed);
    unsafe {
        rc.fs_base.store(x86_64::registers::model_specific::Msr::new(IA32_FS_BASE).read(), Ordering::Relaxed);
        crate::sched::fpu_save_task(idx);
    }
    let d = now.saturating_sub(SLICE_AT[cpu].load(Ordering::Relaxed));
    rc.ap_tsc.fetch_add(d, Ordering::Relaxed);
    rc.run_since.fetch_add(d, Ordering::Relaxed);
    crate::sched::RUN_TSC[idx].fetch_add(d, Ordering::Relaxed);
    ST_USER_TSC[cpu].fetch_add(d, Ordering::Relaxed);
}

/// Send task `idx` (already saved) home to the BSP.
fn to_inbox(cpu: usize, idx: usize) {
    let rc = &RUNCTX[idx];
    rc.flags.fetch_or(F_NEEDS_BSP, Ordering::Relaxed);
    INBOX[cpu][idx / 64].fetch_or(1 << (idx % 64), Ordering::Release);
    INBOX_ANY.store(true, Ordering::Release);
    crate::sched::resched_ipi();
}

/// Leave user mode for this AP's own kernel context (the idle loop, kwork).
fn to_idle(cpu: usize) -> u64 {
    CUR_ON[cpu].store(IDLE, Ordering::SeqCst);
    // Never sit in a process's address space while idle: the BSP may free it.
    let boot = crate::sched::boot_pml4();
    if boot != 0 {
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags)) };
    }
    CUR_CR3[cpu].store(0, Ordering::SeqCst);
    IDLE_RSP[cpu].load(Ordering::Relaxed)
}

/// The next context of this AP after its user task left: a queued task, else idle.
fn next_or_idle(cpu: usize, now: u64) -> u64 {
    if !crate::kwork::ap_pending(cpu) {
        if let Some(n) = take_next(cpu) {
            return switch_in(cpu, n, now);
        }
    }
    let r = to_idle(cpu);
    // A task offloaded between the look above and CUR_ON = IDLE saw a busy AP
    // and sent no IPI: look once more (SeqCst on both sides).
    if !crate::kwork::ap_pending(cpu) {
        if let Some(n) = take_next(cpu) {
            return switch_in(cpu, n, now);
        }
    }
    r
}

/// The scheduling decision of an AP (tick, reschedule IPI, kwork ping). `rsp`:
/// the 20-word frame the stub saved. Returns the frame to resume.
pub fn ap_switch(cpu: usize, rsp: u64, why: u8) -> u64 {
    let now = rdtsc();
    let cs = unsafe { frame_word(rsp, 16) };
    if cs & 3 != 3 {
        // The AP's own kernel context: the idle loop (or a kwork job).
        if why == WHY_TICK {
            ST_IDLE_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
        }
        if CUR_ON[cpu].load(Ordering::Relaxed) != IDLE || !crate::kwork::ap_quiet(cpu) {
            return rsp;
        }
        match take_next(cpu) {
            Some(t) => {
                IDLE_RSP[cpu].store(rsp, Ordering::Relaxed);
                switch_in(cpu, t, now)
            }
            None => rsp,
        }
    } else {
        let cur = CUR_ON[cpu].load(Ordering::Relaxed);
        if cur == IDLE {
            return rsp;
        }
        let t = cur as usize;
        let rc = &RUNCTX[t];
        if why == WHY_TICK {
            ST_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
            crate::sched::UTICKS[t].fetch_add(1, Ordering::Relaxed);
        }
        if rc.flags.load(Ordering::Acquire) & F_EVICT != 0 || mode() == OFF {
            save_out(cpu, t, rsp, now);
            rc.flags.fetch_and(!F_EVICT, Ordering::Relaxed);
            ST_EVICT[cpu].fetch_add(1, Ordering::Relaxed);
            to_inbox(cpu, t);
            return next_or_idle(cpu, now);
        }
        if crate::kwork::ap_pending(cpu) {
            save_out(cpu, t, rsp, now);
            requeue(cpu, t);
            return to_idle(cpu);
        }
        let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
        let over = now.saturating_sub(SLICE_AT[cpu].load(Ordering::Relaxed)) >= per;
        if (why == WHY_RESCHED || (why == WHY_TICK && over)) && NR[cpu].load(Ordering::SeqCst) > 0 {
            if let Some(n) = take_next(cpu) {
                save_out(cpu, t, rsp, now);
                requeue(cpu, t);
                return switch_in(cpu, n, now);
            }
        }
        rsp
    }
}

/// A ring-3 exception or system call on an AP (the `ap_trap_N` stubs): send the
/// task home. The BSP re-executes the instruction on its own path.
#[no_mangle]
pub extern "sysv64" fn ap_bounce(rsp: u64, vec: u64) -> u64 {
    let cpu = (crate::apic::lapic_id() & 7) as usize;
    let now = rdtsc();
    let cur = CUR_ON[cpu].load(Ordering::Relaxed);
    if cur == IDLE {
        // Ring 3 without a task of ours cannot happen: stop this core.
        loop {
            x86_64::instructions::interrupts::disable();
            x86_64::instructions::hlt();
        }
    }
    let t = cur as usize;
    if vec == 3 || vec == 4 {
        // int3 / into are traps: rip is past the instruction; retake it on the BSP.
        unsafe { ((rsp + 15 * 8) as *mut u64).write_volatile(frame_word(rsp, 15) - 1) };
    }
    match vec {
        6 => ST_BOUNCE_SC[cpu].fetch_add(1, Ordering::Relaxed),
        14 => ST_BOUNCE_PF[cpu].fetch_add(1, Ordering::Relaxed),
        _ => ST_BOUNCE_OTHER[cpu].fetch_add(1, Ordering::Relaxed),
    };
    save_out(cpu, t, rsp, now);
    let rc = &RUNCTX[t];
    rc.bounces.fetch_add(1, Ordering::Relaxed);
    rc.flags.fetch_and(!F_EVICT, Ordering::Relaxed);
    to_inbox(cpu, t);
    next_or_idle(cpu, now)
}

#[no_mangle]
pub extern "sysv64" fn ap_resched_tick(rsp: u64) -> u64 {
    crate::apic::eoi();
    ap_switch((crate::apic::lapic_id() & 7) as usize, rsp, WHY_RESCHED)
}

#[no_mangle]
pub extern "sysv64" fn ap_ping_tick(rsp: u64) -> u64 {
    let cpu = (crate::apic::lapic_id() & 7) as usize;
    crate::interrupts::IPI_COUNT[cpu].fetch_add(1, Ordering::Relaxed);
    crate::apic::eoi();
    ap_switch(cpu, rsp, WHY_PING)
}

/// The AP timer (0x41) once the demo workers are parked (sched::ap_schedule_tick).
pub fn ap_tick(cpu: usize, rsp: u64) -> u64 {
    ap_switch(cpu, rsp, WHY_TICK)
}

// ── AP IDT and entry stubs ───────────────────────────────────────────────────

/// Original handler of each exception vector in the BSP's IDT (ring-0 traps on
/// an AP go there unchanged); `ap_dead` where the BSP has none.
#[no_mangle]
static mut AP_ORIG: [u64; 32] = [0; 32];

core::arch::global_asm!(
    ".global ap_bounce_common",
    "ap_bounce_common:",
    // rax was pushed by the stub and holds the vector: with the pushes below the
    // frame is the 20-word layout of timer_switch (rax first).
    "cld",
    "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "mov rsi, rax",
    "and rsp, -16",
    "call ap_bounce",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
    ".global ap_dead",
    "ap_dead:",
    "cli",
    "hlt",
    "jmp ap_dead",
    ".global ap_resched_switch",
    "ap_resched_switch:",
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call ap_resched_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
    ".global ap_ping_switch",
    "ap_ping_switch:",
    "cld",
    "push rax", "push rbx", "push rcx", "push rdx", "push rsi", "push rdi", "push rbp",
    "push r8", "push r9", "push r10", "push r11", "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "and rsp, -16",
    "call ap_ping_tick",
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop r11", "pop r10", "pop r9", "pop r8",
    "pop rbp", "pop rdi", "pop rsi", "pop rdx", "pop rcx", "pop rbx", "pop rax",
    "iretq",
);

/// An exception stub without an error code: ring 0 goes to the original handler,
/// ring 3 is bounced.
macro_rules! ap_trap_noerr {
    ($name:literal, $vec:literal) => {
        core::arch::global_asm!(concat!(
            ".global ", $name, "\n",
            $name, ":\n",
            "test byte ptr [rsp + 8], 3\n",
            "jnz 2f\n",
            "jmp qword ptr [rip + AP_ORIG + ", $vec, " * 8]\n",
            "2:\n",
            "push rax\n",
            "mov eax, ", $vec, "\n",
            "jmp ap_bounce_common\n",
        ));
    };
}
/// The same for a vector with an error code (dropped for ring 3: the BSP takes
/// the exception again).
macro_rules! ap_trap_err {
    ($name:literal, $vec:literal) => {
        core::arch::global_asm!(concat!(
            ".global ", $name, "\n",
            $name, ":\n",
            "test byte ptr [rsp + 16], 3\n",
            "jnz 2f\n",
            "jmp qword ptr [rip + AP_ORIG + ", $vec, " * 8]\n",
            "2:\n",
            "add rsp, 8\n",
            "push rax\n",
            "mov eax, ", $vec, "\n",
            "jmp ap_bounce_common\n",
        ));
    };
}
ap_trap_noerr!("ap_trap_0", "0");
ap_trap_noerr!("ap_trap_1", "1");
ap_trap_noerr!("ap_trap_3", "3");
ap_trap_noerr!("ap_trap_4", "4");
ap_trap_noerr!("ap_trap_5", "5");
ap_trap_noerr!("ap_trap_6", "6");
ap_trap_noerr!("ap_trap_7", "7");
ap_trap_err!("ap_trap_10", "10");
ap_trap_err!("ap_trap_11", "11");
ap_trap_err!("ap_trap_12", "12");
ap_trap_err!("ap_trap_13", "13");
ap_trap_err!("ap_trap_14", "14");
ap_trap_noerr!("ap_trap_16", "16");
ap_trap_err!("ap_trap_17", "17");
ap_trap_noerr!("ap_trap_19", "19");
ap_trap_noerr!("ap_trap_20", "20");
ap_trap_err!("ap_trap_21", "21");

extern "C" {
    fn ap_trap_0(); fn ap_trap_1(); fn ap_trap_3(); fn ap_trap_4(); fn ap_trap_5(); fn ap_trap_6(); fn ap_trap_7();
    fn ap_trap_10(); fn ap_trap_11(); fn ap_trap_12(); fn ap_trap_13(); fn ap_trap_14(); fn ap_trap_16(); fn ap_trap_17();
    fn ap_trap_19(); fn ap_trap_20(); fn ap_trap_21();
    fn ap_dead();
    fn ap_resched_switch();
    fn ap_ping_switch();
}

static mut AP_IDT: Option<x86_64::structures::idt::InterruptDescriptorTable> = None;

/// Build the APs' IDT from the BSP's (on the BSP, before the APs start). The
/// BSP IDT is not touched.
pub fn build_ap_idt() {
    use x86_64::VirtAddr;
    let mut idt = crate::interrupts::bsp_idt().clone();
    macro_rules! hook {
        ($field:ident, $vec:expr, $stub:ident) => {{
            let a = idt.$field.handler_addr().as_u64();
            unsafe {
                (*core::ptr::addr_of_mut!(AP_ORIG))[$vec] = if a != 0 { a } else { (ap_dead as unsafe extern "C" fn()) as usize as u64 };
                idt.$field.set_handler_addr(VirtAddr::new(($stub as unsafe extern "C" fn()) as usize as u64));
            }
        }};
    }
    hook!(divide_error, 0, ap_trap_0);
    hook!(debug, 1, ap_trap_1);
    hook!(breakpoint, 3, ap_trap_3);
    hook!(overflow, 4, ap_trap_4);
    hook!(bound_range_exceeded, 5, ap_trap_5);
    hook!(invalid_opcode, 6, ap_trap_6);
    hook!(device_not_available, 7, ap_trap_7);
    hook!(invalid_tss, 10, ap_trap_10);
    hook!(segment_not_present, 11, ap_trap_11);
    hook!(stack_segment_fault, 12, ap_trap_12);
    hook!(general_protection_fault, 13, ap_trap_13);
    hook!(page_fault, 14, ap_trap_14);
    hook!(x87_floating_point, 16, ap_trap_16);
    hook!(alignment_check, 17, ap_trap_17);
    hook!(simd_floating_point, 19, ap_trap_19);
    hook!(virtualization, 20, ap_trap_20);
    hook!(cp_protection_exception, 21, ap_trap_21);
    unsafe {
        // A kernel stack overflow on an AP still faults onto its own IST stack.
        idt.page_fault
            .set_handler_addr(VirtAddr::new((ap_trap_14 as unsafe extern "C" fn()) as usize as u64))
            .set_stack_index(crate::gdt::PAGE_FAULT_IST_INDEX);
        idt[crate::interrupts::IPI_RESCHED_VECTOR].set_handler_addr(VirtAddr::new((ap_resched_switch as unsafe extern "C" fn()) as usize as u64));
        idt[crate::interrupts::IPI_PING_VECTOR].set_handler_addr(VirtAddr::new((ap_ping_switch as unsafe extern "C" fn()) as usize as u64));
        *core::ptr::addr_of_mut!(AP_IDT) = Some(idt);
    }
    record_parity(BSP_SLOT);
}

const BSP_SLOT: usize = MAX_CPU;
/// CR0, CR4, EFER, XCR0, PAT of every AP and (slot MAX_CPU) of the BSP.
static PARITY: [[AtomicU64; 5]; MAX_CPU + 1] = [const { [const { AtomicU64::new(0) }; 5] }; MAX_CPU + 1];

fn record_parity(slot: usize) {
    unsafe {
        let (cr0, cr4): (u64, u64);
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        let efer = x86_64::registers::model_specific::Msr::new(0xC000_0080).read();
        let xcr0 = if cr4 & (1 << 18) != 0 {
            let (lo, hi): (u32, u32);
            core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
            (hi as u64) << 32 | lo as u64
        } else {
            0
        };
        let pat = x86_64::registers::model_specific::Msr::new(0x277).read();
        for (i, v) in [cr0, cr4, efer, xcr0, pat].into_iter().enumerate() {
            PARITY[slot][i].store(v, Ordering::Release);
        }
    }
}

/// Ring 3 on this AP (on the AP, after `gdt::init_ap`, the feature copy and
/// `sched::ap_setup`, before its timer starts): its entry stack as TSS.rsp0, the
/// AP IDT, its APIC id, the parity record.
pub fn ap_init(cpu: usize) {
    let cpu = cpu % MAX_CPU;
    unsafe {
        let Some(idt) = (*core::ptr::addr_of!(AP_IDT)).as_ref() else { return };
        crate::gdt::set_ap_rsp0(cpu, crate::sched::ap_entry_stack_top(cpu));
        idt.load_unsafe();
    }
    APIC_OF[cpu].store(crate::apic::lapic_id() as u8, Ordering::Relaxed);
    record_parity(cpu);
    USER_OK[cpu].store(true, Ordering::Release);
}

/// `[ap-parity]`: every bit ring 3 depends on is the same on each AP as on the
/// BSP. `EFER.SCE` is the one deliberate difference (system calls bounce).
pub fn parity_report() -> alloc::string::String {
    use alloc::format;
    const CR0_MASK: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 5) | (1 << 16) | (1 << 18) | (1 << 31);
    const CR4_MASK: u64 = (1 << 5) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 16) | (1 << 17) | (1 << 18) | (1 << 20) | (1 << 21);
    const EFER_MASK: u64 = (1 << 8) | (1 << 10) | (1 << 11);
    let masks = [CR0_MASK, CR4_MASK, EFER_MASK, !0, !0];
    let names = ["CR0", "CR4", "EFER", "XCR0", "PAT"];
    let b: [u64; 5] = core::array::from_fn(|i| PARITY[BSP_SLOT][i].load(Ordering::Acquire));
    let aps = user_aps();
    let mut bad = alloc::string::String::new();
    for &c in &aps {
        for i in 0..5 {
            let v = PARITY[c][i].load(Ordering::Acquire);
            if v & masks[i] != b[i] & masks[i] {
                bad.push_str(&format!(" cpu{c} {} {:#x} (BSP {:#x})", names[i], v & masks[i], b[i] & masks[i]));
            }
        }
    }
    let sce_ap = aps.iter().any(|&c| PARITY[c][2].load(Ordering::Acquire) & 1 != 0);
    let verdict = if aps.is_empty() {
        alloc::string::String::from("⬜ (no AP)")
    } else if bad.is_empty() && !sce_ap {
        alloc::string::String::from("OK ✓")
    } else {
        format!("FAILED:{bad}")
    };
    let line = format!(
        "[ap-parity] {} AP(s) against the BSP: CR0 {:#x} CR4 {:#x} EFER {:#x} XCR0 {:#x} PAT {:#x}; EFER.SCE BSP {} APs {} (system calls bounce) → {verdict}",
        aps.len(), b[0] & CR0_MASK, b[1] & CR4_MASK, b[2] & EFER_MASK, b[3], b[4], b[2] & 1, sce_ap as u8
    );
    crate::serial_println!("{line}");
    line
}

/// One line per AP: what ran there.
pub fn stat_lines() -> alloc::vec::Vec<alloc::string::String> {
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    let mut v = alloc::vec::Vec::new();
    v.push(alloc::format!(
        "[smpstat] smpuser={} offloads {} sent home {}, shootdowns {} ({} IPIs, {} late), tasks brought home {}",
        mode_name(mode()), ST_OFFLOADS.load(Ordering::Relaxed), ST_DRAINED.load(Ordering::Relaxed),
        ST_SHOOTDOWNS.load(Ordering::Relaxed), ST_SHOOT_IPIS.load(Ordering::Relaxed), ST_SHOOT_LATE.load(Ordering::Relaxed),
        ST_KICKS.load(Ordering::Relaxed)
    ));
    for c in user_aps() {
        let run = match CUR_ON[c].load(Ordering::Relaxed) {
            IDLE => alloc::string::String::from("idle"),
            t => alloc::format!("task {t}"),
        };
        v.push(alloc::format!(
            "[smpstat] cpu{c}: switch-ins {} user {} ms, bounces syscall {} fault {} other {}, evictions {}, ticks user {} idle {}, queued {}, running {run}",
            ST_IN[c].load(Ordering::Relaxed),
            ST_USER_TSC[c].load(Ordering::Relaxed) * 10 / per,
            ST_BOUNCE_SC[c].load(Ordering::Relaxed),
            ST_BOUNCE_PF[c].load(Ordering::Relaxed),
            ST_BOUNCE_OTHER[c].load(Ordering::Relaxed),
            ST_EVICT[c].load(Ordering::Relaxed),
            ST_TICKS[c].load(Ordering::Relaxed),
            ST_IDLE_TICKS[c].load(Ordering::Relaxed),
            NR[c].load(Ordering::Relaxed),
        ));
    }
    v
}

// ── S4: TLB shootdown, bringing tasks home ───────────────────────────────────

const CR3_ADDR: u64 = 0x000F_FFFF_FFFF_F000;
pub static ST_SHOOTDOWNS: AtomicU64 = AtomicU64::new(0);
pub static ST_SHOOT_IPIS: AtomicU64 = AtomicU64::new(0);
pub static ST_SHOOT_LATE: AtomicU64 = AtomicU64::new(0);
pub static ST_KICKS: AtomicU64 = AtomicU64::new(0);
/// An address space whose tasks must not leave the BSP (fork copies it).
static HOLD_CR3: AtomicU64 = AtomicU64::new(0);

/// The BSP changed or removed translations of address space `cr3` (a flush on
/// the BSP's own TLB happens at the call site): every AP that runs `cr3` now
/// reloads it before this returns. An AP that switches to `cr3` later loads it
/// fresh (switch_in writes CR3 after publishing CUR_CR3; the fence orders the
/// caller's table writes before the look at CUR_CR3).
/// Test knob (`smpuser noshoot`): no shootdown, no fork hold.
pub static SHOOT_OFF: AtomicBool = AtomicBool::new(false);

pub fn shootdown(cr3: u64) {
    let cr3 = cr3 & CR3_ADDR;
    if cr3 == 0 || SHOOT_OFF.load(Ordering::Relaxed) {
        return;
    }
    core::sync::atomic::fence(Ordering::SeqCst);
    let mut want = [u64::MAX; MAX_CPU];
    let mut any = false;
    for c in 0..MAX_CPU {
        if CUR_CR3[c].load(Ordering::SeqCst) & CR3_ADDR == cr3 {
            want[c] = crate::interrupts::TLB_COUNT[c].load(Ordering::Acquire);
            any = true;
        }
    }
    if !any {
        return;
    }
    ST_SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    for c in 0..MAX_CPU {
        if want[c] != u64::MAX {
            ST_SHOOT_IPIS.fetch_add(1, Ordering::Relaxed);
            crate::apic::send_ipi(APIC_OF[c].load(Ordering::Relaxed), crate::interrupts::IPI_TLB_VECTOR);
        }
    }
    // The acknowledgement is the AP's TLB counter (ipi_tlb_handler counts after
    // its CR3 reload). An AP in user mode takes the IPI at once.
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    let t0 = rdtsc();
    for c in 0..MAX_CPU {
        if want[c] == u64::MAX {
            continue;
        }
        while crate::interrupts::TLB_COUNT[c].load(Ordering::Acquire) == want[c] {
            if rdtsc().saturating_sub(t0) > per * 10 {
                if ST_SHOOT_LATE.fetch_add(1, Ordering::Relaxed) < 4 {
                    crate::serial_println!("[tlb-smp] cpu{c} did not acknowledge a shootdown within 100 ms");
                }
                break;
            }
            core::hint::spin_loop();
        }
    }
}

/// Bring task `idx` home from its AP (BSP, scheduler lock held by the caller,
/// who then drains the inbox): a queued task is taken off the queue, a running
/// one is evicted with a reschedule IPI and awaited. For everything that ends or
/// changes a task the BSP does not run: kill, exit, exec, a signal, a park.
pub fn evict_wait(idx: usize) {
    let rc = &RUNCTX[idx];
    if rc.cpu.load(Ordering::Acquire) == BSP {
        return;
    }
    ST_KICKS.fetch_add(1, Ordering::Relaxed);
    let (w, bit) = (idx / 64, 1u64 << (idx % 64));
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1);
    let t0 = rdtsc();
    let mut asked = BSP;
    loop {
        // The owner may change under us (S5: an idle AP steals a queued task).
        let c = rc.cpu.load(Ordering::Acquire);
        if c == BSP {
            return;
        }
        let c = c as usize;
        if INBOX[c][w].load(Ordering::Acquire) & bit != 0 {
            return; // on its way home: the caller drains
        }
        if READY[c][w].fetch_and(!bit, Ordering::SeqCst) & bit != 0 {
            // Queued, not running: its frame and state were saved when it left.
            NR[c].fetch_sub(1, Ordering::SeqCst);
            INBOX[c][w].fetch_or(bit, Ordering::Release);
            INBOX_ANY.store(true, Ordering::Release);
            return;
        }
        if asked != c as u8 {
            rc.flags.fetch_or(F_EVICT, Ordering::SeqCst);
            send_resched(c);
            asked = c as u8;
        }
        if rdtsc().saturating_sub(t0) > per * 10 {
            crate::serial_println!("[kill-smp] task {idx} on cpu{c} did not come home within 100 ms");
            return;
        }
        core::hint::spin_loop();
    }
}

/// Bring home every task of address space `cr3` and keep them on the BSP
/// until `release_hold` (fork copies the address space: it must not move).
/// Returns the tasks that were away (the caller drains the inbox).
pub fn hold_cr3(cr3: u64) -> usize {
    if SHOOT_OFF.load(Ordering::Relaxed) {
        return 0;
    }
    let cr3 = cr3 & CR3_ADDR;
    HOLD_CR3.store(cr3, Ordering::SeqCst);
    let mut n = 0;
    for i in 0..MAX_TASKS {
        let rc = &RUNCTX[i];
        if rc.cpu.load(Ordering::Acquire) != BSP && rc.cr3.load(Ordering::Relaxed) & CR3_ADDR == cr3 {
            evict_wait(i);
            n += 1;
        }
    }
    n
}
pub fn release_hold() {
    HOLD_CR3.store(0, Ordering::SeqCst);
}
fn held(cr3: u64) -> bool {
    let h = HOLD_CR3.load(Ordering::Relaxed);
    h != 0 && cr3 & CR3_ADDR == h
}

/// `shootdown` for the address space the BSP runs now (a flush site that
/// works on the current tables).
pub fn shootdown_current() {
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    shootdown(cr3);
}
