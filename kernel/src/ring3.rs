//! Ring-3 userspace + real syscalls (Track 3.4).
//!
//! A userspace program (loaded from EuroFS) runs in **ring 3** and invokes
//! syscalls via `SYSCALL`:
//!   - `sys_write(ptr, len)` (nr 1): write text to the kernel console
//!   - `sys_exit(code)`      (nr 0): stop the program, back to the kernel
//!
//! `sys_write` returns to ring 3 via `SYSRET` (the program keeps running);
//! `sys_exit` returns to the kernel caller. Privilege separation + a
//! real syscall round-trip, with a program that comes from disk.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use alloc::string::String;
use alloc::vec::Vec;
use euromm::FrameAllocator;
use spin::Mutex;

// ── Capability-based security (security-spec) ─────────────────────────────
// A process gets exactly the rights it needs; the kernel enforces
// this at the syscall boundary (least privilege, no root/non-root).
pub const CAP_CONSOLE: u64 = 1 << 0; // write to console
pub const CAP_PROC_INFO: u64 = 1 << 1; // getpid/uname
pub const CAP_FILE: u64 = 1 << 2; // open/read/close
pub const CAP_NET: u64 = 1 << 3; // network access
pub const CAP_IMMUTABLE_ADMIN: u64 = 1 << 4; // L2: set/clear immutability flags

static CURRENT_CAPS: AtomicU64 = AtomicU64::new(0);
// If true: the current process uses the LINUX syscall ABI (different numbers +
// semantics). The kernel then translates to its own handlers (Track 6 phase 6.6).
static LINUX_ABI: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// App identity of the running process (argv[0], e.g. "/bin/msock"). EuroGuard
// (Track 7) uses this to attribute policy decisions, statistics and audit events
// to the right app.
static CURRENT_APP: Mutex<String> = Mutex::new(String::new());

/// The app identity of the current ring-3 process (for EuroGuard).
pub fn current_app() -> String {
    CURRENT_APP.lock().clone()
}

// Userspace heap (for sbrk/malloc): break pointer + limit.
static HEAP_BREAK: AtomicU64 = AtomicU64::new(0);
static HEAP_END: AtomicU64 = AtomicU64::new(0);

/// Virtual base of the 2 MiB arena of the running ring-3 process (audit C1).
/// Set at program start; used to validate user pointers before the kernel
/// dereferences them, so a process cannot make the kernel read/write kernel memory.
static ARENA_BASE: AtomicU64 = AtomicU64::new(0);
/// The arena is 2 MiB in size (same `MIB2` as the paging layer).
const ARENA_SPAN: u64 = 2 * 1024 * 1024;

/// Does `[ptr, ptr+len)` lie entirely within the arena of the running process?
/// (Overflow-safe. If no arena has been set yet — a purely kernel-internal call —
/// we allow it.)
/// Recover mmap's 6th argument (`off`) from the syscall trampoline's saved
/// register block. The trampoline pushes registers in a fixed order before
/// remapping the sysv args; the original `r9` (Linux syscall arg6) lands at
/// `SAVED_REGS + 40` (r15@0, r14@8, r13@16, r12@24, r10@32, r9@40). A bogus/huge
/// value (no active saved block) is clamped to 0 by the caller's bounds checks.
///
/// # Safety
/// Reads a kernel-owned saved-register slot; only meaningful inside a syscall.
unsafe fn recover_mmap_offset() -> u64 {
    let regs = SAVED_REGS;
    if regs == 0 {
        return 0;
    }
    core::ptr::read_volatile((regs + 40) as *const u64)
}

fn in_user_arena(ptr: u64, len: usize) -> bool {
    let base = ARENA_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return true; // no ring-3 context active
    }
    let top = base + ARENA_SPAN;
    match ptr.checked_add(len as u64) {
        Some(end) => ptr >= base && end <= top,
        None => false,
    }
}

/// `-EFAULT`: returned by a syscall when a supplied user pointer does not lie
/// entirely within the arena of the running process.
const EFAULT: u64 = (-14i64) as u64;

// ── Safe user memory access ──────────────────────────────────────────────
// ONE gate into/out of userspace. Each function checks `[ptr, ptr+len)` with
// `in_user_arena` BEFORE it dereferences, so a process can never make the kernel
// read or overwrite kernel memory by passing a forged pointer. All
// syscall handlers that touch a user pointer MUST go through these helpers —
// never directly `as *mut`/`as *const` on a syscall argument.

/// Copy `src` to user address `dst`. `false` = pointer fails the arena check
/// (the caller then returns `-EFAULT`); nothing is written.
#[must_use]
fn copy_to_user(dst: u64, src: &[u8]) -> bool {
    if !in_user_arena(dst, src.len()) {
        return false;
    }
    // SAFETY: arena-validated; arena is identity-mapped and writable.
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    true
}

/// Read `len` bytes from user address `src`. `None` = pointer fails the arena check.
#[must_use]
fn copy_from_user(src: u64, len: usize) -> Option<alloc::vec::Vec<u8>> {
    if !in_user_arena(src, len) {
        return None;
    }
    let mut v = alloc::vec::Vec::with_capacity(len);
    // SAFETY: arena-validated; identity-mapped.
    unsafe {
        v.set_len(len);
        core::ptr::copy_nonoverlapping(src as *const u8, v.as_mut_ptr(), len);
    }
    Some(v)
}

/// Write `len` zero bytes to user address `dst`. `false` = arena check fails.
#[must_use]
fn zero_user(dst: u64, len: usize) -> bool {
    if !in_user_arena(dst, len) {
        return false;
    }
    // SAFETY: arena-validated; identity-mapped.
    unsafe { core::ptr::write_bytes(dst as *mut u8, 0, len) };
    true
}

/// Write a scalar (`u32`/`u64`/…) at user address `ptr`. `false` = arena check fails.
#[must_use]
fn write_user<T: Copy>(ptr: u64, val: T) -> bool {
    if !in_user_arena(ptr, core::mem::size_of::<T>()) {
        return false;
    }
    // SAFETY: arena-validated; `write_unaligned` requires no alignment.
    unsafe { (ptr as *mut T).write_unaligned(val) };
    true
}

/// Read a scalar from user address `ptr`. `None` = arena check fails.
#[must_use]
fn read_user<T: Copy>(ptr: u64) -> Option<T> {
    if !in_user_arena(ptr, core::mem::size_of::<T>()) {
        return None;
    }
    // SAFETY: arena-validated; `read_unaligned` requires no alignment.
    Some(unsafe { (ptr as *const T).read_unaligned() })
}

fn has_cap(c: u64) -> bool {
    CURRENT_CAPS.load(Ordering::Relaxed) & c == c
}

/// The capability a syscall requires (0 = always allowed).
fn required_cap(num: u64) -> u64 {
    match num {
        1 => CAP_CONSOLE,
        2 | 4 => CAP_PROC_INFO,
        20 | 21 | 22 => CAP_FILE,
        60 => CAP_NET,
        _ => 0, // exit (0) and the like always allowed
    }
}
use x86_64::registers::control::{Cr4, Cr4Flags};
use x86_64::registers::model_specific::Msr;

use crate::serial_print;

// Globals shared by the assembly stubs (single-threaded; userspace runs
// before the scheduler).
#[no_mangle]
static mut SAVED_KERNEL_RSP: u64 = 0; // return point for sys_exit
#[no_mangle]
static mut KERNEL_RSP: u64 = 0; // stack for the syscall handler
#[no_mangle]
static mut USER_RSP: u64 = 0; // saved user-rsp during a syscall
#[no_mangle]
static mut USER_RIP: u64 = 0; // saved user-rip (clone: thread resume point)
#[no_mangle]
static mut SAVED_REGS: u64 = 0; // points to the saved register block (clone: child inherits the regs)
#[no_mangle]
static mut EXITED: u64 = 0; // set by sys_exit

static mut EXIT_CODE: u64 = 0;
static OUTPUT: Mutex<String> = Mutex::new(String::new());

/// The system environment (environment variables) that every ring-3 process inherits
/// via `envp` on the SysV stack. Programs read this with `getenv()` (musl/libc).
static ENV: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// Set the system environment (replaces the current set). Set at boot.
pub fn set_env(vars: &[&str]) {
    let mut e = ENV.lock();
    e.clear();
    for v in vars {
        e.push(String::from(*v));
    }
}

/// Add a single environment variable "KEY=value" (or replace an existing one with
/// the same key). For runtime-determined values, e.g. a DNS result.
pub fn push_env(entry: &str) {
    let key = match entry.split_once('=') {
        Some((k, _)) => k,
        None => entry,
    };
    let mut e = ENV.lock();
    e.retain(|v| v.split_once('=').map(|(k, _)| k) != Some(key));
    e.push(String::from(entry));
}

/// Optional stdout redirection: if set, everything the process writes to fd 1/2
/// goes to this VFS file (index in FILES) instead of the console. This is how the
/// shell handles `prog > file` / `prog >> file` (redirection).
static STDOUT_REDIRECT: Mutex<Option<usize>> = Mutex::new(None);

// Minimal VFS for userspace file I/O: files (path, content) loaded from
// EuroFS, plus an open-file-descriptor table. Syscalls open/read/close operate on it.
static FILES: Mutex<alloc::vec::Vec<(String, alloc::vec::Vec<u8>)>> = Mutex::new(alloc::vec::Vec::new());
const MAX_FD: usize = 16;
static OPEN_FDS: Mutex<[Option<(usize, usize)>; MAX_FD]> = Mutex::new([None; MAX_FD]);
/// Open DIRECTORY fds (Linux getdents64): (normalized dir path, cursor in the
/// children list). Separate table so a dir fd is not read as a file.
static OPEN_DIRS: Mutex<[Option<(String, usize)>; MAX_FD]> =
    Mutex::new([const { None }; MAX_FD]);

// ── PIPES (S3 IPC between processes) ─────────────────────────────────────────
// A pipe is an in-kernel FIFO buffer with two ends (read/write). The
// `pipe2` syscall returns two fds; after fork() parent and child share them (the
// fd tables are global), so they can communicate over the pipe.
static PIPES: Mutex<alloc::vec::Vec<alloc::vec::Vec<u8>>> = Mutex::new(alloc::vec::Vec::new());
/// Pipe fds: per fd (pipe-id, is_write_end). Separate table alongside file/dir fds.
static PIPE_FDS: Mutex<[Option<(usize, bool)>; MAX_FD]> = Mutex::new([None; MAX_FD]);

/// pipe2(fds, flags): create a pipe; assign a read fd and a write fd and write
/// them to `fds[0]`/`fds[1]`. Returns 0 / -EMFILE.
fn pipe_create(user_fds: u64) -> u64 {
    let id = {
        let mut p = PIPES.lock();
        p.push(alloc::vec::Vec::new());
        p.len() - 1
    };
    let files = OPEN_FDS.lock();
    let dirs = OPEN_DIRS.lock();
    let mut pf = PIPE_FDS.lock();
    let mut got = [usize::MAX; 2];
    let mut k = 0;
    for fd in 3..MAX_FD {
        if pf[fd].is_none() && files[fd].is_none() && dirs[fd].is_none() {
            got[k] = fd;
            k += 1;
            if k == 2 {
                break;
            }
        }
    }
    if k < 2 {
        return (-24i64) as u64; // -EMFILE
    }
    // Validate the output pointer (int[2]) BEFORE we commit the fds, so a
    // forged `fds` cannot overwrite kernel memory.
    let fds = [got[0] as i32, got[1] as i32];
    if !in_user_arena(user_fds, 8) {
        return EFAULT;
    }
    pf[got[0]] = Some((id, false)); // read end
    pf[got[1]] = Some((id, true)); // write end
    let _ = write_user(user_fds, fds[0]);
    let _ = write_user(user_fds + 4, fds[1]);
    0
}

/// Write to a pipe fd (write end). None = `fd` is not a pipe write fd.
fn pipe_write_fd(fd: usize, bytes: &[u8]) -> Option<u64> {
    if fd >= MAX_FD {
        return None;
    }
    if let Some((id, true)) = PIPE_FDS.lock()[fd] {
        PIPES.lock()[id].extend_from_slice(bytes);
        return Some(bytes.len() as u64);
    }
    None
}

/// Read from a pipe fd (read end). Empty -> -EAGAIN (the reader polls). None = not a
/// pipe read fd.
fn pipe_read_fd(fd: usize, buf: u64, len: usize) -> Option<u64> {
    if fd >= MAX_FD {
        return None;
    }
    if let Some((id, false)) = PIPE_FDS.lock()[fd] {
        let mut pipes = PIPES.lock();
        let p = &mut pipes[id];
        if p.is_empty() {
            return Some((-11i64) as u64); // -EAGAIN
        }
        let n = len.min(p.len());
        // Validate the destination buffer BEFORE we consume from the pipe; on a
        // forged pointer the read fails without losing data or touching kernel
        // memory.
        if !in_user_arena(buf, n) {
            return Some(EFAULT);
        }
        let data: alloc::vec::Vec<u8> = p.drain(0..n).collect();
        let _ = copy_to_user(buf, &data);
        if let Ok(s) = core::str::from_utf8(&data) {
            crate::kinfo!("[pipe] fd {fd} read {n} bytes from pipe {id}: \"{s}\"");
        }
        return Some(n as u64);
    }
    None
}

/// Give the current process a FRESH fd table (fd 0/1/2 implicitly console/VFS).
/// In the synchronous foreground model one process runs at a time, so this gives
/// real per-process fd semantics: open fds do not leak between programs.
fn reset_fd_table() {
    *OPEN_FDS.lock() = [None; MAX_FD];
    for slot in OPEN_DIRS.lock().iter_mut() {
        *slot = None;
    }
}

/// Register a file (path + content) so userspace can read it via open/read.
pub fn register_file(path: &str, bytes: alloc::vec::Vec<u8>) {
    FILES.lock().push((String::from(path), bytes));
}

/// Program registry: per executable path the granted capabilities and the ABI
/// (native EuroOS or Linux). This lets a shell start a binary by NAME and lets
/// the kernel know with which rights + syscall ABI it must run.
static PROGRAMS: Mutex<alloc::vec::Vec<(String, u64, bool)>> = Mutex::new(alloc::vec::Vec::new());

/// Install an executable file: record caps + ABI for a later `exec`.
pub fn register_program(path: &str, caps: u64, linux_abi: bool) {
    let mut p = PROGRAMS.lock();
    if let Some(e) = p.iter_mut().find(|(q, _, _)| q == path) {
        e.1 = caps;
        e.2 = linux_abi;
    } else {
        p.push((String::from(path), caps, linux_abi));
    }
}

/// Look up the caps + ABI of an installed program (None = unknown).
pub fn program_caps_abi(path: &str) -> Option<(u64, bool)> {
    PROGRAMS
        .lock()
        .iter()
        .find(|(q, _, _)| q == path)
        .map(|(_, c, a)| (*c, *a))
}

/// All installed programs with their granted capabilities + ABI flag —
/// for the `caps` overview that the NATIVE EuroGuard security model shows.
pub fn program_list() -> alloc::vec::Vec<(String, u64, bool)> {
    PROGRAMS.lock().clone()
}

/// Decode a capability bitmask into readable names (EuroGuard rights).
pub fn cap_names(caps: u64) -> String {
    let mut v: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    if caps & CAP_CONSOLE != 0 {
        v.push("console");
    }
    if caps & CAP_PROC_INFO != 0 {
        v.push("procinfo");
    }
    if caps & CAP_FILE != 0 {
        v.push("file");
    }
    if caps & CAP_NET != 0 {
        v.push("net");
    }
    if v.is_empty() {
        v.push("(none)");
    }
    v.join(" ")
}

/// /proc synthesis (Track 8.2): generate the content of known /proc files LIVE
/// (version/cpuinfo/meminfo/uptime/self/maps) and place it in the VFS, so Linux
/// programs that read /proc get real values instead of -ENOENT. Returns true if
/// `path` is a /proc file that is now (freshly generated) in the VFS.
fn ensure_proc(path: &[u8]) -> bool {
    if !path.starts_with(b"/proc/") {
        return false;
    }
    let cores = crate::smp::AP_ONLINE.load(Ordering::Relaxed) + 1;
    let up = crate::interrupts::ticks() / 100;
    let (_used, free) = crate::allocator::stats();
    let content: alloc::vec::Vec<u8> = match path {
        b"/proc/version" => {
            b"Linux version 6.6.0-euroos (euro@eurokernel) (EuroToolchain rustc) #1 SMP EuroOS\n"
                .to_vec()
        }
        b"/proc/cpuinfo" => {
            let mut s = String::new();
            for i in 0..cores {
                s.push_str(&alloc::format!(
                    "processor\t: {i}\nvendor_id\t: EuroOS\nmodel name\t: EuroOS Virtual CPU @ LAPIC\n\
                     flags\t\t: fpu tsc msr apic sse sse2 long\ncores\t\t: {cores}\n\n"
                ));
            }
            s.into_bytes()
        }
        b"/proc/meminfo" => {
            // MemTotal: the QEMU RAM (256 MiB). MemFree/Available: live kernel-heap-free.
            let free_kb = free as u64 / 1024;
            alloc::format!(
                "MemTotal:       262144 kB\nMemFree:        {free_kb:>8} kB\n\
                 MemAvailable:   {free_kb:>8} kB\nBuffers:             0 kB\nCached:              0 kB\n"
            )
            .into_bytes()
        }
        b"/proc/uptime" => alloc::format!("{up}.00 {up}.00\n").into_bytes(),
        b"/proc/self/maps" => {
            // One line for the heap window of the current foreground process.
            let lo = HEAP_BREAK.load(Ordering::Relaxed) & !0xFFF;
            let hi = (HEAP_END.load(Ordering::Relaxed) + 0xFFF) & !0xFFF;
            alloc::format!("{lo:012x}-{hi:012x} rw-p 00000000 00:00 0          [heap]\n").into_bytes()
        }
        b"/proc/self/stat" => {
            alloc::format!("1 (prog) R 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {up} 0 0\n").into_bytes()
        }
        b"/proc/self/cmdline" => {
            // NUL-terminated argv (here only argv[0] = the program path).
            let mut v = CURRENT_APP.lock().clone().into_bytes();
            v.push(0);
            v
        }
        b"/proc/loadavg" => alloc::format!("0.00 0.00 0.00 1/{cores} 1\n").into_bytes(),
        b"/proc/stat" => {
            // Minimal cpu line + per-core lines (as tools like `top` read).
            let mut s = alloc::string::String::from("cpu  0 0 0 0 0 0 0 0 0 0\n");
            for i in 0..cores {
                s.push_str(&alloc::format!("cpu{i} 0 0 0 0 0 0 0 0 0 0\n"));
            }
            s.push_str(&alloc::format!("btime 0\nprocesses {cores}\n"));
            s.into_bytes()
        }
        _ => return false,
    };
    let p = String::from_utf8_lossy(path).into_owned();
    let mut files = FILES.lock();
    match files.iter_mut().find(|(q, _)| q.as_str() == p) {
        Some(e) => e.1 = content, // refresh existing /proc content
        None => files.push((p, content)),
    }
    true
}

/// Open a path (bytes) in the VFS -> fd, or u64::MAX if not found / full.
/// Shared by the native (sys_open) and Linux (openat) ABI.
fn vfs_open(path: &[u8]) -> u64 {
    ensure_proc(path); // freshly generate /proc files before the lookup
    let files = FILES.lock();
    match files.iter().position(|(p, _)| p.as_bytes() == path) {
        Some(fi) => {
            let mut fds = OPEN_FDS.lock();
            for (fd, slot) in fds.iter_mut().enumerate().skip(3) {
                if slot.is_none() {
                    *slot = Some((fi, 0));
                    return fd as u64;
                }
            }
            u64::MAX
        }
        None => u64::MAX,
    }
}

/// Read from an open fd into a user buffer -> number of bytes (u64::MAX on error).
fn vfs_read(fd: usize, buf: u64, len: usize) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let mut fds = OPEN_FDS.lock();
    let (fi, off) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    let files = FILES.lock();
    let data = &files[fi].1;
    let n = len.min(data.len().saturating_sub(off));
    // Validate the user buffer before the kernel writes into it (audit C1): a process
    // may not specify kernel memory as the destination.
    if !in_user_arena(buf, n) {
        return u64::MAX;
    }
    // SAFETY: buf now provably lies within the arena of the running process.
    unsafe {
        core::ptr::copy_nonoverlapping(data[off..].as_ptr(), buf as *mut u8, n);
    }
    fds[fd] = Some((fi, off + n));
    n as u64
}

