//! `[boot-time]`: where the boot spends its time, per phase. Always on, in
//! every image: one serial line per phase and one summary line when the desktop
//! loop starts.
//!
//! Clock: the HPET main counter (`hpet::ns`), which runs with interrupts off
//! and before the tick exists. The HPET is only reachable on our own page
//! tables, so the stretch before that (UEFI calls, ExitBootServices, GDT/IDT,
//! paging) is stamped with the TSC and converted once the TSC rate is known:
//! it is measured against the HPET over the first phase that is long enough.
//! Until then the early phases are held back and printed together. Without an
//! HPET the tick clock (10 ms, zero until the timer runs) is the fallback and
//! the lines say so.
//!
//! Cost: one `rdtsc` and one HPET read per phase mark, one formatted line.
//!
//! A phase mark names the stretch that ENDS there: `phase("storage")` after the
//! root file system is mounted means "everything since the previous mark was
//! storage".

use alloc::string::String;
use core::arch::x86_64::_rdtsc;
use core::fmt::Write;

const MAX_PHASES: usize = 40;
/// HPET time the TSC rate is measured over, at least.
const CAL_MIN_NS: u64 = 2_000_000;

#[derive(Clone, Copy)]
struct Mark {
    name: &'static str,
    tsc: u64,
    /// HPET nanoseconds at the mark (0 while there was no HPET yet).
    hpet_ns: u64,
    /// Tick clock at the mark (fallback).
    ticks: u64,
}

struct State {
    /// TSC at kernel entry.
    tsc0: u64,
    /// (TSC, HPET ns) when the HPET came up.
    anchor: Option<(u64, u64)>,
    /// `clock_ready` was called: it is known whether there is an HPET.
    clock_known: bool,
    /// TSC ticks per millisecond, once measured.
    tsc_per_ms: u64,
    marks: [Mark; MAX_PHASES],
    n: usize,
    /// Marks already printed.
    printed: usize,
    done: bool,
}

const NO_MARK: Mark = Mark { name: "", tsc: 0, hpet_ns: 0, ticks: 0 };

static STATE: spin::Mutex<State> = spin::Mutex::new(State {
    tsc0: 0,
    anchor: None,
    clock_known: false,
    tsc_per_ms: 0,
    marks: [NO_MARK; MAX_PHASES],
    n: 0,
    printed: 0,
    done: false,
});

fn tsc() -> u64 {
    // SAFETY: rdtsc is unprivileged and has no side effects.
    unsafe { _rdtsc() }
}

/// Kernel entry: the zero of every `[boot-time]` number.
pub fn start() {
    let mut s = STATE.lock();
    s.tsc0 = tsc();
}

/// The clock question is settled: `hpet` = `hpet::init()` found one, and it
/// answers from here on. Marks made before this call are printed after it.
pub fn clock_ready(hpet: bool) {
    let mut s = STATE.lock();
    if !s.clock_known {
        s.clock_known = true;
        if hpet && crate::hpet::present() {
            s.anchor = Some((tsc(), crate::hpet::ns()));
        }
    }
}

impl State {
    /// Microseconds since kernel entry of mark `m`, if it can be told yet.
    fn at_us(&self, m: &Mark) -> Option<u64> {
        if !self.clock_known {
            return None;
        }
        match self.anchor {
            Some((a_tsc, a_ns)) => {
                if self.tsc_per_ms == 0 {
                    return None;
                }
                let anchor_us = a_tsc.saturating_sub(self.tsc0).saturating_mul(1000) / self.tsc_per_ms;
                if m.hpet_ns != 0 {
                    Some(anchor_us + m.hpet_ns.saturating_sub(a_ns) / 1000)
                } else {
                    Some(m.tsc.saturating_sub(self.tsc0).saturating_mul(1000) / self.tsc_per_ms)
                }
            }
            // No HPET: the tick clock, which starts with the timer.
            None => Some(m.ticks * 10_000),
        }
    }

    fn calibrate(&mut self) {
        if self.tsc_per_ms != 0 {
            return;
        }
        if let Some((a_tsc, a_ns)) = self.anchor {
            let dns = crate::hpet::ns().saturating_sub(a_ns);
            if dns >= CAL_MIN_NS {
                let dtsc = tsc().saturating_sub(a_tsc);
                self.tsc_per_ms = (dtsc as u128 * 1_000_000 / dns as u128).max(1) as u64;
            }
        }
    }

    /// Print every mark that has a time and was not printed yet.
    fn flush(&mut self) {
        while self.printed < self.n {
            let i = self.printed;
            let m = self.marks[i];
            let Some(at) = self.at_us(&m) else { return };
            let prev = if i == 0 { 0 } else { self.at_us(&self.marks[i - 1]).unwrap_or(0) };
            let took = at.saturating_sub(prev);
            crate::serial_println!(
                "[boot-time] phase {}: {}.{:03} ms (at {} ms)",
                m.name,
                took / 1000,
                took % 1000,
                at / 1000
            );
            self.printed += 1;
        }
    }
}

/// The stretch since the previous mark (or kernel entry) was `name`.
pub fn phase(name: &'static str) {
    let mut s = STATE.lock();
    if s.done || s.n >= MAX_PHASES {
        return;
    }
    let hpet_ns = if s.anchor.is_some() { crate::hpet::ns() } else { 0 };
    let i = s.n;
    s.marks[i] = Mark { name, tsc: tsc(), hpet_ns, ticks: crate::interrupts::ticks() };
    s.n += 1;
    s.calibrate();
    s.flush();
}

/// The desktop loop starts: the summary line. Later marks are ignored.
pub fn desktop() {
    let mut s = STATE.lock();
    if s.done {
        return;
    }
    s.calibrate();
    s.flush();
    s.done = true;
    let total = if s.n == 0 { 0 } else { s.at_us(&s.marks[s.n - 1]).unwrap_or(0) };
    let mut line = String::new();
    let mut prev = 0u64;
    for i in 0..s.n {
        let m = s.marks[i];
        let at = s.at_us(&m).unwrap_or(prev);
        let _ = write!(line, "{}{}={}", if i == 0 { "" } else { " " }, m.name, at.saturating_sub(prev) / 1000);
        prev = at;
    }
    crate::serial_println!(
        "[boot-time] desktop after {} ms since kernel entry ({}, {} build): {} (ms per phase)",
        total / 1000,
        if s.anchor.is_some() { "HPET" } else { "tick clock, no HPET" },
        if cfg!(feature = "selftest") { "selftest" } else { "release" },
        line
    );
}
