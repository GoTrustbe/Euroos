//! BUG-032: a hardware write watch on the BSP scheduler's `current` and `count`.
//!
//! Once, on the R630 under KVM, `schedule_core` found `SCHED.current` holding
//! eight bytes of the kernel's own machine code (`core::num::dec2flt` …
//! `compute_float::<f32>`, copied at a 4-byte offset): some code path wrote
//! over the scheduler state, and by the time the scheduler tripped over it the
//! writer was long gone. A debug-register watch names the writer at the moment
//! of the write instead: DR0 covers `current`, DR1 `count` (8 bytes each, write
//! only), on every core, so a wild write from an AP is caught too.
//!
//! Every legitimate write (each context switch stores `current`) also traps;
//! the #DB handler only checks that the new values are task indexes and returns.
//! A value that is not is reported with the writing instruction's address, the
//! CR3, the core and the RBP chain of the writer, then the kernel stops (the
//! state is already corrupt). A DMA write does not go through the CPU and is not
//! seen here; `sched::schedule_core` validates `current` before using it and
//! dumps the surrounding memory, which covers that case.
//!
//! On with `debug.schedwatch=on` in `euroos-boot.conf` (bootopt.rs). Off by
//! default: every context switch then costs one #DB (a VM exit under KVM).
//! The cause of BUG-032 turned out to be a wild READ (RFLAGS.DF set in the
//! scheduler, sched.rs), which this watch cannot see; it stays for the next
//! suspected wild write.
//!
//! Not in Redox; the DR7 layout is the Intel SDM's (Vol. 3, 17.2).

use core::sync::atomic::{AtomicU64, Ordering};

/// Address of `SCHED.current` / `SCHED.count` (0 until the scheduler is up).
static ADDR_CUR: AtomicU64 = AtomicU64::new(0);
static ADDR_CNT: AtomicU64 = AtomicU64::new(0);
/// The largest valid values (task index bound, task table size).
static MAX_CUR: AtomicU64 = AtomicU64::new(0);
/// Legitimate writes seen (each one a #DB): proves the watch is live.
pub static HITS: AtomicU64 = AtomicU64::new(0);
/// Cores whose DR7 holds the watch (bit per LAPIC id & 63).
static ARMED: AtomicU64 = AtomicU64::new(0);
/// Self-test: the word DR2 watches, and the RIP its trap reported.
static TEST_WORD: AtomicU64 = AtomicU64::new(0);
static TEST_RIP: AtomicU64 = AtomicU64::new(0);

/// DR7: L0 + L1 (and L2 for the self-test), RW = 01 (data write), LEN = 10 (8 bytes).
const DR7_BASE: u64 = (1 << 0) | (1 << 2) | (0b01 << 16) | (0b10 << 18) | (0b01 << 20) | (0b10 << 22) | (1 << 8);
const DR7_TEST: u64 = (1 << 4) | (0b01 << 24) | (0b10 << 26);

/// Record the watched fields (call with the scheduler lock held, once).
pub fn set_targets(cur: u64, cnt: u64, max_tasks: usize) {
    MAX_CUR.store(max_tasks as u64, Ordering::Relaxed);
    ADDR_CNT.store(cnt, Ordering::Relaxed);
    ADDR_CUR.store(cur, Ordering::Release);
}

fn cpu_bit() -> u64 {
    1u64 << (crate::apic::lapic_id() & 63)
}

unsafe fn write_dr7(v: u64) {
    core::arch::asm!("mov dr7, {}", in(reg) v, options(nostack, preserves_flags));
}

/// Arm the watch on THIS core (BSP after `sched::init`, every AP in `ap_main`).
pub fn arm_this_cpu() {
    let cur = ADDR_CUR.load(Ordering::Acquire);
    let cnt = ADDR_CNT.load(Ordering::Relaxed);
    if !crate::bootopt::sched_watch() || cur == 0 || cur & 7 != 0 || cnt & 7 != 0 {
        return;
    }
    unsafe {
        core::arch::asm!("mov dr0, {}", in(reg) cur, options(nostack, preserves_flags));
        core::arch::asm!("mov dr1, {}", in(reg) cnt, options(nostack, preserves_flags));
        core::arch::asm!("mov dr6, {}", in(reg) 0xFFFF_0FF0u64, options(nostack, preserves_flags));
        write_dr7(DR7_BASE);
    }
    ARMED.fetch_or(cpu_bit(), Ordering::AcqRel);
}

