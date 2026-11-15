//! Scheduler measurement and tests (docs/design/SMP-SCHEDULER.md, step S0).
//!
//! - `sched-stat start` / `sched-stat`: per task the run time on the TSC, the
//!   timer ticks that found it in ring 3 and in ring 0, its system calls and
//!   switches per second, the kernel share of all ring-3 tasks, and `[t0-yield]`:
//!   where the desktop loop gave the CPU away.
//! - `[if0-window]`: a timer tick that comes late names the window it ends and its
//!   likely cause: host steal time (the KVM steal-time MSR), a disk wait with IF=0,
//!   demand faults, or the system call the task was in (BUG-182).
//! - `hogtest N [rounds] [churn]` (`[fair]`): N ring-3 hogs beside `churn proc 8`
//!   on the desktop task (BUG-192); `hogtest nice` (`[fair-nice]`); `wakelat`
//!   (`[wakelat]`, two threads pass a futex turn next to 4 hogs); `hogs N`;
//!   `if0test` (disk reads with IF=0: the longest window without a tick).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use euromm::FrameAllocator;

use crate::sched::{self, MAX_TASKS};

fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn per_tick() -> u64 {
    crate::apic::TSC_PER_TICK.load(Ordering::Relaxed).max(1)
}

/// TSC cycles to milliseconds (the tick is 10 ms).
fn ms(cycles: u64) -> u64 {
    cycles.saturating_mul(10) / per_tick()
}

fn ms_f(cycles: u64) -> f64 {
    cycles as f64 * 10.0 / per_tick() as f64
}

// ── KVM steal time ─────────────────────────────────────────────────────────

/// `struct kvm_steal_time` of the BSP: the host writes how long this vCPU was
/// runnable but not running (ns). 64-byte aligned, identity-mapped (kernel image).
#[repr(C, align(64))]
struct StealTime {
    steal: u64,
    version: u32,
    flags: u32,
    preempted: u8,
    _pad: [u8; 47],
}
static mut STEAL: StealTime = StealTime { steal: 0, version: 0, flags: 0, preempted: 0, _pad: [0; 47] };
/// 0 = not tried, 1 = on, 2 = not available (not KVM, or no steal-time feature).
static STEAL_STATE: AtomicU64 = AtomicU64::new(0);

fn steal_init() {
    use core::arch::x86_64::__cpuid;
    // SAFETY: CPUID is always available on x86_64; leaf 0x4000_0000 is the
    // hypervisor range (zeros on bare metal).
    let (sig, feat) = {
        let l = __cpuid(0x4000_0000);
        let mut sig = [0u8; 12];
        sig[0..4].copy_from_slice(&l.ebx.to_le_bytes());
        sig[4..8].copy_from_slice(&l.ecx.to_le_bytes());
        sig[8..12].copy_from_slice(&l.edx.to_le_bytes());
        let f = if &sig == b"KVMKVMKVM\0\0\0" { __cpuid(0x4000_0001).eax } else { 0 };
        (sig, f)
    };
    if &sig != b"KVMKVMKVM\0\0\0" || feat & (1 << 5) == 0 {
        STEAL_STATE.store(2, Ordering::Relaxed);
        return;
    }
    // SAFETY: MSR_KVM_STEAL_TIME (0x4b564d03) takes the physical address of a
    // 64-byte aligned area | enable bit. Kernel statics are identity-mapped.
    unsafe {
        let pa = core::ptr::addr_of!(STEAL) as u64;
        x86_64::registers::model_specific::Msr::new(0x4b56_4d03).write(pa | 1);
    }
    STEAL_STATE.store(1, Ordering::Relaxed);
}

/// Host steal time of the BSP so far (ns); 0 without KVM steal time.
pub fn steal_ns() -> u64 {
    if STEAL_STATE.load(Ordering::Relaxed) != 1 {
        return 0;
    }
    // SAFETY: the host writes the area; read it as volatile, retry while the
    // version is odd (an update in progress) or changed under the read.
    unsafe {
        let p = core::ptr::addr_of!(STEAL);
        for _ in 0..8 {
            let v0 = core::ptr::read_volatile(core::ptr::addr_of!((*p).version));
            let s = core::ptr::read_volatile(core::ptr::addr_of!((*p).steal));
            let v1 = core::ptr::read_volatile(core::ptr::addr_of!((*p).version));
            if v0 == v1 && v0 & 1 == 0 {
                return s;
            }
        }
        core::ptr::read_volatile(core::ptr::addr_of!((*p).steal))
    }
}

/// " N" after `cause=syscall`, nothing otherwise (no allocation in the tick).
struct SyscallNo(Option<u64>);
impl core::fmt::Display for SyscallNo {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self.0 {
            Some(n) => write!(f, " {n}"),
            None => Ok(()),
        }
    }
}

// ── [if0-window] ───────────────────────────────────────────────────────────

static LAST_STEAL: AtomicU64 = AtomicU64::new(0);
static LAST_FCYC: AtomicU64 = AtomicU64::new(0);
static LAST_FC: AtomicU64 = AtomicU64::new(0);
/// Windows of 50 ms or more without a timer tick, the longest one (cycles), and
/// how many were logged.
pub static IF0_WINDOWS: AtomicU64 = AtomicU64::new(0);
pub static IF0_MAX: AtomicU64 = AtomicU64::new(0);
static IF0_LOGGED: AtomicU64 = AtomicU64::new(0);
/// The longest gap between two ticks since the test reset it (cycles).
static WIN_MAX: AtomicU64 = AtomicU64::new(0);

