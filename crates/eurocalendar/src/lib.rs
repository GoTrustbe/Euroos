//! EuroCalendar — de agenda van EuroOS (Sprint AC-3).
//!
//! Afspraken met **herhaling** (dagelijks/wekelijks/maandelijks/jaarlijks, met
//! `interval`, `count` of `until`), **herinneringen**, en maand/week-indeling met
//! **maandag als eerste dag** (EU-conventie). Bevat een eigen civiele-datum-
//! rekenkern (geen libc, geen tijdzone-database) op basis van het bekende
//! days-from-civil-algoritme. Tijd = unix-seconden (UTC). Pure `no_std`-logica,
//! host-getest. EuroAgent kan dit als `calendar_read`-backend gebruiken.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

const DAY: i64 = 86_400;

// ── Civiele datum ↔ dagen sinds 1970-01-01 (Howard Hinnant's algoritme) ──

/// (jaar, maand 1–12, dag 1–31) → dagen sinds 1970-01-01.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0,365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Dagen sinds 1970-01-01 → (jaar, maand, dag).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Dag van de week voor een unix-tijdstip: 0=maandag … 6=zondag (EU-conventie).
pub fn weekday_mon0(epoch: i64) -> u32 {
    // 1970-01-01 was een donderdag (=3 in maandag-0).
    (((epoch.div_euclid(DAY) % 7) + 3 + 7 * 1000) % 7) as u32
}

/// Begin van de dag (00:00 UTC) voor een tijdstip.
pub fn day_start(epoch: i64) -> i64 {
    epoch.div_euclid(DAY) * DAY
}

/// Begin van de week (maandag 00:00) voor een tijdstip.
pub fn week_start(epoch: i64) -> i64 {
    day_start(epoch) - weekday_mon0(epoch) as i64 * DAY
}

// ── Herhaling ──

/// Herhalingsfrequentie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// Een herhalingsregel (RRULE-subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recurrence {
    pub freq: Freq,
    pub interval: u32,
    /// Maximaal aantal voorkomens (inclusief het eerste); `0` = onbegrensd.
    pub count: u32,
    /// Stop ná dit tijdstip; `0` = onbegrensd.
    pub until: i64,
}

impl Recurrence {
    pub fn new(freq: Freq, interval: u32) -> Self {
        Recurrence { freq, interval: interval.max(1), count: 0, until: 0 }
    }
    pub fn count(mut self, n: u32) -> Self {
        self.count = n;
        self
    }
    pub fn until(mut self, t: i64) -> Self {
        self.until = t;
        self
    }

    /// Het `n`-de voorkomen (0-gebaseerd) van een afspraak die op `start` begint.
    fn nth(&self, start: i64, n: u32) -> i64 {
        let k = (self.interval * n) as i64;
        match self.freq {
            Freq::Daily => start + k * DAY,
            Freq::Weekly => start + k * 7 * DAY,
            Freq::Monthly => add_months(start, k),
            Freq::Yearly => add_months(start, k * 12),
        }
    }
}

/// Tel `months` kalendermaanden bij een tijdstip op (behoudt tijd-van-de-dag,
/// klemt de dag op het maand-einde, bv. 31 jan + 1 maand → 28/29 feb).
pub fn add_months(epoch: i64, months: i64) -> i64 {
    let day_secs = epoch.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(epoch.div_euclid(DAY));
    let total = (y * 12 + (m - 1)) + months;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let dim = days_in_month(ny, nm);
    let nd = d.min(dim);
    days_from_civil(ny, nm, nd) * DAY + day_secs
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

// ── Afspraak & agenda ──

/// Een afspraak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub title: String,
    pub start: i64,
    pub duration: i64,
    pub location: String,
    pub recur: Option<Recurrence>,
    /// Herinnering: zoveel minuten vóór de start; `None` = geen.
    pub reminder_min: Option<u32>,
}