/// Paths that userspace opened for writing — the shell writes these back to
/// EuroFS afterward (so they become persistent + appear in the file list).
static DIRTY: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// Open a path for WRITING: create it if it does not exist (O_CREAT),
/// truncate it on `truncate` (O_TRUNC). Marks the path as 'dirty'.
fn vfs_open_create(path: &[u8], truncate: bool) -> u64 {
    let name = String::from_utf8_lossy(path).into_owned();
    {
        let mut files = FILES.lock();
        match files.iter_mut().find(|(p, _)| p.as_bytes() == path) {
            Some((_, d)) => {
                if truncate {
                    d.clear();
                }
            }
            None => files.push((name.clone(), alloc::vec::Vec::new())),
        }
    }
    let mut dirty = DIRTY.lock();
    if !dirty.iter().any(|p| p == &name) {
        dirty.push(name);
    }
    vfs_open(path)
}

/// Write `len` bytes from a user buffer to an open fd (in the VFS); the file
/// grows as needed. Returns the number of bytes written.
fn vfs_write(fd: usize, buf: u64, len: usize) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let mut fds = OPEN_FDS.lock();
    let (fi, off) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    // Validate the user buffer + overflow-safe offset computation (audit C1/M9):
    // the kernel may only read from the arena of the running process.
    if !in_user_arena(buf, len) {
        return u64::MAX;
    }
    let end = match off.checked_add(len) {
        Some(e) => e,
        None => return u64::MAX,
    };
    let mut files = FILES.lock();
    let data = &mut files[fi].1;
    if end > data.len() {
        data.resize(end, 0);
    }
    // SAFETY: buf now provably lies within the arena of the running process.
    unsafe {
        core::ptr::copy_nonoverlapping(buf as *const u8, data[off..].as_mut_ptr(), len);
    }
    fds[fd] = Some((fi, end));
    len as u64
}

/// Fetch the paths+content that userspace wrote since the previous call (and clear
/// the list). The shell uses this to synchronize EuroFS after an `exec`.
pub fn take_dirty() -> alloc::vec::Vec<(String, alloc::vec::Vec<u8>)> {
    // BUG-007 class: FILES + DIRTY are also taken by syscall_dispatch (interrupts OFF via
    // FMASK). This task-context caller holds FILES across `d.clone()`; if a timer preempts
    // it mid-hold and switches to a bg-musl process whose file syscall takes FILES, that
    // syscall spins forever on the lock we still hold → deadlock. Hold them irqsave so no
    // preemption can occur while held (mirrors the reap_dead/BG fix).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let paths: alloc::vec::Vec<String> = core::mem::take(&mut *DIRTY.lock());
        let files = FILES.lock();
        paths
            .into_iter()
            .filter_map(|p| {
                files
                    .iter()
                    .find(|(q, _)| q == &p)
                    .map(|(_, d)| (p.clone(), d.clone()))
            })
            .collect()
    })
}

/// Redirect stdout (fd 1/2) to a VFS file for the duration of the next run
/// (shell redirection). `append`=true appends (`>>`), otherwise truncate (`>`).
/// `None` restores the console. The path becomes 'dirty' (the shell syncs it).
pub fn set_stdout_redirect(path: Option<&str>, append: bool) {
    // BUG-007 class: FILES/DIRTY/STDOUT_REDIRECT are also taken by syscall_dispatch with
    // interrupts off; hold them irqsave here so this task-context caller can't be preempted
    // mid-hold and deadlock a bg-musl file syscall spinning on the same lock.
    x86_64::instructions::interrupts::without_interrupts(|| match path {
        Some(p) => {
            let idx = {
                let mut files = FILES.lock();
                match files.iter().position(|(q, _)| q.as_str() == p) {
                    Some(i) => {
                        if !append {
                            files[i].1.clear();
                        }
                        i
                    }
                    None => {
                        files.push((String::from(p), alloc::vec::Vec::new()));
                        files.len() - 1
                    }
                }
            };
            let mut dirty = DIRTY.lock();
            if !dirty.iter().any(|q| q == p) {
                dirty.push(String::from(p));
            }
            *STDOUT_REDIRECT.lock() = Some(idx);
        }
        None => *STDOUT_REDIRECT.lock() = None,
    })
}

/// Append bytes to the stdout redirection file (internal, for write/writev).
fn redirect_append(fi: usize, bytes: &[u8]) {
    FILES.lock()[fi].1.extend_from_slice(bytes);
}

/// Standard input (fd 0): content + read position. The shell fills this with the
/// stdout of the previous program in a pipe (`a | b`); `read(0)` reads from it.
static STDIN: Mutex<(alloc::vec::Vec<u8>, usize)> = Mutex::new((alloc::vec::Vec::new(), 0));

/// Set the standard input for the next run (pipe). Empty slice = no input.
pub fn set_stdin(data: &[u8]) {
    let mut s = STDIN.lock();
    s.0 = data.to_vec();
    s.1 = 0;
}

/// Read from standard input into a user buffer (fd 0). Returns the number of bytes.
fn stdin_read(buf: u64, len: usize) -> u64 {
    let mut s = STDIN.lock();
    let off = s.1;
    let n = len.min(s.0.len().saturating_sub(off));
    if !copy_to_user(buf, &s.0[off..off + n]) {
        return EFAULT;
    }
    s.1 = off + n;
    n as u64
}

/// The number of bytes still in standard input (for fstat of fd 0).
fn stdin_len() -> usize {
    STDIN.lock().0.len()
}

// ── Background daemon: a ring-3 program that runs PREEMPTIVELY scheduled
// (not synchronously like run()) and periodically does syscalls. Its syscalls get
// their OWN dispatcher + output buffer, selected on the current scheduler task
// (so it does not clash with the global foreground state; foreground execs run IF=0
// and can therefore never overlap with the daemon). The daemon never ends, so the
// tricky "sys_exit from a scheduled task" does not occur.
static DAEMON_TASK: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
static DAEMON_OUTPUT: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());
/// Incomplete line (the daemon writes a line in multiple write calls).
static DAEMON_PARTIAL: Mutex<String> = Mutex::new(String::new());

/// The recent output lines of the background daemon (for display).
pub fn daemon_lines() -> alloc::vec::Vec<String> {
    DAEMON_OUTPUT.lock().clone()
}

/// Separate syscall dispatcher for the daemon task (native ABI; own output buffer).
fn daemon_dispatch(num: u64, a1: u64, _a2: u64, _a3: u64) -> u64 {
    // The daemon NEVER ends: force EXITED=0 so that after this syscall
    // `syscall_entry` takes the normal SYSRET return and not the sys_exit path with the
    // (for the daemon invalid) SAVED_KERNEL_RSP of the last foreground exec.
    unsafe { EXITED = 0 };
    match num {
        1 => {
            // sys_write(NUL-string): accumulate in a line buffer; emit complete
            // lines (the daemon writes a single line in multiple write calls).
            let s = user_cstr(a1, 512);
            if let Ok(t) = core::str::from_utf8(&s) {
                let mut partial = DAEMON_PARTIAL.lock();
                partial.push_str(t);
                while let Some(nl) = partial.find('\n') {
                    let line: String = partial.drain(..=nl).collect();
                    let mut out = DAEMON_OUTPUT.lock();
                    out.push(String::from(line.trim_end()));
                    let len = out.len();
                    if len > 14 {
                        out.drain(0..len - 14); // keep only the most recent lines
                    }
                }
            }
            s.len() as u64
        }
        2 => 7, // getpid -> the daemon is pid 7
        _ => 0, // other syscalls: silently succeed (daemon never ends)
    }
}

/// Load `program` (native ABI) as a PREEMPTIVELY scheduled background daemon.
pub fn spawn_daemon(falloc: &mut FrameAllocator, program: &[u8]) {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;
    // Own isolated 2 MiB arena + PML4 (just like bg-musl) instead of loose frames on
    // the boot CR3: this way the daemon NO LONGER runs on the supervisor-only boot PML4 and
    // SMEP/SMAP stay enforced.
    // Exactly 2 MiB, 2 MiB-aligned in one go (the daemon never reaps -> no
    // free path; no more 4 MiB over-allocation).
    let arena = falloc.allocate_aligned(512, 512).expect("daemon-arena");
    let code = arena;
    let stack_top = arena + MIB2; // user stack grows downward from the arena top
    let kstack = falloc.allocate_contiguous(4).expect("daemon-kstack");
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    // SysV stack (argv[0]="daemon") so musl/native _start also starts up validly.
    let rsp = unsafe { setup_user_stack(stack_top, &[b"daemon"], &info) };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    // Build the address space FIRST, then spawn the task with its cr3 already set
    // (before it is Ready) — see BUG-007 / spawn_user.
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let idx = crate::sched::spawn_user(info.entry, rsp, user_cs, user_ss, kstack_top, pml4);
    DAEMON_TASK.store(idx, Ordering::Relaxed);
    crate::serial_println!("[euro] daemon scheduled as task {idx} (pid 7), own address space PML4 {pml4:#x}");
}

// ── Preemptive per-process model ───────────────────────────────────────────
// Multiple REAL musl processes at once, each preemptively scheduled with its
// OWN context: own kernel stack (sched), own FS_BASE/TLS (sched saves
// it per task), own heap, own output buffer and pid. The syscall layer
// routes per task to this per-process control block (PCB).
struct BgProc {
    task: usize,
    pid: u64,
    heap_break: u64,
    heap_end: u64,
    output: alloc::vec::Vec<String>,
    partial: String,
    // Physical frames of this process (to free on reaping).
    arena_raw: u64, // start of the arena allocation
    arena_frames: u64, // number of arena frames (512 for aligned bg-musl, 1024 for pooled fork)
    /// VIRTUAL address at which the 2 MiB arena is mapped in THIS process (= where code/
    /// stack run). For identity processes == physical; for a FORKED child it
    /// is the virtual arena of the PARENT (the copy runs at the same virtual addresses,
    /// different frames). execve uses this as the load/entry/stack base.
    arena_virt: u64,
    kstack: u64,    // ring-0 stack (4 frames)
    pml4: u64,      // own address space (PML4+PDPT+PD = 3 frames)
    /// Terminated and awaiting cleanup (freeing frames).
    zombie: bool,
    /// Reason for termination (for the tombstone in the system log).
    kill_reason: Option<String>,
    /// Scheduler task indices of the THREADS of this process (clone, CLONE_VM).
    /// Threads share the address space/heap/output/pid; own stack/TLS/kstack.
    threads: alloc::vec::Vec<usize>,
    /// Per thread task the CLONE_CHILD_CLEARTID address: on thread exit the
    /// kernel writes 0 here and does a futex-wake — exactly where pthread_join
    /// waits. (task, ctid userspace address)
    thread_ctids: alloc::vec::Vec<(usize, u64)>,
    /// Parent pid (S3 fork): 0 = no parent (e.g. the boot demo processes).
    ppid: u64,
    /// Do this process's frames come from the PROCESS POOL (fork/exec) instead of the
    /// main allocator? Determines where the reaper returns them.
    pooled: bool,
}

/// Exit statuses of terminated children, kept separate from frame reaping, so that
/// waitpid can still retrieve the status after the frames are already freed. (ppid, pid, code).
static CHILD_EXITS: Mutex<alloc::vec::Vec<(u64, u64, i64)>> = Mutex::new(alloc::vec::Vec::new());

/// Pid counter for forked children (starts high, separate from the fixed demo pids 1-16).
static NEXT_FORK_PID: AtomicU64 = AtomicU64::new(1000);

static BG: Mutex<alloc::vec::Vec<BgProc>> = Mutex::new(alloc::vec::Vec::new());
/// Tombstones of cleaned-up processes (for display).
static REAPED: Mutex<alloc::vec::Vec<String>> = Mutex::new(alloc::vec::Vec::new());

/// The recent "cleaned up" notices of terminated processes.
pub fn reaped_lines() -> alloc::vec::Vec<String> {
    REAPED.lock().clone()
}

/// Free the frames of all terminated (zombie) processes and remove them
/// from the table. Called from the desktop loop (task 0, boot PML4), where it is
/// safe: a dead process never runs again and its frames are not in use.
pub fn reap_dead(falloc: &mut FrameAllocator) {
    // BUG-007: do NOT hold BG.lock() across the (preemptible) frame-freeing below. If the
    // timer preempts task 0 mid-free, the scheduler may switch to a bg-musl process whose
    // `syscall_dispatch` also takes BG.lock() — it would spin on the lock we still hold while
    // we can't run to release it: a silent core deadlock. So extract the zombies under a
    // SHORT, interrupt-free critical section, then free their resources with BG RELEASED.
    let dead: alloc::vec::Vec<BgProc> =
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut bg = BG.lock();
            let mut out = alloc::vec::Vec::new();
            let mut i = 0;
            while i < bg.len() {
                if bg[i].zombie {
                    out.push(bg.remove(i));
                } else {
                    i += 1;
                }
            }
            out
        });
    for p in dead {
        if p.pooled {
            // Forked children: return frames to the PROCESS POOL.
            for f in 0..p.arena_frames {
                crate::procpool::free(p.arena_raw + f * 4096);
            }
            for f in 0..4u64 {
                crate::procpool::free(p.kstack + f * 4096);
            }
            // First look up the arena PT (walks through pml4->pdpt->pd) BEFORE we free
            // those table frames, otherwise use-after-free.
            let arena_pt = crate::paging::arena_pt(p.pml4, p.arena_virt);
            let (a, b, c) = crate::paging::table_frames(p.pml4);
            crate::procpool::free(a);
            crate::procpool::free(b);
            crate::procpool::free(c);
            if let Some(ptf) = arena_pt {
                crate::procpool::free(ptf);
            }
        } else {
            for f in 0..p.arena_frames {
                let _ = falloc.free(p.arena_raw + f * 4096);
            }
            for f in 0..4u64 {
                let _ = falloc.free(p.kstack + f * 4096);
            }
            crate::paging::free_address_space(falloc, p.pml4);
        }
        let kib = (p.arena_frames + 4 + 4) as usize * 4; // arena + kstack(4) + table frames(~4)
        // Show the last output (e.g. the result of a job) if there is
        // one, otherwise the termination reason (e.g. the isolation violation).
        let label = p
            .output
            .last()
            .cloned()
            .or(p.kill_reason)
            .unwrap_or_else(|| String::from("terminated"));
        REAPED.lock().push(alloc::format!("pid {}: {label} -> reaped ({kib} KiB free)", p.pid));
        let n = REAPED.lock().len();
        if n > 4 {
            REAPED.lock().drain(0..n - 4);
        }
    }
}

/// Is a process with this pid still alive? (a LIVE, non-zombie BgProc). Used by
/// EuroInit to see whether a service is still running or must be restarted.
pub fn is_pid_alive(pid: u64) -> bool {
    // BUG-007 hardening: hold BG non-preemptibly (irqsave) so task 0 is never suspended
    // while holding it — matching the interrupts-off syscall path. See reap_dead.
    x86_64::instructions::interrupts::without_interrupts(|| {
        BG.lock().iter().any(|p| p.pid == pid && !p.zombie)
    })
}

/// The most recent output line of each background musl process (for display).
pub fn bg_lines() -> alloc::vec::Vec<String> {
    // BUG-007 hardening: irqsave BG hold (non-preemptible).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let bg = BG.lock();
        let mut out = alloc::vec::Vec::new();
        for p in bg.iter() {
            if let Some(last) = p.output.last() {
                out.push(last.clone());
            }
        }
        out
    })
}

// Is an ISOLATED foreground exec running right now (own PML4, synchronous)? If so,
// a page fault terminates that process cleanly (back to run_args) instead of
// killing task 0/the shell.
static FG_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn fg_active() -> bool {
    FG_ACTIVE.load(Ordering::Relaxed)
}

/// Called by the fault handler on a fault in a foreground exec: set the
/// exit status and jump (via the trampoline) cleanly back into run_args.
pub fn fg_force_exit(addr: u64) -> ! {
    unsafe {
        EXIT_CODE = 139; // 128 + SIGSEGV
        EXITED = 1;
        FG_ACTIVE.store(false, Ordering::Relaxed);
    }
    crate::serial_println!("[isolation] foreground exec page fault at {addr:#x} -> clean exit (code 139)");
    // SAFETY: SAVED_KERNEL_RSP points to the run_args return point (enter_ring3
    // saved it); the trampoline restores the stack and returns there.
    unsafe { force_kernel_return() };
    loop {
        core::hint::spin_loop(); // unreachable: the trampoline does not return here
    }
}

/// Called by the page-fault handler when a ring-3 process reaches outside its
/// address space: note it in its output buffer and return the pid.
pub fn note_isolation_kill(task: usize, addr: u64) -> u64 {
    let mut bg = BG.lock();
    if let Some(p) = bg.iter_mut().find(|p| p.task == task) {
        p.zombie = true; // ready to be reaped
        p.output.clear(); // the isolation reason is more informative than the last output
        p.kill_reason = Some(alloc::format!("memory isolation: access {addr:#x} denied"));
        return p.pid;
    }
    0
}

/// Process overview (`ps`): the background musl processes + the fixed system tasks.
pub fn ps_lines() -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    out.push(String::from("  PID  TYPE     ADDRESS SPACE  STATUS"));
    out.push(String::from("    1  shell    shared        active (foreground)"));
    out.push(String::from("    7  daemon   shared        active (EuroMonitor)"));
    // BUG-007 hardening: irqsave BG hold (non-preemptible).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let bg = BG.lock();
        for p in bg.iter() {
            let status = if p.zombie { "terminated (reap)" } else { "active" };
            out.push(alloc::format!("  {:3}  musl     own PML4      {}", p.pid, status));
        }
    });
    out
}

/// `kill <pid>`: terminate a background musl process. It is cleaned up by the reaper
/// (frames freed). Returns whether a process was found.
pub fn kill_pid(pid: u64) -> bool {
    // BUG-007 hardening: irqsave BG hold (non-preemptible).
    let task = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut bg = BG.lock();
        bg.iter_mut().find(|p| p.pid == pid && !p.zombie).map(|p| {
            p.zombie = true;
            p.kill_reason = Some(String::from("terminated via shell (kill)"));
            p.task
        })
    });
    let task = match task {
        Some(t) => t,
        None => return false,
    };
    crate::sched::mark_dead(task);
    true
}

/// Futex wait queue: (userspace address, blocked task). FUTEX_WAIT blocks the
/// task (the scheduler skips it); FUTEX_WAKE unblocks up to `n` waiters.
static FUTEX_QUEUE: Mutex<alloc::vec::Vec<(u64, usize)>> = Mutex::new(alloc::vec::Vec::new());

/// futex-wake: unblock up to `n` tasks waiting on `uaddr`. Returns the number
/// of woken tasks.
fn futex_wake(uaddr: u64, n: i32) -> u32 {
    let mut q = FUTEX_QUEUE.lock();
    let mut woken = 0i32;
    let mut i = 0;
    while i < q.len() && woken < n {
        if q[i].0 == uaddr {
            let task = q[i].1;
            crate::sched::unblock(task);
            q.swap_remove(i);
            woken += 1;
        } else {
            i += 1;
        }
    }
    woken as u32
}

/// FUTEX_WAIT: if *uaddr == val, block the current task on uaddr and return 0
/// (the waiter is switched out on the next tick until a wake
/// unblocks it; musl re-checks after a spurious wakeup). Otherwise -EAGAIN.
fn futex_wait(uaddr: u64, val: u32) -> u64 {
    let cur_val: u32 = match read_user(uaddr) {
        Some(v) => v,
        None => return EFAULT,
    };
    if cur_val != val {
        return (-11i64) as u64; // -EAGAIN: the value already changed
    }
    let cur = crate::sched::current();
    let mut q = FUTEX_QUEUE.lock();
    if !q.iter().any(|&(a, t)| a == uaddr && t == cur) {
        q.push((uaddr, cur));
    }
    drop(q);
    crate::sched::block_current();
    0
}

