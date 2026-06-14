//! SMP bring-up (Track 3.5): start the application processors (APs).
//!
//! The BSP copies the [`asm/trampoline.S`] blob to physical `0x8000`, patches the
//! boot PML4 (CR3), a per-AP stack and the `ap_main` entry, and sends per core
//! the INIT-SIPI-SIPI sequence via the Local APIC. Each AP enters long mode, counts
//! itself up in [`AP_ONLINE`] and parks (`hlt`). SMP scheduling (per-CPU
//! run queues) is the next step; this proves that all cores run kernel code.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alloc::vec::Vec;

use crate::serial_println;

/// Size of the parallel-sum demo (0..N), distributed over the cores. Deliberately
/// modest: the demo proves the parallel distribution + correct sum, but a
/// gigantic N costs needless boot time especially under TCG emulation (SPERF).
const WORK_N: u64 = 2_000_000;

/// Flat-binary trampoline assembled by build.rs (org 0x8000).
static TRAMPOLINE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin"));

const TRAMP: u64 = 0x8000;
const OFF_CR3: u64 = 0xF00;
const OFF_STACK: u64 = 0xF08;
const OFF_ENTRY: u64 = 0xF10;

const MAX_AP: usize = 7;
const AP_STACK_SIZE: usize = 16 * 1024;
static mut AP_STACKS: [[u8; AP_STACK_SIZE]; MAX_AP] = [[0; AP_STACK_SIZE]; MAX_AP];

/// G1: per-AP GUARDED kernel stacktop (0 = not set → fall back to the BSS stack).
/// main.rs fills this before `init()` from the guarded-stack pool, so that an AP-stack
/// overflow faults on an unmapped guard page instead of overwriting the neighbouring
/// AP stack. The guarded region sits in the shared high PDPT → also valid under
/// the boot PML4 on which the APs run.
static AP_GUARDED_TOP: [AtomicU64; MAX_AP] = [const { AtomicU64::new(0) }; MAX_AP];

/// Set the guarded stacktop for AP slot `idx` (call before [`init`]).
pub fn set_ap_guarded_top(idx: usize, top: u64) {
    if idx < MAX_AP {
        AP_GUARDED_TOP[idx].store(top, Ordering::Release);
    }
}

/// G1: allocate guarded kernel stacks for ALL AP slots from the guarded-stack pool.
/// Call before [`init`] (the main frame allocator is then still available). Returns
/// the number of stacks set.
pub fn setup_guarded_stacks(falloc: &mut euromm::FrameAllocator) -> usize {
    let mut n = 0;
    for idx in 0..MAX_AP {
        let top = crate::paging::guarded_stack_alloc(falloc);
        if top == 0 {
            break;
        }
        set_ap_guarded_top(idx, top);
        n += 1;
    }
    n
}

/// Number of APs that reached long mode and run Rust code.
pub static AP_ONLINE: AtomicU32 = AtomicU32::new(0);

// ── Parallel-work demo: each core sums its own range [lo,hi). ──
// Indexed by (LAPIC-id & 7); the BSP fills the ranges before bring-up.
const MAX_CPU: usize = 8;
static WORK_LO: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_HI: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_SUM: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_DONE: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];

/// Sum `lo..hi` (the core workload — proves a core really computes).
fn compute_range(lo: u64, hi: u64) -> u64 {
    let mut s = 0u64;
    let mut i = lo;
    while i < hi {
        s = s.wrapping_add(i);
        i += 1;
    }
    s
}

