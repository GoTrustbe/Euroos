//! Self-tests of BUG-351, BUG-352 and BUG-355 (`[fork-cow-write]`,
//! `[fork-fault-wait]`, `[vdso-mono]`).
//!
//! `[fork-cow-write]` (BUG-351): `gaslrx fork`; at the first system call of the
//! fork child the kernel makes system calls of its own in that child, each
//! writing into a page the child still shares copy-on-write with its parent
//! (demand pages and arena pages): getrandom, clock_gettime, gettimeofday,
//! uname, sysinfo, getrlimit, getcwd, pipe2, read, socketpair, recvfrom,
//! getdents64, fstat (of a directory). Every call must succeed, the child must end up with its own
//! copy of the page, and the parent's frame must be unchanged. The bytes the
//! calls wrote are put back, so the child runs on as before.
//!
//! `[fork-fault-wait]` (BUG-352): `gaslrx fork`; at the first system call of the
//! fork child its return address becomes 0x10, so the child faults in ring 3
//! (an instruction fetch outside its memory) and memory isolation ends it. The
//! parent's `waitpid` must return within 10 s with "killed by SIGSEGV" (status
//! 11), not after the 60 s wait4 bound with nothing.
//!
//! `[vdso-mono]` (BUG-355, BUG-380, BUG-371): the vDSO clock (the data page
//! read with the very formula of the `vdso.so` in this kernel) against the
//! system call, over late timer ticks (interrupts held off 15 ms: one tick
//! arrives 1.5 periods late, as on a busy host), in a free run of 2 s, and with
//! reads stopped between their page loads and their rdtsc while a late tick
//! moves the anchor (a vCPU the host does not run). Every online AP reads the
//! page at the same time (some reads stopped for a tick period). MONOTONIC,
//! BOOTTIME and REALTIME must never go back, on one CPU or against an earlier
//! read of another CPU, by any amount, and the system call must lie between
//! two vDSO reads around it. A fork child and an exec'd child of `gaslrx exec`
//! must map the same clock frame (one clock for every process).

use crate::ring3;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use euromm::FrameAllocator;

static GASLR_ELF: &[u8] = include_bytes!("../../userland/glibc/gaslrx");

const OFF: u64 = 0x200;
/// What the hook does at the next system call of a fork child.
static MODE: AtomicU8 = AtomicU8::new(0);
const M_COW: u8 = 1;
const M_FAULT: u8 = 2;
const M_VDSO: u8 = 3;
static RESULT: spin::Mutex<String> = spin::Mutex::new(String::new());
/// The last child wait4 reaped: pid << 32 | status.
static LAST_REAP: AtomicU64 = AtomicU64::new(0);
static HOOK_PID: AtomicU64 = AtomicU64::new(0);
/// `[vdso-mono]`: children seen, of them with the shared clock frame mapped.
static VDSO_KIDS: AtomicU64 = AtomicU64::new(0);
static VDSO_KIDS_OK: AtomicU64 = AtomicU64::new(0);

pub fn note_reap(pid: u64, status: u32) {
    LAST_REAP.store(pid << 32 | status as u64, Ordering::Relaxed);
}

