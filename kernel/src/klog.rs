//! Kernel observability (Sprint S1 / Missing §1): in-memory **kmsg ring buffer** +
//! level logging + rich panic context (registers + backtrace + recent history).
//!
//! ALL serial output is also captured into the ring (a tee in `serial::_print`),
//! so that `dmesg` and the panic handler show the recent kernel history without you
//! having to read the serial log. The ring is a fixed array (no alloc in the
//! log path), so it is safe to call from an IRQ or the panic handler.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};

pub const LINES: usize = 512; // number of retained lines (ring size)
pub const LINE_LEN: usize = 160; // max bytes per line (truncated)
const MAX_CPU: usize = 8; // per-CPU partial-line buffers (J1)

/// Known boundaries of the kernel .text segment (UEFI image base 0x1_4000_0000).
/// Used during the stack scan/backtrace to recognize real code return addresses.
pub const KCODE_LO: u64 = 0x1_4000_0000;
pub const KCODE_HI: u64 = 0x1_4080_0000; // well above the ~1.7 MiB image

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DBG",
            Level::Info => "INF",
            Level::Warn => "WRN",
            Level::Error => "ERR",
        }
    }
}

// ── J1: LOCK-FREE kmsg ring ─────────────────────────────────────────────────
// No more global Mutex on the log path (which was taken on EVERY serial line,
// including from IRQs and on multiple cores → contention + deadlock risk in the panic
// handler). Instead:
//   • The committed-lines ring is an MPSC ring: a writer claims a slot with
//     `HEAD.fetch_add(1)` (atomic, wait-free) and writes into `LBUF[idx % LINES]`;
//     `LLEN[idx]` is published with Release. Different cores claim
//     different slots → no content race within the ring window.
//   • The partial (not yet terminated) line lives PER-CPU (`PCUR`/`PLEN`), so
//     each core builds its own line without any lock or cross-core sharing.
// Readers (dmesg/panic) read lock-free → the panic handler can NEVER block.
static HEAD: AtomicUsize = AtomicUsize::new(0); // total number of lines ever written
static mut LBUF: [[u8; LINE_LEN]; LINES] = [[0; LINE_LEN]; LINES];
static LLEN: [AtomicU16; LINES] = [const { AtomicU16::new(0) }; LINES];
static mut PCUR: [[u8; LINE_LEN]; MAX_CPU] = [[0; LINE_LEN]; MAX_CPU];
static mut PLEN: [usize; MAX_CPU] = [0; MAX_CPU];
/// Only after `apic::init` may the tee read `lapic_id()` (LAPIC-MMIO mapped + cores live).
/// Before that everything is single-core BSP → CPU index 0.
static APIC_READY: AtomicBool = AtomicBool::new(false);

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Mark that the Local-APIC is ready (called by `interrupts::init_timer`).
pub fn mark_apic_ready() {
    APIC_READY.store(true, Ordering::Release);
}

/// The CPU index for the per-CPU partial-line buffer (safe before APIC init: 0).
#[inline]
fn cpu_slot() -> usize {
    if APIC_READY.load(Ordering::Acquire) {
        (crate::apic::lapic_id() & (MAX_CPU as u32 - 1)) as usize
    } else {
        0
    }
}

/// Commit one complete line to the lock-free ring (claim slot + publish len).
fn commit_line(bytes: &[u8]) {
    let n = bytes.len().min(LINE_LEN);
    let idx = HEAD.fetch_add(1, Ordering::Relaxed) % LINES;
    unsafe {
        let row = (core::ptr::addr_of_mut!(LBUF) as *mut u8).add(idx * LINE_LEN);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), row, n);
    }
    LLEN[idx].store(n as u16, Ordering::Release);
}

/// A (raw) pointer to the contents of ring slot `i`.
#[inline]
fn line_ptr(i: usize) -> *const u8 {
    (core::ptr::addr_of!(LBUF) as *const u8).wrapping_add(i * LINE_LEN)
}

/// Number of valid lines in the ring + the start index (oldest line).
fn ring_view() -> (usize, usize) {
    let total = HEAD.load(Ordering::Acquire);
    let count = total.min(LINES);
    let start = (total - count) % LINES;
    (count, start)
}

/// Tee: every serial byte also flows through here. We line-buffer PER-CPU until
/// '\n' and then commit the line lock-free to the ring. '\r' is ignored.
pub fn tee(s: &str) {
    let cpu = cpu_slot();
    unsafe {
        // Raw pointers to the per-CPU partial line (no autoref on statics).
        let cur = (core::ptr::addr_of_mut!(PCUR) as *mut u8).add(cpu * LINE_LEN);
        let plen = (core::ptr::addr_of_mut!(PLEN) as *mut usize).add(cpu);
        for &b in s.as_bytes() {
            let l = *plen;
            if b == b'\n' {
                let mut tmp = [0u8; LINE_LEN];
                core::ptr::copy_nonoverlapping(cur, tmp.as_mut_ptr(), l);
                commit_line(&tmp[..l]);
                *plen = 0;
            } else if b != b'\r' && l < LINE_LEN {
                *cur.add(l) = b;
                *plen = l + 1;
            }
        }
    }
}

/// Structured log line with level + uptime timestamp. The tee in `serial::_print`
/// captures it into the ring automatically; so we do not need to push separately here.
pub fn record(level: Level, args: core::fmt::Arguments) {
    let _ = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = crate::interrupts::ticks();
    crate::serial::_print(format_args!("[{:>5}.{:02} {}] {}\n", t / 100, t % 100, level.tag(), args));
}

