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
//! `[vdso-mono]` (BUG-355): the vDSO clock (the data page read with the very
//! formula of `userland/glibc/vdso.c`) against the system call, over late timer
//! ticks (interrupts held off 15 ms: one tick arrives 1.5 periods late, as on a
//! busy host) and in a free run of 2 s: MONOTONIC, BOOTTIME and REALTIME must
//! never go back, and the system call must lie between two vDSO reads around it.
//! A fork child and an exec'd child of `gaslrx exec` must map the same clock
//! frame (one clock for every process).

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
    let (base, span) = ring3::test_arena();
    let mut va = base;
    while va < base + span && ar.len() < 16 {
        if let Some(e) = crate::paging::leaf_entry_in(pml4, va) {
            if e & cow != 0 && e & 0x80 == 0 {
                ar.push(va);
            }
        }
        va += 4096;
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

// ---- BUG-355 -------------------------------------------------------------

/// The vDSO's read, as `userland/glibc/vdso.c` `read_clock` does it.
pub fn vdso_read(real: bool) -> u64 {
    let d = ring3::test_vdso_frame() as *const u64;
    if d.is_null() {
        return 0;
    }
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
            let mut dt_ns = 0u64;
            if per != 0 {
                let dt = core::arch::x86_64::_rdtsc().wrapping_sub(tsc0);
                if dt < 1 << 40 {
                    dt_ns = ((dt as u128 * per as u128) >> 20) as u64;
                }
                dt_ns = dt_ns.min(20_000_000);
            }
            return base + dt_ns;
        }
    }
}

struct Mono {
    back: [u64; 3], // largest step back: mono, boot, real
    backs: u64,
    // Steps back under 1 us: vdso.c reads the TSC with a plain rdtsc, which
    // the CPU may run ahead of the loads before it (BUG-371).
    small: u64,
    small_max: u64,
    order: u64,     // a system call outside the two vDSO reads around it, by more than 1 us
    order_max: u64,
    jitter: u64,    // ... by less (rdtsc does not wait for earlier instructions)
    last: [u64; 3],
}

impl Mono {
    fn sample(&mut self) {
        let v1 = vdso_read(false);
        let s = ring3::clock_ns(1);
        let v2 = vdso_read(false);
        let b = vdso_read(false); // BOOTTIME is served as MONOTONIC
        let sb = ring3::clock_ns(7);
        let r1 = vdso_read(true);
        let sr = ring3::clock_ns(0);
        let r2 = vdso_read(true);
        for (i, now) in [v1, b, r1].into_iter().enumerate() {
            if now < self.last[i] {
                let by = self.last[i] - now;
                if by > 1000 {
                    self.backs += 1;
                    self.back[i] = self.back[i].max(by);
                } else {
                    self.small += 1;
                    self.small_max = self.small_max.max(by);
                }
            }
            self.last[i] = now;
        }
        for (lo, x, hi) in [(v1, s, v2), (b, sb, vdso_read(false)), (r1, sr, r2)] {
            if x < lo || x > hi {
                let by = lo.saturating_sub(x).max(x.saturating_sub(hi));
                if by > 1000 {
                    self.order += 1;
                    self.order_max = self.order_max.max(by);
                } else {
                    self.jitter += 1;
                }
            }
        }
        // after the last reads of each kind, a later read is later still
        self.last[0] = self.last[0].max(v2);
        self.last[2] = self.last[2].max(r2);
    }
}

fn vdso_mono(kids: u64, kids_ok: u64, exec_vdso: Option<bool>) {
    if ring3::test_vdso_frame() == 0 {
        crate::serial_println!("[vdso-mono] no vDSO clock page in this boot: not measured ⬜");
        return;
    }
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed);
    let mut m = Mono { back: [0; 3], backs: 0, small: 0, small_max: 0, order: 0, order_max: 0, jitter: 0, last: [0; 3] };
    // A. Late ticks: interrupts off for 1.5 periods right after a tick.
    let mut late = 0;
    if per != 0 {
        for _ in 0..16 {
            let t = crate::interrupts::ticks();
            while crate::interrupts::ticks() == t {
                core::hint::spin_loop();
            }
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            x86_64::instructions::interrupts::disable();
            while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(t0) < per + per / 2 {
                m.sample();
            }
            let t1 = crate::interrupts::ticks();
            x86_64::instructions::interrupts::enable();
            let end = unsafe { core::arch::x86_64::_rdtsc() } + per * 2;
            while unsafe { core::arch::x86_64::_rdtsc() } < end {
                m.sample();
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
        m.sample();
        n += 1;
    }
    let (behind, behind_max) = ring3::vdso_behind_stats();
    let ahead = vdso_read(false).saturating_sub(crate::interrupts::ticks() * 10_000_000);
    let us = |ns: u64| ns / 1000;
    let line = alloc::format!(
        "{late} late ticks of 1.5 periods and a 2 s run ({n} rounds): steps back mono {} us, boot {} us, real {} us ({} times; under 1 us {} times, max {} ns); system call outside its two vDSO reads {} times (max {} us; by under 1 us {}); tick anchor behind the readers {behind} times (max {} us), clock ahead of the tick count now {} us; clock frame shared by {kids_ok} of {kids} child calls (fork and exec), exec'd child has a vDSO: {}",
        us(m.back[0]), us(m.back[1]), us(m.back[2]), m.backs, m.small, m.small_max, m.order, us(m.order_max), m.jitter, us(behind_max), us(ahead),
        match exec_vdso { Some(true) => "yes", Some(false) => "no", None => "not measured (the probe cannot exec)" });
    let ok = m.backs == 0 && m.order == 0 && late > 0 && kids > 0 && kids_ok == kids && exec_vdso != Some(false);
    crate::serial_println!("[vdso-mono] {line} → {}", if ok { "OK ✓" } else { "FAILED" });
}
