//! EuroClock — the EuroOS clock app (Sprint AC-2).
//!
//! Pure time logic for the clock app: **world time** per time zone with 12/24-hour
//! notation (EuroLocale determines the preference), an **alarm**, a **timer** and a
//! **stopwatch**. Everything operates on a supplied "now" value (unix seconds or
//! kernel ticks), so the logic is deterministic and host-testable.
//!
//! Pure `no_std` logic, host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

const DAY: u64 = 86_400;

/// Time-of-day (hour, minute, second) at a time-zone offset (in minutes relative to UTC).
pub fn time_of_day(epoch_secs: u64, offset_min: i32) -> (u32, u32, u32) {
    // Apply the offset (may be negative) and take modulo one day.
    let shifted = epoch_secs as i64 + offset_min as i64 * 60;
    let day_secs = shifted.rem_euclid(DAY as i64) as u32;
    (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60)
}

/// Format a time-of-day in 24- or 12-hour notation (`hour24=false` → AM/PM).
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

/// Format a duration (seconds) as `MM:SS` or `H:MM:SS`.
pub fn format_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        alloc::format!("{}:{:02}:{:02}", h, m, s)
    } else {
        alloc::format!("{:02}:{:02}", m, s)
    }
}

/// A single world-clock zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldZone {
    pub label: String,
    pub offset_min: i32,
}

impl WorldZone {
    pub fn new(label: &str, offset_min: i32) -> Self {
        WorldZone { label: String::from(label), offset_min }
    }
    /// The current time in this zone, formatted.
    pub fn formatted(&self, epoch_secs: u64, hour24: bool) -> String {
        let (h, m, _) = time_of_day(epoch_secs, self.offset_min);
        format_time(h, m, hour24)
    }
}

/// A world clock = a list of zones.
#[derive(Debug, Clone, Default)]
pub struct WorldClock {
    pub zones: Vec<WorldZone>,
}

impl WorldClock {
    /// A default EU-oriented set of zones.
    pub fn eu_default() -> Self {
        WorldClock {
            zones: alloc::vec![
                WorldZone::new("Brussels", 60),   // CET (UTC+1)
                WorldZone::new("London", 0),      // GMT
                WorldZone::new("Athens", 120),    // EET (UTC+2)
                WorldZone::new("New York", -300), // EST (UTC-5)
            ],
        }
    }
}

/// A countdown timer.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    duration: u64,
    /// Time remaining when last paused; `started`=Some while running.
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
    /// Remaining seconds (0 when finished).
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

/// A stopwatch with lap times.
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
    /// Elapsed time (seconds).
    pub fn elapsed(&self, now: u64) -> u64 {
        match self.started {
            Some(t0) => self.accumulated + now.saturating_sub(t0),
            None => self.accumulated,
        }
    }
    /// Record a lap time.
    pub fn lap(&mut self, now: u64) {
        self.laps.push(self.elapsed(now));
    }
}

/// An alarm at a time-of-day.
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
    /// Does the alarm fire between `prev` and `now` (epoch seconds, at offset)? Detects
    /// the alarm time being passed even across midnight.
    pub fn fires_between(&self, prev: u64, now: u64, offset_min: i32) -> bool {
        if !self.enabled || now <= prev {
            return false;
        }
        let target = (self.hour * 3600 + self.minute * 60) as i64;
        // A per-second loop is too expensive; check whether the target time falls in [prev,now)
        // by projecting both to tod and testing the interval (max 1 day).
        let span = now - prev;
        if span >= DAY {
            return true; // more than a day → certainly passed
        }
        let prev_tod = ((prev as i64 + offset_min as i64 * 60).rem_euclid(DAY as i64)) as i64;
        let now_tod = prev_tod + span as i64; // may be > DAY (wraps around midnight)
        // Target in [prev_tod, now_tod) or, on wrap-around, in the next day.
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
        // Just after midnight UTC, with -300 → previous day 19:00.
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
        // 10:00 UTC → Brussels 11:00, New York 05:00.
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
        assert_eq!(t.remaining(200), 50); // paused → frozen
        t.start(200);
        assert_eq!(t.remaining(230), 20);
        assert!(t.is_done(260));
        assert_eq!(t.remaining(300), 0); // clamps at 0
    }

    #[test]
    fn stopwatch_with_laps() {
        let mut sw = Stopwatch::new();
        sw.start(0);
        sw.lap(10);
        sw.lap(25);
        assert_eq!(sw.laps, alloc::vec![10, 25]);
        sw.stop(30);
        assert_eq!(sw.elapsed(999), 30); // stopped → frozen
        sw.start(40);
        assert_eq!(sw.elapsed(50), 40); // 30 + 10
    }

    #[test]
    fn alarm_fires_when_crossed() {
        let a = Alarm::new(7, 0, "Wake up"); // 07:00 = 25200s tod
        // Interval 06:59:50 → 07:00:10 UTC (offset 0) contains 07:00.
        assert!(a.fires_between(25_190, 25_210, 0));
        // Interval that does not contain 07:00.
        assert!(!a.fires_between(25_210, 25_260, 0));
        // Disabled never fires.
        let mut off = a.clone();
        off.enabled = false;
        assert!(!off.fires_between(25_190, 25_210, 0));
    }

    #[test]
    fn alarm_crosses_midnight() {
        let a = Alarm::new(0, 0, "Midnight"); // 00:00
        // 23:59:50 → 00:00:10 (epoch around a day boundary).
        let prev = DAY - 10;
        let now = DAY + 10;
        assert!(a.fires_between(prev, now, 0));
    }
}