// Static pool of kernel stacks for THREADS (clone). Supervisor-mapped
// (kernel .bss), so a thread cannot touch its own saved kernel context
// from ring 3. 8 threads system-wide (enough for the demos).
const MAX_THREADS: usize = 8;
const TKSTACK_SIZE: usize = 16 * 1024;
static mut THREAD_KSTACKS: [[u8; TKSTACK_SIZE]; MAX_THREADS] = [[0; TKSTACK_SIZE]; MAX_THREADS];
static THREAD_KSTACK_NEXT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Per-process syscall dispatcher (Linux-ABI subset) with the state of ONE process:
/// own heap, own output buffer, own pid. Threads of the process also route
/// here (shared heap/output/pid; own stack/TLS).
/// S3 fork(): duplicate the background process at index `pos`. Copies the 2 MiB
/// user arena to FRESH frames from the process pool, builds a remapped address space
/// (same virtual addresses -> new physical frames), and starts a child task that
/// resumes in ring 3 with rax=0. The PARENT gets the child pid back.
fn do_fork(bg: &mut alloc::vec::Vec<BgProc>, pos: usize) -> u64 {
    const MIB2: u64 = 1 << 21;
    let (parent_pid, parent_arena_raw, parent_virt, heap_break, heap_end, parent_pml4) = {
        let p = &bg[pos];
        (p.pid, p.arena_raw, p.arena_virt, p.heap_break, p.heap_end, p.pml4)
    };
    let parent_arena = (parent_arena_raw + (MIB2 - 1)) & !(MIB2 - 1);

    // Frames from the process pool: 4 MiB arena + 4 frames kstack + 3 table frames.
    let child_raw = match crate::procpool::alloc_contiguous(1024) {
        Some(a) => a,
        None => return (-12i64) as u64, // -ENOMEM
    };
    let child_arena = (child_raw + (MIB2 - 1)) & !(MIB2 - 1);
    let child_kstack = match crate::procpool::alloc_contiguous(4) {
        Some(a) => a,
        None => {
            for f in 0..1024u64 { crate::procpool::free(child_raw + f * 4096); }
            return (-12i64) as u64;
        }
    };
    // Four table frames: PML4 + PDPT + PD + the fine-grained arena PT (for W^X).
    let (pml4, pdpt, pd, pt) = match (crate::procpool::alloc(), crate::procpool::alloc(), crate::procpool::alloc(), crate::procpool::alloc()) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        (a, b, c, d) => {
            for f in 0..1024u64 { crate::procpool::free(child_raw + f * 4096); }
            for f in 0..4u64 { crate::procpool::free(child_kstack + f * 4096); }
            for fr in [a, b, c, d].into_iter().flatten() { crate::procpool::free(fr); }
            return (-12i64) as u64;
        }
    };

    // SAFETY: parent and child arena are both identity-mapped; copy the whole
    // 2 MiB (code + stack + heap as it is NOW, including the fork syscall frame).
    unsafe {
        core::ptr::copy_nonoverlapping(parent_arena as *const u8, child_arena as *mut u8, MIB2 as usize);
    }
    // Map the parent's VIRTUAL arena -> the child's physical frames, with the
    // SAME W^X rights per page as the parent (cloned from its arena PT).
    let parent_pt = crate::paging::arena_pt(parent_pml4, parent_virt);
    crate::paging::fill_remap_tables_wx(pml4, pdpt, pd, pt, parent_virt, child_arena, parent_pt);

    // Child task: resume at the fork return point (USER_RIP) with the PARENT user stack
    // (USER_RSP, now in the copy) and rax=0; own kstack + remapped address space.
    let kstack_top = (child_kstack + 4 * 4096) & !0xF;
    let (user_rip, user_rsp, saved) = unsafe { (USER_RIP, USER_RSP, SAVED_REGS) };
    let fs = unsafe { Msr::new(0xC000_0100).read() };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let child_pid = NEXT_FORK_PID.fetch_add(1, Ordering::Relaxed);
    let task = crate::sched::spawn_thread(user_rip, user_rsp, user_cs, user_ss, kstack_top, pml4, fs, saved);
    crate::sched::set_ident(task, child_pid as u32, parent_pid as u32);

    bg.push(BgProc {
        task,
        pid: child_pid,
        heap_break,
        heap_end,
        output: alloc::vec::Vec::new(),
        partial: String::new(),
        arena_raw: child_raw,
        arena_frames: 1024, // pooled fork arena: 4 MiB from the process pool
        arena_virt: parent_virt, // the child runs on the parent's virtual arena
        kstack: child_kstack,
        pml4,
        zombie: false,
        kill_reason: None,
        threads: alloc::vec::Vec::new(),
        thread_ctids: alloc::vec::Vec::new(),
        ppid: parent_pid,
        pooled: true,
    });
    crate::kinfo!("[fork] pid {parent_pid} -> child pid {child_pid} (task {task}, copy-arena {child_arena:#x}, pml4 {pml4:#x})");
    child_pid // the PARENT gets the child pid; the child got rax=0 via spawn_thread
}

/// S3 waitpid/wait4: NON-BLOCKING reap. Fetches a finished child of `parent_pid`
/// from CHILD_EXITS, writes the Linux wait status (WEXITSTATUS = (status>>8)&0xff)
/// and returns the child pid. No zombie yet -> 0 (the caller polls again).
fn do_wait4(parent_pid: u64, _pid_arg: u64, status_ptr: u64) -> u64 {
    let mut ce = CHILD_EXITS.lock();
    if let Some(idx) = ce.iter().position(|&(pp, _, _)| pp == parent_pid) {
        let (_, cpid, code) = ce.remove(idx);
        if status_ptr != 0 {
            let status = (((code as i32) & 0xff) << 8) as u32;
            if !write_user(status_ptr, status) {
                return EFAULT;
            }
        }
        crate::kinfo!("[wait] pid {parent_pid} reaped child {cpid} (exitcode {code})");
        return cpid;
    }
    0
}

/// S3 execve(path, argv, envp): replace the IMAGE of the current process with a
/// new program from the userspace VFS, IN the same arena/address space. On success
/// the syscall returns in the NEW image (we rewrite the saved
/// register block so sysret jumps to the new entry). Fails -> errno returned.
fn do_execve(p: &mut BgProc, path_ptr: u64, argv_ptr: u64) -> u64 {
    const MIB2: u64 = 1 << 21;
    let path_bytes = user_cstr(path_ptr, 256);
    let path = String::from_utf8_lossy(&path_bytes).into_owned();
    // Program bytes from the VFS; verify-before-execute (Ed25519) as with every exec.
    let program = match FILES.lock().iter().find(|(q, _)| *q == path) {
        Some((_, d)) => d.clone(),
        None => return (-2i64) as u64, // -ENOENT
    };
    if !verify_program(&path, &program) {
        return (-13i64) as u64; // -EACCES: invalid signature
    }
    // Parse argv from userspace (NULL-terminated array of char*).
    let mut argv_owned: alloc::vec::Vec<alloc::vec::Vec<u8>> = alloc::vec::Vec::new();
    if argv_ptr != 0 {
        let mut i = 0;
        loop {
            // Each argv[i] pointer is read arena-validated; a forged
            // array pointer thus cannot leak kernel memory as an argv element.
            let pptr: u64 = match read_user(argv_ptr + (i as u64) * 8) {
                Some(p) => p,
                None => break,
            };
            if pptr == 0 || i >= 64 {
                break;
            }
            argv_owned.push(user_cstr(pptr, 256));
            i += 1;
        }
    }
    if argv_owned.is_empty() {
        argv_owned.push(path_bytes);
    }
    let argv_refs: alloc::vec::Vec<&[u8]> = argv_owned.iter().map(|v| v.as_slice()).collect();

    // Load into the EXISTING arena at the VIRTUAL address at which this process runs
    // (for a forked child != physical). The current cr3 maps this USER -> its own
    // frames; with SMAP off the kernel writes through here. Fresh user stack + heap.
    let arena = p.arena_virt;
    let stack_top = arena + MIB2;
    let pages = program_span_pages(&program);
    // W^X: the arena runs R-X code; make it fully writable for a moment to load the NEW
    // image, then restore W^X based on that image's segments.
    // (No fine-grained PT -> old RWX arena, just load.)
    let arena_pt = crate::paging::arena_pt(p.pml4, arena);
    if let Some(pt) = arena_pt {
        crate::paging::arena_set_writable(pt);
    }
    let info = load_program(&program, arena, pages);
    let rsp = unsafe { setup_user_stack(stack_top, &argv_refs, &info) };
    if let Some(pt) = arena_pt {
        crate::paging::arena_set_wx(pt, &info.exec_pages, &info.writ_pages);
    }
    p.heap_break = arena + 0x80000;
    p.heap_end = arena + 0x180000;

    // Make the current syscall RETURN into the new image: rewrite the
    // saved register block (slot 13 = rcx = sysret-rip, slot 12 = r11 = rflags,
    // 0..11 = cleared GP regs) and set USER_RSP to the fresh stack.
    unsafe {
        let regs = SAVED_REGS as *mut u64;
        for k in 0..14 {
            regs.add(k).write(0);
        }
        regs.add(13).write(info.entry); // sysret target = new entry
        regs.add(12).write(0x202); // rflags with IF=1
        USER_RSP = rsp;
    }
    crate::kinfo!("[exec] pid {} execve {path} -> entry {:#x} (same arena {arena:#x})", p.pid, info.entry);
    0
}

fn bg_dispatch(p: &mut BgProc, num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    // Just like the daemon: never take the global sys_exit path.
    unsafe { EXITED = 0 };
    match num {
        1 | 20 => {
            // write(fd,buf,len) / writev(fd,iov,cnt) -> own line buffer.
            let text: alloc::vec::Vec<u8> = if num == 1 {
                match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                }
            } else {
                // writev: bound iovcnt and validate each iov struct + base/len
                // BEFORE dereference. Without the bound a large `a3` can make the kernel
                // wander; without the base check a forged iov can read kernel
                // memory.
                if a3 > 1024 {
                    return (-22i64) as u64; // -EINVAL
                }
                let mut v = alloc::vec::Vec::new();
                for i in 0..a3 {
                    let iov_base = a2 + (i * 16);
                    let base: u64 = match read_user(iov_base) {
                        Some(b) => b,
                        None => return EFAULT,
                    };
                    let len = match read_user::<u64>(iov_base + 8) {
                        Some(l) => l as usize,
                        None => return EFAULT,
                    };
                    if len > 0 {
                        match copy_from_user(base, len) {
                            Some(chunk) => v.extend_from_slice(&chunk),
                            None => return EFAULT,
                        }
                    }
                }
                v
            };
            // write to a pipe write fd -> the pipe FIFO (IPC), otherwise the
            // own line buffer (fd 1/2 = console).
            if let Some(r) = pipe_write_fd(a1 as usize, &text) {
                return r;
            }
            if let Ok(t) = core::str::from_utf8(&text) {
                p.partial.push_str(t);
                while let Some(nl) = p.partial.find('\n') {
                    let line: String = p.partial.drain(..=nl).collect();
                    p.output.push(String::from(line.trim_end()));
                    let len = p.output.len();
                    if len > 6 {
                        p.output.drain(0..len - 6);
                    }
                }
            }
            text.len() as u64
        }
        0 => pipe_read_fd(a1 as usize, a2, a3 as usize).unwrap_or(0), // read: pipe or EOF
        22 | 293 => pipe_create(a1),                                  // pipe / pipe2
        32 => a1, // dup(fd) -> same fd (simplified)
        33 => {
            // dup2(oldfd, newfd): copy the pipe end to newfd.
            let (old, new) = (a1 as usize, a2 as usize);
            if old < MAX_FD && new < MAX_FD {
                let v = PIPE_FDS.lock()[old];
                PIPE_FDS.lock()[new] = v;
                new as u64
            } else {
                (-9i64) as u64
            }
        }
        9 => {
            // mmap -> bump from the OWN heap (anonymous allocation, page-aligned).
            let len = (a2 + 0xFFF) & !0xFFF;
            let base = (p.heap_break + 0xFFF) & !0xFFF;
            if base + len > p.heap_end {
                return (-12i64) as u64; // -ENOMEM
            }
            p.heap_break = base + len;
            base
        }
        12 => {
            // brk(addr) -> new break from the own heap.
            if a1 == 0 || a1 > p.heap_end {
                return p.heap_break;
            }
            p.heap_break = a1;
            a1
        }
        158 => match a1 {
            0x1002 => {
                unsafe { Msr::new(0xC000_0100).write(a2) }; // FS_BASE (musl-TLS)
                0
            }
            0x1001 => {
                unsafe { Msr::new(0xC000_0101).write(a2) };
                0
            }
            _ => (-22i64) as u64,
        },
        39 => p.pid,  // getpid
        218 => p.pid, // set_tid_address -> tid
        318 => {
            // getrandom: deterministic fill (no crypto source needed here).
            if !in_user_arena(a1, a2 as usize) {
                return EFAULT;
            }
            let buf: alloc::vec::Vec<u8> =
                (0..a2).map(|i| (0x9Eu64.wrapping_mul(i + 1)) as u8).collect();
            let _ = copy_to_user(a1, &buf);
            a2
        }
        56 => {
            // clone(flags, child_stack, ptid, ctid, tls): create a THREAD that shares
            // the address space (CLONE_VM) but has its own stack/TLS/kernel
            // stack. Foundation for pthreads. No child_stack = (v)fork: not supported.
            let (flags, child_stack) = (a1, a2);
            if child_stack == 0 {
                return (-38i64) as u64; // -ENOSYS (no fork)
            }
            let slot = THREAD_KSTACK_NEXT.fetch_add(1, Ordering::Relaxed);
            if slot >= MAX_THREADS {
                return (-11i64) as u64; // -EAGAIN
            }
            let kbase = unsafe { core::ptr::addr_of_mut!(THREAD_KSTACKS[slot]) as u64 };
            let kstack_top = (kbase + TKSTACK_SIZE as u64) & !0xF;
            let user_rip = unsafe { USER_RIP };
            let sel = crate::gdt::selectors();
            let user_cs = (sel.user_code.0 | 3) as u64;
            let user_ss = (sel.user_data.0 | 3) as u64;
            // TLS: on CLONE_SETTLS (0x80000) use the supplied tls (a5),
            // otherwise inherit the current FS_BASE.
            let fs = if flags & 0x0008_0000 != 0 {
                a5
            } else {
                unsafe { Msr::new(0xC000_0100).read() }
            };
            let saved_regs = unsafe { SAVED_REGS };
            let child = crate::sched::spawn_thread(user_rip, child_stack, user_cs, user_ss, kstack_top, p.pml4, fs, saved_regs);
            p.threads.push(child);
            crate::serial_println!("[thread] clone: pid {} -> thread task {child} (shared address space, own stack/TLS)", p.pid);
            // CLONE_PARENT_SETTID (0x100000) / CLONE_CHILD_SETTID (0x1000000):
            // write the tid to *ptid / *ctid.
            if flags & 0x0010_0000 != 0 && a3 != 0 {
                let _ = write_user(a3, child as i32);
            }
            if flags & 0x0100_0000 != 0 && a4 != 0 {
                let _ = write_user(a4, child as i32);
            }
            // CLONE_CHILD_CLEARTID (0x200000): remember the address; on thread exit
            // the kernel writes 0 here (where pthread_join futex-waits).
            if flags & 0x0020_0000 != 0 && a4 != 0 {
                p.thread_ctids.push((child, a4));
            }
            child as u64 // the parent gets the thread id
        }
        59 => do_execve(p, a1, a2), // execve(path, argv, envp) — image-replace
        202 => {
            // futex(uaddr, op, val, ...). FUTEX_WAIT=0, FUTEX_WAKE=1 (low 7 bits;
            // ignore PRIVATE/CLOCK flags). Real blocking + wake.
            match a2 & 0x7f {
                0 => futex_wait(a1, a3 as u32),
                1 => futex_wake(a1, a3 as i32) as u64,
                _ => 0,
            }
        }
        // EuroIPC — own message-bus syscalls (own number space 500-502).
        500 => crate::euroipc::register(p.pid, a1 as u32) as u64,
        501 => {
            let data = match copy_from_user(a2, a3 as usize) {
                Some(v) => v,
                None => return EFAULT,
            };
            crate::euroipc::send(p.pid, a1 as u32, &data) as u64
        }
        502 => crate::euroipc::recv(p.pid, a1, a2 as usize) as u64,
        // Memory/signal/time stubs that silently succeed.
        10 | 11 | 13 | 14 | 16 | 35 | 228 | 234 | 273 => 0,
        60 | 231 => {
            let cur = crate::sched::current();
            if p.threads.contains(&cur) {
                // THREAD exit: terminate only this thread; the process lives on.
                // CLONE_CHILD_CLEARTID: write 0 to the ctid address + futex-wake,
                // so pthread_join in the parent thread continues.
                if let Some(idx) = p.thread_ctids.iter().position(|&(t, _)| t == cur) {
                    let (_, ctid) = p.thread_ctids[idx];
                    let _ = write_user(ctid, 0i32);
                    futex_wake(ctid, i32::MAX);
                    p.thread_ctids.swap_remove(idx);
                }
                // We leave it IN p.threads: musl calls exit in a for(;;) loop,
                // and those follow-up syscalls must keep routing HERE (so that
                // EXITED=0 stays) until the scheduler skips the dead thread. Removing
                // it now would let the next exit fall through to linux_dispatch,
                // which sets EXITED=1 -> the sys_exit path with a stale
                // SAVED_KERNEL_RSP -> ret into garbage. (Found with QEMU+gdb.)
                crate::sched::mark_dead(cur);
                return 0;
            }
            // PROCESS exit (main task): mark the whole process as done (zombie);
            // the reaper frees the frames. musl spins afterward until the timer switches.
            p.zombie = true;
            p.kill_reason = Some(alloc::format!("done (exit {a1})"));
            // S3: save the exit status for the parent (waitpid) — only if there is
            // a parent (ppid != 0). Services (ppid 0) would otherwise grow CHILD_EXITS
            // unbounded (nobody waitpids on ppid 0).
            if p.ppid != 0 {
                CHILD_EXITS.lock().push((p.ppid, p.pid, a1 as i64));
            }
            crate::sched::mark_current_dead();
            0
        }
        _ => 0,
    }
}

/// Load `program` (musl, Linux ABI) as a PREEMPTIVELY scheduled process with
/// its own PCB (heap/output/pid/TLS). The program runs indefinitely.
pub fn spawn_bg_musl(falloc: &mut FrameAllocator, program: &[u8], pid: u64, argv0: &[u8]) {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;
    // One 2 MiB-aligned user arena: ALL user frames of this process (code,
    // stack, heap/TLS) lie within it. Only this block gets the USER bit in
    // its own PML4 -> no other ring-3 process can reach it (memory isolation).
    // Exactly 2 MiB, 2 MiB-aligned in one go (no more 4 MiB over-allocation):
    // saves ~2 MiB per background process. On reaping we return exactly these 512
    // frames (arena_frames below).
    let arena = falloc.allocate_aligned(512, 512).expect("bg-arena");
    let arena_raw = arena;
    let code = arena; // program code/segments at the bottom of the arena
    let heap = arena + 0x80000; // +512 KiB: own heap (musl mmap/TLS block)
    let stack_top = arena + MIB2; // user stack grows downward from the arena top
    let kstack = falloc.allocate_contiguous(4).expect("bg-kstack"); // ring-0 stack (supervisor)
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    let rsp = unsafe { setup_user_stack(stack_top, &[argv0], &info) };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    // Own isolated W^X address space; from the next switch on this process runs on it.
    // Build it FIRST, then spawn with cr3 set before Ready (BUG-007: a task must never be
    // schedulable with cr3=0, or a preempting timer IRQ runs its ring-3 code on the boot PML4).
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let idx = crate::sched::spawn_user(info.entry, rsp, user_cs, user_ss, kstack_top, pml4);
    // BUG-007 hardening: irqsave BG hold (non-preemptible) for the registration push.
    x86_64::instructions::interrupts::without_interrupts(|| {
        BG.lock().push(BgProc {
            task: idx,
            pid,
            heap_break: heap,
            heap_end: arena + 0x180000, // ~1 MiB heap (room for thread stacks)
            output: alloc::vec::Vec::new(),
            partial: String::new(),
            arena_raw,
            arena_frames: 512, // exactly 2 MiB (allocated aligned)
            arena_virt: arena, // identity-mapped: virtual == physical
            kstack,
            pml4,
            zombie: false,
            kill_reason: None,
            threads: alloc::vec::Vec::new(),
            thread_ctids: alloc::vec::Vec::new(),
            ppid: 0,
            pooled: false,
        });
    });
    crate::serial_println!("[euro] bg-musl (pid {pid}) -> task {idx}, own address space PML4 {pml4:#x}, arena {arena:#x}");
}

/// Close an fd.
fn vfs_close(fd: usize) -> u64 {
    if fd < MAX_FD {
        OPEN_FDS.lock()[fd] = None;
        OPEN_DIRS.lock()[fd] = None;
    }
    0
}

/// Is `path` a DIRECTORY in the userspace VFS? A directory has no FILES entry of its own,
/// but is the prefix of at least one file (or is the root "/").
fn is_vfs_dir(path: &[u8]) -> bool {
    if path == b"/" {
        return true;
    }
    let mut prefix = path.to_vec();
    prefix.push(b'/');
    FILES.lock().iter().any(|(p, _)| p.as_bytes().starts_with(&prefix))
}

/// Direct children of a VFS directory: (name, is_dir). Derived from the flat
/// FILES path list — intermediate path components are recognized as subdirectories.
fn dir_children(path: &str) -> alloc::vec::Vec<(String, bool)> {
    let prefix = if path == "/" { String::from("/") } else { alloc::format!("{path}/") };
    let mut out: alloc::vec::Vec<(String, bool)> = alloc::vec::Vec::new();
    for (p, _) in FILES.lock().iter() {
        if let Some(rest) = p.strip_prefix(&prefix) {
            if rest.is_empty() {
                continue;
            }
            let (name, is_dir) = match rest.find('/') {
                Some(i) => (&rest[..i], true), // subdirectory (first component)
                None => (rest, false),         // file
            };
            if !out.iter().any(|(n, _)| n == name) {
                out.push((String::from(name), is_dir));
            }
        }
    }
    out
}