impl Event {
    pub fn new(title: &str, start: i64, duration: i64) -> Self {
        Event {
            title: title.to_string(),
            start,
            duration,
            location: String::new(),
            recur: None,
            reminder_min: None,
        }
    }
    pub fn recurring(mut self, r: Recurrence) -> Self {
        self.recur = Some(r);
        self
    }
    pub fn remind(mut self, minutes: u32) -> Self {
        self.reminder_min = Some(minutes);
        self
    }

    /// Alle starttijden in `[from, to)` (begrensd, herhaling uitgevouwen).
    pub fn occurrences(&self, from: i64, to: i64) -> Vec<i64> {
        let mut out = Vec::new();
        match self.recur {
            None => {
                if self.start >= from && self.start < to {
                    out.push(self.start);
                }
            }
            Some(r) => {
                let mut n = 0u32;
                loop {
                    if r.count != 0 && n >= r.count {
                        break;
                    }
                    let t = r.nth(self.start, n);
                    if r.until != 0 && t > r.until {
                        break;
                    }
                    if t >= to {
                        break;
                    }
                    if t >= from {
                        out.push(t);
                    }
                    n += 1;
                    if n > 100_000 {
                        break; // veiligheidsplafond
                    }
                }
            }
        }
        out
    }

    /// Vuurt de herinnering in `(prev, now]`?
    pub fn reminder_due(&self, prev: i64, now: i64) -> bool {
        let lead = match self.reminder_min {
            Some(m) => m as i64 * 60,
            None => return false,
        };
        // Zoek voorkomens waarvan (start - lead) in (prev, now] valt.
        let window_from = prev + lead;
        let window_to = now + lead;
        self.occurrences(window_from, window_to + 1)
            .iter()
            .any(|&s| {
                let trigger = s - lead;
                trigger > prev && trigger <= now
            })
    }
}

/// Een agenda = een verzameling afspraken.
#[derive(Debug, Clone, Default)]
pub struct Calendar {
    pub events: Vec<Event>,
}

impl Calendar {
    pub fn new() -> Self {
        Calendar::default()
    }
    pub fn add(&mut self, e: Event) {
        self.events.push(e);
    }

    /// Alle (afspraak, starttijd)-paren op de dag van `epoch`, op tijd gesorteerd.
    pub fn on_day(&self, epoch: i64) -> Vec<(&Event, i64)> {
        let from = day_start(epoch);
        let to = from + DAY;
        let mut out = Vec::new();
        for e in &self.events {
            for s in e.occurrences(from, to) {
                out.push((e, s));
            }
        }
        out.sort_by_key(|(_, s)| *s);
        out
    }

    /// De eerstvolgende afspraak na `now` (binnen het zoekvenster `horizon` seconden).
    pub fn next_after(&self, now: i64, horizon: i64) -> Option<(&Event, i64)> {
        let mut best: Option<(&Event, i64)> = None;
        for e in &self.events {
            for s in e.occurrences(now + 1, now + horizon) {
                if best.map(|(_, bs)| s < bs).unwrap_or(true) {
                    best = Some((e, s));
                }
            }
        }
        best
    }

