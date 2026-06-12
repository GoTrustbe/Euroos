//! Boot-zelftest voor **EuroClock** (AC-2): wereldtijd, timer, stopwatch, wekker.
//! Kern: [`euroclock`].

use crate::serial_println;
use euroclock::{format_time, time_of_day, Alarm, Stopwatch, Timer, WorldClock};

pub fn selftest() {
    // Wereldtijd: 10:00 UTC → Brussel 11:00, New York 05:00.
    let wc = WorldClock::eu_default();
    let bru = wc.zones[0].formatted(36_000, true) == "11:00";
    let nyc = wc.zones[3].formatted(36_000, true) == "05:00";
    let ampm = format_time(15, 42, false) == "3:42 PM";
    let (h, _, _) = time_of_day(43_200, 60);
    let tz = h == 13;

    // Timer: 60s, 10s verstreken → 50s resterend.
    let mut t = Timer::new(60);
    t.start(100);
    let timer_ok = t.remaining(110) == 50 && t.is_done(160);

    // Stopwatch + ronde.
    let mut sw = Stopwatch::new();
    sw.start(0);
    sw.lap(10);
    let sw_ok = sw.elapsed(25) == 25 && sw.laps == alloc::vec![10];

    // Wekker vuurt wanneer 07:00 gepasseerd wordt.
    let a = Alarm::new(7, 0, "Opstaan");
    let alarm_ok = a.fires_between(25_190, 25_210, 0) && !a.fires_between(25_210, 25_260, 0);

    let ok = bru && nyc && ampm && tz && timer_ok && sw_ok && alarm_ok;
    serial_println!(
        "[ck] EuroClock: Brussel=11:00({}) NYC=05:00({}) 12u={} TZ+1={} | timer={} stopwatch={} wekker={} {}",
        bru, nyc, ampm, tz, timer_ok, sw_ok, alarm_ok,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
