//! Time zones for the settings panel: the zones EU member states use, with the
//! EU summer-time rule (Directive 2000/84/EC): clocks go forward one hour at
//! 01:00 UTC on the last Sunday of March and back at 01:00 UTC on the last
//! Sunday of October.
//!
//! Honest scope: this is a fixed table plus that one rule, not the IANA
//! database. Historical changes and zones outside the table are not covered.

/// One selectable time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    /// Stable identifier stored in the settings file (an IANA name).
    pub id: &'static str,
    /// Name shown in the list.
    pub label: &'static str,
    /// Offset from UTC in winter, in minutes.
    pub std_offset_min: i32,
    /// Does the EU summer-time rule apply?
    pub eu_dst: bool,
}

/// The selectable zones. Index 0 is the default (UTC: the hardware clock as is).
pub const ZONES: [Zone; 5] = [
    Zone { id: "UTC", label: "UTC (no summer time)", std_offset_min: 0, eu_dst: false },
    Zone { id: "Atlantic/Azores", label: "Azores (UTC-1)", std_offset_min: -60, eu_dst: true },
    Zone { id: "Europe/Lisbon", label: "Western Europe: Lisbon, Dublin (UTC+0)", std_offset_min: 0, eu_dst: true },
    Zone { id: "Europe/Brussels", label: "Central Europe: Brussels, Paris, Berlin (UTC+1)", std_offset_min: 60, eu_dst: true },
    Zone { id: "Europe/Helsinki", label: "Eastern Europe: Helsinki, Athens, Sofia (UTC+2)", std_offset_min: 120, eu_dst: true },
];

