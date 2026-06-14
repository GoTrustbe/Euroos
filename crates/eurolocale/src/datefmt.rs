//! Date formatting per language: the numeric pattern (day-month-year vs year-month-day)
//! and the long form with month names.

use crate::lang::{DatePattern, Lang};
use alloc::string::String;

/// Format a date in the language's short numeric form.
/// `2026-12-31` → nl `"31-12-2026"`, en `"31/12/2026"`, sv `"2026-12-31"`.
pub fn format_short(lang: Lang, year: i32, month: u8, day: u8) -> String {
    let sep = short_separator(lang);
    match lang.date_pattern() {
        DatePattern::Dmy => {
            alloc::format!("{day:02}{sep}{month:02}{sep}{year:04}")
        }
        DatePattern::Ymd => {
            alloc::format!("{year:04}{sep}{month:02}{sep}{day:02}")
        }
    }
}

/// The separator in the short date form.
fn short_separator(lang: Lang) -> char {
    use Lang::*;
    match lang {
        Nl | Da | Fi | De => '-',  // 31-12-2026
        Sv | Lt | Hu => '-',       // ISO-like 2026-12-31
        _ => '/',                  // 31/12/2026 (en/fr/es/it/…)
    }
}

/// Format a date in the long form with month name, where available.
/// Falls back to the short form for languages without a built-in month table.
pub fn format_long(lang: Lang, year: i32, month: u8, day: u8) -> String {
    let Some(name) = month_name(lang, month) else {
        return format_short(lang, year, month, day);
    };
    match lang.date_pattern() {
        DatePattern::Ymd => alloc::format!("{year} {name} {day}"),
        DatePattern::Dmy => alloc::format!("{day} {name} {year}"),
    }
}

/// The month name (1–12) in the language, or `None` if the language has no table (yet).
pub fn month_name(lang: Lang, month: u8) -> Option<&'static str> {
    use Lang::*;
    let idx = month.checked_sub(1)? as usize;
    if idx >= 12 {
        return None;
    }
    let table: [&str; 12] = match lang {
        Nl => ["januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september", "oktober", "november", "december"],
        En => ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
        De => ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"],
        Fr => ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"],
        Es => ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"],
        It => ["gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto", "settembre", "ottobre", "novembre", "dicembre"],
        Pt => ["janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro", "novembro", "dezembro"],
        Sv => ["januari", "februari", "mars", "april", "maj", "juni", "juli", "augusti", "september", "oktober", "november", "december"],
        _ => return None,
    };
    Some(table[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_forms() {
        assert_eq!(format_short(Lang::Nl, 2026, 12, 31), "31-12-2026");
        assert_eq!(format_short(Lang::En, 2026, 1, 5), "05/01/2026");
        assert_eq!(format_short(Lang::Sv, 2026, 12, 31), "2026-12-31");
        assert_eq!(format_short(Lang::Fr, 2026, 3, 9), "09/03/2026");
    }

    #[test]
    fn long_forms() {
        assert_eq!(format_long(Lang::Nl, 2026, 6, 6), "6 juni 2026");
        assert_eq!(format_long(Lang::En, 2026, 6, 6), "6 June 2026");
        assert_eq!(format_long(Lang::De, 2026, 3, 1), "1 März 2026");
        assert_eq!(format_long(Lang::Sv, 2026, 5, 17), "2026 maj 17");
    }

    #[test]
    fn long_falls_back_when_no_table() {
        // Bulgarian has no month table (yet) → short form.
        assert_eq!(format_long(Lang::Bg, 2026, 12, 31), "31/12/2026");
    }
}