/// Open a DIRECTORY -> dir fd (registered in OPEN_DIRS), or u64::MAX if full.
fn diropen(path: &[u8]) -> u64 {
    let norm = String::from_utf8_lossy(path).into_owned();
    let fds = OPEN_FDS.lock();
    let mut dirs = OPEN_DIRS.lock();
    for fd in 3..MAX_FD {
        if fds[fd].is_none() && dirs[fd].is_none() {
            dirs[fd] = Some((norm, 0));
            return fd as u64;
        }
    }
    u64::MAX
}

/// getdents64(fd, buf, count): fill Linux `linux_dirent64` records from the cursor.
/// Returns the number of bytes written, 0 at the end.
fn vfs_getdents64(fd: usize, buf: u64, count: usize) -> u64 {
    if fd >= MAX_FD {
        return (-9i64) as u64; // -EBADF
    }
    // The entire destination buffer is arena-validated once; after that all
    // per-record writes (bounded by `written + reclen <= count`) are guaranteed
    // to lie within [buf, buf+count) and cannot touch kernel memory.
    if !in_user_arena(buf, count) {
        return EFAULT;
    }
    let (path, mut cursor) = match &OPEN_DIRS.lock()[fd] {
        Some((p, c)) => (p.clone(), *c),
        None => return (-20i64) as u64, // -ENOTDIR
    };
    // "." and ".." first, then the real children.
    let mut all: alloc::vec::Vec<(String, bool)> =
        alloc::vec![(String::from("."), true), (String::from(".."), true)];
    all.extend(dir_children(&path));

    let mut written = 0usize;
    while cursor < all.len() {
        let (name, is_dir) = &all[cursor];
        let reclen = (19 + name.len() + 1 + 7) & !7; // 8-byte aligned
        if written + reclen > count {
            break; // no longer fits in this buffer call
        }
        let rec = (buf as usize + written) as *mut u8;
        unsafe {
            core::ptr::write_bytes(rec, 0, reclen);
            (rec as *mut u64).write_unaligned((cursor as u64) + 1); // d_ino
            (rec.add(8) as *mut i64).write_unaligned((cursor as i64) + 1); // d_off
            (rec.add(16) as *mut u16).write_unaligned(reclen as u16); // d_reclen
            rec.add(18).write(if *is_dir { 4 } else { 8 }); // d_type DT_DIR/DT_REG
            core::ptr::copy_nonoverlapping(name.as_ptr(), rec.add(19), name.len());
        }
        written += reclen;
        cursor += 1;
    }
    OPEN_DIRS.lock()[fd] = Some((path, cursor));
    written as u64
}

/// The size (bytes) of the file behind an open fd, or None.
fn vfs_size(fd: usize) -> Option<usize> {
    if fd >= MAX_FD {
        return None;
    }
    let fds = OPEN_FDS.lock();
    let (fi, _) = fds[fd]?;
    Some(FILES.lock()[fi].1.len())
}

/// lseek(fd, offset, whence) -> new offset (u64::MAX on error).
fn vfs_lseek(fd: usize, offset: i64, whence: u64) -> u64 {
    if fd >= MAX_FD {
        return u64::MAX;
    }
    let size = match vfs_size(fd) {
        Some(s) => s as i64,
        None => return u64::MAX,
    };
    let mut fds = OPEN_FDS.lock();
    let (fi, cur) = match fds[fd] {
        Some(x) => x,
        None => return u64::MAX,
    };
    let base = match whence {
        0 => 0,            // SEEK_SET
        1 => cur as i64,   // SEEK_CUR
        2 => size,         // SEEK_END
        _ => return u64::MAX,
    };
    let newoff = base + offset;
    if newoff < 0 {
        return u64::MAX;
    }
    fds[fd] = Some((fi, newoff as usize));
    newoff as u64
}

/// Read a NUL-terminated string from userspace. Stops at the NUL, at `max`,
/// OR as soon as the next byte would fall outside the arena — so a forged
/// pointer can never make kernel memory be read out. An out-of-arena pointer yields
/// an empty vector (the caller treats that as "no path").
fn user_cstr(ptr: u64, max: usize) -> alloc::vec::Vec<u8> {
    let mut v = alloc::vec::Vec::new();
    let mut i = 0;
    while i < max {
        let addr = match ptr.checked_add(i as u64) {
            Some(a) => a,
            None => break,
        };
        if !in_user_arena(addr, 1) {
            break;
        }
        // SAFETY: per-byte arena-validated; identity-mapped.
        let b = unsafe { *(addr as *const u8) };
        if b == 0 {
            break;
        }
        v.push(b);
        i += 1;
    }
    v
}

// Kernel stack for the syscall handler.
const KSTACK_SIZE: usize = 16 * 1024;
static mut KSTACK: [u8; KSTACK_SIZE] = [0; KSTACK_SIZE];

global_asm!(
    // SYSCALL entry from ring 3: rcx=user-rip, r11=user-rflags, rsp=user-rsp.
    ".global syscall_entry",
    "syscall_entry:",
    "mov [rip + USER_RSP], rsp",
    "mov rsp, [rip + KERNEL_RSP]",
    // SMAP window OPEN: set RFLAGS.AC (bit 18) so that for the duration of this
    // syscall ring 0 may read/write user pages (U=1). The syscall runs with IF=0
    // (FMASK clears IF) -> non-preemptive, so the window cannot leak through a
    // task switch. AC instead of `stac` -> also correct if the CPU has no SMAP
    // (no-op). In ring 0 AC enables NO alignment checks (that requires CPL=3).
    "pushfq",
    "bts qword ptr [rsp], 18",
    "popfq",
    // Save ALL user registers that must be preserved across a
    // syscall (real syscall ABI: only rax/rcx/r11 may change).
    "push rcx",                       // user-rip
    "push r11",                       // user-rflags
    "push rbx",
    "push rbp",
    "push rdi",
    "push rsi",
    "push rdx",
    "push r8",
    "push r9",
    "push r10",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rip + SAVED_REGS], rsp",    // pointer to the saved register block (clone)
    "mov [rip + USER_RIP], rcx",      // save user-rip (clone: thread resume point)
    "mov r9, r8",                     // dispatch arg5 = original r8 (clone: tls)
    "mov r8, r10",                    // dispatch arg4 = original r10 (clone: ctid)
    "mov rcx, rdx",                   // dispatch arg3 (original rdx)
    "mov rdx, rsi",                   // dispatch arg2 (rdi/rsi still original)
    "mov rsi, rdi",                   // dispatch arg1
    "mov rdi, rax",                   // dispatch num
    "call syscall_dispatch",          // rax = return value
    "mov r10, [rip + EXITED]",
    "test r10, r10",
    "jnz 9f",
    // Normal syscall -> restore registers (rax stays the return value) and SYSRET.
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    "pop rbp",
    "pop rbx",
    "pop r11",                        // user-rflags
    "pop rcx",                        // user-rip
    "mov rsp, [rip + USER_RSP]",
    "sysretq",
    "9:",                             // sys_exit -> back to the kernel.
    "mov rsp, [rip + SAVED_KERNEL_RSP]",
    "pushfq",                         // SMAP window CLOSED: clear AC (no sysret that restores r11)
    "btr qword ptr [rsp], 18",
    "popfq",
    "pop r15",                        // restore callee-saved registers of run()
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    // force_kernel_return: identical to the sys_exit epilogue, but callable
    // from the page-fault handler to abort a FAULTED foreground exec
    // (clean return to run_args instead of killing task 0/the shell). Never returns
    // to the caller.
    ".global force_kernel_return",
    "force_kernel_return:",
    "mov rsp, [rip + SAVED_KERNEL_RSP]",
    "pushfq",                         // SMAP window CLOSED (a faulted exec may have left AC open)
    "btr qword ptr [rsp], 18",
    "popfq",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    // enter_ring3(rdi=cs, rsi=ss, rdx=rip, rcx=rsp): jump into ring 3 via iretq.
    ".global enter_ring3",
    "enter_ring3:",
    // Save callee-saved registers: the ring-3 program clobbers them, but
    // run() (the caller) expects them intact after the sys_exit return.
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rip + SAVED_KERNEL_RSP], rsp", // now points to the saved registers
    "push rsi",                       // ss
    "push rcx",                       // rsp
    "push 0x002",                     // rflags (IF=0: run() is synchronous and NON-preemptive;
                                      // otherwise the timer interrupts the ring-3 excursion and the
                                      // scheduler switches stacks -> stack corruption/canary fault.
                                      // Scheduled ring-3 tasks run via sched::spawn_user with IF=1.)
    "push rdi",                       // cs
    "push rdx",                       // rip
    "iretq",
);

// The userspace program /bin/hello — a real, stripped **ELF64** binary,
// compiled by the EuroToolchain (Track 6) from C source. The kernel parses
// the ELF headers and loads the PT_LOAD segments (see load_elf64).
static HELLO_ELF: &[u8] = include_bytes!("../../userland/hello.elf");
static CAT_ELF: &[u8] = include_bytes!("../../userland/cat.elf");
static LINUXPROG_ELF: &[u8] = include_bytes!("../../userland/linuxprog.elf");
static FORKTEST_ELF: &[u8] = include_bytes!("../../userland/forktest.elf");
static EXECEE_ELF: &[u8] = include_bytes!("../../userland/execee.elf");
static FORKPIPE_ELF: &[u8] = include_bytes!("../../userland/forkpipe.elf");
static TICKER_ELF: &[u8] = include_bytes!("../../userland/ticker.elf");
static MUSLPROG_ELF: &[u8] = include_bytes!("../../userland/muslprog.elf");
static ARGVPROG_ELF: &[u8] = include_bytes!("../../userland/argvprog.elf");
static PIEPROG_ELF: &[u8] = include_bytes!("../../userland/pieprog.elf");
static TLSPROG_ELF: &[u8] = include_bytes!("../../userland/tlsprog.elf");
static DYNTLS_ELF: &[u8] = include_bytes!("../../userland/dyntls.elf");
static LIBTLS_SO: &[u8] = include_bytes!("../../userland/libtls.so");
// H3: dynamic-linking test artifacts — a dynamically-linked exe + the .so.
static DYNTEST_ELF: &[u8] = include_bytes!("../../userland/dyntest.elf");
static LIBEURO_SO: &[u8] = include_bytes!("../../userland/libeuro.so");
// 3C-3: PT_INTERP path — a dynamically-linked exe + a from-scratch USERSPACE
// dynamic linker (ld-euro.so) + its libc-euro.so.
static INTERPEXE_ELF: &[u8] = include_bytes!("../../userland/interpexe.elf");
static LDEURO_SO: &[u8] = include_bytes!("../../userland/ld-euro.so");
static LIBCEURO_SO: &[u8] = include_bytes!("../../userland/libc-euro.so");
static MUSLREAL_ELF: &[u8] = include_bytes!("../../userland/muslreal.elf");
static MUSLFILE_ELF: &[u8] = include_bytes!("../../userland/muslfile.elf");
static MCAT_ELF: &[u8] = include_bytes!("../../userland/mcat.elf");
static MWRITE_ELF: &[u8] = include_bytes!("../../userland/mwrite.elf");
static MECHO_ELF: &[u8] = include_bytes!("../../userland/mecho.elf");
static MUPPER_ELF: &[u8] = include_bytes!("../../userland/mupper.elf");
static DAEMON_ELF: &[u8] = include_bytes!("../../userland/daemon.elf");
static MSUM_ELF: &[u8] = include_bytes!("../../userland/msum.elf");
static MENV_ELF: &[u8] = include_bytes!("../../userland/menv.elf");
static MSOCK_ELF: &[u8] = include_bytes!("../../userland/msock.elf");
static MDNS_ELF: &[u8] = include_bytes!("../../userland/mdns.elf");
static MTRACK_ELF: &[u8] = include_bytes!("../../userland/mtrack.elf");
static TLSCOUNT_ELF: &[u8] = include_bytes!("../../userland/tlscount.elf");
static ISOTEST_ELF: &[u8] = include_bytes!("../../userland/isotest.elf");
static WORKER_ELF: &[u8] = include_bytes!("../../userland/worker.elf");
static MTHREAD_ELF: &[u8] = include_bytes!("../../userland/mthread.elf");
static MPTHREAD_ELF: &[u8] = include_bytes!("../../userland/mpthread.elf");
static MMUTEX_ELF: &[u8] = include_bytes!("../../userland/mmutex.elf");
static IPCRECV_ELF: &[u8] = include_bytes!("../../userland/ipcrecv.elf");
static IPCSEND_ELF: &[u8] = include_bytes!("../../userland/ipcsend.elf");

/// The ELF bytes of /bin/mmutex (pthread_mutex under contention via futex).
pub fn mmutex_bytes() -> &'static [u8] {
    MMUTEX_ELF
}

/// The ELF bytes of the EuroIPC demos (receiver + sender).
pub fn ipcrecv_bytes() -> &'static [u8] {
    IPCRECV_ELF
}
pub fn ipcsend_bytes() -> &'static [u8] {
    IPCSEND_ELF
}

/// The ELF bytes of /bin/mthread (threads demo: clone + shared memory).
pub fn mthread_bytes() -> &'static [u8] {
    MTHREAD_ELF
}

/// The ELF bytes of /bin/mpthread (real musl pthreads: create + join).
pub fn mpthread_bytes() -> &'static [u8] {
    MPTHREAD_ELF
}

/// The ELF bytes of /bin/tlscount (musl demo: per-process __thread counter).
pub fn tlscount_bytes() -> &'static [u8] {
    TLSCOUNT_ELF
}

/// The ELF bytes of /bin/isotest (musl demo: memory-isolation violation).
pub fn isotest_bytes() -> &'static [u8] {
    ISOTEST_ELF
}

/// The ELF bytes of /bin/worker (musl demo: computes, reports, exit(0)).
pub fn worker_bytes() -> &'static [u8] {
    WORKER_ELF
}

// A ring-3 process that endlessly increments a counter in its OWN user memory.
// Preemptively interleaved by the scheduler; the kernel reads the counter and shows
// that the process makes progress — proof of userspace multitasking.
global_asm!(
    ".global utask_start",
    ".global utask_cnt",
    ".global utask_end",
    "utask_start:",
    "utask_loop:",
    "inc qword ptr [rip + utask_cnt]",
    "jmp utask_loop",
    ".align 8",
    "utask_cnt:",
    ".quad 0",
    "utask_end:",
);

extern "sysv64" {
    fn syscall_entry();
    fn enter_ring3(cs: u64, ss: u64, rip: u64, rsp: u64);
    /// Abort a foreground exec after a page fault: clean return into run_args.
    fn force_kernel_return();
}
extern "C" {
    static utask_start: u8;
    static utask_cnt: u8;
    static utask_end: u8;
}

/// Start a ring-3 process that increments a counter and add it to the
/// scheduler. EACH process gets its own code, stack AND kernel stack so that
/// multiple ring-3 processes can run preemptively at once. Returns the address
/// of the counter (the kernel reads it for display).
pub fn spawn_counter_task(falloc: &mut FrameAllocator) -> u64 {
    init_syscall_msrs();
    const MIB2: u64 = 1 << 21;

    let start = core::ptr::addr_of!(utask_start) as usize;
    let end = core::ptr::addr_of!(utask_end) as usize;
    let cnt = core::ptr::addr_of!(utask_cnt) as usize;
    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
    let cnt_off = (cnt - start) as u64;

    // Own isolated 2 MiB arena + PML4 instead of loose frames on the boot CR3, so that
    // this ring-3 process does not run on the supervisor-only boot PML4 (SMEP/SMAP-safe).
    // 2 MiB, exactly 2 MiB-aligned in one go (no more 4 MiB over-allocation):
    // the counter task never reaps, so this is the safest place to use
    // allocate_aligned. Saves ~2 MiB compared to allocate_contiguous(1024)+manual alignment.
    let arena = falloc.allocate_aligned(512, 512).expect("utask-arena");
    let code = arena;
    let stack_top = arena + MIB2; // user stack grows downward from the arena top
    // Own kernel stack (4 frames = 16 KiB) for the ring3->ring0 interrupt frames.
    let kstack = falloc.allocate_contiguous(4).expect("utask kernel-stack");
    let kstack_top = (kstack + 4 * 4096) & !0xF;
    // SAFETY: arena lies in the identity-mapped lowest 1 GiB; under the boot CR3
    // (where we now run) that is a supervisor page -> writing is allowed.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), code as *mut u8, bytes.len().min(4096));
    }
    let counter_ptr = code + cnt_off;

    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    // Counter demo: raw machine-code blob with the counter variable IN the code page ->
    // code/data cannot be separated, so RWX instead of W^X (see build_address_space_rwx).
    // Build the address space first, then spawn with cr3 set before Ready (BUG-007).
    let pml4 = crate::paging::build_address_space_rwx(falloc, arena);
    let idx = crate::sched::spawn_user(code, stack_top, user_cs, user_ss, kstack_top, pml4);
    counter_ptr
}

/// Read a counter value. `ptr` is the PHYSICAL arena address; under the boot CR3 (where
/// the kernel/shell runs) that is a supervisor-identity page -> simply readable.
pub fn read_counter(ptr: u64) -> u64 {
    if ptr == 0 {
        return 0;
    }
    // SAFETY: ptr points to the identity-mapped (supervisor) arena page of a process.
    unsafe { core::ptr::read_volatile(ptr as *const u64) }
}

/// The ELF bytes of /bin/hello compiled by the EuroToolchain.
pub fn program_bytes() -> &'static [u8] {
    HELLO_ELF
}

/// The ELF bytes of /bin/cat.
pub fn cat_bytes() -> &'static [u8] {
    CAT_ELF
}

/// The ELF bytes of /bin/linuxprog (Linux ABI).
pub fn linuxprog_bytes() -> &'static [u8] {
    LINUXPROG_ELF
}

/// The ELF bytes of /bin/forktest (S3 fork/waitpid test, Linux ABI).
pub fn forktest_bytes() -> &'static [u8] {
    FORKTEST_ELF
}

/// The ELF bytes of /bin/execee (S3 execve target, Linux ABI).
pub fn execee_bytes() -> &'static [u8] {
    EXECEE_ELF
}

/// The ELF bytes of /bin/forkpipe (S3 pipe+fork IPC test, Linux ABI).
pub fn forkpipe_bytes() -> &'static [u8] {
    FORKPIPE_ELF
}

/// The ELF bytes of /bin/ticker (S4 demo service, Linux ABI).
pub fn ticker_bytes() -> &'static [u8] {
    TICKER_ELF
}

/// The ELF bytes of /bin/muslprog (musl-like Linux startup).
pub fn muslprog_bytes() -> &'static [u8] {
    MUSLPROG_ELF
}

/// The ELF bytes of /bin/argvprog (reads argc/argv/envp/auxv from the SysV stack).
pub fn argvprog_bytes() -> &'static [u8] {
    ARGVPROG_ELF
}

/// The ELF bytes of /bin/pieprog (real PIE with R_X86_64_RELATIVE relocations).
pub fn pieprog_bytes() -> &'static [u8] {
    PIEPROG_ELF
}

/// The ELF bytes of /bin/muslreal (real binary linked against musl libc).
pub fn muslreal_bytes() -> &'static [u8] {
    MUSLREAL_ELF
}

/// The ELF bytes of /bin/muslfile (musl binary that reads EuroFS via fopen/fgets).
pub fn muslfile_bytes() -> &'static [u8] {
    MUSLFILE_ELF
}

/// The ELF bytes of /bin/mcat (musl `cat` that uses argv[1] as the file name).
pub fn mcat_bytes() -> &'static [u8] {
    MCAT_ELF
}

/// The ELF bytes of /bin/mwrite (musl binary that writes a file).
pub fn mwrite_bytes() -> &'static [u8] {
    MWRITE_ELF
}

/// The ELF bytes of /bin/mecho (musl `echo`: print the arguments).
pub fn mecho_bytes() -> &'static [u8] {
    MECHO_ELF
}

/// The ELF bytes of /bin/mupper (musl filter: stdin -> UPPERCASE).
pub fn mupper_bytes() -> &'static [u8] {
    MUPPER_ELF
}

/// The ELF bytes of /bin/daemon (native background heartbeat daemon).
pub fn daemon_bytes() -> &'static [u8] {
    DAEMON_ELF
}

/// The ELF bytes of /bin/menv (musl program that reads envp/getenv).
pub fn menv_bytes() -> &'static [u8] {
    MENV_ELF
}

/// The ELF bytes of /bin/msock (musl program that networks via POSIX sockets).
pub fn msock_bytes() -> &'static [u8] {
    MSOCK_ELF
}

/// The ELF bytes of /bin/mdns (musl program: DNS lookup via a UDP socket).
pub fn mdns_bytes() -> &'static [u8] {
    MDNS_ELF
}

/// The ELF bytes of /bin/mtrack (EuroGuard demo: blocked tracker connection).
pub fn mtrack_bytes() -> &'static [u8] {
    MTRACK_ELF
}

