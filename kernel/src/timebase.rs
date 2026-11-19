//! The time base (BUG-400): real time from the TSC, its rate held to a reference
//! counter (the HPET, else the ACPI PM timer).
//!
//! Before, the clock was the tick count: `ticks x 10 ms`, plus up to two ticks
//! of TSC time in between. The tick count lost time that nothing corrected (see
//! `docs/proof/11f-bug400/`).
//!
//! Now the clock is `C(tsc) = N0 + (tsc - T0) x MULT`. The timer tick, on the
//! BSP with interrupts off, takes a sample of the reference counter about once a
//! second (bracketed by two TSC reads, tried again when the bracket is wide) and
//! sets MULT from the TSC rate over the whole time since the reference was
//! taken, corrected by the offset of the clock from the reference (removed over
//! 4 s, at most 500 ppm). The anchor moves to the sample at every change, so `C`
//! is continuous. The tick count follows `C` (`schedule_tick`): it advances by
//! the periods `C` passed, 0, 1 or more per interrupt, so a lost or late
//! interrupt costs no time and an early one gains none. Forced advances from
//! task context (tickless idle, a frozen-clock poll) still add to the count as
//! before.
//!
//! Without a reference counter the rate is the one of the 10 ms calibration, as
//! before, but lost interrupts are still made up from the TSC.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// Seqlock over T0, N0 and MULT (odd: an update is in progress).
static SEQ: AtomicU64 = AtomicU64::new(0);
static T0: AtomicU64 = AtomicU64::new(0);
static N0: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds per TSC cycle << 32 (0: the time base is not running yet).
static MULT: AtomicU64 = AtomicU64::new(0);

const REF_NONE: u8 = 0;
const REF_HPET: u8 = 1;
const REF_PM: u8 = 2;
static REF: AtomicU8 = AtomicU8::new(REF_NONE);
/// Reference: counter mask, frequency (Hz), port (PM timer), TSC and clock at
/// its base, counter ticks since the base, the last raw value and its TSC, the
/// TSC of the next sample.
static REF_MASK: AtomicU64 = AtomicU64::new(0);
static REF_HZ: AtomicU64 = AtomicU64::new(0);
static REF_PORT: AtomicU64 = AtomicU64::new(0);
static REF_TSC0: AtomicU64 = AtomicU64::new(0);
static REF_C0: AtomicU64 = AtomicU64::new(0);
static REF_ACC: AtomicU64 = AtomicU64::new(0);
static REF_LAST: AtomicU64 = AtomicU64::new(0);
static REF_TSC_LAST: AtomicU64 = AtomicU64::new(0);
static NEXT_SAMPLE: AtomicU64 = AtomicU64::new(0);
/// TSC cycles the reference may go unsampled before its counter could wrap
/// unseen (then its base is taken again).
static REF_SPAN_TSC: AtomicU64 = AtomicU64::new(u64::MAX);

/// Diagnostics: samples taken, refused (wide bracket), reference restarts, the
/// last offset of the clock from the reference (ns, signed) and the largest.
static SAMPLES: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
static RESTARTS: AtomicU64 = AtomicU64::new(0);
static LAST_OFFSET: AtomicU64 = AtomicU64::new(0);
static MAX_OFFSET: AtomicU64 = AtomicU64::new(0);

/// The offset is taken out over this long (ns), changing the rate by at most
/// this much (ppm).
const SLEW_NS: i128 = 4_000_000_000;
const SLEW_MAX_PPM: i128 = 500;

pub fn running() -> bool {
    MULT.load(Ordering::Acquire) != 0
}