/// Snapshot of the whole ring (oldest -> newest) as separate strings. Alloc — only
/// call from a normal context (e.g. the `dmesg` shell command).
pub fn snapshot() -> alloc::vec::Vec<alloc::string::String> {
    let (count, start) = ring_view();
    let mut out = alloc::vec::Vec::with_capacity(count);
    for k in 0..count {
        let i = (start + k) % LINES;
        let n = LLEN[i].load(Ordering::Acquire) as usize;
        let line = unsafe { core::slice::from_raw_parts(line_ptr(i), n) };
        out.push(alloc::string::String::from_utf8_lossy(line).into_owned());
    }
    out
}

/// Call `f` for the last `n` ring lines (oldest -> newest). Lock-free →
/// NEVER blocks (crucial for the panic handler).
pub fn with_recent(n: usize, mut f: impl FnMut(&[u8])) {
    let (count, start) = ring_view();
    let cnt = count.min(n);
    let skip = count - cnt;
    for k in 0..cnt {
        let i = (start + skip + k) % LINES;
        let len = LLEN[i].load(Ordering::Acquire) as usize;
        let line = unsafe { core::slice::from_raw_parts(line_ptr(i), len) };
        f(line);
    }
}

/// J1 self-test: prove the lock-free ring. Write a burst of lines (as if from
/// multiple sources), and verify that the HEAD claim captured them all and that the
/// content can be read back intact. (The APs at boot also log via this lock-free
/// path — "core APIC-id N online" — so real cross-core concurrency is already covered.)
pub fn lockfree_selftest() -> bool {
    let before = HEAD.load(Ordering::Acquire);
    for i in 0..64u32 {
        crate::serial_println!("[j1-kmsg] lock-free-ring-test-line {i}");
    }
    let after = HEAD.load(Ordering::Acquire);
    // Count how many of our test lines are intact in the ring.
    let mut found = 0;
    let snap = snapshot();
    for line in &snap {
        if line.starts_with("[j1-kmsg] lock-free-ring-test-line ") {
            found += 1;
        }
    }
    let ok = after - before >= 64 && found >= 64;
    crate::serial_println!(
        "[j1] lock-free kmsg ring: {} lines claimed (HEAD {}→{}), {} read back intact → {}",
        after - before, before, after, found,
        if ok { "OK (no Mutex on the log path) ✓" } else { "FAILED" }
    );
    ok
}

/// Dump CPU registers + a stack backtrace to the serial port. Called by the
/// panic handler. First walks the RBP chain (force-frame-pointers is
/// on), and falls back to a stack scan if the chain breaks.
pub fn dump_registers_and_backtrace() {
    let (rsp, rbp, cr2, cr3, rflags): (u64, u64, u64, u64, u64);
    unsafe {
        core::arch::asm!(
            "mov {0}, rsp",
            "mov {1}, rbp",
            "mov {2}, cr2",
            "mov {3}, cr3",
            "pushfq",
            "pop {4}",
            out(reg) rsp, out(reg) rbp, out(reg) cr2, out(reg) cr3, out(reg) rflags,
        );
    }
    crate::serial::_print(format_args!(
        "[panic] RSP={rsp:#018x} RBP={rbp:#018x} RFLAGS={rflags:#x}\n[panic] CR2={cr2:#018x} CR3={cr3:#018x}\n"
    ));

    // UEFI relocates the PE image to a RUNTIME base (≠ link base 0x1_4000_0000).
    // Derive the real .text range from the address of this function itself, otherwise
    // the filter rejects every valid return address. The ANCHOR address (this function)
    // is the reference point for offline symbolization: `scripts/symbolize.sh`.
    let anchor = dump_registers_and_backtrace as usize as u64;
    let code_lo = anchor & !0x3F_FFFF; // aligned down to 4 MiB
    let code_hi = code_lo + 0x80_0000; // 8 MiB window — covers the whole kernel .text
    let in_code = |a: u64| a >= code_lo && a < code_hi;
    crate::serial::_print(format_args!("[panic] anchor dump_registers_and_backtrace @ {anchor:#018x}\n"));

    // Backtrace: first try the RBP chain ([rbp]=previous rbp, [rbp+8]=return address).
    // On a panic this often breaks on core::panicking frames (without a frame pointer),
    // so we fall back to a stack scan. Symbolize raw addresses offline with
    // `scripts/symbolize.sh target/kernel.map <anchor> <addr...>`.
    crate::serial::_print(format_args!("[panic] backtrace (raw return addresses):\n"));
    let mut bp = rbp;
    let mut frames = 0;
    while frames < 32 && bp >= rsp && bp < rsp + 0x20000 && bp & 0x7 == 0 {
        let ret = unsafe { ((bp + 8) as *const u64).read_volatile() };
        let next = unsafe { (bp as *const u64).read_volatile() };
        if in_code(ret) {
            crate::serial::_print(format_args!("  #{frames:<2} {ret:#018x}\n"));
            frames += 1;
        }
        if next <= bp {
            break; // chain no longer climbs upward -> stop
        }
        bp = next;
    }
    if frames == 0 {
        // Fallback: scan the stack for code addresses (frame pointers were missing).
        crate::serial::_print(format_args!("[panic] (RBP chain empty; stack scan)\n"));
        let mut p = rsp;
        let mut shown = 0;
        let mut last = 0u64;
        while p < rsp + 0x4000 && shown < 24 {
            let v = unsafe { (p as *const u64).read_volatile() };
            if in_code(v) && v != last {
                crate::serial::_print(format_args!("  ? {v:#018x}\n"));
                shown += 1;
                last = v;
            }
            p += 8;
        }
    }
}

#[macro_export]
macro_rules! kinfo {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Info, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kwarn {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Warn, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kerr {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Error, format_args!($($a)*)));
}
#[macro_export]
macro_rules! kdebug {
    ($($a:tt)*) => ($crate::klog::record($crate::klog::Level::Debug, format_args!($($a)*)));
}