/// The baked-in Ed25519 signature (64 bytes) of an installed program,
/// made on the host with the EuroOS developer key (userland/sign.py). The kernel
/// verifies it against the baked-in public key before execution.
/// An installable package that is NOT in the boot set: (ELF bytes, caps, abi).
/// Installed via the shell `install <name>` after Ed25519 verification.
pub fn installable(name: &str) -> Option<(&'static [u8], u64, bool)> {
    match name {
        "msum" => Some((MSUM_ELF, CAP_CONSOLE, true)),
        _ => None,
    }
}

pub fn program_sig(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "/bin/hello" => include_bytes!("../../userland/hello.elf.sig"),
        "/bin/msum" => include_bytes!("../../userland/msum.elf.sig"),
        "/bin/menv" => include_bytes!("../../userland/menv.elf.sig"),
        "/bin/msock" => include_bytes!("../../userland/msock.elf.sig"),
        "/bin/mdns" => include_bytes!("../../userland/mdns.elf.sig"),
        "/bin/mtrack" => include_bytes!("../../userland/mtrack.elf.sig"),
        "/bin/tlscount" => include_bytes!("../../userland/tlscount.elf.sig"),
        "/bin/isotest" => include_bytes!("../../userland/isotest.elf.sig"),
        "/bin/worker" => include_bytes!("../../userland/worker.elf.sig"),
        "/bin/mthread" => include_bytes!("../../userland/mthread.elf.sig"),
        "/bin/mpthread" => include_bytes!("../../userland/mpthread.elf.sig"),
        "/bin/mmutex" => include_bytes!("../../userland/mmutex.elf.sig"),
        "/bin/ipcrecv" => include_bytes!("../../userland/ipcrecv.elf.sig"),
        "/bin/ipcsend" => include_bytes!("../../userland/ipcsend.elf.sig"),
        "/bin/cat" => include_bytes!("../../userland/cat.elf.sig"),
        "/bin/linuxprog" => include_bytes!("../../userland/linuxprog.elf.sig"),
        "/bin/forktest" => include_bytes!("../../userland/forktest.elf.sig"),
        "/bin/execee" => include_bytes!("../../userland/execee.elf.sig"),
        "/bin/forkpipe" => include_bytes!("../../userland/forkpipe.elf.sig"),
        "/bin/ticker" => include_bytes!("../../userland/ticker.elf.sig"),
        "/bin/muslprog" => include_bytes!("../../userland/muslprog.elf.sig"),
        "/bin/argvprog" => include_bytes!("../../userland/argvprog.elf.sig"),
        "/bin/pieprog" => include_bytes!("../../userland/pieprog.elf.sig"),
        "/bin/muslreal" => include_bytes!("../../userland/muslreal.elf.sig"),
        "/bin/muslfile" => include_bytes!("../../userland/muslfile.elf.sig"),
        "/bin/mcat" => include_bytes!("../../userland/mcat.elf.sig"),
        "/bin/mwrite" => include_bytes!("../../userland/mwrite.elf.sig"),
        "/bin/mecho" => include_bytes!("../../userland/mecho.elf.sig"),
        "/bin/mupper" => include_bytes!("../../userland/mupper.elf.sig"),
        "/bin/daemon" => include_bytes!("../../userland/daemon.elf.sig"),
        _ => return None,
    })
}

/// Verify the Ed25519 signature of a program (by name) over the
/// actually-loaded bytes. `true` = authentic + unchanged -> may run.
pub fn verify_program(path: &str, bytes: &[u8]) -> bool {
    match program_sig(path) {
        Some(sig) => crate::crypto::verify(bytes, sig),
        None => false, // no signature known -> not trusted
    }
}

// ── Minimal ELF64 loader ─────────────────────────────────────────────────
// Bounds-safe (audit H11/kernel-H6): a malformed/too-short ELF must not make these
// readers panic; on an out-of-range offset -> 0 (and the bound checks above
// reject the header further on).
fn rd_u16(b: &[u8], o: usize) -> u16 {
    match b.get(o..o + 2) {
        Some(s) => u16::from_le_bytes([s[0], s[1]]),
        None => 0,
    }
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    match b.get(o..o + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    match b.get(o..o + 8) {
        Some(s) => a.copy_from_slice(s),
        None => return 0,
    }
    u64::from_le_bytes(a)
}

/// Max. number of contiguous User pages a program may span (1 MiB).
/// Bounds the allocation and keeps everything within the USER-mapped lowest 1 GiB.
const MAX_PROG_PAGES: usize = 256;

/// How many User pages does this program need (highest vaddr+memsz, or the
/// flat length)? Determines the contiguous frame allocation in advance.
fn program_span_pages(program: &[u8]) -> usize {
    let span = if program.len() >= 4 && &program[0..4] == b"\x7fELF" && program.len() >= 64 {
        let e_phoff = rd_u64(program, 32) as usize;
        let e_phentsize = rd_u16(program, 54) as usize;
        let e_phnum = rd_u16(program, 56) as usize;
        let mut hi = 0u64;
        for i in 0..e_phnum {
            let ph = e_phoff + i * e_phentsize;
            if ph + 56 > program.len() || rd_u32(program, ph) != 1 {
                continue; // PT_LOAD only
            }
            hi = hi.max(rd_u64(program, ph + 16) + rd_u64(program, ph + 40)); // vaddr+memsz
        }
        hi as usize
    } else {
        program.len()
    };
    (((span + 0xFFF) / 4096).max(1)).min(MAX_PROG_PAGES)
}

/// Result of loading: entry + program-header info for the auxv.
/// (musl's `_start` reads AT_PHDR/AT_PHENT/AT_PHNUM/AT_ENTRY/AT_BASE.)
#[derive(Clone, Copy)]
struct LoadInfo {
    entry: u64,
    phdr: u64,  // runtime address of the program-header table (0 = none)
    phent: u64, // size of one program header
    phnum: u64, // number of program headers
    base: u64,  // load bias (start of the frame window)
    /// W^X bitmaps over the 512 4 KiB pages of the 2 MiB arena. `exec_pages`: page
    /// falls under an EXECUTABLE segment (PF_X). `writ_pages`: under a WRITABLE
    /// segment (PF_W). build_address_space maps exec-only -> R-X, exec+writ -> RWX (a
    /// binary with a mixed RWE segment cannot enforce W^X), the rest -> RW + NX.
    exec_pages: [u64; 8],
    writ_pages: [u64; 8],
}

/// Mark the 4 KiB pages that `[start, start+len)` (arena-relative offset)
/// touch as executable in the W^X bitmap.
fn mark_exec_pages(bits: &mut [u64; 8], start: u64, len: u64) {
    if len == 0 {
        return;
    }
    let first = (start / 4096) as usize;
    let last = ((start + len - 1) / 4096) as usize;
    let mut p = first;
    while p <= last && p < 512 {
        bits[p / 64] |= 1u64 << (p % 64);
        p += 1;
    }
}

/// Apply R_X86_64_RELATIVE relocations: for a PIE (ET_DYN) linked at 0 and
/// loaded at `base`, `*(base + r_offset) = base + r_addend` holds. This is exactly
/// what musl's static-PIE self-reloc otherwise does itself — we do it in the kernel.
/// We read all tables from the LOADED memory (base + vaddr): file offset and
/// vaddr diverge in a PIE, but in the loaded image vaddr is always correct.
fn apply_relocations(elf: &[u8], base: u64, limit: u64) {
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    // Find PT_DYNAMIC (p_type == 2).
    let mut dyn_vaddr = 0u64;
    let mut dyn_sz = 0usize;
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() || rd_u32(elf, ph) != 2 {
            continue;
        }
        dyn_vaddr = rd_u64(elf, ph + 16);
        dyn_sz = rd_u64(elf, ph + 32) as usize;
        break;
    }
    if dyn_vaddr == 0 {
        return; // no dynamic section (flat/statically-linked ELF)
    }
    // Read the dynamic entries from the loaded memory; collect the RELA table.
    let rd_loaded = |a: u64| unsafe { ((base + a) as *const u64).read() };
    let mut rela = 0u64;
    let mut relasz = 0u64;
    let mut relaent = 24u64;
    let mut o = 0u64;
    while (o as usize) + 16 <= dyn_sz {
        let tag = rd_loaded(dyn_vaddr + o);
        let val = rd_loaded(dyn_vaddr + o + 8);
        match tag {
            7 => rela = val,    // DT_RELA   (vaddr of the table)
            8 => relasz = val,  // DT_RELASZ (bytes)
            9 => relaent = val, // DT_RELAENT
            0 => break,         // DT_NULL
            _ => {}
        }
        o += 16;
    }
    if rela == 0 || relasz == 0 || relaent == 0 {
        return;
    }
    let mut off = 0u64;
    let mut applied = 0u32;
    while off + relaent <= relasz {
        let e = rela + off;
        let r_offset = rd_loaded(e);
        let r_info = rd_loaded(e + 8);
        let r_addend = rd_loaded(e + 16);
        if (r_info & 0xffff_ffff) == 8 && r_offset < limit {
            // R_X86_64_RELATIVE: *(base + r_offset) = base + r_addend
            unsafe { ((base + r_offset) as *mut u64).write(base.wrapping_add(r_addend)) };
            applied += 1;
        }
        off += relaent;
    }
    crate::serial_println!("[elf] {applied} R_X86_64_RELATIVE relocations applied @ base {base:#x}");
}

/// Mark pages writable in the W^X bitmap (same mechanism as exec).
fn mark_writ_pages(bits: &mut [u64; 8], start: u64, len: u64) {
    mark_exec_pages(bits, start, len);
}

/// Find the PT_TLS segment (p_type==7): (vaddr, filesz, memsz, align>=8).
fn find_pt_tls(elf: &[u8]) -> Option<(u64, u64, u64, u64)> {
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() {
            break;
        }
        if rd_u32(elf, ph) == 7 {
            let vaddr = rd_u64(elf, ph + 16);
            let filesz = rd_u64(elf, ph + 32);
            let memsz = rd_u64(elf, ph + 40);
            let align = rd_u64(elf, ph + 48).max(8);
            return Some((vaddr, filesz, memsz, align));
        }
    }
    None
}

/// Arena offset for the static TLS block (above the heap, below the stack).
const TLS_WINDOW: u64 = 0x188000;

/// **Kernel-as-ld.so: static TLS setup (variant-II, x86-64).** Builds the
/// static TLS block from the `PT_TLS` of EACH given module (exe + .so's): each
/// module gets an offset BELOW the thread pointer (TP), the TCB self-pointer word
/// sits at TP (`%fs:0x0` -> TP). A `__thread` var at template offset v in module m
/// sits at `TP - tlsoffset[m] + v`. Returns (TP, [(module_base, tlsoffset)]) —
/// the offsets are needed to patch `R_X86_64_TPOFF64` relocations.
fn setup_static_tls(arena: u64, modules: &[(u64, &[u8])], info: &mut LoadInfo) -> (Option<u64>, Vec<(u64, u64)>) {
    // Collect the TLS modules: (base, vaddr, filesz, memsz, align).
    let mut tls: Vec<(u64, u64, u64, u64, u64)> = Vec::new();
    for (base, elf) in modules {
        if let Some((v, f, m, a)) = find_pt_tls(elf) {
            tls.push((*base, v, f, m, a));
        }
    }
    if tls.is_empty() {
        return (None, Vec::new());
    }
    // Assign offsets (glibc algorithm): offset accumulates per module.
    let mut offset = 0u64;
    let mut offsets: Vec<(u64, u64)> = Vec::new();
    for (base, _v, _f, memsz, align) in &tls {
        offset = (offset + memsz + align - 1) & !(align - 1);
        offsets.push((*base, offset));
    }
    let total = offset;
    let region = arena + TLS_WINDOW;
    let tp = region + total;
    unsafe {
        core::ptr::write_bytes(region as *mut u8, 0, (total + 8) as usize); // zero the whole block + TCB word
        for ((base, vaddr, filesz, _memsz, _a), (_b, toff)) in tls.iter().zip(offsets.iter()) {
            let dst = tp - toff;
            core::ptr::copy_nonoverlapping((base + vaddr) as *const u8, dst as *mut u8, *filesz as usize);
        }
        (tp as *mut u64).write(tp); // TCB self-pointer at TP (%fs:0x0)
    }
    mark_writ_pages(&mut info.writ_pages, TLS_WINDOW, total + 4096);
    crate::serial_println!(
        "[tls] static TLS block @ {region:#x}, TP={tp:#x}, {} module(s), total {total} B",
        tls.len()
    );
    (Some(tp), offsets)
}

/// Patch the `R_X86_64_TPOFF64` relocations (type 18) of one module: write the
/// initial-exec TP offset into the GOT slot — `tpoff = sym.st_value - tlsoffset + addend`
/// (the var sits at `%fs + tpoff`). Returns the number of patched relocations.
fn apply_tls_relocs(base: u64, elf: &[u8], tlsoffset: u64) -> u32 {
    let symtab = match dyn_value(base, elf, 6) {
        Some(s) => s,
        None => return 0,
    };
    let tables = [
        (dyn_value(base, elf, 23), dyn_value(base, elf, 2)), // .rela.plt
        (dyn_value(base, elf, 7), dyn_value(base, elf, 8)),  // .rela.dyn
    ];
    let mut patched = 0u32;
    for (rela_opt, sz_opt) in tables {
        let (rela, sz) = match (rela_opt, sz_opt) {
            (Some(r), Some(s)) => (r, s),
            _ => continue,
        };
        let mut off = 0u64;
        while off + 24 <= sz {
            let e = base + rela + off;
            let r_offset = unsafe { (e as *const u64).read() };
            let r_info = unsafe { ((e + 8) as *const u64).read() };
            let r_addend = unsafe { ((e + 16) as *const u64).read() };
            if (r_info & 0xffff_ffff) == 18 {
                let sym_idx = r_info >> 32;
                let st_value = unsafe { ((base + symtab + sym_idx * 24 + 8) as *const u64).read() };
                let tpoff = (st_value as i64) - (tlsoffset as i64) + (r_addend as i64);
                unsafe { ((base + r_offset) as *mut u64).write(tpoff as u64) };
                patched += 1;
            }
            off += 24;
        }
    }
    patched
}

/// Load the PT_LOAD segments of an ELF64 binary at base address `base` (position-
/// independent; linked at vaddr 0). `pages` = size of the frame window.
fn load_elf64(elf: &[u8], base: u64, pages: usize) -> Option<LoadInfo> {
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return None; // not a 64-bit little-endian ELF
    }
    if rd_u16(elf, 18) != 0x3E {
        return None; // not x86-64
    }
    let limit = (pages * 4096) as u64;
    let e_entry = rd_u64(elf, 24);
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    let mut phdr_vaddr = 0u64; // vaddr of the PHDR table if it falls in a PT_LOAD
    let mut exec_pages = [0u64; 8]; // W^X: which pages are executable (PF_X)
    let mut writ_pages = [0u64; 8]; // W^X: which pages are writable (PF_W)
    for i in 0..e_phnum {
        // Overflow-safe (audit H11): a huge e_phoff/e_phentsize must not bypass the
        // bound check via wrap-around.
        let ph = match e_phoff.checked_add(i.checked_mul(e_phentsize)?) {
            Some(v) => v,
            None => continue,
        };
        if ph.checked_add(56).map_or(true, |e| e > elf.len()) {
            continue;
        }
        let p_type = rd_u32(elf, ph);
        // PT_PHDR (6) gives the vaddr of the program-header table directly.
        if p_type == 6 {
            phdr_vaddr = rd_u64(elf, ph + 16);
        }
        if p_type != 1 {
            continue; // beyond this only PT_LOAD
        }
        let p_flags = rd_u32(elf, ph + 4);
        let p_offset = rd_u64(elf, ph + 8) as usize;
        let p_vaddr = rd_u64(elf, ph + 16);
        let p_filesz = rd_u64(elf, ph + 32) as usize;
        let p_memsz = rd_u64(elf, ph + 40) as usize;
        // Overflow-safe (audit H11): wrap-around must not bypass the window check.
        let file_end = p_offset.checked_add(p_filesz)?;
        let mem_end = p_vaddr.checked_add(p_memsz as u64)?;
        if file_end > elf.len() || mem_end > limit {
            return None;
        }
        // W^X: note per page whether an executable (PF_X = bit 0) and/or writable
        // (PF_W = bit 1) segment covers it.
        if p_flags & 1 != 0 {
            mark_exec_pages(&mut exec_pages, p_vaddr, p_memsz as u64);
        }
        if p_flags & 2 != 0 {
            mark_exec_pages(&mut writ_pages, p_vaddr, p_memsz as u64);
        }
        // The PHDR table sits by default within the first PT_LOAD (at file offset
        // e_phoff). If there is no PT_PHDR, we derive the vaddr from it.
        if phdr_vaddr == 0 && p_offset <= e_phoff && e_phoff < p_offset + p_filesz {
            phdr_vaddr = p_vaddr + (e_phoff - p_offset) as u64;
        }
        // SAFETY: the segment fits within the assigned frame window (checked).
        unsafe {
            let dst = (base + p_vaddr) as *mut u8;
            core::ptr::copy_nonoverlapping(elf[p_offset..].as_ptr(), dst, p_filesz);
            if p_memsz > p_filesz {
                core::ptr::write_bytes(dst.add(p_filesz), 0, p_memsz - p_filesz); // zero .bss
            }
        }
    }
    // Apply relocations (no-op for non-PIE/flat-static binaries).
    apply_relocations(elf, base, limit);
    Some(LoadInfo {
        entry: base + e_entry,
        phdr: if phdr_vaddr != 0 { base + phdr_vaddr } else { 0 },
        phent: e_phentsize as u64,
        phnum: e_phnum as u64,
        base,
        exec_pages,
        writ_pages,
    })
}

/// Load a program (ELF or flat) at `base` (window of `pages` frames).
fn load_program(program: &[u8], base: u64, pages: usize) -> LoadInfo {
    if program.len() >= 4 && &program[0..4] == b"\x7fELF" {
        if let Some(info) = load_elf64(program, base, pages) {
            return info;
        }
    }
    // Flat blob (not ELF): entry = base, no program headers. The entire loaded
    // region is machine code -> mark those pages executable (W^X).
    let n = program.len().min(pages * 4096);
    // SAFETY: flat blob, fits in the window.
    unsafe {
        core::ptr::copy_nonoverlapping(program.as_ptr(), base as *mut u8, n);
    }
    // Flat blob = mixed code+data (RWX); mark the loaded region both
    // executable and writable so build_address_space maps it RWX.
    let mut exec_pages = [0u64; 8];
    let mut writ_pages = [0u64; 8];
    mark_exec_pages(&mut exec_pages, 0, n as u64);
    mark_exec_pages(&mut writ_pages, 0, n as u64);
    LoadInfo { entry: base, phdr: 0, phent: 0, phnum: 0, base, exec_pages, writ_pages }
}

// ── H3: in-kernel dynamic linker ────────────────────────────────────────
// Loads a dynamically-linked executable + its DT_NEEDED shared libraries into
// the same address space and resolves the cross-module symbols (R_X86_64_JUMP_SLOT /
// GLOB_DAT) — like a userspace `ld.so`, but in the kernel (deterministic,
// EuroGuard-controlled). All tables are read from the LOADED memory (base+vaddr);
// the .so is placed at its own sub-offset within the 2 MiB arena.

/// Merge a W^X bitmap shifted by `page_off` pages (for a module loaded at an
/// arena offset) into the combined arena bitmap.
fn merge_shifted(dst: &mut [u64; 8], src: &[u64; 8], page_off: usize) {
    for p in 0..512usize {
        if src[p / 64] & (1u64 << (p % 64)) != 0 {
            let q = p + page_off;
            if q < 512 {
                dst[q / 64] |= 1u64 << (q % 64);
            }
        }
    }
}

/// Read a `DT_<want>` value from the loaded dynamic table of a module.
fn dyn_value(base: u64, elf: &[u8], want: u64) -> Option<u64> {
    let e_phoff = rd_u64(elf, 32) as usize;
    let e_phentsize = rd_u16(elf, 54) as usize;
    let e_phnum = rd_u16(elf, 56) as usize;
    let (mut dv, mut dsz) = (0u64, 0usize);
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > elf.len() || rd_u32(elf, ph) != 2 {
            continue;
        }
        dv = rd_u64(elf, ph + 16);
        dsz = rd_u64(elf, ph + 32) as usize;
        break;
    }
    if dv == 0 {
        return None;
    }
    let mut o = 0u64;
    while (o as usize) + 16 <= dsz {
        let tag = unsafe { ((base + dv + o) as *const u64).read() };
        let val = unsafe { ((base + dv + o + 8) as *const u64).read() };
        if tag == 0 {
            return None; // DT_NULL
        }
        if tag == want {
            return Some(val);
        }
        o += 16;
    }
    None
}