/// Start the time base at the first timer tick: `C(tsc) = ns` there, at the
/// rate of the 10 ms calibration (`tsc_per_tick` cycles per 10 ms).
pub fn start(tsc: u64, ns: u64, tsc_per_tick: u64) {
    if tsc_per_tick == 0 || running() {
        return;
    }
    // The reference: the HPET (64-bit or 32-bit counter), else the PM timer.
    if crate::hpet::present() && crate::hpet::freq_hz() != 0 {
        REF_MASK.store(crate::hpet::counter_mask(), Ordering::Relaxed);
        REF_HZ.store(crate::hpet::freq_hz(), Ordering::Relaxed);
        REF.store(REF_HPET, Ordering::Relaxed);
    } else if let Some(f) = crate::acpi::fadt() {
        if f.pm_tmr_blk != 0 && f.pm_tmr_blk <= 0xFFFF {
            let port = f.pm_tmr_blk as u16;
            let mask = if f.pm_tmr_32bit { 0xFFFF_FFFFu64 } else { 0x00FF_FFFF };
            // SAFETY: reading the PM timer port has no side effects.
            let a = unsafe { x86_64::instructions::port::Port::<u32>::new(port).read() } as u64 & mask;
            if a != mask {
                REF_PORT.store(port as u64, Ordering::Relaxed);
                REF_MASK.store(mask, Ordering::Relaxed);
                REF_HZ.store(3_579_545, Ordering::Relaxed);
                REF.store(REF_PM, Ordering::Relaxed);
            }
        }
    }
    let hz = REF_HZ.load(Ordering::Relaxed);
    let mask = REF_MASK.load(Ordering::Relaxed);
    if hz != 0 && mask != u64::MAX {
        // half the time the counter takes to wrap, in TSC cycles
        let wrap_ticks = ((mask as u128 + 1) * 100 / hz as u128) as u64; // 10 ms units
        REF_SPAN_TSC.store((wrap_ticks / 2).saturating_mul(tsc_per_tick), Ordering::Relaxed);
    }
    NEXT_SAMPLE.store(tsc, Ordering::Relaxed);
    let mult = ((10_000_000u128 << 32) / tsc_per_tick as u128) as u64;
    publish(tsc, ns, mult);
}

fn publish(t0: u64, n0: u64, mult: u64) {
    let s = SEQ.load(Ordering::Relaxed);
    SEQ.store(s | 1, Ordering::Relaxed);
    core::sync::atomic::fence(Ordering::Release);
    T0.store(t0, Ordering::Relaxed);
    N0.store(n0, Ordering::Relaxed);
    MULT.store(mult, Ordering::Relaxed);
    SEQ.store((s | 1) + 1, Ordering::Release);
}

fn at(t0: u64, n0: u64, mult: u64, tsc: u64) -> u64 {
    let dt = tsc.wrapping_sub(t0);
    if dt as i64 <= 0 {
        return n0; // a TSC from before the anchor (another CPU): no time yet
    }
    n0 + ((dt as u128 * mult as u128) >> 32) as u64
}

/// The clock at `tsc`, in ns (0 before the first tick).
pub fn ns_at(tsc: u64) -> u64 {
    loop {
        let s = SEQ.load(Ordering::Acquire);
        if s & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let (t0, n0, m) = (T0.load(Ordering::Relaxed), N0.load(Ordering::Relaxed), MULT.load(Ordering::Relaxed));
        core::sync::atomic::fence(Ordering::Acquire);
        if SEQ.load(Ordering::Relaxed) == s {
            return if m == 0 { 0 } else { at(t0, n0, m, tsc) };
        }
    }
}

/// The rate as the vDSO page carries it: ns per TSC cycle << 20.
pub fn rate_shift20() -> u64 {
    MULT.load(Ordering::Relaxed) >> 12
}

fn read_ref() -> u64 {
    match REF.load(Ordering::Relaxed) {
        REF_HPET => crate::hpet::counter() & REF_MASK.load(Ordering::Relaxed),
        // SAFETY: reading the PM timer port has no side effects.
        REF_PM => {
            let port = REF_PORT.load(Ordering::Relaxed) as u16;
            (unsafe { x86_64::instructions::port::Port::<u32>::new(port).read() } as u64) & REF_MASK.load(Ordering::Relaxed)
        }
        _ => 0,
    }
}