/// Index of the zone with this id (ASCII case ignored).
pub fn index_of(id: &str) -> Option<usize> {
    ZONES.iter().position(|z| z.id.eq_ignore_ascii_case(id.trim()))
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days from 1970-01-01 to `y-m-d` (proleptic Gregorian; m 1..=12).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Howard Hinnant's algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch second of 01:00 UTC on the last Sunday of `month` in `year`.
fn last_sunday_0100(year: i64, month: u32) -> i64 {
    let last_day = match month {
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let days = days_from_civil(year, month, last_day);
    // 1970-01-01 was a Thursday: (days + 4) mod 7 = weekday with 0 = Sunday.
    let dow = (days + 4).rem_euclid(7);
    (days - dow) * 86_400 + 3600
}

/// Is EU summer time in effect at this UTC instant?
pub fn eu_summer_time(utc_epoch: u64) -> bool {
    let secs = utc_epoch.min(i64::MAX as u64 / 2) as i64;
    let (year, _, _) = civil_from_days(secs.div_euclid(86_400));
    secs >= last_sunday_0100(year, 3) && secs < last_sunday_0100(year, 10)
}

/// The zone's offset from UTC at this UTC instant, in minutes.
pub fn offset_minutes(zone: &Zone, utc_epoch: u64) -> i32 {
    zone.std_offset_min + if zone.eu_dst && eu_summer_time(utc_epoch) { 60 } else { 0 }
}

/// Local wall-clock seconds for a UTC instant (never below zero).
pub fn local_epoch(zone: &Zone, utc_epoch: u64) -> u64 {
    let off = offset_minutes(zone, utc_epoch) as i64 * 60;
    (utc_epoch as i64).saturating_add(off).max(0) as u64
}

/// `"UTC+1"`, `"UTC+2"`, `"UTC"`, `"UTC-1"` for an offset in minutes.
pub fn offset_label(offset_min: i32) -> alloc::string::String {
    let (h, m) = (offset_min.abs() / 60, offset_min.abs() % 60);
    let sign = if offset_min < 0 { '-' } else { '+' };
    match (offset_min, m) {
        (0, _) => alloc::string::String::from("UTC"),
        (_, 0) => alloc::format!("UTC{sign}{h}"),
        _ => alloc::format!("UTC{sign}{h}:{m:02}"),
    }
}

/// `"14:05"` or `"2:05 PM"`.
pub fn format_clock(hour: u32, min: u32, hour24: bool) -> alloc::string::String {
    if hour24 {
        return alloc::format!("{:02}:{:02}", hour % 24, min % 60);
    }
    let h = hour % 24;
    let h12 = match h % 12 {
        0 => 12,
        x => x,
    };
    alloc::format!("{}:{:02} {}", h12, min % 60, if h < 12 { "AM" } else { "PM" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, m: u32, d: u32, h: i64, min: i64) -> u64 {
        (days_from_civil(y, m, d) * 86_400 + h * 3600 + min * 60) as u64
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 10), 20_767);
        for days in [0i64, 59, 60, 365, 11_016, 19_782, 20_767, 30_000, 47_482] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn eu_summer_time_boundaries_2026() {
        // 2026: last Sunday of March = 29th, of October = 25th.
        assert!(!eu_summer_time(utc(2026, 3, 29, 0, 59)));
        assert!(eu_summer_time(utc(2026, 3, 29, 1, 0)));
        assert!(eu_summer_time(utc(2026, 7, 1, 12, 0)));
        assert!(eu_summer_time(utc(2026, 10, 25, 0, 59)));
        assert!(!eu_summer_time(utc(2026, 10, 25, 1, 0)));
        assert!(!eu_summer_time(utc(2026, 11, 10, 9, 0)));
        assert!(!eu_summer_time(utc(2026, 1, 1, 0, 0)));
    }

    #[test]
    fn eu_summer_time_other_years() {
        // 2024: 31 March / 27 October. 2025: 30 March / 26 October. 2027: 28 March / 31 October.
        assert!(!eu_summer_time(utc(2024, 3, 31, 0, 59)) && eu_summer_time(utc(2024, 3, 31, 1, 0)));
        assert!(eu_summer_time(utc(2024, 10, 27, 0, 59)) && !eu_summer_time(utc(2024, 10, 27, 1, 0)));
        assert!(!eu_summer_time(utc(2025, 3, 30, 0, 59)) && eu_summer_time(utc(2025, 3, 30, 1, 0)));
        assert!(eu_summer_time(utc(2027, 10, 31, 0, 59)) && !eu_summer_time(utc(2027, 10, 31, 1, 0)));
        assert!(!eu_summer_time(0));
        assert!(!eu_summer_time(u64::MAX) || eu_summer_time(u64::MAX)); // no overflow panic
    }

    #[test]
    fn zone_offsets() {
        let bru = &ZONES[index_of("europe/brussels").unwrap()];
        let hel = &ZONES[index_of("Europe/Helsinki").unwrap()];
        let lis = &ZONES[index_of("Europe/Lisbon").unwrap()];
        let azo = &ZONES[index_of("Atlantic/Azores").unwrap()];
        let winter = utc(2026, 11, 10, 9, 0);
        let summer = utc(2026, 7, 10, 9, 0);
        assert_eq!((offset_minutes(bru, winter), offset_minutes(bru, summer)), (60, 120));
        assert_eq!((offset_minutes(hel, winter), offset_minutes(hel, summer)), (120, 180));
        assert_eq!((offset_minutes(lis, winter), offset_minutes(lis, summer)), (0, 60));
        assert_eq!((offset_minutes(azo, winter), offset_minutes(azo, summer)), (-60, 0));
        assert_eq!(offset_minutes(&ZONES[0], summer), 0);
        assert_eq!(local_epoch(bru, winter), winter + 3600);
        assert_eq!(local_epoch(azo, 10), 0);
        assert_eq!(index_of("Mars/Olympus"), None);
        assert_eq!(ZONES[0].id, "UTC");
    }

    #[test]
    fn labels_and_clock_format() {
        assert_eq!(offset_label(0), "UTC");
        assert_eq!(offset_label(60), "UTC+1");
        assert_eq!(offset_label(-60), "UTC-1");
        assert_eq!(offset_label(330), "UTC+5:30");
        assert_eq!(format_clock(0, 5, true), "00:05");
        assert_eq!(format_clock(0, 5, false), "12:05 AM");
        assert_eq!(format_clock(12, 0, false), "12:00 PM");
        assert_eq!(format_clock(15, 42, false), "3:42 PM");
        assert_eq!(format_clock(23, 59, true), "23:59");
    }
}