/// Read a C-string (max `buf.len()`) at a loaded address into `buf`; return the length.
fn read_cstr(addr: u64, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n + 1 < buf.len() {
        let c = unsafe { ((addr + n as u64) as *const u8).read() };
        if c == 0 {
            break;
        }
        buf[n] = c;
        n += 1;
    }
    n
}

/// Find an EXPORTED symbol by name in a loaded module -> `base + st_value`.
/// Iterates the dynamic symbol table (count from DT_HASH's nchain).
fn find_export(base: u64, elf: &[u8], name: &[u8]) -> Option<u64> {
    let symtab = dyn_value(base, elf, 6)?; // DT_SYMTAB
    let strtab = dyn_value(base, elf, 5)?; // DT_STRTAB
    // Symbol count: from DT_HASH's nchain if present, otherwise (modern .so's have
    // only GNU_HASH) derived from (DT_STRTAB - DT_SYMTAB)/DT_SYMENT — the linker places
    // `.dynsym` always directly before `.dynstr`.
    let syment = dyn_value(base, elf, 11).unwrap_or(24); // DT_SYMENT
    let count = if let Some(hash) = dyn_value(base, elf, 4) {
        (unsafe { ((base + hash + 4) as *const u32).read() }) as u64 // DT_HASH nchain
    } else if strtab > symtab && syment > 0 {
        (strtab - symtab) / syment
    } else {
        0
    };
    let mut nb = [0u8; 64];
    for i in 1..count {
        let sym = base + symtab + i * syment;
        let st_name = unsafe { (sym as *const u32).read() } as u64;
        let st_shndx = unsafe { ((sym + 6) as *const u16).read() };
        if st_shndx == 0 {
            continue; // SHN_UNDEF: not defined here
        }
        let nl = read_cstr(base + strtab + st_name, &mut nb);
        if &nb[..nl] == name {
            let st_value = unsafe { ((sym + 8) as *const u64).read() };
            return Some(base + st_value);
        }
    }
    None
}

/// Resolve the symbol relocations (R_X86_64_JUMP_SLOT + GLOB_DAT) of the exe against the
/// loaded libs: write the real symbol address into the GOT slot. Returns (resolved,
/// unresolved).
fn link_symbol_relocations(exe_base: u64, exe_elf: &[u8], libs: &[(u64, &[u8])]) -> (u32, u32) {
    let (symtab, strtab) = match (dyn_value(exe_base, exe_elf, 6), dyn_value(exe_base, exe_elf, 5)) {
        (Some(s), Some(t)) => (s, t),
        _ => return (0, 0),
    };
    let (mut resolved, mut unresolved) = (0u32, 0u32);
    // .rela.plt (DT_JMPREL=23, DT_PLTRELSZ=2) + .rela.dyn (DT_RELA=7, DT_RELASZ=8).
    let tables = [
        (dyn_value(exe_base, exe_elf, 23), dyn_value(exe_base, exe_elf, 2)),
        (dyn_value(exe_base, exe_elf, 7), dyn_value(exe_base, exe_elf, 8)),
    ];
    let mut nb = [0u8; 64];
    for (rela_opt, sz_opt) in tables {
        let (rela, sz) = match (rela_opt, sz_opt) {
            (Some(r), Some(s)) => (r, s),
            _ => continue,
        };
        let mut off = 0u64;
        while off + 24 <= sz {
            let e = exe_base + rela + off;
            let r_offset = unsafe { (e as *const u64).read() };
            let r_info = unsafe { ((e + 8) as *const u64).read() };
            let rtype = r_info & 0xffff_ffff;
            let sym_idx = r_info >> 32;
            // 7 = JUMP_SLOT (PLT), 6 = GLOB_DAT (data). Both: *(GOT) = symbol address.
            if (rtype == 7 || rtype == 6) && sym_idx != 0 {
                let sym = exe_base + symtab + sym_idx * 24;
                let st_name = unsafe { (sym as *const u32).read() } as u64;
                let nl = read_cstr(exe_base + strtab + st_name, &mut nb);
                let name = &nb[..nl];
                let mut done = false;
                for (lb, le) in libs {
                    if let Some(addr) = find_export(*lb, le, name) {
                        unsafe { ((exe_base + r_offset) as *mut u64).write(addr) };
                        resolved += 1;
                        done = true;
                        break;
                    }
                }
                if !done {
                    unresolved += 1;
                    crate::serial_println!(
                        "[h3] UNRESOLVED symbol: {}",
                        core::str::from_utf8(name).unwrap_or("?")
                    );
                }
            }
            off += 24;
        }
    }
    (resolved, unresolved)
}

/// H3 self-test: load the dynamically-linked `dyntest.elf` + `libeuro.so` into one
/// address space, link them in-kernel, and run dyntest in ring 3. dyntest calls
/// `euro_answer()` from the .so (via PLT/GOT) -> "H3: 42" + exit(42). Returns
/// (output, exit_code).
/// The embedded dynamically-linked test exe + .so (for populate_fs/self-tests).
pub fn dyntest_bytes() -> &'static [u8] {
    DYNTEST_ELF
}
pub fn libeuro_bytes() -> &'static [u8] {
    LIBEURO_SO
}

/// Parse the DT_NEEDED shared-library names from a dynamically-linked ELF (from the
/// FILE bytes; translates the DT_STRTAB vaddr to a file offset via the PT_LOADs).
pub fn needed_libs(program: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if program.len() < 64 || &program[0..4] != b"\x7fELF" {
        return out;
    }
    let e_phoff = rd_u64(program, 32) as usize;
    let e_phentsize = rd_u16(program, 54) as usize;
    let e_phnum = rd_u16(program, 56) as usize;
    let mut dyn_off = 0usize;
    let mut dyn_sz = 0usize;
    let mut loads: Vec<(u64, usize, usize)> = Vec::new(); // (vaddr, file_off, filesz)
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 56 > program.len() {
            continue;
        }
        match rd_u32(program, ph) {
            2 => {
                dyn_off = rd_u64(program, ph + 8) as usize;
                dyn_sz = rd_u64(program, ph + 32) as usize;
            }
            1 => loads.push((rd_u64(program, ph + 16), rd_u64(program, ph + 8) as usize, rd_u64(program, ph + 32) as usize)),
            _ => {}
        }
    }
    if dyn_off == 0 {
        return out;
    }
    let v2o = |vaddr: u64| -> Option<usize> {
        loads.iter().find(|&&(va, _, fsz)| vaddr >= va && vaddr < va + fsz as u64).map(|&(va, fo, _)| fo + (vaddr - va) as usize)
    };
    let mut strtab = 0u64;
    let mut needed: Vec<u64> = Vec::new();
    let mut o = 0;
    while o + 16 <= dyn_sz && dyn_off + o + 16 <= program.len() {
        let tag = rd_u64(program, dyn_off + o);
        let val = rd_u64(program, dyn_off + o + 8);
        match tag {
            0 => break,
            1 => needed.push(val), // DT_NEEDED
            5 => strtab = val,     // DT_STRTAB
            _ => {}
        }
        o += 16;
    }
    let strtab_off = match v2o(strtab) {
        Some(x) => x,
        None => return out,
    };
    for noff in needed {
        let mut p = strtab_off + noff as usize;
        let mut s = String::new();
        while p < program.len() && program[p] != 0 {
            s.push(program[p] as char);
            p += 1;
        }
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

/// Load a dynamically-linked exe + its shared libraries into one address space, link
/// them in-kernel (DT_NEEDED -> load .so -> resolve JUMP_SLOT/GLOB_DAT), and run the
/// exe in ring 3. Returns (output, exit_code). Up to 2 libs (in the arena before the heap).
pub fn run_dynamic(
    falloc: &mut FrameAllocator,
    exe: &[u8],
    libs: &[&[u8]],
    argv: &[&[u8]],
    caps: u64,
    linux_abi: bool,
) -> (String, u64) {
    init_syscall_msrs();
    CURRENT_CAPS.store(caps, Ordering::Relaxed);
    LINUX_ABI.store(linux_abi, Ordering::Relaxed);
    *CURRENT_APP.lock() = argv.first().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
    unsafe {
        EXITED = 0;
        EXIT_CODE = 0;
    }
    OUTPUT.lock().clear();
    reset_fd_table();

    const MIB2: u64 = 1 << 21;
    let arena = match falloc.allocate_aligned(512, 512) {
        Ok(a) => a,
        Err(_) => return (String::from("(no arena)"), u64::MAX),
    };
    let code = arena;
    let stack_top = arena + MIB2;
    HEAP_BREAK.store(arena + 0x80000, Ordering::Relaxed);
    ARENA_BASE.store(arena, Ordering::Relaxed); // audit C1: validate user pointers against this arena
    HEAP_END.store(arena + 0x180000, Ordering::Relaxed);

    let exe_pages = program_span_pages(exe);
    let mut info = load_program(exe, code, exe_pages);
    // Place each .so in its own 128 KiB window (0x40000, 0x60000) before the heap.
    let mut loaded: Vec<(u64, &[u8])> = Vec::new();
    for (i, lib) in libs.iter().enumerate().take(2) {
        let lib_base = arena + 0x40000 + (i as u64) * 0x20000;
        let lib_pages = program_span_pages(lib);
        if let Some(lib_info) = load_elf64(lib, lib_base, lib_pages) {
            let lib_poff = ((lib_base - arena) / 4096) as usize;
            merge_shifted(&mut info.exec_pages, &lib_info.exec_pages, lib_poff);
            merge_shifted(&mut info.writ_pages, &lib_info.writ_pages, lib_poff);
            loaded.push((lib_base, lib));
        }
    }
    let (resolved, unresolved) = link_symbol_relocations(code, exe, &loaded);
    crate::serial_println!(
        "[h3] dynlinker: {} lib(s) loaded, {} symbol relocation(s) resolved, {} unresolved",
        loaded.len(),
        resolved,
        unresolved
    );

    // Kernel-as-ld.so: set up the static TLS block + thread pointer (before we
    // build the address space, so the TLS pages are mapped user-writable), and
    // patch the cross-module TPOFF64 relocations (IE-TLS) per module.
    let mut tls_modules: Vec<(u64, &[u8])> = alloc::vec![(code, exe)];
    for (lb, le) in &loaded {
        tls_modules.push((*lb, *le));
    }
    let (tls_tp, tls_offsets) = setup_static_tls(arena, &tls_modules, &mut info);
    let mut tls_patched = 0u32;
    for (mbase, toff) in &tls_offsets {
        let elf = if *mbase == code { exe } else { loaded.iter().find(|(b, _)| b == mbase).map(|(_, e)| *e).unwrap_or(exe) };
        tls_patched += apply_tls_relocs(*mbase, elf, *toff);
    }
    if tls_patched > 0 {
        crate::serial_println!("[tls] {tls_patched} TPOFF64 relocation(s) patched (initial-exec)");
    }

    let rsp = unsafe { setup_user_stack(stack_top, argv, &info) };
    let entry = info.entry;
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let boot = crate::sched::boot_pml4();
    unsafe { crate::gdt::set_rsp0(KERNEL_RSP) };
    // Load FS_BASE = TP (the musl/IE-TLS pointer) if the program uses TLS.
    if let Some(tp) = tls_tp {
        unsafe { Msr::new(0xC000_0100).write(tp) };
    }
    FG_ACTIVE.store(true, Ordering::Relaxed);
    // SAFETY: same pattern as run_args — return via sys_exit or force-return.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
        enter_ring3(user_cs, user_ss, entry, rsp);
        core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags));
    }
    FG_ACTIVE.store(false, Ordering::Relaxed);
    for f in 0..512u64 {
        let _ = falloc.free(arena + f * 4096);
    }
    crate::paging::free_address_space(falloc, pml4);
    let exit_code = unsafe { EXIT_CODE };
    let out = OUTPUT.lock().clone();
    (out, exit_code)
}

/// H3 self-test with the embedded artifacts: dyntest.elf + libeuro.so.
pub fn dynlink_selftest(falloc: &mut FrameAllocator) -> (String, u64) {
    run_dynamic(falloc, DYNTEST_ELF, &[LIBEURO_SO], &[b"dyntest"], CAP_CONSOLE, true)
}

/// **3C-3: the PT_INTERP path.** Unlike [`run_dynamic`] (which links in-kernel),
/// this loads `exe` + `libc` + the **interpreter** (`ld-euro.so`), leaves the
/// exe's symbol relocations UNRESOLVED, and jumps to the *interpreter's* entry
/// with an auxv carrying `AT_BASE` + the exe/libc load bases. The userspace
/// `ld-euro.so` then performs the JUMP_SLOT/GLOB_DAT/RELATIVE relocations itself
/// — the real Linux dynamic-linking flow — before entering the program.
pub fn run_interp(
    falloc: &mut FrameAllocator,
    exe: &[u8],
    libc: &[u8],
    interp: &[u8],
    argv: &[&[u8]],
    caps: u64,
) -> (String, u64) {
    init_syscall_msrs();
    CURRENT_CAPS.store(caps, Ordering::Relaxed);
    LINUX_ABI.store(true, Ordering::Relaxed);
    *CURRENT_APP.lock() = argv.first().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
    unsafe {
        EXITED = 0;
        EXIT_CODE = 0;
    }
    OUTPUT.lock().clear();
    reset_fd_table();

    const MIB2: u64 = 1 << 21;
    let arena = match falloc.allocate_aligned(512, 512) {
        Ok(a) => a,
        Err(_) => return (String::from("(no arena)"), u64::MAX),
    };
    let code = arena;
    let stack_top = arena + MIB2;
    HEAP_BREAK.store(arena + 0x80000, Ordering::Relaxed);
    ARENA_BASE.store(arena, Ordering::Relaxed);
    HEAP_END.store(arena + 0x180000, Ordering::Relaxed);

    // Load the exe (its own R_X86_64_RELATIVE are applied by load_program; the
    // JUMP_SLOT/GLOB_DAT are deliberately LEFT for the userspace interpreter).
    let mut info = load_program(exe, code, program_span_pages(exe));
    // libc-euro.so and the interpreter each get a 128 KiB window.
    let libc_base = arena + 0x40000;
    let interp_base = arena + 0x60000;
    let mut interp_entry = 0u64;
    if let Some(li) = load_elf64(libc, libc_base, program_span_pages(libc)) {
        merge_shifted(&mut info.exec_pages, &li.exec_pages, 0x40000 / 4096);
        merge_shifted(&mut info.writ_pages, &li.writ_pages, 0x40000 / 4096);
    }
    if let Some(ii) = load_elf64(interp, interp_base, program_span_pages(interp)) {
        merge_shifted(&mut info.exec_pages, &ii.exec_pages, 0x60000 / 4096);
        merge_shifted(&mut info.writ_pages, &ii.writ_pages, 0x60000 / 4096);
        interp_entry = ii.entry;
    }

    let rsp = unsafe { setup_user_stack_interp(stack_top, argv, &info, interp_base, code, libc_base) };
    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let boot = crate::sched::boot_pml4();
    unsafe { crate::gdt::set_rsp0(KERNEL_RSP) };
    FG_ACTIVE.store(true, Ordering::Relaxed);
    // Enter the INTERPRETER (not the exe); it links the exe and jumps to AT_ENTRY.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
        enter_ring3(user_cs, user_ss, interp_entry, rsp);
        core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags));
    }
    FG_ACTIVE.store(false, Ordering::Relaxed);
    for f in 0..512u64 {
        let _ = falloc.free(arena + f * 4096);
    }
    crate::paging::free_address_space(falloc, pml4);
    let exit_code = unsafe { EXIT_CODE };
    let out = OUTPUT.lock().clone();
    (out, exit_code)
}

/// `[3c3]` self-test: run a PT_INTERP dynamically-linked program whose external
/// symbol is resolved by the from-scratch **userspace** `ld-euro.so`.
pub fn interp_selftest(falloc: &mut FrameAllocator) {
    let has_interp = INTERPEXE_ELF.windows(13).any(|w| w == b"/lib/ld-euro.");
    let (out, code) = run_interp(falloc, INTERPEXE_ELF, LIBCEURO_SO, LDEURO_SO, &[b"interpexe"], CAP_CONSOLE);
    crate::serial_println!(
        "[3c3] PT_INTERP + userspace ld.so: exe names /lib/ld-euro.so={}, interpreter resolved the cross-module symbol in userspace → output={:?} exit={} {}",
        has_interp,
        out.trim_end(),
        code,
        if code == 42 && out.contains("3C3: 42") {
            "OK (dynamic linking done by a userspace interpreter via PT_INTERP, not the kernel) ✓"
        } else {
            "✗ ERROR"
        }
    );
}

/// `[tls]` self-test (Sprint 1): run a standalone PIE with a `__thread` counter
/// that sets up NO TLS itself — the kernel-ld.so does the TLS setup. tls_value 41->42 ->
/// exit(42) proves the static TLS block + FS_BASE.
pub fn tls_selftest(falloc: &mut FrameAllocator) {
    let (_out, code) = run_dynamic(falloc, TLSPROG_ELF, &[], &[b"tlsprog"], CAP_CONSOLE, true);
    crate::serial_println!(
        "[tls] kernel-ld.so TLS setup: standalone __thread PIE (41->42) -> exit {} {}",
        code,
        if code == 42 { "✓ (static TLS block + FS_BASE set up by the kernel)" } else { "✗ ERROR" }
    );
}

/// `[tls2]` self-test (Sprint 1, stage 1b): CROSS-MODULE TLS — dyntls calls bump()
/// from libtls.so, which reads its own `__thread ctr` via `%fs` (TPOFF64). The
/// kernel-ld.so sets up the multi-module TLS block + patches the TPOFF64 relocation.
/// 41->42 -> exit(42) proves dynamic cross-module IE-TLS.
pub fn tls_cross_selftest(falloc: &mut FrameAllocator) {
    let (_out, code) = run_dynamic(falloc, DYNTLS_ELF, &[LIBTLS_SO], &[b"dyntls"], CAP_CONSOLE, true);
    crate::serial_println!(
        "[tls2] cross-module IE-TLS (.so __thread via TPOFF64): bump() 41->42 -> exit {} {}",
        code,
        if code == 42 { "✓ (multi-module TLS block + TPOFF64 patch by the kernel-ld.so)" } else { "✗ ERROR" }
    );
}

/// `[uptr]` — proves that the syscall layer validates user pointers against the arena:
/// a pointer INSIDE the arena succeeds, a forged pointer OUTSIDE (kernel
/// address, or a length that overruns the arena) is denied instead of reading/writing
/// kernel memory. Temporarily sets up a fake arena over a real
/// stack buffer and restores `ARENA_BASE` afterward.
pub fn user_ptr_selftest() {
    let mut scratch = [0u8; 64];
    let base = scratch.as_ptr() as u64;
    let prev = ARENA_BASE.load(Ordering::Relaxed);
    // Fake arena with `base` as the lower bound. The arena span is ARENA_SPAN (2 MiB),
    // so we touch ONLY offset 0 with real access (within the 64-B scratch); the
    // "denied" cases use addresses REALLY outside [base, base+ARENA_SPAN),
    // so the check fails before any dereference — no OOB on the stack.
    ARENA_BASE.store(base, Ordering::Relaxed);
    let outside = base.wrapping_add(ARENA_SPAN); // == top, falls outside the arena

    // 1) Inside the arena (offset 0): write + read-back succeeds.
    let inside_ok = copy_to_user(base, b"euro") && {
        let rb: u32 = read_user(base).unwrap_or(0);
        rb == u32::from_le_bytes(*b"euro")
    };

    // 2) A kernel address just before the arena (base-1) is denied.
    let below_denied = !in_user_arena(base.wrapping_sub(1), 1)
        && !copy_to_user(base.wrapping_sub(1), b"x")
        && read_user::<u32>(base.wrapping_sub(1)).is_none();

    // 3) An address just after the arena (base+ARENA_SPAN) is denied; the helpers
    //    do not touch the memory (the check fails first).
    let above_denied = !in_user_arena(outside, 1)
        && !copy_to_user(outside, b"x")
        && !write_user(outside, 0xFFu8)
        && copy_from_user(outside, 16).is_none();

    // 4) A length that overruns the arena upper bound is denied without reading.
    let span_denied =
        !in_user_arena(base, ARENA_SPAN as usize + 1) && copy_from_user(base, ARENA_SPAN as usize + 1).is_none();

    // 5) user_cstr on an out-of-arena pointer reads nothing (empty string).
    let cstr_bounded = user_cstr(outside, 64).is_empty();

    ARENA_BASE.store(prev, Ordering::Relaxed); // restore arena

    let all = inside_ok && below_denied && above_denied && span_denied && cstr_bounded;
    crate::serial_println!(
        "[uptr] user-pointer validation: inside={} below={} above={} span={} cstr={} -> {}",
        inside_ok, below_denied, above_denied, span_denied, cstr_bounded,
        if all { "OK" } else { "FAIL" }
    );
}

