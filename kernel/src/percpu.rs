//! percpu: a cheap "which core am I on?" for per-CPU data (J1).
//!
//! The obvious answer, the Local APIC id, is an MMIO read. Under QEMU/TCG an
//! MMIO read leaves translated code for the device model, and measured on the
//! lockbench image it HALVED single-core lock throughput once every lock
//! operation asked for its CPU (sharded counters, replicated tables, the
//! per-CPU heap caches). There is no RDPID/RDTSCP on the `qemu64` CPU model,
//! and GS base is not usable because ring-3 programs on the BSP may set it
//! (arch_prctl ARCH_SET_GS).
//!
//! What every core does have is its own kernel stacks. The APs run kernel code
//! only on stacks this module knows (the AP boot stack, which is also the kwork
//! idle/job stack, and the AP scheduler's worker stacks); every other kernel
//! stack belongs to the BSP (its tasks never run on an AP). So the current
//! stack pointer identifies the core with a handful of compares.
//!
//! Correctness rule: a range is registered BEFORE the AP that owns it runs any
//! code that asks (`smp::init` registers the boot stack before INIT-SIPI-SIPI,
//! `sched::ap_setup` registers the worker stacks before the AP timer starts).

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const MAX_RANGES: usize = 16;

static LO: [AtomicU64; MAX_RANGES] = [const { AtomicU64::new(0) }; MAX_RANGES];
static HI: [AtomicU64; MAX_RANGES] = [const { AtomicU64::new(0) }; MAX_RANGES];
static CPU: [AtomicUsize; MAX_RANGES] = [const { AtomicUsize::new(0) }; MAX_RANGES];
static N: AtomicUsize = AtomicUsize::new(0);
static BSP: AtomicUsize = AtomicUsize::new(0);

/// Record the BSP's core index (its Local APIC id & 7).
pub fn set_bsp(cpu: usize) {
    BSP.store(cpu & 7, Ordering::Release);
}

/// Declare `[lo, hi)` a kernel stack that only core `cpu` runs on.
pub fn register_stack(lo: u64, hi: u64, cpu: usize) {
    let i = N.load(Ordering::Acquire);
    if i >= MAX_RANGES {
        return;
    }
    LO[i].store(lo, Ordering::Relaxed);
    HI[i].store(hi, Ordering::Relaxed);
    CPU[i].store(cpu & 7, Ordering::Relaxed);
    N.store(i + 1, Ordering::Release);
}

/// The current core's index (0..8), without touching the Local APIC.
#[inline]
pub fn cpu() -> usize {
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags)) };
    let n = N.load(Ordering::Acquire);
    for i in 0..n {
        if rsp >= LO[i].load(Ordering::Relaxed) && rsp < HI[i].load(Ordering::Relaxed) {
            return CPU[i].load(Ordering::Relaxed);
        }
    }
    BSP.load(Ordering::Relaxed)
}

/// Self-check (run on each core by the kwork self-test): the stack-derived
/// index must equal the Local APIC id.
pub fn agrees_with_apic() -> bool {
    cpu() == (crate::apic::lapic_id() & 7) as usize
}