/// Called by the BSP timer tick with the TSC of the previous tick and of this one
/// and the interrupted frame. Lock-free, no allocation.
pub fn on_tick(prev_tsc: u64, now_tsc: u64, rsp: u64) {
    if STEAL_STATE.load(Ordering::Relaxed) == 0 {
        steal_init();
    }
    let steal = steal_ns();
    let st_prev = LAST_STEAL.swap(steal, Ordering::Relaxed);
    let (fc, fcyc) = crate::ring3::fault_counters();
    let fcyc_prev = LAST_FCYC.swap(fcyc, Ordering::Relaxed);
    let fc_prev = LAST_FC.swap(fc, Ordering::Relaxed);
    let (blk_any, blk_if0) = crate::virtio_blk::take_wait_max();
    if prev_tsc == 0 {
        return;
    }
    let gap = now_tsc.wrapping_sub(prev_tsc);
    WIN_MAX.fetch_max(gap, Ordering::Relaxed);
    let per = per_tick();
    if gap < per * 5 {
        return;
    }
    IF0_WINDOWS.fetch_add(1, Ordering::Relaxed);
    let was_max = IF0_MAX.fetch_max(gap, Ordering::Relaxed) < gap;
    let n = IF0_LOGGED.fetch_add(1, Ordering::Relaxed);
    if !(n < 64 || n % 64 == 0 || was_max) {
        return;
    }
    let win_ms = ms(gap);
    let steal_ms = steal.saturating_sub(st_prev) / 1_000_000;
    let blk_ms = ms(blk_if0);
    let fault_ms = ms(fcyc.wrapping_sub(fcyc_prev));
    let (rip, cs) = unsafe { (*((rsp + 120) as *const u64), *((rsp + 128) as *const u64)) };
    let cur = sched::current_lockfree();
    let (sn, _, _) = crate::ring3::last_syscall(cur);
    let half = win_ms / 2;
    let cause: &str = if steal_ms >= half && steal_ms > 0 {
        "steal"
    } else if blk_ms >= half && blk_ms > 0 {
        "blk"
    } else if fault_ms >= half && fault_ms > 0 {
        "fault"
    } else if cs & 3 == 3 || sched::task_is_user(cur) {
        "syscall"
    } else {
        "kernel"
    };
    crate::serial_println!(
        "[if0-window] {win_ms} ms cause={cause}{} (steal {steal_ms} ms, disk wait with IF=0 {blk_ms} ms (any {} ms), faults +{} {fault_ms} ms, task {cur} last syscall {sn}, resumed at rip={rip:#x} cs={cs:#x}) #{} longest {} ms",
        SyscallNo(if cause == "syscall" { Some(sn) } else { None }),
        ms(blk_any), fc.wrapping_sub(fc_prev), IF0_WINDOWS.load(Ordering::Relaxed), ms(IF0_MAX.load(Ordering::Relaxed))
    );
}

