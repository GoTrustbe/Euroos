//! Boot self-test for **EuroCalendar** (AC-3): appointments, recurrence, reminders,
//! civil-date arithmetic core. Core: [`eurocalendar`].

use crate::serial_println;
use eurocalendar::{civil_from_days, days_from_civil, weekday_mon0, Calendar, Event, Freq, Recurrence};

const DAY: i64 = 86_400;

pub fn selftest() {
    // Date arithmetic core: round-trip + weekday (2026-06-06 = Saturday = mon0 5).
    let d = days_from_civil(2026, 6, 6);
    let date_ok = civil_from_days(d) == (2026, 6, 6) && weekday_mon0(d * DAY) == 5;

    // Weekly recurrence, 3x.
    let e = Event::new("Standup", 0, 900).recurring(Recurrence::new(Freq::Weekly, 1).count(3));
    let weekly_ok = e.occurrences(0, 100 * DAY) == alloc::vec![0, 7 * DAY, 14 * DAY];

    // Monthly recurrence with month-end clamp (31 Jan +1 -> 28 Feb).
    let jan31 = days_from_civil(2026, 1, 31) * DAY;
    let m = Event::new("Month", jan31, 0).recurring(Recurrence::new(Freq::Monthly, 1).count(2));
    let occ = m.occurrences(jan31, days_from_civil(2026, 4, 1) * DAY);
    let monthly_ok = occ.len() == 2 && civil_from_days(occ[1] / DAY) == (2026, 2, 28);

    // Calendar: reminder + next-upcoming.
    let mut c = Calendar::new();
    c.add(Event::new("Call", 10_000, 600).remind(15)); // triggers at 9100
    c.add(Event::new("Later", 20_000, 0));
    let remind_ok = c.reminders_due(9000, 9200).len() == 1 && c.reminders_due(9200, 9500).is_empty();
    let next_ok = c.next_after(0, 100_000).map(|(e, s)| e.title == "Call" && s == 10_000).unwrap_or(false);

    let ok = date_ok && weekly_ok && monthly_ok && remind_ok && next_ok;
    serial_println!(
        "[cal] EuroCalendar: date/weekday={}, weekly x3={}, monthly(feb-clamp)={}, reminder={}, next-upcoming={} {}",
        date_ok, weekly_ok, monthly_ok, remind_ok, next_ok,
        if ok { "✓" } else { "✗ ERROR" }
    );
}