/// Entry for each AP (from the trampoline, 64-bit): report yourself online, compute
/// your assigned part of the parallel sum, then park (`hlt`).
#[no_mangle]
pub extern "sysv64" fn ap_main() -> ! {
    let id = (crate::apic::lapic_id() & 7) as usize;
    AP_ONLINE.fetch_add(1, Ordering::SeqCst);
    // 1) Assigned part of the parallel sum.
    let lo = WORK_LO[id].load(Ordering::Acquire);
    let hi = WORK_HI[id].load(Ordering::Acquire);
    let sum = compute_range(lo, hi);
    WORK_SUM[id].store(sum, Ordering::Release);
    WORK_DONE[id].store(true, Ordering::Release);
    // 2) Set up the per-CPU scheduler: shared GDT segments + shared IDT + an
    //    own run queue (idle + 2 workers) + own LAPIC timer (vector 0x41).
    //    After sti this core ticks itself and alternates its own tasks. This hlt loop
    //    is the idle task: the first tick saves it and switches to a worker.
    crate::gdt::init_ap();
    crate::interrupts::init();
    crate::sched::ap_setup(id);
    crate::apic::start_timer_on_this_cpu(0x41);
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Start all enabled AP cores. Call after `init_timer` (APIC + timer
/// calibrated) and before `interrupts::enable()` — then the BSP is still on the
/// boot PML4 and the scheduler does not yet switch the CR3.
pub fn init() {
    let madt = match crate::acpi::parse() {
        Some(m) => m,
        None => {
            serial_println!("[smp] no MADT — single-core");
            return;
        }
    };
    let bsp = crate::apic::lapic_id() as u8;
    let cr3 = crate::sched::boot_pml4();
    if cr3 == 0 {
        serial_println!("[smp] boot PML4 unknown — bring-up skipped");
        return;
    }

    // Distribute a parallel sum 0..N over ALL enabled cores (BSP + APs).
    let enabled: Vec<u8> = madt.cores.iter().filter(|c| c.enabled).map(|c| c.apic_id).collect();
    let ncores = enabled.len().max(1) as u64;
    let chunk = WORK_N / ncores;
    for (i, &id) in enabled.iter().enumerate() {
        let lo = i as u64 * chunk;
        let hi = if i as u64 + 1 == ncores { WORK_N } else { lo + chunk };
        let s = (id & 7) as usize;
        WORK_LO[s].store(lo, Ordering::Release);
        WORK_HI[s].store(hi, Ordering::Release);
        WORK_DONE[s].store(false, Ordering::Release);
    }

    // Copy trampoline to 0x8000 + patch CR3/entry (per-AP stack follows).
    unsafe {
        core::ptr::copy_nonoverlapping(TRAMPOLINE.as_ptr(), TRAMP as *mut u8, TRAMPOLINE.len());
        ((TRAMP + OFF_CR3) as *mut u64).write_volatile(cr3);
        ((TRAMP + OFF_ENTRY) as *mut u64).write_volatile(ap_main as usize as u64);
    }

    let mut idx = 0usize;
    let mut started = 0u32;
    for c in &madt.cores {
        if !c.enabled || c.apic_id == bsp {
            continue;
        }
        if idx >= MAX_AP {
            break;
        }
        // Per-AP stacktop (16-aligned); APs start one by one, so no race.
        // G1: use a GUARDED stacktop if it is set (overflow → guard #PF
        // instead of silent corruption); otherwise fall back to the BSS stack.
        let guarded = AP_GUARDED_TOP[idx].load(Ordering::Acquire);
        let sp = if guarded != 0 {
            guarded & !0xF
        } else {
            unsafe {
                let base = core::ptr::addr_of_mut!(AP_STACKS[idx]) as u64;
                (base + AP_STACK_SIZE as u64) & !0xF
            }
        };
        unsafe { ((TRAMP + OFF_STACK) as *mut u64).write_volatile(sp) };

        let before = AP_ONLINE.load(Ordering::SeqCst);
        // INIT-SIPI-SIPI (Intel MP protocol).
        crate::apic::send_init(c.apic_id);
        crate::apic::busy_wait_us(10_000);
        crate::apic::send_sipi(c.apic_id, (TRAMP >> 12) as u8);
        crate::apic::busy_wait_us(200);
        crate::apic::send_sipi(c.apic_id, (TRAMP >> 12) as u8);

        // Wait until this AP reports in (timeout ~100 ms).
        let mut ok = false;
        for _ in 0..20 {
            crate::apic::busy_wait_us(5_000);
            if AP_ONLINE.load(Ordering::SeqCst) > before {
                ok = true;
                break;
            }
        }
        if ok {
            started += 1;
            serial_println!("[smp] core APIC-id {} online", c.apic_id);
        } else {
            serial_println!("[smp] core APIC-id {} did NOT come online (timeout)", c.apic_id);
        }
        idx += 1;
    }
    serial_println!(
        "[smp] {}/{} AP core(s) online (BSP = APIC-id {}, {} cores total)",
        started,
        idx,
        bsp,
        madt.cores.len()
    );

    // The BSP computes its own part, waits for the APs and verifies the total sum —
    // proof that all cores really did (parallel) kernel computation.
    let bid = (bsp & 7) as usize;
    let blo = WORK_LO[bid].load(Ordering::Acquire);
    let bhi = WORK_HI[bid].load(Ordering::Acquire);
    WORK_SUM[bid].store(compute_range(blo, bhi), Ordering::Release);
    WORK_DONE[bid].store(true, Ordering::Release);

    for &id in &enabled {
        let s = (id & 7) as usize;
        let mut g = 0u32;
        while !WORK_DONE[s].load(Ordering::Acquire) {
            crate::apic::busy_wait_us(1_000);
            g += 1;
            if g > 5_000 {
                serial_println!("[smp] core APIC-id {} did not finish its work (timeout)", id);
                break;
            }
        }
    }
    let mut total = 0u64;
    for &id in &enabled {
        total = total.wrapping_add(WORK_SUM[(id & 7) as usize].load(Ordering::Acquire));
    }
    let expected = if WORK_N % 2 == 0 {
        (WORK_N / 2).wrapping_mul(WORK_N - 1)
    } else {
        WORK_N.wrapping_mul((WORK_N - 1) / 2)
    };
    serial_println!(
        "[smp] parallel sum 0..{} over {} core(s) = {} (expected {}) -> {}",
        WORK_N,
        ncores,
        total,
        expected,
        if total == expected { "OK" } else { "WRONG" }
    );

    // Give the APs ~80 ms and read their per-CPU work counters: proof that each core
    // independently runs its OWN run queue and alternates tasks (SMP scheduling).
    crate::apic::busy_wait_us(200_000);
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        let s = (id & 7) as usize;
        let a = crate::sched::AP_WORK_A[s].load(Ordering::Relaxed);
        let b = crate::sched::AP_WORK_B[s].load(Ordering::Relaxed);
        serial_println!(
            "[smp] core APIC-id {} per-CPU scheduler: worker-A={} worker-B={} (own run queue)",
            id, a, b
        );
    }

    // ── Run 2: cross-CPU IPIs + TLB shootdown ──
    // Ping each AP 5× and do one TLB shootdown; verify via the per-CPU counters.
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        for _ in 0..5 {
            crate::apic::send_ipi(id, crate::interrupts::IPI_PING_VECTOR);
            crate::apic::busy_wait_us(300);
        }
    }
    tlb_shootdown(&enabled, bsp);
    crate::apic::busy_wait_us(5_000);
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        let s = (id & 7) as usize;
        serial_println!(
            "[smp] core APIC-id {} cross-CPU: {} ping IPIs received, {} TLB shootdown(s)",
            id,
            crate::interrupts::IPI_COUNT[s].load(Ordering::Relaxed),
            crate::interrupts::TLB_COUNT[s].load(Ordering::Relaxed)
        );
    }

    // ── Run 2: load-balancing — place an extra task on the least-loaded AP ──
    let aps: Vec<u8> = enabled.iter().copied().filter(|&id| id != bsp).collect();
    if let Some(&target) = aps.iter().min_by_key(|&&id| crate::sched::ap_load((id & 7) as usize)) {
        let s = (target & 7) as usize;
        if crate::sched::ap_enqueue_worker(s) {
            serial_println!("[smp] load-balance: extra task placed on least-loaded core APIC-id {}", target);
            crate::apic::busy_wait_us(40_000);
            serial_println!(
                "[smp] core APIC-id {} runs the balanced task: worker-C={}",
                target,
                crate::sched::AP_WORK_C[s].load(Ordering::Relaxed)
            );
        }
    }
}

/// Send a TLB shootdown to all other cores (call after modifying
/// shared kernel page tables, so that no core keeps running with stale TLB entries).
pub fn tlb_shootdown(cores: &[u8], self_id: u8) {
    for &id in cores {
        if id != self_id {
            crate::apic::send_ipi(id, crate::interrupts::IPI_TLB_VECTOR);
        }
    }
}

/// Stop all other cores (e.g. before a shutdown or in the panic handler).
pub fn halt_others(cores: &[u8], self_id: u8) {
    for &id in cores {
        if id != self_id {
            crate::apic::send_ipi(id, crate::interrupts::IPI_HALT_VECTOR);
        }
    }
}
