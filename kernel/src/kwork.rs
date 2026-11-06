//! kwork: per-CPU kernel work queues on the application processors (J1).
//!
//! Before J1 the APs came online, proved a parallel sum and then ran two demo
//! busy-loops forever: real kernel work (filesystem, network, IPC) only ever ran
//! on the BSP, so no lock was ever contended across cores and SMP could not
//! scale. This module lets an AP run real kernel jobs:
//!
//! - each core has its **own queue** (lock class `RunQueue`, `Mode::Irq`) and a
//!   lock-free `PENDING` hint, so a core never takes another core's lock just to
//!   find out there is nothing to do;
//! - an idle core **steals** from a busy core with `try_lock` only, never blocking
//!   on another core's queue (the approach of Redox's per-CPU run queues and work
//!   stealing, re-implemented here; no Redox code);
//! - an idle core sleeps in `sti; hlt` instead of spinning (the demo workers are
//!   parked once the `[smp]` self-test has read their counters), and a submit
//!   wakes it with the existing ping IPI.
//!
//! Jobs are kernel closures (no ring 3 on APs: that needs per-CPU TSS/syscall
//! state, see the sprint plan). They run with interrupts enabled.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use eurolock::sync::Mutex;
use eurolock::{LockClass, LockSite, Mode};

const MAX_CPU: usize = 8;

pub type Job = Box<dyn FnOnce() + Send + 'static>;

static SITE_RUNQ: LockSite = LockSite::new("kwork.runqueue (per-CPU)", LockClass::RunQueue);

static QUEUES: [Mutex<VecDeque<Job>>; MAX_CPU] =
    [const { Mutex::new_site(&SITE_RUNQ, Mode::Irq, VecDeque::new()) }; MAX_CPU];
/// Jobs queued per core (a hint read without locking).
static PENDING: [AtomicUsize; MAX_CPU] = [const { AtomicUsize::new(0) }; MAX_CPU];
/// Core is inside a job right now (a steal target).
static BUSY: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];
/// Core runs its kwork idle loop.
static ONLINE: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];
/// Jobs executed / stolen per core.
pub static JOBS_RUN: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
pub static JOBS_STOLEN: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
/// Set once the `[smp]` demo workers are parked: only then may jobs run on APs
/// (before that an AP time-slices jobs with the demo workers, whose switch does
/// not preserve FPU/SSE state).
static ACCEPTING: AtomicBool = AtomicBool::new(false);

/// The APs that can take jobs right now (core indexes, BSP excluded).
pub fn online_aps() -> alloc::vec::Vec<usize> {
    (0..MAX_CPU).filter(|&c| ONLINE[c].load(Ordering::Acquire)).collect()
}

/// True once APs take jobs (else `parallel` runs every worker inline on the BSP).
pub fn accepting() -> bool {
    ACCEPTING.load(Ordering::Acquire)
}

/// Called by `smp` once the demo workers are parked.
pub fn start_accepting() {
    ACCEPTING.store(true, Ordering::Release);
}

/// Queue `job` on core `cpu`. Gives the job back if that core does not run kwork.
pub fn submit(cpu: usize, job: Job) -> Result<(), Job> {
    if cpu >= MAX_CPU || !ONLINE[cpu].load(Ordering::Acquire) || !ACCEPTING.load(Ordering::Acquire) {
        return Err(job);
    }
    QUEUES[cpu].lock().push_back(job);
    PENDING[cpu].fetch_add(1, Ordering::Release);
    // Wake the core if it sleeps in hlt (the ping vector only counts and EOIs).
    crate::apic::send_ipi(cpu as u8, crate::interrupts::IPI_PING_VECTOR);
    Ok(())
}

fn pop_local(cpu: usize) -> Option<Job> {
    if PENDING[cpu].load(Ordering::Acquire) == 0 {
        return None;
    }
    let j = QUEUES[cpu].lock().pop_front();
    if j.is_some() {
        PENDING[cpu].fetch_sub(1, Ordering::Release);
    }
    j
}

/// Take work from a core that is busy with another job. `try_lock` only: an
/// idle core never waits on someone else's queue.
fn steal(cpu: usize) -> Option<Job> {
    for k in 1..MAX_CPU {
        let v = (cpu + k) % MAX_CPU;
        if !ONLINE[v].load(Ordering::Acquire) || !BUSY[v].load(Ordering::Acquire) || PENDING[v].load(Ordering::Acquire) == 0 {
            continue;
        }
        if let Some(mut q) = QUEUES[v].try_lock() {
            if let Some(j) = q.pop_back() {
                PENDING[v].fetch_sub(1, Ordering::Release);
                JOBS_STOLEN[cpu].fetch_add(1, Ordering::Relaxed);
                return Some(j);
            }
        }
    }
    None
}

