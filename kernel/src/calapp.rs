//! Boot-zelftest voor **EuroCalendar** (AC-3): afspraken, herhaling, herinneringen,
//! civiele-datumrekenkern. Kern: [`eurocalendar`].

use crate::serial_println;
use eurocalendar::{civil_from_days, days_from_civil, weekday_mon0, Calendar, Event, Freq, Recurrence};

const DAY: i64 = 86_400;

pub fn selftest() {
    // Datumrekenkern: round-trip + weekdag (2026-06-06 = zaterdag = mon0 5).
    let d = days_from_civil(2026, 6, 6);
    let date_ok = civil_from_days(d) == (2026, 6, 6) && weekday_mon0(d * DAY) == 5;

    // Wekelijkse herhaling, 3×.
    let e = Event::new("Standup", 0, 900).recurring(Recurrence::new(Freq::Weekly, 1).count(3));
    let weekly_ok = e.occurrences(0, 100 * DAY) == alloc::vec![0, 7 * DAY, 14 * DAY];

    // Maandelijkse herhaling met maand-einde-clamp (31 jan +1 → 28 feb).
    let jan31 = days_from_civil(2026, 1, 31) * DAY;
    let m = Event::new("Maand", jan31, 0).recurring(Recurrence::new(Freq::Monthly, 1).count(2));
    let occ = m.occurrences(jan31, days_from_civil(2026, 4, 1) * DAY);
    let monthly_ok = occ.len() == 2 && civil_from_days(occ[1] / DAY) == (2026, 2, 28);

    // Agenda: herinnering + eerstvolgende.
    let mut c = Calendar::new();
    c.add(Event::new("Call", 10_000, 600).remind(15)); // trigger op 9100
    c.add(Event::new("Later", 20_000, 0));
    let remind_ok = c.reminders_due(9000, 9200).len() == 1 && c.reminders_due(9200, 9500).is_empty();
    let next_ok = c.next_after(0, 100_000).map(|(e, s)| e.title == "Call" && s == 10_000).unwrap_or(false);

    let ok = date_ok && weekly_ok && monthly_ok && remind_ok && next_ok;
    serial_println!(
        "[cal] EuroCalendar: datum/weekdag={}, wekelijks×3={}, maandelijks(feb-clamp)={}, herinnering={}, eerstvolgende={} {}",
        date_ok, weekly_ok, monthly_ok, remind_ok, next_ok,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