/// The S8 test: `n` reads of 4 KiB from virtio disk 0 with IF=0, as a system
/// call that reads the disk does them. Passes when no window without a timer
/// tick is longer than 50 ms. Meant for a throttled disk (QEMU
/// `throttling.iops-total`): on a fast disk every read is well under a tick.
pub fn if0test(n: usize) -> Vec<String> {
    if !crate::virtio_blk::present_dev(0) {
        let l = String::from("[if0-window] test: no virtio disk -> SKIPPED ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    let cap = crate::virtio_blk::capacity_sectors_dev(0);
    let n = n.clamp(1, 200);
    let mut buf = alloc::vec![0u8; 4096];
    let tick_wait = |k: u64| {
        let t = crate::interrupts::ticks();
        while crate::interrupts::ticks() < t + k {
            core::hint::spin_loop();
        }
    };
    tick_wait(2);
    WIN_MAX.store(0, Ordering::Relaxed);
    let t0 = rdtsc();
    let mut ok = 0usize;
    let mut longest_read = 0u64;
    for i in 0..n {
        // Spread over the disk: QEMU's throttle counts requests, not bytes.
        let sector = ((i as u64 * 7919 * 2048) % cap.saturating_sub(16).max(1)) & !7;
        let r0 = rdtsc();
        let good = x86_64::instructions::interrupts::without_interrupts(|| crate::virtio_blk::read_io_dev(0, sector, &mut buf));
        longest_read = longest_read.max(rdtsc() - r0);
        if good {
            ok += 1;
        }
        // A tick between the reads: each read is its own window.
        tick_wait(1);
    }
    let total = rdtsc() - t0;
    tick_wait(2);
    let w = WIN_MAX.load(Ordering::Relaxed);
    let pass = ok == n && ms(w) <= 50;
    let l = format!(
        "[if0-window] test: {n} disk reads with IF=0 on virtio disk 0 ({ok} ok, longest read {} ms, {} ms in all): longest window without a timer tick {} ms (limit 50 ms) -> {}",
        ms(longest_read), ms(total), ms(w), if pass { "OK ✓" } else { "FAILED" }
    );
    crate::serial_println!("{l}");
    alloc::vec![l]
}

// ── sched-stat ─────────────────────────────────────────────────────────────

struct Snap {
    tsc: u64,
    ticks: u64,
    idle_ticks: u64,
    t0_idle: u64,
    run: Vec<u64>,
    ut: Vec<u64>,
    kt: Vec<u64>,
    sw: Vec<u64>,
    sc: Vec<u64>,
    kt_tsc: Vec<u64>,
    user: Vec<bool>,
    yields: Vec<(u64, u64)>,
}

fn snap() -> Snap {
    let tasks = sched::snapshot_tasks();
    let mut user = alloc::vec![false; MAX_TASKS];
    for (i, cr3, st) in tasks {
        if i < MAX_TASKS && cr3 != 0 && st != sched::State::Dead {
            user[i] = true;
        }
    }
    Snap {
        tsc: rdtsc(),
        ticks: crate::interrupts::ticks(),
        idle_ticks: sched::IDLE_TICKS.load(Ordering::Relaxed),
        t0_idle: sched::T0_IDLE_TSC.load(Ordering::Relaxed),
        run: (0..MAX_TASKS).map(sched::run_tsc_now).collect(),
        ut: (0..MAX_TASKS).map(|i| sched::UTICKS[i].load(Ordering::Relaxed)).collect(),
        kt: (0..MAX_TASKS).map(|i| sched::KTICKS[i].load(Ordering::Relaxed)).collect(),
        sw: (0..MAX_TASKS).map(|i| sched::SWITCHES_IN[i].load(Ordering::Relaxed)).collect(),
        sc: (0..MAX_TASKS).map(|i| crate::ring3::SYSCALLS_PER_TASK[i].load(Ordering::Relaxed)).collect(),
        kt_tsc: (0..MAX_TASKS).map(|i| sched::KTSC[i].load(Ordering::Relaxed)).collect(),
        user,
        yields: sched::t0_yield_sites(),
    }
}

static BASE: spin::Mutex<Option<Snap>> = spin::Mutex::new(None);

pub fn sched_stat(arg: &str) -> Vec<String> {
    let now = snap();
    if arg == "start" || BASE.lock().is_none() {
        *BASE.lock() = Some(now);
        let l = String::from("[sched-stat] window started (sched-stat prints what happened since)");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    let g = BASE.lock();
    let b = g.as_ref().unwrap();
    let mut out = Vec::new();
    let secs = ms_f(now.tsc - b.tsc) / 1000.0;
    let d = |a: &Vec<u64>, z: &Vec<u64>, i: usize| a[i].saturating_sub(z[i]);
    let total_run: u64 = (0..MAX_TASKS).map(|i| d(&now.run, &b.run, i)).sum::<u64>().max(1);
    let sw: u64 = (0..MAX_TASKS).map(|i| d(&now.sw, &b.sw, i)).sum();
    let sc: u64 = (0..MAX_TASKS).map(|i| d(&now.sc, &b.sc, i)).sum();
    let per_s = |x: u64| if secs > 0.0 { x as f64 / secs } else { 0.0 };
    out.push(format!(
        "[sched-stat] {secs:.1} s: {} ticks ({} with the desktop loop idle), desktop loop halted {:.0} ms, {:.0} switches/s, {:.0} syscalls/s",
        now.ticks - b.ticks, now.idle_ticks - b.idle_ticks, ms_f(now.t0_idle - b.t0_idle), per_s(sw), per_s(sc)
    ));
    let mut rows: Vec<usize> = (0..MAX_TASKS).filter(|&i| d(&now.run, &b.run, i) > 0).collect();
    rows.sort_by(|x, y| d(&now.run, &b.run, *y).cmp(&d(&now.run, &b.run, *x)));
    for &i in rows.iter().take(12) {
        out.push(format!(
            "[sched-stat]   t{i}{}: ran {:.1} % ({:.0} ms, in system calls and faults {:.0} ms), ticks user {} kernel {}, {:.0} syscalls/s, {:.0} switches/s",
            if now.user[i] || b.user[i] { "" } else { " (kernel task)" },
            d(&now.run, &b.run, i) as f64 * 100.0 / total_run as f64, ms_f(d(&now.run, &b.run, i)), ms_f(d(&now.kt_tsc, &b.kt_tsc, i)),
            d(&now.ut, &b.ut, i), d(&now.kt, &b.kt, i), per_s(d(&now.sc, &b.sc, i)), per_s(d(&now.sw, &b.sw, i))
        ));
    }
    // A slot whose task was a ring-3 task at either end, or that ran system calls.
    let (mut u, mut k, mut run, mut kt, mut n) = (0u64, 0u64, 0u64, 0u64, 0usize);
    for i in 1..MAX_TASKS {
        if now.user[i] || b.user[i] || d(&now.sc, &b.sc, i) > 0 {
            u += d(&now.ut, &b.ut, i);
            k += d(&now.kt, &b.kt, i);
            run += d(&now.run, &b.run, i);
            kt += d(&now.kt_tsc, &b.kt_tsc, i);
            if d(&now.run, &b.run, i) > 0 {
                n += 1;
            }
        }
    }
    out.push(format!(
        "[sched-stat] ring-3 tasks ({n} ran): run {:.0} ms, of it in system calls and demand faults {:.0} ms: kernel share {} % (ticks: user {u}, kernel {k}; a system call runs with IF=0 and is never sampled)",
        ms_f(run), ms_f(kt), if run > 0 { kt * 100 / run } else { 0 }
    ));
    let mut ys: Vec<(u64, u64)> = now
        .yields
        .iter()
        .map(|&(key, c)| (key, c - b.yields.iter().find(|y| y.0 == key).map(|y| y.1).unwrap_or(0)))
        .filter(|y| y.1 > 0)
        .collect();
    ys.sort_by(|x, y| y.1.cmp(&x.1));
    for (key, c) in ys.iter().take(8) {
        out.push(format!("[t0-yield] {} x{c} ({:.0}/s)", sched::site_name(*key), per_s(*c)));
    }
    for l in out.iter() {
        crate::serial_println!("{l}");
    }
    out
}

// ── hogs ───────────────────────────────────────────────────────────────────

static HOGS: spin::Mutex<Vec<(usize, u64)>> = spin::Mutex::new(Vec::new());

/// At least `n` parked hogs; returns the first `n` (task, counter address).
fn hogs(mem: &mut FrameAllocator, n: usize) -> Vec<(usize, u64)> {
    let mut g = HOGS.lock();
    while g.len() < n {
        match crate::ring3::spawn_hog(mem) {
            Some(h) => g.push(h),
            None => break,
        }
    }
    g.iter().take(n).copied().collect()
}

fn start(h: &[(usize, u64)]) {
    for &(t, _) in h {
        sched::start_parked(t);
    }
}

fn park_all() {
    let g = HOGS.lock().clone();
    for &(t, _) in g.iter() {
        sched::park(t);
        sched::set_nice(t, 0);
    }
}

/// Let the other tasks run for `n` ticks (the desktop task sleeps meanwhile).
fn wait_ticks(n: u64) {
    let end = crate::interrupts::ticks() + n;
    while crate::interrupts::ticks() < end {
        sched::sleep_ticks(1);
        sched::yield_now();
    }
}

/// `hogs N`: leave N hogs running (0 parks them all).
pub fn hogs_cmd(mem: &mut FrameAllocator, arg: &str) -> Vec<String> {
    let n: usize = arg.trim().parse().unwrap_or(0).min(16);
    park_all();
    let h = hogs(mem, n);
    start(&h);
    let l = format!("[hogs] {} ring-3 hog(s) running: tasks {:?}", h.len(), h.iter().map(|x| x.0).collect::<Vec<_>>());
    crate::serial_println!("{l}");
    alloc::vec![l]
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

/// `[fair]`: `n` hogs beside `churn proc <churn>` on the desktop task, `rounds`
/// times. The share is the run time of the desktop task plus that of the glibc
/// processes it starts, of all run time in the window (TSC, not ticks: a short
/// turn right after a tick is never sampled by a tick).
pub fn hogtest(mem: &mut FrameAllocator, line: &str) -> Vec<String> {
    let mut it = line.split_whitespace().skip(1);
    let a1 = it.next().unwrap_or("5");
    if a1 == "nice" {
        return fair_nice(mem);
    }
    let n: usize = a1.parse().unwrap_or(5).clamp(1, 16);
    let rounds: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(1).clamp(1, 100);
    let churn: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(8).clamp(1, 64);
    park_all();
    let h = hogs(mem, n);
    let mut out = Vec::new();
    let mut times = Vec::new();
    let mut shares = Vec::new();
    for r in 0..rounds {
        // A slot that got a new task in the window holds a process of the churn.
        let gen0: Vec<u64> = (0..MAX_TASKS).map(|i| sched::SPAWN_GEN[i].load(Ordering::Relaxed)).collect();
        let r0: Vec<u64> = (0..MAX_TASKS).map(sched::run_tsc_now).collect();
        start(&h);
        let t0 = rdtsc();
        crate::churn::proc_churn_n(mem, churn);
        let t1 = rdtsc();
        for &(t, _) in h.iter() {
            sched::park(t);
        }
        let r1: Vec<u64> = (0..MAX_TASKS).map(sched::run_tsc_now).collect();
        let fresh: Vec<bool> = (0..MAX_TASKS).map(|i| sched::SPAWN_GEN[i].load(Ordering::Relaxed) != gen0[i]).collect();
        let (mut desk, mut kids, mut hog, mut other) = (0u64, 0u64, 0u64, 0u64);
        for i in 0..MAX_TASKS {
            let dlt = r1[i].saturating_sub(r0[i]);
            if i == 0 {
                desk += dlt;
            } else if h.iter().any(|x| x.0 == i) {
                hog += dlt;
            } else if fresh[i] {
                kids += dlt;
            } else {
                other += dlt;
            }
        }
        let all = (desk + kids + hog + other).max(1) as f64;
        let secs = ms_f(t1 - t0) / 1000.0;
        let share = (desk + kids) as f64 * 100.0 / all;
        times.push(secs);
        shares.push(share);
        let l = format!(
            "[fair] hogtest {} + churn proc {churn}: {secs:.1} s, task 0 share {share:.0} % (desktop task {:.0} %, its glibc processes {:.0} %, hogs {:.0} %, other tasks {:.0} %){} -> {}",
            h.len(), desk as f64 * 100.0 / all, kids as f64 * 100.0 / all, hog as f64 * 100.0 / all, other as f64 * 100.0 / all,
            if rounds > 1 { format!(" round {}/{rounds}", r + 1) } else { String::new() },
            if share >= 50.0 { "OK ✓" } else { "FAILED" }
        );
        crate::serial_println!("{l}");
        out.push(l);
    }
    if rounds > 1 {
        let worst = times.iter().cloned().fold(0.0f64, f64::max);
        let low = shares.iter().cloned().fold(100.0f64, f64::min);
        let med = median(&mut times.clone());
        let l = format!(
            "[fair] {rounds} rounds of hogtest {} + churn proc {churn}: median {med:.1} s, worst {worst:.1} s (limit 12.6 s), lowest task 0 share {low:.0} % -> {}",
            h.len(), if worst <= 12.6 && low >= 50.0 { "OK ✓" } else { "FAILED" }
        );
        crate::serial_println!("{l}");
        out.push(l);
    }
    out
}

/// `[fair-nice]`: a nice-0 hog and a nice-5 hog for 3 s: their run times must
/// be 3:1 (+-15 %), the ratio of the weights 1024 and 335.
fn fair_nice(mem: &mut FrameAllocator) -> Vec<String> {
    park_all();
    let h = hogs(mem, 2);
    if h.len() < 2 {
        return alloc::vec![String::from("[fair-nice] no memory for two hogs -> SKIPPED ⬜")];
    }
    sched::set_nice(h[1].0, 5);
    let a0 = sched::run_tsc_now(h[0].0);
    let b0 = sched::run_tsc_now(h[1].0);
    start(&h);
    wait_ticks(300);
    park_all();
    let a = sched::run_tsc_now(h[0].0) - a0;
    let b = (sched::run_tsc_now(h[1].0) - b0).max(1);
    let ratio = a as f64 / b as f64;
    let want = 1024.0 / 335.0;
    let pass = (ratio / want - 1.0).abs() <= 0.15;
    let l = format!(
        "[fair-nice] nice 0 against nice 5 for 3 s: run time {:.0} ms against {:.0} ms = {ratio:.2}:1 (want {want:.2}:1 +-15 %) -> {}",
        ms_f(a), ms_f(b), if pass { "OK ✓" } else { "FAILED" }
    );
    crate::serial_println!("{l}");
    alloc::vec![l]
}

fn field(s: &str, key: &str) -> Option<u64> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `[wakelat]`: gwakelat (two threads pass a futex turn; the waker has run 2 ms
/// before the wake and runs 3 ms on after it) next to 4 hogs. Wake to run p50
/// < 1 ms and p99 < 12 ms, and the hogs keep >= 85 % of the CPU share they
/// have alone.
pub fn wakelat(mem: &mut FrameAllocator, arg: &str) -> Vec<String> {
    let rounds: usize = arg.trim().parse().unwrap_or(200).clamp(10, 5000);
    park_all();
    let h = hogs(mem, 4);
    start(&h);
    wait_ticks(20);
    // The hogs' share of the CPU (their run time on the TSC per wall time), alone
    // and beside gwakelat. Their counter rate varied with the load of the host
    // (79 % and 111 % "kept" for the same image), the run time does not.
    let hog_run = |h: &[(usize, u64)]| h.iter().map(|x| sched::run_tsc_now(x.0)).sum::<u64>();
    let (c0, t0) = (hog_run(&h), rdtsc());
    wait_ticks(300);
    let (c1, t1) = (hog_run(&h), rdtsc());
    let base = (c1 - c0) as f64 / (t1 - t0).max(1) as f64;
    let cpu_us = (per_tick() / 10_000).max(1);
    let rs = format!("{rounds}");
    let cs = format!("{cpu_us}");
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let (c2, t2) = (hog_run(&h), rdtsc());
    let (outp, exit) = crate::ring3::run_glibc(mem, crate::ring3::gwakelat_bytes(), crate::ring3::ldlinux_bytes(),
        &[b"/bin/gwakelat", rs.as_bytes(), cs.as_bytes()], &[b"PATH=/bin"], caps);
    let (c3, t3) = (hog_run(&h), rdtsc());
    park_all();
    crate::ring3::reap_dead(mem);
    let during = (c3 - c2) as f64 / (t3 - t2).max(1) as f64;
    let kept = if base > 0.0 { during * 100.0 / base } else { 0.0 };
    let line = outp.lines().find(|l| l.contains("WAKELAT:")).unwrap_or("");
    let g = |k: &str| field(line, k).unwrap_or(u64::MAX);
    let (w50, w99, wmax) = (g("wake_p50_us="), g("wake_p99_us="), g("wake_max_us="));
    let (r50, r99) = (g("rt_p50_us="), g("rt_p99_us="));
    let pass = exit == 77 && w50 < 1000 && w99 < 12_000 && kept >= 85.0;
    let l = format!(
        "[wakelat] two threads pass a futex turn next to 4 hogs, {} rounds (exit {exit}): wake to run p50 {:.2} ms p99 {:.2} ms max {:.2} ms, round trip p50 {:.2} ms p99 {:.2} ms; hogs kept {kept:.0} % of their CPU share -> {}",
        field(line, "rounds=").unwrap_or(0), w50 as f64 / 1000.0, w99 as f64 / 1000.0, wmax as f64 / 1000.0,
        r50 as f64 / 1000.0, r99 as f64 / 1000.0, if pass { "OK ✓" } else { "FAILED" }
    );
    crate::serial_println!("{l}");
    alloc::vec![l]
}

/// `smpuser [off|test|on]`: where ring-3 tasks may run (S3-S6).
pub fn smpuser_cmd(arg: &str) -> Vec<String> {
    if let Some(m) = crate::apuser::parse_mode(arg) {
        crate::apuser::set_mode(m);
    }
    let l = format!(
        "[smpuser] {} ({} AP(s) can run ring 3)",
        crate::apuser::mode_name(crate::apuser::mode()),
        crate::apuser::user_aps().len()
    );
    crate::serial_println!("{l}");
    alloc::vec![l]
}

/// Per-AP counters of the user-mode scheduler (bounces, AP user time).
fn ap_counters() -> [(u64, u64, u64, u64); 8] {
    use crate::apuser::*;
    core::array::from_fn(|c| {
        (
            ST_BOUNCE_SC[c].load(Ordering::Relaxed),
            ST_BOUNCE_PF[c].load(Ordering::Relaxed),
            ST_BOUNCE_OTHER[c].load(Ordering::Relaxed),
            ST_USER_TSC[c].load(Ordering::Relaxed),
        )
    })
}

/// `[ap-user]` (S3), `[cow-smp]` (S4): gapuser with worker threads pinned to the
/// APs in turn (`apuser [per-AP] [syscalls] [pages] [cow]`). Each worker computes
/// a checksum with SSE (and AVX2 where the CPU has it), makes the system calls
/// and touches fresh pages; the result must equal the main thread's run on the
/// BSP, and each worker must have seen its own AP's APIC id from ring 3.
pub fn apuser_test(mem: &mut FrameAllocator, line: &str) -> Vec<String> {
    let mut it = line.split_whitespace().skip(1);
    let per: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(2).clamp(1, 4);
    let nsys: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(10_000);
    let pages: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1_000);
    let cow = it.next() == Some("cow");
    let tag = if cow { "[cow-smp]" } else { "[ap-user]" };
    let aps = crate::apuser::user_aps();
    if aps.is_empty() {
        let l = format!("{tag} no application processor can run ring 3: nothing to place ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    // The main thread (the reference) stays on the BSP; the workers go to the APs
    // in turn; the fork child of the cow mode to the first AP.
    let mut plan = alloc::vec![usize::MAX];
    for k in 0..per * aps.len() {
        plan.push(aps[k % aps.len()]);
    }
    if cow {
        plan.push(aps[0]);
    }
    let n = per * aps.len();
    let mode0 = crate::apuser::mode();
    if mode0 == crate::apuser::OFF {
        crate::apuser::set_mode(crate::apuser::TEST);
    }
    let before = ap_counters();
    let t0 = rdtsc();
    crate::apuser::pin_plan(&plan);
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let (ns, ss, ps) = (format!("{n}"), format!("{nsys}"), format!("{pages}"));
    let mut argv: Vec<&[u8]> = alloc::vec![b"/bin/gapuser", ns.as_bytes(), ss.as_bytes(), ps.as_bytes()];
    if cow {
        argv.push(b"cow");
    }
    let (outp, exit) = crate::ring3::run_glibc(mem, crate::ring3::gapuser_bytes(), crate::ring3::ldlinux_bytes(), &argv, &[b"PATH=/bin"], caps);
    crate::apuser::pin_plan(&[]);
    crate::apuser::set_mode(mode0);
    crate::ring3::reap_dead(mem);
    let t1 = rdtsc();
    let after = ap_counters();
    let mut out = Vec::new();
    let (mut equal, mut on_ap, mut saw_bsp) = (0usize, 0usize, 0usize);
    let bsp_apic = crate::apic::lapic_id() as u64;
    for l in outp.lines().filter(|l| l.starts_with("APUSER thread ")) {
        let k: usize = l.split_whitespace().nth(2).and_then(|x| x.parse().ok()).unwrap_or(usize::MAX);
        let mask = l.split("apic-mask=").nth(1).and_then(|x| x.split_whitespace().next())
            .and_then(|x| u64::from_str_radix(x.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
        if l.ends_with(" ok") {
            equal += 1;
        }
        if let Some(&c) = plan.get(k + 1) {
            if c != usize::MAX && mask & (1u64 << (crate::apuser::apic_of(c) & 63)) != 0 {
                on_ap += 1;
            }
        }
        if mask & (1u64 << (bsp_apic & 63)) != 0 {
            saw_bsp += 1;
        }
        out.push(format!("{tag}   {l}"));
    }
    let (mut sc, mut pf, mut ot, mut us) = (0u64, 0u64, 0u64, 0u64);
    for c in 0..8 {
        sc += after[c].0 - before[c].0;
        pf += after[c].1 - before[c].1;
        ot += after[c].2 - before[c].2;
        us += after[c].3 - before[c].3;
    }
    let cow_ok = !cow || outp.lines().any(|l| l.starts_with("APUSER cow child=ok") && l.ends_with("parent-pages-changed=0"));
    if let Some(l) = outp.lines().find(|l| l.starts_with("APUSER cow")) {
        out.push(format!("{tag}   {l}"));
    }
    let pass = exit == 77 && equal == n && on_ap == n && cow_ok && sc >= n as u64 * nsys / 2;
    let l = format!(
        "{tag} {n} worker thread(s) pinned to {} AP(s): result equal to the BSP run {equal}/{n}, ran on its own AP (CPUID from ring 3) {on_ap}/{n}, also seen on the BSP {saw_bsp}; bounces: system calls {sc}, page faults {pf}, other {ot}; AP user time {:.0} ms of {:.0} ms; exit {exit} → {}",
        aps.len(), ms_f(us), ms_f(t1 - t0), if pass { "OK ✓" } else { "FAILED" }
    );
    out.push(l);
    for l in out.iter() {
        crate::serial_println!("{l}");
    }
    out
}

/// Run `f` with smpuser at least `test` (restored after).
fn with_test_mode<R>(f: impl FnOnce() -> R) -> R {
    let m0 = crate::apuser::mode();
    if m0 == crate::apuser::OFF {
        crate::apuser::set_mode(crate::apuser::TEST);
    }
    let r = f();
    crate::apuser::pin_plan(&[]);
    crate::apuser::set_mode(m0);
    r
}

fn tagged(tag: &str, outp: &str, key: &str) -> Vec<String> {
    outp.lines().filter(|l| l.starts_with(key)).map(|l| format!("{tag}   {l}")).collect()
}

/// `[tlb-smp]` (S4): gtlbsmp. Its reader thread runs on an AP and reads one page
/// while the main thread (BSP) gives that address new contents: MADV_DONTNEED,
/// an mmap(MAP_FIXED) over it, a copy-on-write break after a fork. The reader
/// must see the new contents within 1 ms, never the old frame (`tlbsmp [rounds]`).
pub fn tlbsmp_test(mem: &mut FrameAllocator, arg: &str) -> Vec<String> {
    let rounds: u64 = arg.trim().parse().unwrap_or(1000).clamp(1, 100_000);
    let aps = crate::apuser::user_aps();
    if aps.is_empty() {
        let l = String::from("[tlb-smp] no application processor can run ring 3: nothing to test ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    let cpu_us = (per_tick() / 10_000).max(1);
    let (cs, rs) = (format!("{cpu_us}"), format!("{rounds}"));
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let (outp, exit) = with_test_mode(|| {
        crate::apuser::pin_plan(&[usize::MAX, aps[0]]);
        crate::ring3::run_glibc(mem, crate::ring3::gtlbsmp_bytes(), crate::ring3::ldlinux_bytes(),
            &[b"/bin/gtlbsmp", cs.as_bytes(), rs.as_bytes()], &[b"PATH=/bin"], caps)
    });
    crate::ring3::reap_dead(mem);
    let mut out = tagged("[tlb-smp]", &outp, "TLBSMP mode=");
    let pass = exit == 77 && outp.lines().any(|l| l == "TLBSMP all ok");
    out.push(format!(
        "[tlb-smp] reader on cpu{}, {rounds} rounds per mode (dontneed, remap, cowbreak): new contents within 1 ms, never the old frame; exit {exit} → {}",
        aps[0], if pass { "OK ✓" } else { "FAILED" }
    ));
    for l in out.iter() {
        crate::serial_println!("{l}");
    }
    out
}

/// `[kill-smp]` (S4): gkillsmp. Rounds of a fork child with 8 threads spinning
/// on the APs, ended by exit_group (even rounds) or SIGKILL (odd rounds); the
/// task census must come back to where it started (`killsmp [rounds]`).
pub fn killsmp_test(mem: &mut FrameAllocator, arg: &str) -> Vec<String> {
    let rounds: u64 = arg.trim().parse().unwrap_or(1000).clamp(1, 100_000);
    let aps = crate::apuser::user_aps();
    if aps.is_empty() {
        let l = String::from("[kill-smp] no application processor can run ring 3: nothing to test ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    crate::ring3::reap_dead(mem);
    let live0 = sched::live_tasks();
    let in0: u64 = (0..8).map(|c| crate::apuser::ST_IN[c].load(Ordering::Relaxed)).sum();
    let rs = format!("{rounds}");
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_FILE | crate::ring3::CAP_PROC_INFO;
    let mut plan = alloc::vec![usize::MAX];
    plan.extend_from_slice(&aps);
    let (outp, exit) = with_test_mode(|| {
        crate::apuser::pin_cycle(&plan);
        crate::ring3::run_glibc(mem, crate::ring3::gkillsmp_bytes(), crate::ring3::ldlinux_bytes(),
            &[b"/bin/gkillsmp", rs.as_bytes()], &[b"PATH=/bin"], caps)
    });
    crate::ring3::reap_dead(mem);
    let live1 = sched::live_tasks();
    let in1: u64 = (0..8).map(|c| crate::apuser::ST_IN[c].load(Ordering::Relaxed)).sum();
    let mut out = tagged("[kill-smp]", &outp, "KILLSMP rounds=");
    let pass = exit == 77 && outp.lines().any(|l| l == "KILLSMP all ok") && live1 == live0 && in1 > in0;
    out.push(format!(
        "[kill-smp] {rounds} rounds of a child with 8 threads spinning on {} AP(s) ended by exit_group or SIGKILL: AP switch-ins {}, live tasks {live0} -> {live1}; exit {exit} → {}",
        aps.len(), in1 - in0, if pass { "OK ✓" } else { "FAILED" }
    ));
    for l in out.iter() {
        crate::serial_println!("{l}");
    }
    out
}

/// `[many-threads]` with every thread on the APs in turn (S4 criterion).
pub fn many_smp_test(mem: &mut FrameAllocator) -> Vec<String> {
    let aps = crate::apuser::user_aps();
    let mut plan = alloc::vec![usize::MAX];
    plan.extend_from_slice(&aps);
    let in0: u64 = (0..8).map(|c| crate::apuser::ST_IN[c].load(Ordering::Relaxed)).sum();
    with_test_mode(|| {
        crate::apuser::pin_cycle(&plan);
        crate::churn::many_threads_test(mem);
    });
    let in1: u64 = (0..8).map(|c| crate::apuser::ST_IN[c].load(Ordering::Relaxed)).sum();
    let l = format!("[many-threads] smpuser=test: threads pinned to {} AP(s) in turn, AP switch-ins {}", aps.len(), in1 - in0);
    crate::serial_println!("{l}");
    alloc::vec![l]
}

/// `[spread]` (S5): gspread, 8 CPU-bound threads of one process without system
/// calls, once with smpuser=off and once with smpuser=on: with the APs the run
/// must take at most 0.4 of the time, and every AP must have run user code
/// (`spread [threads] [units]`).
pub fn spread_test(mem: &mut FrameAllocator, line: &str) -> Vec<String> {
    let mut it = line.split_whitespace().skip(1);
    let n: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(8).clamp(1, 64);
    let units: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(2000);
    let aps = crate::apuser::user_aps();
    if aps.is_empty() {
        let l = String::from("[spread] no application processor can run ring 3: nothing to spread ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    let (ns, us) = (format!("{n}"), format!("{units}"));
    let caps = crate::ring3::CAP_CONSOLE | crate::ring3::CAP_PROC_INFO;
    let m0 = crate::apuser::mode();
    let mut wall = [0u64; 2];
    let mut ok = [false; 2];
    let mut per_ap = Vec::new();
    for (k, m) in [crate::apuser::OFF, crate::apuser::ON].into_iter().enumerate() {
        crate::apuser::set_mode(m);
        let u0: Vec<u64> = (0..8).map(|c| crate::apuser::ST_USER_TSC[c].load(Ordering::Relaxed)).collect();
        let t0 = rdtsc();
        let (outp, exit) = crate::ring3::run_glibc(mem, crate::ring3::gspread_bytes(), crate::ring3::ldlinux_bytes(),
            &[b"/bin/gspread", ns.as_bytes(), us.as_bytes()], &[b"PATH=/bin"], caps);
        wall[k] = rdtsc() - t0;
        ok[k] = exit == 77 && outp.contains("SPREAD threads=");
        if k == 1 {
            per_ap = aps.iter().map(|&c| crate::apuser::ST_USER_TSC[c].load(Ordering::Relaxed) - u0[c]).collect();
        }
        crate::ring3::reap_dead(mem);
    }
    crate::apuser::set_mode(m0);
    let ratio = wall[1] as f64 / wall[0].max(1) as f64;
    let all_used = per_ap.iter().all(|&t| t > 0);
    let pass = ok[0] && ok[1] && ratio <= 0.4 && all_used;
    let used: Vec<String> = aps.iter().zip(per_ap.iter()).map(|(c, t)| format!("cpu{c} {:.0} ms", ms_f(*t))).collect();
    let l = format!(
        "[spread] {n} threads x {units} units: smpuser=off {:.0} ms, smpuser=on {:.0} ms, ratio {ratio:.2} (want <= 0.40); user time on the APs: {} → {}",
        ms_f(wall[0]), ms_f(wall[1]), used.join(", "), if pass { "OK ✓" } else { "FAILED" }
    );
    crate::serial_println!("{l}");
    alloc::vec![l]
}

/// `[ap-idle]` (S6): with nothing to run, an AP takes fewer than 5 timer
/// interrupts in 10 s (`apidle [seconds]`).
pub fn apidle_test(arg: &str) -> Vec<String> {
    let secs: u64 = arg.trim().parse().unwrap_or(10).clamp(1, 120);
    let aps = crate::apuser::user_aps();
    if aps.is_empty() {
        let l = String::from("[ap-idle] no application processor: nothing to measure ⬜");
        crate::serial_println!("{l}");
        return alloc::vec![l];
    }
    let snap = || -> Vec<u64> {
        (0..8).map(|c| crate::apuser::ST_IDLE_TICKS[c].load(Ordering::Relaxed) + crate::apuser::ST_TICKS[c].load(Ordering::Relaxed)).collect()
    };
    let a = snap();
    wait_ticks(secs * 100);
    let b = snap();
    let per: Vec<u64> = aps.iter().map(|&c| b[c] - a[c]).collect();
    let pass = per.iter().all(|&n| n < 5);
    let txt: Vec<String> = aps.iter().zip(per.iter()).map(|(c, n)| format!("cpu{c} {n}")).collect();
    let l = format!("[ap-idle] timer interrupts per AP in {secs} s with nothing to run: {} (want < 5) → {}", txt.join(", "), if pass { "OK ✓" } else { "FAILED" });
    crate::serial_println!("{l}");
    alloc::vec![l]
}