/// The idle loop of an AP: run local jobs, then stolen ones, else sleep.
pub fn ap_idle_loop(cpu: usize) -> ! {
    ONLINE[cpu].store(true, Ordering::Release);
    loop {
        let job = if ACCEPTING.load(Ordering::Acquire) { pop_local(cpu).or_else(|| steal(cpu)) } else { None };
        match job {
            Some(j) => {
                BUSY[cpu].store(true, Ordering::Release);
                j();
                BUSY[cpu].store(false, Ordering::Release);
                JOBS_RUN[cpu].fetch_add(1, Ordering::Relaxed);
            }
            // `sti; hlt` is atomic with respect to interrupt delivery: a ping that
            // arrives after the check above still wakes this hlt.
            None => x86_64::instructions::interrupts::enable_and_hlt(),
        }
    }
}

/// Run `f(worker)` for `worker` in `0..n` in parallel: worker 0 on the calling
/// core (the BSP), worker `i` on the `i`-th online AP. Returns when all are done.
/// `n` is capped at 1 + the number of online APs; the effective count is
/// returned.
pub fn parallel(n: usize, f: Arc<dyn Fn(usize) + Send + Sync>) -> usize {
    let aps = online_aps();
    let n = n.clamp(1, 1 + aps.len());
    let done = Arc::new(AtomicUsize::new(0));
    for (i, &cpu) in aps.iter().take(n - 1).enumerate() {
        let f = f.clone();
        let done = done.clone();
        let worker = i + 1;
        if let Err(job) = submit(
            cpu,
            Box::new(move || {
                f(worker);
                done.fetch_add(1, Ordering::Release);
            }),
        ) {
            // Could not queue: run it here so the caller still gets all workers.
            job();
        }
    }
    f(0);
    done.fetch_add(1, Ordering::Release);
    while done.load(Ordering::Acquire) < n {
        core::hint::spin_loop();
    }
    n
}

/// `[kwork]` boot self-test: every online AP runs a job that allocates, formats
/// and uses SSE-capable library code (proves the AP CPU state is set up for
/// general kernel code), and one AP steals a job queued behind a busy one.
pub fn selftest() {
    let aps = online_aps();
    if aps.is_empty() || !ACCEPTING.load(Ordering::Acquire) {
        crate::serial_println!("[kwork] no APs accepting work (single core) — per-CPU queues idle");
        return;
    }
    let hits = Arc::new(AtomicU64::new(0));
    let ran = parallel(
        1 + aps.len(),
        Arc::new({
            let hits = hits.clone();
            move |w| {
                let v: alloc::vec::Vec<u64> = (0..1000u64).map(|x| x * (w as u64 + 1)).collect();
                let s = alloc::format!("{}", v.iter().sum::<u64>());
                let f = (w as f64 + 0.5) * 2.0; // SSE arithmetic on the AP
                // The cheap stack-derived core index must match the Local APIC.
                if !s.is_empty() && f > 0.0 && crate::percpu::agrees_with_apic() {
                    hits.fetch_add(1, Ordering::Relaxed);
                }
            }
        }),
    );
    // Stealing: keep one AP busy with a long job, queue two short ones behind it;
    // another idle AP should steal one of them.
    let mut steal_ok = None;
    if aps.len() >= 2 {
        let victim = aps[0];
        let before: u64 = JOBS_STOLEN.iter().map(|a| a.load(Ordering::Relaxed)).sum();
        let release = Arc::new(AtomicBool::new(false));
        let r2 = release.clone();
        let started = Arc::new(AtomicBool::new(false));
        let s2 = started.clone();
        let _ = submit(
            victim,
            Box::new(move || {
                s2.store(true, Ordering::Release);
                let t0 = crate::interrupts::ticks();
                while !r2.load(Ordering::Acquire) && crate::interrupts::ticks() < t0 + 300 {
                    core::hint::spin_loop();
                }
            }),
        );
        while !started.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        let left = Arc::new(AtomicUsize::new(2));
        for _ in 0..2 {
            let l = left.clone();
            let _ = submit(victim, Box::new(move || {
                l.fetch_sub(1, Ordering::Release);
            }));
        }
        let t0 = crate::interrupts::ticks();
        while left.load(Ordering::Acquire) == 2 && crate::interrupts::ticks() < t0 + 200 {
            core::hint::spin_loop();
        }
        let stole = JOBS_STOLEN.iter().map(|a| a.load(Ordering::Relaxed)).sum::<u64>() > before;
        release.store(true, Ordering::Release);
        while left.load(Ordering::Acquire) > 0 && crate::interrupts::ticks() < t0 + 400 {
            core::hint::spin_loop();
        }
        steal_ok = Some(stole && left.load(Ordering::Acquire) == 0);
    }
    let ok = ran == 1 + aps.len() && hits.load(Ordering::Relaxed) as usize == ran && steal_ok != Some(false);
    crate::serial_println!(
        "[kwork] per-CPU work queues: {} AP(s) online, {} worker(s) ran alloc+format+SSE jobs with a correct per-CPU id ({} ok), work-stealing={} -> {}",
        aps.len(),
        ran,
        hits.load(Ordering::Relaxed),
        match steal_ok {
            Some(true) => "yes",
            Some(false) => "NO",
            None => "n/a (one AP)",
        },
        if ok { "OK ✓" } else { "FAILED" }
    );
}