    /// Afspraken waarvan de herinnering in `(prev, now]` afgaat.
    pub fn reminders_due(&self, prev: i64, now: i64) -> Vec<&Event> {
        self.events.iter().filter(|e| e.reminder_due(prev, now)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_roundtrip() {
        // 1970-01-01 = dag 0.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-02-29 bestaat (schrikkeljaar).
        let d = days_from_civil(2000, 2, 29);
        assert_eq!(civil_from_days(d), (2000, 2, 29));
        // 2026-06-06.
        let d2 = days_from_civil(2026, 6, 6);
        assert_eq!(civil_from_days(d2), (2026, 6, 6));
    }

    #[test]
    fn weekday_and_week_start() {
        // 1970-01-01 was donderdag → mon0 = 3.
        assert_eq!(weekday_mon0(0), 3);
        // 2026-06-06 is een zaterdag → mon0 = 5.
        let sat = days_from_civil(2026, 6, 6) * DAY;
        assert_eq!(weekday_mon0(sat), 5);
        // Week begint op maandag 2026-06-01.
        let ws = week_start(sat);
        assert_eq!(civil_from_days(ws / DAY), (2026, 6, 1));
        assert_eq!(weekday_mon0(ws), 0);
    }

    #[test]
    fn add_months_clamps_end_of_month() {
        // 31 jan 2026 + 1 maand → 28 feb 2026 (geen schrikkeljaar).
        let jan31 = days_from_civil(2026, 1, 31) * DAY + 12 * 3600;
        let feb = add_months(jan31, 1);
        let (y, m, d) = civil_from_days(feb / DAY);
        assert_eq!((y, m, d), (2026, 2, 28));
        assert_eq!(feb.rem_euclid(DAY), 12 * 3600); // tijd-van-de-dag behouden
    }

    #[test]
    fn non_recurring_occurrence() {
        let e = Event::new("Vergadering", 1000, 3600);
        assert_eq!(e.occurrences(0, 2000), alloc::vec![1000]);
        assert_eq!(e.occurrences(2000, 5000), Vec::<i64>::new());
    }

    #[test]
    fn weekly_recurrence_with_count() {
        let e = Event::new("Standup", 0, 900).recurring(Recurrence::new(Freq::Weekly, 1).count(3));
        let occ = e.occurrences(0, 100 * DAY);
        assert_eq!(occ, alloc::vec![0, 7 * DAY, 14 * DAY]); // precies 3
    }

    #[test]
    fn daily_recurrence_until() {
        let e = Event::new("Pillen", 8 * 3600, 60)
            .recurring(Recurrence::new(Freq::Daily, 1).until(3 * DAY));
        let occ = e.occurrences(0, 30 * DAY);
        // 8:00 op dag 0,1,2,3 (until = 3*DAY < dag3 8:00? 3*DAY=259200, dag3 8:00=3*DAY+28800 > until → uit).
        assert_eq!(occ, alloc::vec![8 * 3600, DAY + 8 * 3600, 2 * DAY + 8 * 3600]);
    }

    #[test]
    fn monthly_recurrence() {
        // 15e van de maand, jan 2026, 3×.
        let start = days_from_civil(2026, 1, 15) * DAY;
        let e = Event::new("Huur", start, 0).recurring(Recurrence::new(Freq::Monthly, 1).count(3));
        let occ = e.occurrences(start, days_from_civil(2027, 1, 1) * DAY);
        let dates: Vec<(i64, i64, i64)> = occ.iter().map(|&t| civil_from_days(t / DAY)).collect();
        assert_eq!(dates, alloc::vec![(2026, 1, 15), (2026, 2, 15), (2026, 3, 15)]);
    }

    #[test]
    fn calendar_on_day_sorted() {
        let mut c = Calendar::new();
        let base = days_from_civil(2026, 6, 6) * DAY;
        c.add(Event::new("Lunch", base + 12 * 3600, 3600));
        c.add(Event::new("Ochtend", base + 9 * 3600, 1800));
        let day = c.on_day(base + 15 * 3600);
        assert_eq!(day.len(), 2);
        assert_eq!(day[0].0.title, "Ochtend"); // op tijd gesorteerd
        assert_eq!(day[1].0.title, "Lunch");
    }

    #[test]
    fn reminders_fire_in_window() {
        let mut c = Calendar::new();
        c.add(Event::new("Call", 10_000, 600).remind(15)); // trigger op 10000-900=9100
        assert!(c.reminders_due(9000, 9200).len() == 1);
        assert!(c.reminders_due(9200, 9500).is_empty());
    }

    #[test]
    fn next_after_picks_earliest() {
        let mut c = Calendar::new();
        c.add(Event::new("Laat", 5000, 0));
        c.add(Event::new("Vroeg", 2000, 0));
        let (e, s) = c.next_after(0, 10_000).unwrap();
        assert_eq!(e.title, "Vroeg");
        assert_eq!(s, 2000);
    }
}