/// From the timer tick (BSP, interrupts off), with the TSC of the tick: about
/// once a second, sample the reference and set the rate.
pub fn on_tick(now: u64, tsc_per_tick: u64) {
    if REF.load(Ordering::Relaxed) == REF_NONE || !running() || tsc_per_tick == 0 {
        return;
    }
    if (now.wrapping_sub(NEXT_SAMPLE.load(Ordering::Relaxed)) as i64) < 0 {
        return;
    }
    // The narrowest of three brackets; one wider than 1/50 of a tick (a vCPU
    // stopped in the middle) is tried again at the next tick.
    let mut best = (u64::MAX, 0u64, 0u64);
    for _ in 0..3 {
        let a = unsafe { core::arch::x86_64::_rdtsc() };
        let r = read_ref();
        let b = unsafe { core::arch::x86_64::_rdtsc() };
        let w = b.wrapping_sub(a);
        if w < best.0 {
            best = (w, a + w / 2, r);
        }
    }
    let (w, t, raw) = best;
    if w > tsc_per_tick / 50 {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        NEXT_SAMPLE.store(now.wrapping_add(tsc_per_tick), Ordering::Relaxed);
        return;
    }
    NEXT_SAMPLE.store(now.wrapping_add(tsc_per_tick * 100), Ordering::Relaxed);
    let c = ns_at(t);
    let first = REF_TSC0.load(Ordering::Relaxed) == 0;
    let gap = t.wrapping_sub(REF_TSC_LAST.load(Ordering::Relaxed));
    if first || gap > REF_SPAN_TSC.load(Ordering::Relaxed) {
        // (Re)start the reference here: the clock as it is, is its base.
        if !first {
            RESTARTS.fetch_add(1, Ordering::Relaxed);
        }
        REF_TSC0.store(t, Ordering::Relaxed);
        REF_C0.store(c, Ordering::Relaxed);
        REF_ACC.store(0, Ordering::Relaxed);
        REF_LAST.store(raw, Ordering::Relaxed);
        REF_TSC_LAST.store(t, Ordering::Relaxed);
        return;
    }
    let mask = REF_MASK.load(Ordering::Relaxed);
    let acc = REF_ACC.load(Ordering::Relaxed) + (raw.wrapping_sub(REF_LAST.load(Ordering::Relaxed)) & mask);
    REF_ACC.store(acc, Ordering::Relaxed);
    REF_LAST.store(raw, Ordering::Relaxed);
    REF_TSC_LAST.store(t, Ordering::Relaxed);
    SAMPLES.fetch_add(1, Ordering::Relaxed);
    let hz = REF_HZ.load(Ordering::Relaxed) as u128;
    let ref_ns = (acc as u128 * 1_000_000_000 / hz) as i128; // since the base
    let tsc_d = t.wrapping_sub(REF_TSC0.load(Ordering::Relaxed)) as i128;
    if ref_ns < 500_000_000 || tsc_d <= 0 {
        return; // too short a base for a rate
    }
    let true_ns = REF_C0.load(Ordering::Relaxed) as i128 + ref_ns;
    let off = c as i128 - true_ns; // > 0: the clock is ahead
    LAST_OFFSET.store(off as i64 as u64, Ordering::Relaxed);
    MAX_OFFSET.fetch_max(off.unsigned_abs() as u64, Ordering::Relaxed);
    // The TSC rate over the whole base, then the offset taken out over SLEW_NS.
    let m_true = (ref_ns << 32) / tsc_d;
    let corr_ppm = (off * 1_000_000 / SLEW_NS).clamp(-SLEW_MAX_PPM, SLEW_MAX_PPM);
    let m = m_true - m_true * corr_ppm / 1_000_000;
    if m <= 0 {
        return;
    }
    publish(t, c, m as u64); // re-anchored at this sample: continuous
}

/// `[clock-drift]` and `uptime`: the reference in use, samples, refused,
/// restarts, last and largest offset of the clock from it (ns).
pub fn stats() -> (&'static str, u64, u64, u64, i64, u64) {
    (
        match REF.load(Ordering::Relaxed) {
            REF_HPET => "HPET",
            REF_PM => "ACPI PM timer",
            _ => "none",
        },
        SAMPLES.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
        RESTARTS.load(Ordering::Relaxed),
        LAST_OFFSET.load(Ordering::Relaxed) as i64,
        MAX_OFFSET.load(Ordering::Relaxed),
    )
}
