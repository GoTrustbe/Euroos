//! EuroClock — de klok-app van EuroOS (Sprint AC-2).
//!
//! Pure tijdslogica voor de klok-app: **wereldtijd** per tijdzone met 12/24-uurs
//! notatie (EuroLocale bepaalt de voorkeur), een **wekker**, een **timer** en een
//! **stopwatch**. Alles werkt op een meegeleverde "nu"-waarde (unix-seconden of
//! kernel-ticks), zodat de logica deterministisch en host-testbaar is.
//!
//! Pure `no_std`-logica, host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

const DAY: u64 = 86_400;

/// Tijd-van-de-dag (uur, minuut, seconde) op een tijdzone-offset (in minuten t.o.v. UTC).
pub fn time_of_day(epoch_secs: u64, offset_min: i32) -> (u32, u32, u32) {
    // Pas de offset toe (kan negatief zijn) en neem modulo één dag.
    let shifted = epoch_secs as i64 + offset_min as i64 * 60;
    let day_secs = shifted.rem_euclid(DAY as i64) as u32;
    (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60)
}

/// Formatteer een tijd-van-de-dag in 24- of 12-uurs notatie (`hour24=false` → AM/PM).
pub fn format_time(h: u32, m: u32, hour24: bool) -> String {
    if hour24 {
        alloc::format!("{:02}:{:02}", h, m)
    } else {
        let (suffix, hh) = if h == 0 {
            ("AM", 12)
        } else if h < 12 {
            ("AM", h)
        } else if h == 12 {
            ("PM", 12)
        } else {
            ("PM", h - 12)
        };
        alloc::format!("{}:{:02} {}", hh, m, suffix)
    }
}

/// Formatteer een duur (seconden) als `MM:SS` of `H:MM:SS`.
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        alloc::format!("{}:{:02}:{:02}", h, m, s)
    } else {
        alloc::format!("{:02}:{:02}", m, s)
    }
}

/// Eén wereldklok-zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldZone {
    pub label: String,
    pub offset_min: i32,
}

impl WorldZone {
    pub fn new(label: &str, offset_min: i32) -> Self {
        WorldZone { label: String::from(label), offset_min }
    }
    /// De huidige tijd in deze zone, geformatteerd.
    pub fn formatted(&self, epoch_secs: u64, hour24: bool) -> String {
        let (h, m, _) = time_of_day(epoch_secs, self.offset_min);
        format_time(h, m, hour24)
    }
}

/// Een wereldklok = een lijst zones.
#[derive(Debug, Clone, Default)]
pub struct WorldClock {
    pub zones: Vec<WorldZone>,
}

impl WorldClock {
    /// Een standaard EU-georiënteerde set zones.
    pub fn eu_default() -> Self {
        WorldClock {
            zones: alloc::vec![
                WorldZone::new("Brussel", 60),    // CET (UTC+1)
                WorldZone::new("Londen", 0),      // GMT
                WorldZone::new("Athene", 120),    // EET (UTC+2)
                WorldZone::new("New York", -300), // EST (UTC-5)
            ],
        }
    }
}

/// Een aftellende timer.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    duration: u64,
    /// Resterende tijd toen voor het laatst gepauzeerd; `started`=Some als lopend.
    remaining_at_pause: u64,
    started: Option<u64>,
}

impl Timer {
    pub fn new(duration: u64) -> Self {
        Timer { duration, remaining_at_pause: duration, started: None }
    }
    pub fn start(&mut self, now: u64) {
        if self.started.is_none() {
            self.started = Some(now);
        }
    }
    pub fn pause(&mut self, now: u64) {
        self.remaining_at_pause = self.remaining(now);
        self.started = None;
    }
    pub fn reset(&mut self) {
        self.remaining_at_pause = self.duration;
        self.started = None;
    }
    /// Resterende seconden (0 als afgelopen).
    pub fn remaining(&self, now: u64) -> u64 {
        match self.started {
            Some(t0) => self.remaining_at_pause.saturating_sub(now.saturating_sub(t0)),
            None => self.remaining_at_pause,
        }
    }
    pub fn is_done(&self, now: u64) -> bool {
        self.remaining(now) == 0
    }
}

/// Een stopwatch met ronde-tijden (laps).
#[derive(Debug, Clone, Default)]
pub struct Stopwatch {
    accumulated: u64,
    started: Option<u64>,
    pub laps: Vec<u64>,
}

impl Stopwatch {
    pub fn new() -> Self {
        Stopwatch::default()
    }
    pub fn start(&mut self, now: u64) {
        if self.started.is_none() {
            self.started = Some(now);
        }
    }
    pub fn stop(&mut self, now: u64) {
        self.accumulated = self.elapsed(now);
        self.started = None;
    }
    pub fn reset(&mut self) {
        self.accumulated = 0;
        self.started = None;
        self.laps.clear();
    }
    /// Verstreken tijd (seconden).
    pub fn elapsed(&self, now: u64) -> u64 {
        match self.started {
            Some(t0) => self.accumulated + now.saturating_sub(t0),
            None => self.accumulated,
        }
    }
    /// Leg een ronde-tijd vast.
    pub fn lap(&mut self, now: u64) {
        self.laps.push(self.elapsed(now));
    }
}