/// Address of `SCHED.current` as the scheduler published it (0 before init).
pub fn current_field() -> u64 {
    ADDR_CUR.load(Ordering::Relaxed)
}

/// Is the watch armed on any core?
pub fn armed_cpus() -> u32 {
    ARMED.load(Ordering::Acquire).count_ones()
}

/// The #DB handler body (interrupts.rs). Returns normally for every legitimate
/// write; stops the kernel on a write that leaves the scheduler state invalid.
pub fn on_debug(rip: u64, cs: u64, rsp: u64, rflags: u64) {
    let dr6: u64;
    unsafe {
        core::arch::asm!("mov {}, dr6", out(reg) dr6, options(nostack, preserves_flags));
        core::arch::asm!("mov dr6, {}", in(reg) 0xFFFF_0FF0u64, options(nostack, preserves_flags));
    }
    if dr6 & (1 << 2) != 0 {
        TEST_RIP.store(rip, Ordering::Release);
    }
    if dr6 & 0b11 == 0 {
        if dr6 & (1 << 2) == 0 {
            // Not ours (a single step, a stray breakpoint): note it once, carry on.
            static ONCE: AtomicU64 = AtomicU64::new(0);
            if ONCE.fetch_add(1, Ordering::Relaxed) == 0 {
                crate::serial_println!("[bug032] #DB not from the scheduler watch: dr6={dr6:#x} rip={rip:#x} cs={cs:#x}");
            }
        }
        return;
    }
    HITS.fetch_add(1, Ordering::Relaxed);
    let pc = ADDR_CUR.load(Ordering::Relaxed);
    let pn = ADDR_CNT.load(Ordering::Relaxed);
    let max = MAX_CUR.load(Ordering::Relaxed);
    // SAFETY: both addresses are fields of the static scheduler, always mapped.
    let (cur, cnt) = unsafe { ((pc as *const u64).read_volatile(), (pn as *const u64).read_volatile()) };
    if cur < max && cnt <= max {
        return;
    }
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
    crate::serial_println!(
        "\n[bug032] WILD WRITE to the scheduler state: current={cur:#018x} count={cnt:#x} (dr6={dr6:#x})\n\
         [bug032]   writer rip={rip:#018x} cs={cs:#x} rsp={rsp:#018x} rflags={rflags:#x} cr3={cr3:#x} cpu={} \
         (the instruction BEFORE rip did the write; anchor dump_registers_and_backtrace @ {:#x})",
        crate::apic::lapic_id(),
        crate::klog::dump_registers_and_backtrace as usize as u64
    );
    crate::sched::dump_corrupt_neighbourhood(pc);
    panic!("BUG-032: wild write to SCHED (current={cur:#x}, count={cnt:#x}) by the instruction before rip={rip:#x}");
}

#[inline(never)]
fn selftest_write(v: u64) {
    // A store the watch has to name: through a raw pointer, as a wild write would be.
    unsafe { (TEST_WORD.as_ptr()).write_volatile(v) };
}

/// Boot self-test: arm DR2 on a test word for a moment, write it, and check that
/// the trap reported an instruction inside the writing function. Proves that
/// the debug registers work on this machine (KVM, TCG, metal) and that the
/// scheduler watch is live, without touching the scheduler.
pub fn selftest() {
    if !crate::bootopt::sched_watch() {
        crate::serial_println!("[bug032] scheduler write watch off (debug.schedwatch=on arms it)");
        return;
    }
    if ARMED.load(Ordering::Acquire) & cpu_bit() == 0 {
        crate::serial_println!("[bug032] scheduler write watch NOT armed on the BSP → FAILED");
        return;
    }
    let f = selftest_write as usize as u64;
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let a = TEST_WORD.as_ptr() as u64;
        core::arch::asm!("mov dr2, {}", in(reg) a, options(nostack, preserves_flags));
        write_dr7(DR7_BASE | DR7_TEST);
        selftest_write(0xB032);
        write_dr7(DR7_BASE);
    });
    let rip = TEST_RIP.load(Ordering::Acquire);
    let caught = rip > f && rip < f + 256;
    let hits = HITS.load(Ordering::Relaxed);
    crate::serial_println!(
        "[bug032] scheduler write watch (DR0 current, DR1 count): armed on {} core(s), {hits} legitimate writes checked; \
         test write caught={caught} (rip {rip:#x}, writer @ {f:#x}) → {}",
        armed_cpus(),
        if caught && hits > 0 { "OK ✓" } else { "FAILED" }
    );
}