/// Called by `syscall_dispatch` before a system call of a fork child.
pub fn on_child_syscall(_num: u64) {
    match MODE.load(Ordering::Relaxed) {
        M_COW => {
            MODE.store(0, Ordering::Relaxed);
            let r = cow_battery();
            *RESULT.lock() = r;
        }
        M_FAULT => {
            MODE.store(0, Ordering::Relaxed);
            let _ = ring3::test_dispatch(39, 0, 0, 0, 0, 0);
            HOOK_PID.store(ring3::test_fork_pid(), Ordering::Relaxed);
            ring3::test_set_user_rip(0x10);
        }
        M_VDSO => {
            let cur = ring3::test_vdso_cur();
            if cur != 0 && VDSO_KIDS.load(Ordering::Relaxed) < 64 {
                VDSO_KIDS.fetch_add(1, Ordering::Relaxed);
                let pml4 = x86_64::registers::control::Cr3::read().0.start_address().as_u64();
                let e = crate::paging::leaf_entry_in(pml4, cur + 0x1000).unwrap_or(0);
                if e & 0x000F_FFFF_FFFF_F000 == ring3::test_vdso_frame() && e & 2 == 0 {
                    VDSO_KIDS_OK.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

fn run(falloc: &mut FrameAllocator, arg: &[u8]) -> (String, u64) {
    let caps = ring3::CAP_CONSOLE | ring3::CAP_FILE | ring3::CAP_PROC_INFO;
    let argv: Vec<&[u8]> = alloc::vec![b"/bin/gaslrx", arg];
    ring3::run_glibc(falloc, GASLR_ELF, ring3::ldlinux_bytes(), &argv, &[b"PATH=/bin"], caps)
}

pub fn selftest(falloc: &mut FrameAllocator) {
    let prev_demand = ring3::DEMAND_ENABLED.swap(true, Ordering::Relaxed);
    let prev_file = ring3::DEMAND_FILE_ENABLED.swap(true, Ordering::Relaxed);
    // BUG-351
    RESULT.lock().clear();
    MODE.store(M_COW, Ordering::Relaxed);
    let (out, _) = run(falloc, b"fork");
    MODE.store(0, Ordering::Relaxed);
    let r = core::mem::take(&mut *RESULT.lock());
    let child_line = out.lines().any(|l| l.starts_with("gaslr-fork: "));
    if r.is_empty() {
        crate::serial_println!("[fork-cow-write] the fork child made no system call: not measured ⬜");
    } else if r.starts_with("ok") && child_line {
        crate::serial_println!("[fork-cow-write] {} → OK ✓", &r[3..]);
    } else {
        crate::serial_println!("[fork-cow-write] {r}; child line {} → FAILED", if child_line { "printed" } else { "missing" });
    }
    // BUG-352
    LAST_REAP.store(0, Ordering::Relaxed);
    HOOK_PID.store(0, Ordering::Relaxed);
    MODE.store(M_FAULT, Ordering::Relaxed);
    let t0 = crate::interrupts::ticks();
    let (_out, _) = run(falloc, b"fork");
    let secs = (crate::interrupts::ticks() - t0) / 100;
    MODE.store(0, Ordering::Relaxed);
    let pid = HOOK_PID.load(Ordering::Relaxed);
    let reap = LAST_REAP.load(Ordering::Relaxed);
    let (rpid, st) = (reap >> 32, reap as u32);
    if pid == 0 {
        crate::serial_println!("[fork-fault-wait] the fork child made no system call: not measured ⬜");
    } else if rpid == pid && st & 0x7f == 11 && secs < 10 {
        crate::serial_println!("[fork-fault-wait] fork child pid {pid} faulted in ring 3: the parent's waitpid got status {st:#x} (killed by SIGSEGV), run {secs} s → OK ✓");
    } else {
        crate::serial_println!("[fork-fault-wait] fork child pid {pid} faulted in ring 3: last reap pid {rpid} status {st:#x}, run {secs} s (want status 0xb, < 10 s) → FAILED");
    }
    // BUG-355: one clock frame for every process (fork child and exec'd child)
    VDSO_KIDS.store(0, Ordering::Relaxed);
    VDSO_KIDS_OK.store(0, Ordering::Relaxed);
    MODE.store(M_VDSO, Ordering::Relaxed);
    let (xout, _) = run(falloc, b"exec");
    MODE.store(0, Ordering::Relaxed);
    // An embedded probe cannot exec (only a program from disk re-executes):
    // the exec'd children of Chromium show it ([aslr-layout] exec ... vdso).
    let exec_vdso = xout.lines().find(|l| l.starts_with("gaslr-exec: exe=")).map(|l| !l.ends_with("vdso=0"));
    ring3::DEMAND_FILE_ENABLED.store(prev_file, Ordering::Relaxed);
    ring3::DEMAND_ENABLED.store(prev_demand, Ordering::Relaxed);
    ring3::clear_demand_file_maps();
    vdso_mono(VDSO_KIDS.load(Ordering::Relaxed), VDSO_KIDS_OK.load(Ordering::Relaxed), exec_vdso);
}

// ---- BUG-351 -------------------------------------------------------------

fn cow_pages(pml4: u64) -> (Vec<u64>, Vec<u64>) {
    let cow = crate::paging::COW;
    let mut dz = Vec::new();
    ring3::test_dz_walk(pml4, &mut |va, pte| {
        if pte & 1 != 0 && pte & cow != 0 && dz.len() < 16 {
            dz.push(va);
        }
    });
    let mut ar = Vec::new();
    // ASLR S6: every segment of the arena (the gaps between them are not mapped).
    for s in ring3::test_arena().list() {
        let mut va = s.virt;
        while va < s.end() && ar.len() < 16 {
            if let Some(e) = crate::paging::leaf_entry_in(pml4, va) {
                if e & cow != 0 && e & 0x80 == 0 {
                    ar.push(va);
                }
            }
            va += 4096;
        }
    }
    (dz, ar)
}

struct Page {
    va: u64,
    frame: u64,
    copy: alloc::boxed::Box<[u8; 4096]>,
}

fn take(pml4: u64, pages: &mut Vec<u64>) -> Option<Page> {
    while let Some(va) = pages.pop() {
        let Some(e) = crate::paging::leaf_entry_in(pml4, va) else { continue };
        if e & crate::paging::COW == 0 {
            continue; // broken meanwhile
        }
        let frame = e & 0x000F_FFFF_FFFF_F000;
        let mut copy = alloc::boxed::Box::new([0u8; 4096]);
        // The parent's view of the page (its address space: in the child's,
        // an arena frame's identity address is the child's own arena page).
        if !ring3::test_read_as_parent(va, &mut copy) {
            continue;
        }
        return Some(Page { va, frame, copy });
    }
    None
}

/// After a call: the child has its own writable copy, the parent's frame is
/// unchanged; then the child's bytes are put back.
fn settle(pml4: u64, p: &Page) -> Result<(), &'static str> {
    let e = crate::paging::leaf_entry_in(pml4, p.va).unwrap_or(0);
    let mut r = Ok(());
    if e & crate::paging::COW != 0 || e & 2 == 0 {
        r = Err("still shared");
    } else if e & 0x000F_FFFF_FFFF_F000 == p.frame {
        r = Err("parent's frame written");
    } else {
        let mut nowb = alloc::boxed::Box::new([0u8; 4096]);
        let _ = ring3::test_read_as_parent(p.va, &mut nowb);
        let now = &nowb[..];
        if now != &p.copy[..] {
            let first = (0..4096).find(|&i| now[i] != p.copy[i]).unwrap_or(0);
            let last = (0..4096).rev().find(|&i| now[i] != p.copy[i]).unwrap_or(0);
            crate::serial_println!("[fork-cow-write]   diag {:#x}: frame {:#x} -> {:#x}, parent's bytes {first:#x}..={last:#x} changed", p.va, p.frame, e & 0x000F_FFFF_FFFF_F000);
            r = Err("parent's bytes changed");
        }
    }
    if e & crate::paging::COW == 0 && e & 2 != 0 {
        // SAFETY: the child's own writable copy, in this address space, inside
        // a system call (RFLAGS.AC set: user pages reachable).
        unsafe { core::ptr::copy_nonoverlapping(p.copy[OFF as usize..].as_ptr(), (p.va + OFF) as *mut u8, 4096 - OFF as usize) };
    }
    r
}

fn cow_battery() -> String {
    let _ = ring3::test_dispatch(39, 0, 0, 0, 0, 0); // the child's globals in
    let pml4 = x86_64::registers::control::Cr3::read().0.start_address().as_u64();
    let (mut dz, mut ar) = cow_pages(pml4);
    let (nd, na) = (dz.len(), ar.len());
    let mut ok = 0usize;
    let mut bad: Vec<String> = Vec::new();
    let names = ["getrandom", "clock_gettime", "gettimeofday", "uname", "sysinfo", "getrlimit", "getcwd",
                 "pipe2", "read", "socketpair", "recvfrom", "getdents64", "fstat"];
    for (zone, pages) in [("demand", &mut dz), ("arena", &mut ar)] {
        // Descriptors made by the calls (closed at the end).
        let mut fds: Vec<u64> = Vec::new();
        let (mut prd, mut pwr, mut sv0, mut sv1) = (u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        let mut dfd = u64::MAX;
        for name in names {
            let Some(p) = take(pml4, pages) else { break };
            let b = p.va + OFF;
            let src = p.va; // bytes to send: read from a shared page (no write)
            let (r, want): (u64, u64) = match name {
                "getrandom" => (ring3::test_dispatch(318, b, 16, 0, 0, 0), 16),
                "clock_gettime" => (ring3::test_dispatch(228, 1, b, 0, 0, 0), 0),
                "gettimeofday" => (ring3::test_dispatch(96, b, 0, 0, 0, 0), 0),
                "uname" => (ring3::test_dispatch(63, b, 0, 0, 0, 0), 0),
                "sysinfo" => (ring3::test_dispatch(99, b, 0, 0, 0, 0), 0),
                "getrlimit" => (ring3::test_dispatch(97, 7, b, 0, 0, 0), 0),
                "getcwd" => {
                    let r = ring3::test_dispatch(79, b, 256, 0, 0, 0);
                    (r, if (r as i64) > 0 { r } else { 1 })
                }
                "pipe2" => {
                    let r = ring3::test_dispatch(293, b, 0, 0, 0, 0);
                    if r == 0 {
                        // SAFETY: the child's own page now, mapped in this address space.
                        let v = unsafe { core::ptr::read_unaligned(b as *const [u32; 2]) };
                        prd = v[0] as u64;
                        pwr = v[1] as u64;
                        fds.extend([prd, pwr]);
                    }
                    (r, 0)
                }
                "read" if prd != u64::MAX => {
                    let w = ring3::test_dispatch(1, pwr, src, 8, 0, 0);
                    (if w == 8 { ring3::test_dispatch(0, prd, b, 8, 0, 0) } else { w }, 8)
                }
                // fstat on a pipe end answers EBADF here (BUG-370): the directory.
                "fstat" if dfd != u64::MAX => (ring3::test_dispatch(5, dfd, b, 0, 0, 0), 0),
                "socketpair" => {
                    let r = ring3::test_dispatch(53, 1, 1, 0, b, 0);
                    if r == 0 {
                        // SAFETY: as for pipe2.
                        let v = unsafe { core::ptr::read_unaligned(b as *const [u32; 2]) };
                        sv0 = v[0] as u64;
                        sv1 = v[1] as u64;
                        fds.extend([sv0, sv1]);
                    }
                    (r, 0)
                }
                "recvfrom" if sv0 != u64::MAX => {
                    let w = ring3::test_dispatch(1, sv0, src, 8, 0, 0);
                    (if w == 8 { ring3::test_dispatch(45, sv1, b, 8, 0, 0) } else { w }, 8)
                }
                "getdents64" => {
                    // The path "/" goes into the child's own copy of the page
                    // first (one write by the kernel), then the listing lands in
                    // the next shared page.
                    let path = p.va + 0xF00;
                    if !ring3::copy_to_user(path, b"/\0") {
                        ((-14i64) as u64, 0)
                    } else {
                        let fd = ring3::test_dispatch(257, (-100i64) as u64, path, 0x10000, 0, 0);
                        if (fd as i64) < 0 {
                            (fd, 0)
                        } else {
                            fds.push(fd);
                            dfd = fd;
                            match take(pml4, pages) {
                                Some(q) => {
                                    let r = ring3::test_dispatch(217, fd, q.va + OFF, 1024, 0, 0);
                                    let s = settle(pml4, &q);
                                    if let Err(e) = s {
                                        bad.push(alloc::format!("{name}@{zone}: {e}"));
                                    }
                                    (r, if (r as i64) > 0 { r } else { 1 })
                                }
                                None => (0, 0),
                            }
                        }
                    }
                }
                _ => {
                    pages.push(p.va); // not measured: page back
                    continue;
                }
            };
            if r != want || (r as i64) == -14 {
                crate::serial_println!("[fork-cow-write]   diag {name}@{zone} = {}: {}", r as i64, ring3::test_user_diag(b, 16));
            }
            let s = settle(pml4, &p);
            if r != want {
                bad.push(alloc::format!("{name}@{zone} = {}", r as i64));
            } else if let Err(e) = s {
                bad.push(alloc::format!("{name}@{zone}: {e}"));
            } else {
                ok += 1;
            }
        }
        for fd in fds {
            let _ = ring3::test_dispatch(3, fd, 0, 0, 0, 0);
        }
    }
    let head = alloc::format!(
        "fork child: {ok} system calls wrote into pages shared copy-on-write with the parent ({nd} demand, {na} arena pages found)");
    if bad.is_empty() && ok >= 13 {
        alloc::format!("ok {head}, each got its own copy, parent's bytes unchanged")
    } else {
        alloc::format!("{head}; refused or wrong: {}", bad.join(", "))
    }
}

// ---- BUG-355, BUG-380, BUG-371 -------------------------------------------

/// lfence; rdtsc; lfence: the TSC read after every earlier load and before
/// every later one (Linux `rdtsc_ordered`).
#[inline(always)]
fn rdtsc_ordered() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: lfence and rdtsc only read the TSC and order instructions.
    unsafe { core::arch::asm!("lfence", "rdtsc", "lfence", out("eax") lo, out("edx") hi, options(nostack, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

/// The interpolation of `vdso.c` before BUG-380: dt under 2^40 else none, at
/// most two ticks.
fn interp_old(base: u64, tsc0: u64, per: u64, now: u64) -> u64 {
    let mut dt_ns = 0u64;
    if per != 0 {
        let dt = now.wrapping_sub(tsc0);
        if dt < 1 << 40 {
            dt_ns = ((dt as u128 * per as u128) >> 20) as u64;
        }
        dt_ns = dt_ns.min(20_000_000);
    }
    base + dt_ns
}

/// The read of `vdso.c` before BUG-380: the page is loaded and checked, then
/// the TSC is read with a plain rdtsc, outside the seqlock. `mid` runs between
/// the check and the rdtsc (a vCPU that stops there). Also returns whether the
/// page changed in that window (the reader used an anchor that was replaced).
fn read_old(real: bool, mid: Option<&mut dyn FnMut()>) -> (u64, bool) {
    let d = ring3::test_vdso_frame() as *const u64;
    if d.is_null() {
        return (0, false);
    }
    let mut mid = mid;
    // SAFETY: the identity-mapped clock frame, read only.
    unsafe {
        loop {
            let s1 = d.read_volatile();
            let base = d.add(if real { 3 } else { 1 }).read_volatile();
            let tsc0 = d.add(5).read_volatile();
            let per = d.add(6).read_volatile();
            if d.read_volatile() != s1 || s1 & 1 != 0 {
                continue;
            }
            if let Some(m) = mid.take() {
                m();
            }
            let now = core::arch::x86_64::_rdtsc();
            return (interp_old(base, tsc0, per, now), d.read_volatile() != s1);
        }
    }
}

/// The read with the TSC inside the seqlock (as the Linux vDSO): load the
/// page, lfence; rdtsc; lfence, and only then check the sequence; a change in
/// between reads again. `mid` as in `read_old`.
fn read_ordered(real: bool, mid: Option<&mut dyn FnMut()>) -> u64 {
    let d = ring3::test_vdso_frame() as *const u64;
    if d.is_null() {
        return 0;
    }
    let mut mid = mid;
    // SAFETY: the identity-mapped clock frame, read only.
    unsafe {
        loop {
            let s1 = d.read_volatile();
            if s1 & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let base = d.add(if real { 3 } else { 1 }).read_volatile();
            let tsc0 = d.add(5).read_volatile();
            let per = d.add(6).read_volatile();
            if let Some(m) = mid.take() {
                m();
            }
            let now = rdtsc_ordered();
            if d.read_volatile() != s1 {
                continue;
            }
            return ring3::test_vdso_interp(base, tsc0, per, now);
        }
    }
}

/// Does the `vdso.so` in this kernel read the TSC as `lfence; rdtsc; lfence`
/// at every rdtsc? Then its loop is that of `read_ordered`, else that of
/// `read_old`. (ordered, ordered rdtsc count, bare rdtsc count)
fn vdso_so_ordered() -> (bool, usize, usize) {
    let b = ring3::test_vdso_so();
    const PAT: [u8; 8] = [0x0f, 0xae, 0xe8, 0x0f, 0x31, 0x0f, 0xae, 0xe8];
    let (mut ord, mut bare) = (0, 0);
    for i in 0..b.len().saturating_sub(1) {
        if b[i] == 0x0f && b[i + 1] == 0x31 {
            if i >= 3 && i + 5 <= b.len() && b[i - 3..i + 5] == PAT {
                ord += 1;
            } else {
                bare += 1;
            }
        }
    }
    (ord > 0 && bare == 0, ord, bare)
}

/// The vDSO read, as the `vdso.so` of this kernel does it.
pub fn vdso_read(real: bool) -> u64 {
    read_as(real, None)
}

fn read_as(real: bool, mid: Option<&mut dyn FnMut()>) -> u64 {
    if VDSO_ORDERED.load(Ordering::Relaxed) {
        read_ordered(real, mid)
    } else {
        read_old(real, mid).0
    }
}

static VDSO_ORDERED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The largest MONOTONIC value any CPU has read so far: a read that started
/// after it was stored must not be below it (one clock for all CPUs).
static GMAX: AtomicU64 = AtomicU64::new(0);
static AP_RUN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static AP_DONE: AtomicU64 = AtomicU64::new(0);
/// AP readers: reads, stalled reads, steps back on their own CPU (count, max
/// ns), reads below a value another CPU had read before (count, max ns).
static AP_STATS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

#[derive(Clone, Copy, Default)]
struct Track {
    last: u64,
    backs: u64,
    max: u64,
}

impl Track {
    fn see(&mut self, v: u64) -> bool {
        let back = v < self.last;
        if back {
            self.backs += 1;
            self.max = self.max.max(self.last - v);
        }
        self.last = v;
        back
    }
}

/// An AP reads the clock page as the vDSO does until `AP_RUN` ends; every
/// 512th read stops for one tick period between its page loads and its rdtsc.
fn ap_reader(per: u64) {
    let (mut own, mut x) = (Track::default(), Track::default());
    let (mut n, mut stalls) = (0u64, 0u64);
    while AP_RUN.load(Ordering::Acquire) {
        n += 1;
        let g = GMAX.load(Ordering::Acquire);
        let v = if n % 512 == 0 && per != 0 {
            stalls += 1;
            let mut stall = || {
                let t = unsafe { core::arch::x86_64::_rdtsc() };
                while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t) < per {
                    core::hint::spin_loop();
                }
            };
            read_as(false, Some(&mut stall))
        } else {
            read_as(false, None)
        };
        own.see(v);
        if v < g {
            x.backs += 1;
            x.max = x.max.max(g - v);
        }
        GMAX.fetch_max(v, Ordering::AcqRel);
    }
    AP_STATS[0].fetch_add(n, Ordering::Relaxed);
    AP_STATS[1].fetch_add(stalls, Ordering::Relaxed);
    AP_STATS[2].fetch_add(own.backs, Ordering::Relaxed);
    AP_STATS[3].fetch_max(own.max, Ordering::Relaxed);
    AP_STATS[4].fetch_add(x.backs, Ordering::Relaxed);
    AP_STATS[5].fetch_max(x.max, Ordering::Relaxed);
}

struct Mono {
    clk: [Track; 3], // as vdso.so: mono, boot, real
    x: Track,        // BSP reads below a value an AP had read before
    order: u64,      // a system call outside the two vDSO reads around it
    order_max: u64,
    old: Track,      // diagnosis: the read of vdso.c before BUG-380 (mono)
    old_stale: u64,  // ... its steps back right after a read that took its TSC after a new anchor
    old_prev_stale: bool,
    ord: Track,      // diagnosis: the read with rdtsc inside the seqlock (mono)
    stalled: u64,
}

impl Mono {
    fn sample(&mut self, mid: Option<&mut dyn FnMut()>) {
        let g = GMAX.load(Ordering::Acquire);
        let v1 = read_as(false, mid);
        if v1 < g {
            self.x.backs += 1;
            self.x.max = self.x.max.max(g - v1);
        }
        let s = ring3::clock_ns(1);
        let v2 = read_as(false, None);
        let b1 = read_as(false, None); // BOOTTIME is served as MONOTONIC
        let sb = ring3::clock_ns(7);
        let b2 = read_as(false, None);
        let r1 = read_as(true, None);
        let sr = ring3::clock_ns(0);
        let r2 = read_as(true, None);
        GMAX.fetch_max(v2, Ordering::AcqRel);
        for (i, a, b) in [(0, v1, v2), (1, b1, b2), (2, r1, r2)] {
            self.clk[i].see(a);
            self.clk[i].see(b);
        }
        for (lo, x, hi) in [(v1, s, v2), (b1, sb, b2), (r1, sr, r2)] {
            if x < lo || x > hi {
                self.order += 1;
                self.order_max = self.order_max.max(lo.saturating_sub(x).max(x.saturating_sub(hi)));
            }
        }
        self.diag(None, None);
    }

    /// The two formulas side by side (diagnosis of BUG-380, not the verdict).
    fn diag(&mut self, old_mid: Option<&mut dyn FnMut()>, ord_mid: Option<&mut dyn FnMut()>) {
        let (o, stale) = read_old(false, old_mid);
        // A read that took its TSC after the page changed ran the old rate
        // past the new anchor: the step back shows at the read after it.
        if self.old.see(o) && self.old_prev_stale {
            self.old_stale += 1;
        }
        self.old_prev_stale = stale;
        let q = read_ordered(false, ord_mid);
        self.ord.see(q);
    }
}

fn vdso_mono(kids: u64, kids_ok: u64, exec_vdso: Option<bool>) {
    if ring3::test_vdso_frame() == 0 {
        crate::serial_println!("[vdso-mono] no vDSO clock page in this boot: not measured ⬜");
        return;
    }
    let (ordered, n_ord, n_bare) = vdso_so_ordered();
    VDSO_ORDERED.store(ordered, Ordering::Relaxed);
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed);
    let (w_nested0, w_ap0, w_tscback0) = ring3::vdso_writer_stats();
    let mut m = Mono { clk: [Track::default(); 3], x: Track::default(), order: 0, order_max: 0, old: Track::default(),
                       old_stale: 0, old_prev_stale: false, ord: Track::default(), stalled: 0 };
    // Every AP reads the same page at the same time.
    for s in &AP_STATS {
        s.store(0, Ordering::Relaxed);
    }
    AP_DONE.store(0, Ordering::Relaxed);
    AP_RUN.store(true, Ordering::Release);
    let mut aps = 0u64;
    for cpu in crate::kwork::online_aps() {
        let job = alloc::boxed::Box::new(move || {
            ap_reader(per);
            AP_DONE.fetch_add(1, Ordering::Release);
        });
        if crate::kwork::submit(cpu, job).is_ok() {
            aps += 1;
        }
    }
    let tick_edge = || {
        let t = crate::interrupts::ticks();
        while crate::interrupts::ticks() == t {
            core::hint::spin_loop();
        }
    };
    // A. Late ticks: interrupts off for 1.5 periods right after a tick.
    let mut late = 0;
    if per != 0 {
        for _ in 0..16 {
            tick_edge();
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            x86_64::instructions::interrupts::disable();
            while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0) < per + per / 2 {
                m.sample(None);
            }
            let t1 = crate::interrupts::ticks();
            x86_64::instructions::interrupts::enable();
            let end = unsafe { core::arch::x86_64::_rdtsc() } + per * 2;
            while unsafe { core::arch::x86_64::_rdtsc() } < end {
                m.sample(None);
            }
            if crate::interrupts::ticks() > t1 {
                late += 1;
            }
        }
    }
    // B. Free run, 2 s.
    let end = crate::interrupts::ticks() + 200;
    let mut n = 0u64;
    while crate::interrupts::ticks() < end {
        m.sample(None);
        n += 1;
    }
    // C. A reader stopped between its page loads and its rdtsc while a late
    // tick moves the anchor (a vCPU the host does not run, BUG-380): right
    // after a tick, interrupts off for 1.5 periods, on again, 2 ms more, then
    // the rdtsc. Each round stalls one read: as vdso.so, the old formula, the
    // ordered one; the reads after it must not be lower.
    if per != 0 {
        for k in 0..48 {
            tick_edge();
            let mut stall = || {
                x86_64::instructions::interrupts::disable();
                let t0 = unsafe { core::arch::x86_64::_rdtsc() };
                while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0) < per + per / 2 {
                    core::hint::spin_loop();
                }
                x86_64::instructions::interrupts::enable();
                let t1 = unsafe { core::arch::x86_64::_rdtsc() };
                while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t1) < per / 5 {
                    core::hint::spin_loop();
                }
            };
            match k % 3 {
                0 => {
                    m.stalled += 1;
                    m.sample(Some(&mut stall));
                }
                1 => m.diag(Some(&mut stall), None),
                _ => m.diag(None, Some(&mut stall)),
            }
            for _ in 0..64 {
                m.sample(None);
            }
        }
    }
    AP_RUN.store(false, Ordering::Release);
    let wait_end = crate::interrupts::ticks() + 300;
    while AP_DONE.load(Ordering::Acquire) < aps && crate::interrupts::ticks() < wait_end {
        core::hint::spin_loop();
    }
    let aps_done = AP_DONE.load(Ordering::Acquire);
    let ap: [u64; 6] = core::array::from_fn(|i| AP_STATS[i].load(Ordering::Relaxed));
    let (behind, behind_max) = ring3::vdso_behind_stats();
    let (w_nested, w_ap, w_tscback) = ring3::vdso_writer_stats();
    let ahead = vdso_read(false).saturating_sub(crate::interrupts::ticks() * 10_000_000);
    let backs = m.clk.iter().map(|t| t.backs).sum::<u64>();
    crate::serial_println!(
        "[vdso-mono]   diag: vdso.so reads the TSC ordered inside the seqlock: {} ({n_ord} ordered, {n_bare} bare rdtsc); old read (rdtsc after the check): steps back {} (max {} ns), {} of them right after a read that took its TSC after the anchor changed; ordered read (lfence; rdtsc; lfence inside the seqlock): steps back {} (max {} ns); writer: entered while another was running {} times, on an AP {} times, anchor tsc below the previous {} times",
        if ordered { "yes" } else { "no" }, m.old.backs, m.old.max, m.old_stale, m.ord.backs, m.ord.max,
        w_nested - w_nested0, w_ap - w_ap0, w_tscback - w_tscback0);
    let line = alloc::format!(
        "{late} late ticks of 1.5 periods, a 2 s run ({n} rounds), {} stalled reads: steps back mono {} ns, boot {} ns, real {} ns ({backs} times); system call outside its two vDSO reads {} times (max {} ns); below an earlier read of another CPU: BSP {} times (max {} ns); {aps_done} of {aps} APs read along ({} reads, {} stalled): steps back {} times (max {} ns), below an earlier read of another CPU {} times (max {} ns); tick anchor behind the readers {behind} times (max {} us), clock ahead of the tick count now {} us; clock frame shared by {kids_ok} of {kids} child calls (fork and exec), exec'd child has a vDSO: {}",
        m.stalled, m.clk[0].max, m.clk[1].max, m.clk[2].max, m.order, m.order_max, m.x.backs, m.x.max,
        ap[0], ap[1], ap[2], ap[3], ap[4], ap[5], behind_max / 1000, ahead / 1000,
        match exec_vdso { Some(true) => "yes", Some(false) => "no", None => "not measured (the probe cannot exec)" });
    let ok = backs == 0 && m.order == 0 && m.x.backs == 0 && ap[2] == 0 && ap[4] == 0 && aps_done == aps
        && late > 0 && m.stalled > 0 && kids > 0 && kids_ok == kids && exec_vdso != Some(false);
    crate::serial_println!("[vdso-mono] {line} → {}", if ok { "OK ✓" } else { "FAILED" });
}