/// Een wekker op een tijd-van-de-dag.
#[derive(Debug, Clone)]
pub struct Alarm {
    pub hour: u32,
    pub minute: u32,
    pub enabled: bool,
    pub label: String,
}

impl Alarm {
    pub fn new(hour: u32, minute: u32, label: &str) -> Self {
        Alarm { hour: hour % 24, minute: minute % 60, enabled: true, label: String::from(label) }
    }
    /// Vuurt de wekker tussen `prev` en `now` (epoch-seconden, op offset)? Detecteert
    /// het passeren van het wekkertijdstip ook over middernacht heen.
    pub fn fires_between(&self, prev: u64, now: u64, offset_min: i32) -> bool {
        if !self.enabled || now <= prev {
            return false;
        }
        let target = (self.hour * 3600 + self.minute * 60) as i64;
        // Loop seconde-grof is te duur; check of het doel-tijdstip in [prev,now) valt
        // door beide naar tod te projecteren en het interval te toetsen (max 1 dag).
        let span = now - prev;
        if span >= DAY {
            return true; // meer dan een dag → zeker gepasseerd
        }
        let prev_tod = ((prev as i64 + offset_min as i64 * 60).rem_euclid(DAY as i64)) as i64;
        let now_tod = prev_tod + span as i64; // kan > DAY zijn (wikkelt om middernacht)
        // Doel in [prev_tod, now_tod) of, bij omwikkeling, in het volgende etmaal.
        (prev_tod..now_tod).contains(&target) || (prev_tod..now_tod).contains(&(target + DAY as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_of_day_with_offsets() {
        // 12:00:00 UTC = 43200s.
        assert_eq!(time_of_day(43_200, 0), (12, 0, 0));
        assert_eq!(time_of_day(43_200, 60), (13, 0, 0)); // CET
        assert_eq!(time_of_day(43_200, -300), (7, 0, 0)); // New York
        // Net na middernacht UTC, met -300 → vorige dag 19:00.
        assert_eq!(time_of_day(0, -300), (19, 0, 0));
    }

    #[test]
    fn formatting_12_24() {
        assert_eq!(format_time(15, 42, true), "15:42");
        assert_eq!(format_time(15, 42, false), "3:42 PM");
        assert_eq!(format_time(0, 5, false), "12:05 AM");
        assert_eq!(format_time(12, 0, false), "12:00 PM");
        assert_eq!(format_duration(65), "01:05");
        assert_eq!(format_duration(3 * 3600 + 4 * 60 + 9), "3:04:09");
    }

    #[test]
    fn world_clock_zones() {
        let wc = WorldClock::eu_default();
        assert_eq!(wc.zones.len(), 4);
        // 10:00 UTC → Brussel 11:00, New York 05:00.
        assert_eq!(wc.zones[0].formatted(36_000, true), "11:00");
        assert_eq!(wc.zones[3].formatted(36_000, true), "05:00");
    }

    #[test]
    fn timer_countdown_pause_resume() {
        let mut t = Timer::new(60);
        assert_eq!(t.remaining(0), 60);
        t.start(100);
        assert_eq!(t.remaining(110), 50);
        t.pause(110);
        assert_eq!(t.remaining(200), 50); // gepauzeerd → bevroren
        t.start(200);
        assert_eq!(t.remaining(230), 20);
        assert!(t.is_done(260));
        assert_eq!(t.remaining(300), 0); // klemt op 0
    }

    #[test]
    fn stopwatch_with_laps() {
        let mut sw = Stopwatch::new();
        sw.start(0);
        sw.lap(10);
        sw.lap(25);
        assert_eq!(sw.laps, alloc::vec![10, 25]);
        sw.stop(30);
        assert_eq!(sw.elapsed(999), 30); // gestopt → bevroren
        sw.start(40);
        assert_eq!(sw.elapsed(50), 40); // 30 + 10
    }

    #[test]
    fn alarm_fires_when_crossed() {
        let a = Alarm::new(7, 0, "Opstaan"); // 07:00 = 25200s tod
        // Interval 06:59:50 → 07:00:10 UTC (offset 0) bevat 07:00.
        assert!(a.fires_between(25_190, 25_210, 0));
        // Interval dat 07:00 niet bevat.
        assert!(!a.fires_between(25_210, 25_260, 0));
        // Uitgeschakeld vuurt nooit.
        let mut off = a.clone();
        off.enabled = false;
        assert!(!off.fires_between(25_190, 25_210, 0));
    }

    #[test]
    fn alarm_crosses_midnight() {
        let a = Alarm::new(0, 0, "Middernacht"); // 00:00
        // 23:59:50 → 00:00:10 (epoch rond een dagovergang).
        let prev = DAY - 10;
        let now = DAY + 10;
        assert!(a.fires_between(prev, now, 0));
    }
}