/// Build a SysV x86-64 initial stack: `argc`, `argv[]`, `envp[]`, `auxv[]`,
/// plus the associated strings + 16 AT_RANDOM bytes. This is exactly the
/// contract that a musl/glibc `_start` expects from the kernel. `info` provides the
/// program-header info for the auxv. Returns the (16-aligned) rsp where
/// `[rsp]==argc`.
unsafe fn setup_user_stack(stack_top: u64, argv: &[&[u8]], info: &LoadInfo) -> u64 {
    let mut p = stack_top;
    // 16 "random" bytes (AT_RANDOM) — musl uses this for stack-canary/TLS-guard.
    p -= 16;
    let random_ptr = p;
    for i in 0..16 {
        (random_ptr as *mut u8).add(i).write(0x5Au8 ^ (i as u8).wrapping_mul(31));
    }
    // Put each argv string (NUL-terminated) on the stack; keep the pointers.
    let mut argptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(argv.len());
    for a in argv {
        p -= a.len() as u64 + 1;
        let ptr = p;
        for (i, b) in a.iter().enumerate() {
            (ptr as *mut u8).add(i).write(*b);
        }
        (ptr as *mut u8).add(a.len()).write(0);
        argptrs.push(ptr);
    }
    // Environment variables (envp) — the system environment that every process inherits.
    let env = ENV.lock();
    let mut envptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(env.len());
    for e in env.iter() {
        let bytes = e.as_bytes();
        p -= bytes.len() as u64 + 1;
        let ptr = p;
        for (i, b) in bytes.iter().enumerate() {
            (ptr as *mut u8).add(i).write(*b);
        }
        (ptr as *mut u8).add(bytes.len()).write(0);
        envptrs.push(ptr);
    }
    p &= !0xF; // strings region 16-aligned

    // auxv pairs (type, value), terminated with AT_NULL. Full set for musl:
    //   AT_PHDR=3, AT_PHENT=4, AT_PHNUM=5, AT_PAGESZ=6, AT_BASE=7,
    //   AT_ENTRY=9, AT_RANDOM=25.
    let aux: [(u64, u64); 8] = [
        (3, info.phdr),
        (4, info.phent),
        (5, info.phnum),
        (6, 4096),
        (7, 0), // no interpreter (static-PIE)
        (9, info.entry),
        (25, random_ptr),
        (0, 0), // AT_NULL
    ];
    // Slots: argc(1) + argv-ptrs(n) + argv-NULL(1) + env-ptrs(m) + env-NULL(1) + auxv(2*8).
    let nslots = 1 + argptrs.len() as u64 + 1 + envptrs.len() as u64 + 1 + (aux.len() as u64) * 2;
    let sp = (p - nslots * 8) & !0xF;
    let mut w = sp;
    let mut put = |val: u64| {
        (w as *mut u64).write(val);
        w += 8;
    };
    put(argptrs.len() as u64); // argc
    for ptr in &argptrs {
        put(*ptr); // argv[i]
    }
    put(0); // argv terminator
    for ptr in &envptrs {
        put(*ptr); // envp[i]
    }
    put(0); // envp terminator
    for (t, v) in aux {
        put(t);
        put(v);
    }
    sp
}

/// 3C-3 variant of [`setup_user_stack`] for the **PT_INTERP** path: the auxv
/// carries `AT_BASE` = the interpreter's load base and `AT_ENTRY` = the exe's
/// real entry, plus two EuroOS entries (`0x6E01`/`0x6E02`) giving the exe and
/// libc load bases so the userspace `ld-euro.so` can do the relocations.
unsafe fn setup_user_stack_interp(
    stack_top: u64,
    argv: &[&[u8]],
    info: &LoadInfo,
    interp_base: u64,
    exe_base: u64,
    libc_base: u64,
) -> u64 {
    let mut p = stack_top;
    p -= 16;
    let random_ptr = p;
    for i in 0..16 {
        (random_ptr as *mut u8).add(i).write(0x5Au8 ^ (i as u8).wrapping_mul(31));
    }
    let mut argptrs: alloc::vec::Vec<u64> = alloc::vec::Vec::with_capacity(argv.len());
    for a in argv {
        p -= a.len() as u64 + 1;
        let ptr = p;
        for (i, b) in a.iter().enumerate() {
            (ptr as *mut u8).add(i).write(*b);
        }
        (ptr as *mut u8).add(a.len()).write(0);
        argptrs.push(ptr);
    }
    p &= !0xF;

    // AT_PHDR/PHENT/PHNUM describe the EXE; AT_BASE is the interpreter; AT_ENTRY
    // is the exe's real entry (the interpreter jumps there after linking).
    let aux: [(u64, u64); 10] = [
        (3, info.phdr),
        (4, info.phent),
        (5, info.phnum),
        (6, 4096),
        (7, interp_base),   // AT_BASE = interpreter load base
        (9, info.entry),    // AT_ENTRY = exe entry
        (25, random_ptr),   // AT_RANDOM
        (0x6E01, exe_base), // AT_EURO_EXE_BASE
        (0x6E02, libc_base),// AT_EURO_LIBC_BASE
        (0, 0),             // AT_NULL
    ];
    let nslots = 1 + argptrs.len() as u64 + 1 + 1 + (aux.len() as u64) * 2; // no envp here
    let sp = (p - nslots * 8) & !0xF;
    let mut w = sp;
    let mut put = |val: u64| {
        (w as *mut u64).write(val);
        w += 8;
    };
    put(argptrs.len() as u64);
    for ptr in &argptrs {
        put(*ptr);
    }
    put(0); // argv terminator
    put(0); // empty envp terminator
    for (t, v) in aux {
        put(t);
        put(v);
    }
    sp
}

// ── D1a: syscall profiling (inventory before the fine-grained SMP locking) ──
// Per syscall number: count + total time (ns), measured with the HPET around the
// dispatch. Shows where the kernel time goes — the hot paths that per-subsystem
// locks (instead of the global IF=0 serialization) will benefit most from later.
const PROF_N: usize = 512;
static PROF_COUNT: [core::sync::atomic::AtomicU64; PROF_N] = {
    const Z: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    [Z; PROF_N]
};
static PROF_NS: [core::sync::atomic::AtomicU64; PROF_N] = {
    const Z: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    [Z; PROF_N]
};

/// RAII meter: reads the HPET on entry and records the elapsed time on exit (every
/// `return` path). Lightweight; does not disturb the syscall semantics.
struct SyscallProfile {
    num: usize,
    start: u64,
}
impl SyscallProfile {
    fn start(num: u64) -> Self {
        SyscallProfile { num: (num as usize).min(PROF_N - 1), start: crate::hpet::ns() }
    }
}
impl Drop for SyscallProfile {
    fn drop(&mut self) {
        let dt = crate::hpet::ns().wrapping_sub(self.start);
        PROF_COUNT[self.num].fetch_add(1, Ordering::Relaxed);
        PROF_NS[self.num].fetch_add(dt, Ordering::Relaxed);
    }
}

/// Profile lines: the syscalls sorted by total time (top 12).
pub fn syscall_profile_lines() -> alloc::vec::Vec<alloc::string::String> {
    let mut rows: alloc::vec::Vec<(usize, u64, u64)> = (0..PROF_N)
        .map(|i| (i, PROF_COUNT[i].load(Ordering::Relaxed), PROF_NS[i].load(Ordering::Relaxed)))
        .filter(|&(_, c, _)| c > 0)
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2)); // by total time
    let mut out = alloc::vec![alloc::string::String::from("SYSCALL  COUNT      TOTAL(us)   AVG(ns)")];
    for (num, count, ns) in rows.into_iter().take(12) {
        out.push(alloc::format!("  {num:<5} {count:>8}  {:>9}  {:>9}", ns / 1000, ns / count.max(1)));
    }
    if out.len() == 1 {
        out.push("  (no syscalls profiled yet)".into());
    }
    out
}

/// Syscall dispatcher (ring 0). Returns the return value in rax.
#[no_mangle]
pub extern "sysv64" fn syscall_dispatch(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let _prof = SyscallProfile::start(num);
    // Does this syscall come from the scheduled background daemon? Then a separate
    // dispatcher + output buffer (independent of the global foreground state).
    let cur = crate::sched::current();
    if cur == DAEMON_TASK.load(Ordering::Relaxed) {
        return daemon_dispatch(num, a1, a2, a3);
    }
    // Preemptive per-process (PCB): route to the right background musl process
    // state (own heap/output/pid). A THREAD shares the PCB of its process, so
    // we match on the main task OR on one of the thread tasks.
    {
        let mut bg = BG.lock();
        if let Some(pos) = bg.iter().position(|p| p.task == cur || p.threads.contains(&cur)) {
            // fork/vfork/wait4 MUTATE the BG table (add a child / get status)
            // and therefore cannot run under the p-borrow of bg_dispatch.
            match num {
                57 | 58 => return do_fork(&mut bg, pos), // fork / vfork
                61 => {
                    // wait4(pid, *status, options, *rusage): non-blocking reap.
                    let parent_pid = bg[pos].pid;
                    return do_wait4(parent_pid, a1, a2);
                }
                _ => {
                    let p = &mut bg[pos];
                    return bg_dispatch(p, num, a1, a2, a3, a4, a5);
                }
            }
        }
    }
    // Linux-ABI compatibility: programs compiled for x86_64-linux
    // use Linux syscall numbers + semantics. Translate to our handlers.
    if LINUX_ABI.load(Ordering::Relaxed) {
        return linux_dispatch(num, a1, a2, a3, a4, a5);
    }
    // Capability enforcement: deny syscalls the process has no right to.
    let need = required_cap(num);
    if need != 0 && !has_cap(need) {
        crate::serial_println!("[cap] syscall {num} DENIED — missing capability");
        return u64::MAX; // -EPERM
    }
    match num {
        60 => 0, // sys_net() — network access (requires CAP_NET; stub that succeeds if allowed)
        12 => {
            // sys_sbrk(inc) -> old break (or -1 on overrun). inc=0 = query.
            let old = HEAP_BREAK.load(Ordering::Relaxed);
            if a1 == 0 {
                return old;
            }
            // Overflow-safe (audit M7): a huge `a1` must not bypass the `> HEAP_END`
            // gate via wrap-around.
            let new = match old.checked_add(a1) {
                Some(n) => n,
                None => return u64::MAX,
            };
            if new > HEAP_END.load(Ordering::Relaxed) {
                return u64::MAX; // out of memory
            }
            HEAP_BREAK.store(new, Ordering::Relaxed);
            old
        }
        0 => {
            // sys_exit(code)
            unsafe {
                EXIT_CODE = a1;
                EXITED = 1;
            }
            0
        }
        2 => 1, // sys_getpid() — first userspace process = pid 1
        20 => {
            // sys_open(path) -> fd (or -1). Path from userspace, look up in the VFS.
            let path = user_cstr(a1, 256);
            vfs_open(&path)
        }
        22 => vfs_read(a1 as usize, a2, a3 as usize), // sys_read(fd, buf, len)
        21 => vfs_close(a1 as usize),                 // sys_close(fd)
        4 => {
            // sys_uname(buf, size) — write the kernel version into the user buffer.
            let s: &[u8] = b"EuroKernel 0.1-alpha x86_64";
            let cap = (a2 as usize).saturating_sub(1);
            let n = s.len().min(cap);
            // Validate buf for n+1 bytes (data + NUL) before writing.
            if !in_user_arena(a1, n + 1) {
                return EFAULT;
            }
            let _ = copy_to_user(a1, &s[..n]);
            let _ = write_user(a1 + n as u64, 0u8); // NUL terminator
            n as u64
        }
        1 => {
            // sys_write(ptr) — NUL-terminated string from userspace (arena-safe).
            let bytes = user_cstr(a1, 4096);
            let len = bytes.len();
            if let Ok(text) = core::str::from_utf8(&bytes) {
                OUTPUT.lock().push_str(text);
                serial_print!("[ring3->sys_write] {text}\n");
            }
            len as u64
        }
        _ => u64::MAX, // ENOSYS
    }
}

/// Linux x86-64 syscall ABI -> our handlers. Linux semantics (e.g. write/read
/// take (fd, buf, count); the exit number is 60). Minimal set for first binaries.
/// The capability a LINUX syscall requires (0 = always allowed). This way
/// least-privilege also applies to the Linux ABI: a musl process without CAP_FILE
/// cannot open files, exactly like our native programs.
fn linux_required_cap(num: u64, a1: u64) -> u64 {
    // I/O on a socket fd (read/write/close) falls under CAP_NET — not under
    // CAP_FILE/CAP_CONSOLE. This way a network program only needs CAP_NET.
    if crate::net::is_sock_fd(a1) && matches!(num, 0 | 1 | 3) {
        return CAP_NET;
    }
    match num {
        1 | 16 | 20 => CAP_CONSOLE,            // write/ioctl/writev (tty)
        0 | 2 | 3 | 5 | 8 | 19 | 89 | 217 | 257 | 262 | 267 => CAP_FILE, // read/open/close/(f)stat/lseek/readv/readlink/getdents64/openat
        41 | 42 | 44 | 45 => CAP_NET,           // socket/connect/sendto/recvfrom
        39 => CAP_PROC_INFO,                    // getpid
        _ => 0, // memory/process management (mmap, brk, arch_prctl, exit, …) free
    }
}

