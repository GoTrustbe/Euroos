//! RTC/CMOS driver (Run 3 / doc §16.1) — REAL wall-clock time from the CMOS registers
//! (ports 0x70/0x71). Replaces the hardcoded "Mon 1 June" + the boot-tick clock
//! with the actual date/time, so the status panel shows the real time live.

use x86_64::instructions::port::Port;

#[derive(Clone, Copy)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub min: u8,
    pub sec: u8,
}

unsafe fn cmos_read(reg: u8) -> u8 {
    let mut addr = Port::<u8>::new(0x70);
    let mut data = Port::<u8>::new(0x71);
    addr.write(reg);
    data.read()
}

fn update_in_progress() -> bool {
    unsafe { cmos_read(0x0A) & 0x80 != 0 }
}

fn bcd_to_bin(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

/// Read the current date/time from the RTC. Reads repeatedly until two consecutive
/// readings are identical (prevents a value that changes in the middle of a tick).
pub fn now() -> DateTime {
    unsafe {
        let raw = || {
            // Wait until no update is in progress — with a safety valve against an
            // RTC that never clears the update-in-progress bit (would otherwise hang).
            let mut g = 0u32;
            while update_in_progress() {
                g += 1;
                if g > 500_000 {
                    break;
                }
            }
            (
                cmos_read(0x00),
                cmos_read(0x02),
                cmos_read(0x04),
                cmos_read(0x07),
                cmos_read(0x08),
                cmos_read(0x09),
            )
        };
        let mut last = raw();
        let mut tries = 0;
        loop {
            let cur = raw();
            if cur == last || tries > 10 {
                break;
            }
            last = cur;
            tries += 1;
        }
        let (mut sec, mut min, mut hour_raw, mut day, mut month, mut year) = last;
        let regb = cmos_read(0x0B);
        let pm = hour_raw & 0x80 != 0;
        let mut hour = hour_raw & 0x7F;
        if regb & 0x04 == 0 {
            // BCD mode → binary.
            sec = bcd_to_bin(sec);
            min = bcd_to_bin(min);
            hour = bcd_to_bin(hour);
            day = bcd_to_bin(day);
            month = bcd_to_bin(month);
            year = bcd_to_bin(year);
        }
        let _ = &mut hour_raw;
        // 12-hour → 24-hour.
        if regb & 0x02 == 0 {
            if pm && hour != 12 {
                hour += 12;
            } else if !pm && hour == 12 {
                hour = 0;
            }
        }
        DateTime { year: year as u16 + 2000, month, day, hour, min, sec }
    }
}

/// Unix time (seconds since 1970-01-01 UTC) from the RTC — the REAL wall clock for
/// `clock_gettime(CLOCK_REALTIME)` and `gettimeofday` in the Linux compat layer, so
/// time-aware Linux programs see the actual time instead of the boot uptime.
pub fn epoch() -> u64 {
    let d = now();
    fn is_leap(y: u16) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }
    let mut days: u64 = 0;
    let mut y = 1970u16;
    while y < d.year {
        days += if is_leap(y) { 366 } else { 365 };
        y += 1;
    }
    const MDAYS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let m = (d.month.max(1) as usize - 1).min(11);
    for (i, &md) in MDAYS.iter().enumerate().take(m) {
        days += md;
        if i == 1 && is_leap(d.year) {
            days += 1; // leap day in February
        }
    }
    days += d.day.saturating_sub(1) as u64;
    days * 86_400 + d.hour as u64 * 3600 + d.min as u64 * 60 + d.sec as u64
}

/// Day of the week (0 = Sunday) via Sakamoto's algorithm.
pub fn weekday(dt: &DateTime) -> u8 {
    let t = [0u32, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = dt.year as u32;
    if dt.month < 3 {
        y -= 1;
    }
    let idx = (dt.month.max(1) as usize - 1).min(11);
    ((y + y / 4 - y / 100 + y / 400 + t[idx] + dt.day as u32) % 7) as u8
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August",
    "September", "October", "November", "December",
];

/// "HH:MM" (24-hour) from the RTC.
pub fn clock_string() -> alloc::string::String {
    let d = now();
    alloc::format!("{:02}:{:02}", d.hour, d.min)
}

/// "Wed 2 June" — real date line for the status panel.
pub fn date_string() -> alloc::string::String {
    let d = now();
    let m = MONTHS[(d.month.max(1) as usize - 1).min(11)];
    alloc::format!("{} {} {}", DAYS[weekday(&d) as usize % 7], d.day, m)
}
