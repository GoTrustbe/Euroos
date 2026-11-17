//! `[clock-drift]` (BUG-400): the clock processes read (the vDSO page, the same
//! formula as `clock_gettime`) and the tick count against the HPET main counter,
//! an independent clock in the guest (QEMU drives it from the host's monotonic
//! clock; on hardware it is its own crystal).
//!
//! Two phases, each measured from its first to its last sample:
//! - `rest`: the BSP halts between ticks for 10 s of HPET time;
//! - `late`: 6 s with interrupts held off in windows of 13 to 35 ms (1.3 to 3.5
//!   periods), each followed by 1 ms with interrupts on, so ticks arrive late
//!   one after another, as on a host that does not run the vCPU in time.
//!
//! The verdict: the clock (MONOTONIC) may differ from the HPET by at most 50 ppm
//! in each phase, and the tick count (ticks x 10 ms) by at most two ticks plus
//! 50 ppm. Printed with it, to tell the causes apart: the TSC frequency the HPET
//! measures against the one the 10 ms PIT calibration gave, the timer interrupts
//! taken against those the calibrated period predicts, the ticks the late-tick
//! rule added, and the forced advances (task context, tickless idle).
//!
//! What this cannot show: a guest-internal test compares two clocks of the same
//! guest. That the HPET follows the host (QEMU's virtual clock) is checked by
//! the host measurement (`drift.txt`), not here.

use core::sync::atomic::Ordering;

#[derive(Clone, Copy)]
struct Sample {
    hpet_ns: u64,
    tsc: u64,
    ticks: u64,
    mono: u64,
    irqs: u64,
    added: u64,
}

fn sample() -> Sample {
    // The HPET read bracketed by two TSC reads, interrupts off: the pair is
    // taken at one moment (a vCPU stopped in between shows as a wide bracket,
    // and the sample is taken again).
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut best: Option<(u64, Sample)> = None;
        for _ in 0..8 {
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            let h = crate::hpet::ns();
            let t1 = unsafe { core::arch::x86_64::_rdtsc() };
            let w = t1.wrapping_sub(t0);
            let s = Sample {
                hpet_ns: h,
                tsc: t0 + w / 2,
                ticks: crate::interrupts::ticks(),
                mono: crate::ring3::clock_ns(1),
                irqs: crate::sched::TICK_IRQS.load(Ordering::Relaxed),
                added: crate::sched::TICK_ADDED.load(Ordering::Relaxed),
            };
            if best.map_or(true, |(bw, _)| w < bw) {
                best = Some((w, s));
            }
        }
        best.map(|b| b.1).unwrap()
    })
}

fn ppm(got: i128, want: i128) -> i128 {
    if want == 0 {
        0
    } else {
        (got - want) * 1_000_000 / want
    }
}

/// One phase; true when the clock and the tick count kept to HPET time.
fn report(name: &str, a: Sample, b: Sample) -> bool {
    let dh = (b.hpet_ns - a.hpet_ns) as i128;
    let dt = b.tsc.wrapping_sub(a.tsc) as i128;
    let dmono = b.mono as i128 - a.mono as i128;
    let dticks = (b.ticks - a.ticks) as i128;
    let dirq = (b.irqs - a.irqs) as i128;
    let dadd = (b.added - a.added) as i128;
    let per = crate::apic::TSC_PER_TICK.load(Ordering::Relaxed) as i128;
    let tsc_hz = if dh > 0 { dt * 1_000_000_000 / dh } else { 0 };
    let clock_ppm = ppm(dmono, dh);
    let tick_ns = dticks * 10_000_000;
    let tick_err = tick_ns - dh;
    let tick_ok = tick_err.abs() <= 20_000_000 + dh * 50 / 1_000_000;
    let irq_want = if per > 0 { dt / per } else { 0 };
    let ok = clock_ppm.abs() <= 50 && tick_ok;
    crate::serial_println!(
        "[clock-drift] {name}: {} ms of HPET time: clock {:+} ppm ({:+} us), tick count {:+} ppm ({:+} ms) -> {}",
        dh / 1_000_000,
        clock_ppm,
        (dmono - dh) / 1000,
        ppm(tick_ns, dh),
        tick_err / 1_000_000,
        if ok { "OK" } else { "FAILED" }
    );
    crate::serial_println!(
        "[clock-drift]   {name} diag: TSC {} kHz by the HPET, {} kHz by the 10 ms calibration ({:+} ppm); timer interrupts {} of {} the calibrated period predicts ({} missing); ticks added for late ones {}, by forced advances {}",
        tsc_hz / 1000,
        per * 100 / 1000,
        ppm(per * 100, tsc_hz),
        dirq,
        irq_want,
        irq_want - dirq,
        dadd - dirq,
        dticks - dadd
    );
    ok
}

/// Wait until the HPET passed `until_ns`, interrupts on.
fn halt_until(until_ns: u64) {
    while crate::hpet::ns() < until_ns {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

pub fn selftest() {
    if !crate::hpet::present() {
        crate::serial_println!("[clock-drift] no HPET in this machine: not measured ⬜");
        return;
    }
    // rest
    let a = sample();
    halt_until(a.hpet_ns + 10_000_000_000);
    let b = sample();
    let rest = report("rest", a, b);
    // late ticks: interrupts off for 13..35 ms, 1 ms on, again
    const WIN_MS: [u64; 6] = [13, 17, 23, 27, 35, 15];
    let c = sample();
    let end = c.hpet_ns + 6_000_000_000;
    let mut i = 0;
    while crate::hpet::ns() < end {
        let w = WIN_MS[i % WIN_MS.len()] * 1_000_000;
        i += 1;
        x86_64::instructions::interrupts::without_interrupts(|| {
            let t = crate::hpet::ns();
            while crate::hpet::ns() < t + w {
                core::hint::spin_loop();
            }
        });
        halt_until(crate::hpet::ns() + 1_000_000);
    }
    // let the last late tick land before the end sample
    halt_until(crate::hpet::ns() + 30_000_000);
    let d = sample();
    let late = report("late", c, d);
    crate::serial_println!(
        "[clock-drift] the clock and the tick count against the HPET (at most 50 ppm; ticks within 2 + 50 ppm), at rest and with late ticks -> {}",
        if rest && late { "OK" } else { "FAILED" }
    );
}

/// Raw counters for `uptime` over the serial console (BUG-400): the host
/// takes them with its own clocks to tell the tick loss from the calibration.
pub fn diag_line() -> alloc::string::String {
    let s = sample();
    alloc::format!(
        "clock diag: hpet {} ns, tsc {}, ticks {}, irqs {}, added {}, tsc/tick {}, mono {} ns",
        s.hpet_ns,
        s.tsc,
        s.ticks,
        s.irqs,
        s.added,
        crate::apic::TSC_PER_TICK.load(Ordering::Relaxed),
        s.mono
    )
}