fn linux_dispatch(num: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let _ = a4; // not every syscall uses arg4/arg5 (r10/r8)
    // Capability enforcement also in the Linux ABI: deny without the proper right.
    let need = linux_required_cap(num, a1);
    if need != 0 && !has_cap(need) {
        crate::serial_println!("[cap] Linux syscall {num} DENIED — missing capability");
        return (-1i64) as u64; // -EPERM
    }
    match num {
        1 => {
            // write(fd, buf, count) — count bytes (NOT NUL-terminated).
            if a1 == 1 || a1 == 2 {
                let bytes = match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                };
                if let Some(fi) = *STDOUT_REDIRECT.lock() {
                    redirect_append(fi, &bytes); // shell redirection: stdout -> file
                } else if let Ok(t) = core::str::from_utf8(&bytes) {
                    OUTPUT.lock().push_str(t);
                    serial_print!("[linux-abi] {t}");
                }
                a3
            } else if crate::net::is_sock_fd(a1) {
                // write() to a socket = send().
                let bytes = match copy_from_user(a2, a3 as usize) {
                    Some(v) => v,
                    None => return EFAULT,
                };
                crate::net::sock_send(a1, &bytes)
            } else {
                // Write to an opened VFS file (fd >= 3).
                vfs_write(a1 as usize, a2, a3 as usize)
            }
        }
        39 => 1,  // getpid()
        60 | 231 => {
            // exit(code) / exit_group(code)
            unsafe {
                EXIT_CODE = a1;
                EXITED = 1;
            }
            0
        }
        12 => {
            // brk(addr) — Linux semantics: set break, return the NEW break.
            let cur = HEAP_BREAK.load(Ordering::Relaxed);
            if a1 == 0 || a1 > HEAP_END.load(Ordering::Relaxed) {
                return cur;
            }
            HEAP_BREAK.store(a1, Ordering::Relaxed);
            a1
        }
        9 => {
            // mmap(addr=a1, len=a2, prot=a3, flags=a4, fd=a5, off=a6).
            // The 6th arg (off) is in the original r9, saved by the syscall
            // trampoline; recover it from the saved register block (r9 sits at
            // SAVED_REGS+40 given the push order in the asm handler).
            const MAP_ANONYMOUS: u64 = 0x20;
            const MAP_FIXED: u64 = 0x10;
            let len = (a2 + 0xFFF) & !0xFFF;
            let file_backed = a4 & MAP_ANONYMOUS == 0 && (a5 as usize) < MAX_FD && a5 != u64::MAX;

            // Pick the target region: MAP_FIXED honours addr (must be in-arena +
            // page-aligned); otherwise bump the heap window.
            let base = if a4 & MAP_FIXED != 0 && a1 != 0 {
                let fixed = a1 & !0xFFF;
                if !in_user_arena(fixed, len as usize) {
                    return (-12i64) as u64; // -ENOMEM: fixed addr out of the arena
                }
                fixed
            } else {
                let b = (HEAP_BREAK.load(Ordering::Relaxed) + 0xFFF) & !0xFFF;
                if b + len > HEAP_END.load(Ordering::Relaxed) {
                    return (-12i64) as u64; // -ENOMEM
                }
                HEAP_BREAK.store(b + len, Ordering::Relaxed);
                b
            };

            if file_backed {
                // File-backed mmap (MAP_PRIVATE copy): read fd's file bytes at
                // `off` and place them at `base`, zero-filling past EOF — exactly
                // what a dynamic loader needs to map a library's LOAD segments.
                let off = unsafe { recover_mmap_offset() } as usize;
                let fds = OPEN_FDS.lock();
                let fi = match fds.get(a5 as usize).and_then(|s| *s) {
                    Some((fi, _)) => fi,
                    None => return (-9i64) as u64, // -EBADF
                };
                drop(fds);
                let files = FILES.lock();
                let data = &files[fi].1;
                let copy = if off < data.len() { (data.len() - off).min(len as usize) } else { 0 };
                if !in_user_arena(base, len as usize) {
                    return (-12i64) as u64;
                }
                unsafe {
                    if copy > 0 {
                        core::ptr::copy_nonoverlapping(data[off..].as_ptr(), base as *mut u8, copy);
                    }
                    // Zero the tail past EOF (mmap fills the rest of the page with 0).
                    if (len as usize) > copy {
                        core::ptr::write_bytes((base + copy as u64) as *mut u8, 0, len as usize - copy);
                    }
                }
                crate::serial_println!("[linux-abi] mmap(fd={a5}, off={off}, {len} B) file-backed -> {base:#x} ({copy} B copied)");
                base
            } else {
                crate::serial_println!("[linux-abi] mmap({len} bytes, anon) -> {base:#x}");
                base
            }
        }
        11 => 0, // munmap — the bump allocator does not give back, but silently succeeds
        158 => {
            // arch_prctl(code, addr): ARCH_SET_FS=0x1002 sets FS_BASE (musl TLS).
            match a1 {
                0x1002 => {
                    unsafe { Msr::new(0xC000_0100).write(a2) }; // IA32_FS_BASE
                    crate::serial_println!("[linux-abi] arch_prctl SET_FS = {a2:#x}");
                    0
                }
                0x1001 => {
                    unsafe { Msr::new(0xC000_0101).write(a2) }; // IA32_GS_BASE
                    0
                }
                _ => (-22i64) as u64, // -EINVAL
            }
        }
        20 => {
            // writev(fd, iov, iovcnt): array of {base,len}; count written bytes.
            // fd 1/2 -> console; fd >= 3 -> write to the VFS file (musl stdio).
            let to_file = a1 != 1 && a1 != 2;
            if a3 > 1024 {
                return (-22i64) as u64; // -EINVAL: bound iovcnt
            }
            let mut written = 0u64;
            for i in 0..a3 {
                let iov_base = a2 + (i * 16);
                let base: u64 = match read_user(iov_base) {
                    Some(b) => b,
                    None => return EFAULT,
                };
                let len = match read_user::<u64>(iov_base + 8) {
                    Some(l) => l as usize,
                    None => return EFAULT,
                };
                if len == 0 {
                    continue;
                }
                if to_file {
                    let n = vfs_write(a1 as usize, base, len); // vfs_write validates base
                    if n == u64::MAX {
                        return if written > 0 { written } else { (-9i64) as u64 };
                    }
                    written += n;
                } else {
                    let bytes = match copy_from_user(base, len) {
                        Some(v) => v,
                        None => return EFAULT,
                    };
                    if let Some(fi) = *STDOUT_REDIRECT.lock() {
                        redirect_append(fi, &bytes); // shell redirection: stdout -> file
                    } else if let Ok(t) = core::str::from_utf8(&bytes) {
                        OUTPUT.lock().push_str(t);
                        serial_print!("[linux-abi] {t}");
                    }
                    written += len as u64;
                }
            }
            written
        }
        0 => {
            // read(fd, buf, count): fd 0 = standard input (pipe), socket, or VFS.
            if a1 == 0 {
                stdin_read(a2, a3 as usize)
            } else if crate::net::is_sock_fd(a1) {
                let data = crate::net::sock_recv(a1, a3 as usize);
                if !copy_to_user(a2, &data) {
                    return EFAULT;
                }
                data.len() as u64
            } else {
                vfs_read(a1 as usize, a2, a3 as usize)
            }
        }
        19 => {
            // readv(fd, iov, iovcnt): read into each iovec buffer; count bytes (musl stdio).
            let fd = a1 as usize;
            if a3 > 1024 {
                return (-22i64) as u64; // -EINVAL: bound iovcnt
            }
            let mut total = 0u64;
            for i in 0..a3 {
                let iov_base = a2 + (i * 16);
                let base: u64 = match read_user(iov_base) {
                    Some(b) => b,
                    None => return EFAULT,
                };
                let len = match read_user::<u64>(iov_base + 8) {
                    Some(l) => l as usize,
                    None => return EFAULT,
                };
                if len == 0 {
                    continue;
                }
                let n = if fd == 0 { stdin_read(base, len) } else { vfs_read(fd, base, len) };
                if n == u64::MAX {
                    return if total > 0 { total } else { (-9i64) as u64 };
                }
                total += n;
                if (n as usize) < len {
                    break; // short read = EOF/end of file
                }
            }
            total
        }
        3 => {
            // close(fd): socket or VFS file.
            if crate::net::is_sock_fd(a1) {
                crate::net::sock_close(a1)
            } else {
                vfs_close(a1 as usize)
            }
        }
        41 => {
            // socket(domain, type, protocol): AF_INET (2) + SOCK_STREAM (1, TCP)
            // or SOCK_DGRAM (2, UDP).
            let typ = a2 & 0xff; // ignore SOCK_CLOEXEC/NONBLOCK flags
            match (a1, typ) {
                (2, 1) => crate::net::sock_open(false), // TCP
                (2, 2) => crate::net::sock_open(true),  // UDP
                _ => (-1i64) as u64,
            }
        }
        42 => {
            // connect(fd, *sockaddr_in, addrlen): sin_port BE @2, sin_addr @4.
            if a3 < 8 {
                return (-1i64) as u64;
            }
            // Read the 8-byte sockaddr arena-safe (port @2..4, addr @4..8).
            let sa = match copy_from_user(a2, 8) {
                Some(v) => v,
                None => return EFAULT,
            };
            let port = ((sa[2] as u16) << 8) | sa[3] as u16;
            crate::net::sock_connect(a1, euronet::ipv4::Ipv4Addr([sa[4], sa[5], sa[6], sa[7]]), port)
        }
        44 => {
            // sendto(fd, buf, len, flags, dest, destlen): connected TCP -> ignore dest.
            let bytes = match copy_from_user(a2, a3 as usize) {
                Some(v) => v,
                None => return EFAULT,
            };
            crate::net::sock_send(a1, &bytes)
        }
        45 => {
            // recvfrom(fd, buf, len, flags, src, srclen).
            let data = crate::net::sock_recv(a1, a3 as usize);
            if !copy_to_user(a2, &data) {
                return EFAULT;
            }
            data.len() as u64
        }
        8 => vfs_lseek(a1 as usize, a2 as i64, a3),  // lseek(fd, offset, whence)
        257 => {
            // openat(dirfd, path, flags, mode): ignore dirfd (AT_FDCWD). flags in a3.
            // O_CREAT=0x40 creates; O_TRUNC=0x200 truncates; O_APPEND=0x400 -> at the end.
            let path = user_cstr(a2, 256);
            let flags = a3;
            // Opening a directory (no O_CREAT) -> dir fd for getdents64.
            if flags & 0x40 == 0 && is_vfs_dir(&path) {
                return diropen(&path);
            }
            let fd = if flags & 0x40 != 0 {
                vfs_open_create(&path, flags & 0x200 != 0)
            } else {
                vfs_open(&path)
            };
            if fd != u64::MAX && flags & 0x400 != 0 {
                // O_APPEND: set the write position to the end of the file.
                if let Some(sz) = vfs_size(fd as usize) {
                    let mut fds = OPEN_FDS.lock();
                    if let Some((fi, _)) = fds[fd as usize] {
                        fds[fd as usize] = Some((fi, sz));
                    }
                }
            }
            fd
        }
        2 => {
            // open(path, flags, mode) — older libc variant (flags in a2).
            let path = user_cstr(a1, 256);
            if a2 & 0x40 == 0 && is_vfs_dir(&path) {
                return diropen(&path);
            }
            if a2 & 0x40 != 0 {
                vfs_open_create(&path, a2 & 0x200 != 0)
            } else {
                vfs_open(&path)
            }
        }
        5 | 262 => {
            // fstat(fd, statbuf) / newfstatat(dirfd, path, statbuf, flags):
            // fill a Linux struct stat (144 B) so musl sees it as a regular
            // file with the correct size (otherwise stdio refuses to buffer).
            // fstat on an open dir fd -> report a DIRECTORY (S_IFDIR), not -EBADF.
            if num == 5 && (a1 as usize) < MAX_FD && OPEN_DIRS.lock()[a1 as usize].is_some() {
                if !in_user_arena(a2, 144) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a2 as *mut u8, 0, 144);
                    (a2 as *mut u32).add(6).write(0o040755); // st_mode: S_IFDIR|0755
                    ((a2 + 56) as *mut u64).write(4096); // st_blksize
                }
                return 0;
            }
            let (fd_ok, statbuf) = if num == 5 {
                // fd 0 = standard input (pipe): report the buffer size.
                let sz = if a1 == 0 { Some(stdin_len()) } else { vfs_size(a1 as usize) };
                (sz, a2)
            } else {
                // newfstatat: path in a2, statbuf in a3.
                let path = user_cstr(a2, 256);
                ensure_proc(&path); // synthesize /proc on demand
                let files = FILES.lock();
                let sz = files
                    .iter()
                    .find(|(p, _)| p.as_bytes() == path.as_slice())
                    .map(|(_, d)| d.len());
                (sz, a3)
            };
            let size = match fd_ok {
                Some(s) => s,
                None => return (-9i64) as u64, // -EBADF / -ENOENT
            };
            if !in_user_arena(statbuf, 144) {
                return EFAULT;
            }
            // SAFETY: statbuf region (144 B) arena-validated; identity-mapped.
            unsafe {
                core::ptr::write_bytes(statbuf as *mut u8, 0, 144);
                (statbuf as *mut u32).add(6).write(0o100644); // st_mode (offset 24): S_IFREG|0644
                ((statbuf + 48) as *mut u64).write(size as u64); // st_size (offset 48)
                ((statbuf + 56) as *mut u64).write(4096); // st_blksize (offset 56)
            }
            0
        }
        89 | 267 => {
            // readlink(path, buf, sz) / readlinkat(dirfd, path, buf, sz): the only
            // "symlinks" are the /proc/self pseudo-links. /proc/self/exe -> the path of
            // the running program (Python/Go/Node find their own binary this way).
            let (pathptr, bufptr, sz) =
                if num == 89 { (a1, a2, a3 as usize) } else { (a2, a3, a4 as usize) };
            let path = user_cstr(pathptr, 256);
            let target: Option<String> = match path.as_slice() {
                b"/proc/self/exe" => Some(CURRENT_APP.lock().clone()),
                b"/proc/self/cwd" | b"/proc/self/root" => Some(String::from("/")),
                _ => None,
            };
            match target {
                Some(t) if bufptr != 0 => {
                    let n = t.len().min(sz);
                    if !copy_to_user(bufptr, &t.as_bytes()[..n]) {
                        return EFAULT;
                    }
                    n as u64
                }
                Some(_) => 0,
                None => (-22i64) as u64, // -EINVAL: not a symlink
            }
        }
        217 => vfs_getdents64(a1 as usize, a2, a3 as usize), // getdents64(fd, dirp, count)
        16 => 0,  // ioctl — pretend success (isatty/TCGETS): stdout is a tty
        10 => 0,  // mprotect — allow (musl makes its RELRO read-only); no-op
        13 => 0,  // rt_sigaction — no signals; pretend it succeeds
        14 => 0,  // rt_sigprocmask
        218 => 1, // set_tid_address -> tid
        273 => 0, // set_robust_list
        202 => 0, // futex — no contention in single-thread; success
        228 => {
            // clock_gettime(clk, *timespec): CLOCK_REALTIME(0)/CLOCK_TAI(11) give the
            // REAL wall clock (RTC epoch); CLOCK_MONOTONIC(1)/BOOTTIME(7) the uptime.
            if a2 != 0 {
                let (sec, nsec) = if a1 == 0 || a1 == 11 {
                    (crate::rtc::epoch(), 0)
                } else {
                    let ticks = crate::interrupts::ticks();
                    (ticks / 100, (ticks % 100) * 10_000_000) // 100 Hz PIT
                };
                if !write_user(a2, sec) || !write_user(a2 + 8, nsec) {
                    return EFAULT;
                }
            }
            0
        }
        96 => {
            // gettimeofday(*timeval, tz): {tv_sec, tv_usec} from the real RTC wall clock.
            if a1 != 0 {
                if !write_user(a1, crate::rtc::epoch()) || !write_user(a1 + 8, 0u64) {
                    return EFAULT;
                }
            }
            0
        }
        63 => {
            // uname(*utsname): 6 fields of 65 bytes. We mirror a Linux kernel
            // (sysname "Linux", machine "x86_64") so unmodified Linux binaries
            // that inspect the kernel version are satisfied — release says EuroOS.
            if a1 != 0 {
                let fields: [&[u8]; 6] =
                    [b"Linux", b"euroos", b"6.6.0-euroos", b"#1 EuroOS SMP", b"x86_64", b""];
                if !in_user_arena(a1, 6 * 65) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a1 as *mut u8, 0, 6 * 65);
                    for (i, f) in fields.iter().enumerate() {
                        let dst = (a1 as *mut u8).add(i * 65);
                        let n = f.len().min(64);
                        core::ptr::copy_nonoverlapping(f.as_ptr(), dst, n);
                    }
                }
            }
            0
        }
        102 | 107 => crate::auth::session_uid() as u64, // getuid/geteuid -> session uid
        104 | 108 => crate::auth::session_gid() as u64, // getgid/getegid -> session gid
        24 => 0,                    // sched_yield — single-thread foreground: no-op
        72 => 0,                    // fcntl — F_GETFL/F_SETFL/F_SETFD: pretend it succeeds
        79 => {
            // getcwd(buf, size): EuroOS foreground process runs in "/".
            if a1 != 0 && a2 >= 2 {
                if !copy_to_user(a1, b"/\0") {
                    return EFAULT;
                }
                2
            } else {
                (-34i64) as u64 // -ERANGE
            }
        }
        97 | 302 => 0, // getrlimit / prlimit64 — unlimited; success
        334 => (-38i64) as u64, // rseq — not supported; glibc falls back gracefully
        21 | 269 => {
            // access(path, mode) / faccessat(dirfd, path, mode): 0 if it exists.
            let pathptr = if num == 21 { a1 } else { a2 };
            let path = user_cstr(pathptr, 256);
            ensure_proc(&path); // synthesize /proc on demand
            let exists = FILES.lock().iter().any(|(p, _)| p.as_bytes() == path.as_slice());
            if exists { 0 } else { (-2i64) as u64 } // -ENOENT
        }
        99 => {
            // sysinfo(*info): fill uptime + ram so tools like `uptime`/`free` work.
            if a1 != 0 {
                let up = crate::interrupts::ticks() / 100;
                if !in_user_arena(a1, 112) {
                    return EFAULT;
                }
                unsafe {
                    core::ptr::write_bytes(a1 as *mut u8, 0, 112);
                    (a1 as *mut i64).write(up as i64); // uptime (seconds)
                    ((a1 + 24) as *mut u64).write(256 * 1024 * 1024); // totalram
                    ((a1 + 32) as *mut u64).write(128 * 1024 * 1024); // freeram
                    ((a1 + 104) as *mut u32).write(1); // mem_unit
                }
            }
            0
        }
        332 => {
            // statx(dirfd, path, flags, mask, *statxbuf): modern glibc stat. statxbuf
            // is arg5 (a5). Fill stx_mask/blksize/nlink/mode/size for a regular
            // file so glibc stdio sees the file correctly.
            let path = user_cstr(a2, 256);
            ensure_proc(&path); // synthesize /proc on demand
            let sz = FILES
                .lock()
                .iter()
                .find(|(p, _)| p.as_bytes() == path.as_slice())
                .map(|(_, d)| d.len());
            match sz {
                Some(size) if a5 != 0 => {
                    if !in_user_arena(a5, 256) {
                        return EFAULT;
                    }
                    unsafe {
                        core::ptr::write_bytes(a5 as *mut u8, 0, 256);
                        (a5 as *mut u32).write(0x7ff); // stx_mask = STATX_BASIC_STATS
                        ((a5 + 0x04) as *mut u32).write(4096); // stx_blksize
                        ((a5 + 0x10) as *mut u32).write(1); // stx_nlink
                        ((a5 + 0x1c) as *mut u16).write(0o100644); // stx_mode: S_IFREG|0644
                        ((a5 + 0x28) as *mut u64).write(size as u64); // stx_size
                    }
                    0
                }
                Some(_) => 0,
                None => (-2i64) as u64, // -ENOENT
            }
        }
        318 => {
            // getrandom(buf, len, flags): pseudo-randomness (deterministic but
            // filled) — enough for musl init; no crypto source.
            if !in_user_arena(a1, a2 as usize) {
                return EFAULT;
            }
            let buf: alloc::vec::Vec<u8> =
                (0..a2).map(|i| (0x9Eu64.wrapping_mul(i + 1)) as u8).collect();
            let _ = copy_to_user(a1, &buf);
            a2
        }
        35 => 0,  // nanosleep
        234 => 0, // tgkill
        _ => {
            crate::serial_println!("[linux-abi] ENOSYS Linux syscall {num}");
            (-38i64) as u64 // -ENOSYS (Linux convention: negative errno)
        }
    }
}

/// Status of the hardware protection, for `shell`/diagnostics.
static SMEP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMAP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static NX_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMAP_LOGGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Turn SMEP + SMAP **on** (provided the CPU supports them — otherwise `Cr4::write`
/// would raise a #GP). SMEP prevents ring 0 from ever executing a user page (U=1);
/// SMAP prevents ring 0 from reading/writing user pages, except within an
/// explicit, brief AC window (see the syscall entry). This replaces the
/// former global *disabling* during process setup. Idempotent.
pub fn enable_smep_smap() {
    // CPUID.(EAX=7,ECX=0):EBX  bit 7 = SMEP, bit 20 = SMAP.
    let leaf7 = unsafe { core::arch::x86_64::__cpuid_count(7, 0) };
    let smep = leaf7.ebx & (1 << 7) != 0;
    let smap = leaf7.ebx & (1 << 20) != 0;
    let mut f = Cr4::read();
    if smep {
        f.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION);
    }
    if smap {
        f.insert(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
    }
    if smep || smap {
        unsafe { Cr4::write(f) };
    }
    SMEP_ON.store(smep, Ordering::Relaxed);
    SMAP_ON.store(smap, Ordering::Relaxed);
    if !SMAP_LOGGED.swap(true, Ordering::Relaxed) {
        crate::serial_println!(
            "[sec] SMEP {} · SMAP {} (CR4; ring 0 can no longer {} user pages, except in a short syscall window)",
            if smep { "ON" } else { "n/a" },
            if smap { "ON" } else { "n/a" },
            if smep && smap { "execute/touch" } else if smap { "touch" } else { "execute" },
        );
    }
}

/// Whether SMAP is now actively enforcing (for the `hardening` shell line).
pub fn smap_active() -> bool {
    SMAP_ON.load(Ordering::Relaxed)
}

/// Whether SMEP is now actively enforcing.
pub fn smep_active() -> bool {
    SMEP_ON.load(Ordering::Relaxed)
}

/// Whether NX (No-Execute / W^X) is now actively enforcing.
pub fn nx_active() -> bool {
    NX_ON.load(Ordering::Relaxed)
}

fn init_syscall_msrs() {
    enable_smep_smap(); // hardware protection ON before every ring-3 excursion (idempotent)
    let sel = crate::gdt::selectors();
    let kcode = sel.code.0 as u64;
    let kdata = sel.data.0 as u64;
    // Enable NX (No-Execute) provided the CPU supports it — CPUID.80000001h:EDX
    // bit 20. Without NXE the NX bit (bit 63) in a PTE has no effect; with NXE
    // it enforces W^X (data/stack/heap not executable). Idempotent.
    let nx = {
        let r = unsafe { core::arch::x86_64::__cpuid(0x8000_0001) };
        r.edx & (1 << 20) != 0
    };
    NX_ON.store(nx, Ordering::Relaxed);
    unsafe {
        let mut efer = Msr::new(0xC000_0080);
        let v = efer.read();
        let nxe = if nx { 1 << 11 } else { 0 }; // EFER.NXE
        efer.write(v | 1 | nxe); // EFER.SCE (+ NXE)
        Msr::new(0xC000_0081).write((kdata << 48) | (kcode << 32)); // STAR
        Msr::new(0xC000_0082).write(syscall_entry as usize as u64); // LSTAR
        Msr::new(0xC000_0084).write(0x200); // FMASK: clear IF on entry
        // Kernel stack for the syscall handler.
        let top = (core::ptr::addr_of!(KSTACK) as u64 + KSTACK_SIZE as u64) & !0xF;
        KERNEL_RSP = top;
    }
}

/// Load `program` into a User frame, run it in ring 3, and return
/// `(exit_code, output)` once it does `sys_exit`.
pub fn run(falloc: &mut FrameAllocator, program: &[u8], caps: u64, linux_abi: bool) -> (u64, String) {
    run_args(falloc, program, &[b"prog"], caps, linux_abi)
}

/// Like [`run`], but with an explicit program name that ends up as `argv[0]` on the
/// SysV stack.
pub fn run_named(
    falloc: &mut FrameAllocator,
    program: &[u8],
    name: &[u8],
    caps: u64,
    linux_abi: bool,
) -> (u64, String) {
    run_args(falloc, program, &[name], caps, linux_abi)
}

/// Like [`run_named`], but with a full `argv` (argv[0] = path, argv[1..] =
/// arguments). The kernel places these on the SysV stack; the program reads them via
/// the standard `main(argc, argv)` contract.
pub fn run_args(
    falloc: &mut FrameAllocator,
    program: &[u8],
    argv: &[&[u8]],
    caps: u64,
    linux_abi: bool,
) -> (u64, String) {
    init_syscall_msrs();
    CURRENT_CAPS.store(caps, Ordering::Relaxed); // the rights of THIS process
    LINUX_ABI.store(linux_abi, Ordering::Relaxed); // Linux or native ABI
    // Record the app identity (argv[0]) for EuroGuard (Track 7).
    *CURRENT_APP.lock() = argv
        .first()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .unwrap_or_default();
    unsafe {
        EXITED = 0;
        EXIT_CODE = 0;
    }
    OUTPUT.lock().clear();
    reset_fd_table(); // fresh per-process fd table

    // ISOLATED address space per foreground exec: all user frames in one
    // 2 MiB arena, only that one gets the USER bit. This way a foreground program
    // (even unsigned/buggy code) can no longer read/write kernel memory.
    const MIB2: u64 = 1 << 21;
    // Exactly 2 MiB, 2 MiB-aligned (no 4 MiB over-allocation); below we free
    // exactly these 512 frames again after the synchronous exec.
    let arena = falloc.allocate_aligned(512, 512).expect("fg-arena");
    let arena_raw = arena;
    let code = arena;
    let heap = arena + 0x80000; // +512 KiB
    let stack_top = arena + MIB2; // user stack grows downward from the arena top
    HEAP_BREAK.store(heap, Ordering::Relaxed);
    ARENA_BASE.store(arena, Ordering::Relaxed); // audit C1
    HEAP_END.store(arena + 0x180000, Ordering::Relaxed); // ~1 MiB heap

    // Load the program into the arena (CR3 still boot: the arena is writable there).
    let pages = program_span_pages(program);
    let info = load_program(program, code, pages);
    let rsp = unsafe { setup_user_stack(stack_top, argv, &info) };
    let entry = info.entry;

    let sel = crate::gdt::selectors();
    let user_cs = (sel.user_code.0 | 3) as u64;
    let user_ss = (sel.user_data.0 | 3) as u64;

    // Build the own W^X PML4 and switch to it just before the ring-3 excursion.
    let pml4 = crate::paging::build_address_space(falloc, arena, &info.exec_pages, &info.writ_pages);
    let boot = crate::sched::boot_pml4();
    // Kernel stack for a possible fault from this foreground process.
    unsafe { crate::gdt::set_rsp0(KERNEL_RSP) };
    FG_ACTIVE.store(true, Ordering::Relaxed);

    // SAFETY: paging/MSR/GDT are set up. We come back via sys_exit (the "9:"
    // epilogue) or, on a page fault, via the force_kernel_return trampoline —
    // both land after `enter_ring3`, so the boot-CR3 restore below runs.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
        enter_ring3(user_cs, user_ss, entry, rsp);
        core::arch::asm!("mov cr3, {}", in(reg) boot, options(nostack, preserves_flags));
    }
    FG_ACTIVE.store(false, Ordering::Relaxed);

    // Clean up the address space (free frames): no leak per foreground exec. Exactly the
    // 512 aligned-allocated arena frames.
    for f in 0..512u64 {
        let _ = falloc.free(arena_raw + f * 4096);
    }
    crate::paging::free_address_space(falloc, pml4);

    let out = OUTPUT.lock().clone();
    unsafe { (core::ptr::read(core::ptr::addr_of!(EXIT_CODE)), out) }
}
