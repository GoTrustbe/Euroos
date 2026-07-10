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

/// The month name (1–12) in the language. Now covers **all 24 EU official
/// languages** (stand-alone/nominative wide form), from CLDR. `None` only for an
/// out-of-range month. Slavic genitive date forms (e.g. Cs "31. ledna") are a
/// separate formatting concern; this returns the nominative month name.
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
        Bg => ["януари", "февруари", "март", "април", "май", "юни", "юли", "август", "септември", "октомври", "ноември", "декември"],
        Hr => ["siječanj", "veljača", "ožujak", "travanj", "svibanj", "lipanj", "srpanj", "kolovoz", "rujan", "listopad", "studeni", "prosinac"],
        Cs => ["leden", "únor", "březen", "duben", "květen", "červen", "červenec", "srpen", "září", "říjen", "listopad", "prosinec"],
        Da => ["januar", "februar", "marts", "april", "maj", "juni", "juli", "august", "september", "oktober", "november", "december"],
        Et => ["jaanuar", "veebruar", "märts", "aprill", "mai", "juuni", "juuli", "august", "september", "oktoober", "november", "detsember"],
        Fi => ["tammikuu", "helmikuu", "maaliskuu", "huhtikuu", "toukokuu", "kesäkuu", "heinäkuu", "elokuu", "syyskuu", "lokakuu", "marraskuu", "joulukuu"],
        El => ["Ιανουάριος", "Φεβρουάριος", "Μάρτιος", "Απρίλιος", "Μάιος", "Ιούνιος", "Ιούλιος", "Αύγουστος", "Σεπτέμβριος", "Οκτώβριος", "Νοέμβριος", "Δεκέμβριος"],
        Hu => ["január", "február", "március", "április", "május", "június", "július", "augusztus", "szeptember", "október", "november", "december"],
        Ga => ["Eanáir", "Feabhra", "Márta", "Aibreán", "Bealtaine", "Meitheamh", "Iúil", "Lúnasa", "Meán Fómhair", "Deireadh Fómhair", "Samhain", "Nollaig"],
        Lv => ["janvāris", "februāris", "marts", "aprīlis", "maijs", "jūnijs", "jūlijs", "augusts", "septembris", "oktobris", "novembris", "decembris"],
        Lt => ["sausis", "vasaris", "kovas", "balandis", "gegužė", "birželis", "liepa", "rugpjūtis", "rugsėjis", "spalis", "lapkritis", "gruodis"],
        Mt => ["Jannar", "Frar", "Marzu", "April", "Mejju", "Ġunju", "Lulju", "Awwissu", "Settembru", "Ottubru", "Novembru", "Diċembru"],
        Pl => ["styczeń", "luty", "marzec", "kwiecień", "maj", "czerwiec", "lipiec", "sierpień", "wrzesień", "październik", "listopad", "grudzień"],
        Ro => ["ianuarie", "februarie", "martie", "aprilie", "mai", "iunie", "iulie", "august", "septembrie", "octombrie", "noiembrie", "decembrie"],
        Sk => ["január", "február", "marec", "apríl", "máj", "jún", "júl", "august", "september", "október", "november", "december"],
        Sl => ["januar", "februar", "marec", "april", "maj", "junij", "julij", "avgust", "september", "oktober", "november", "december"],
    };
    Some(table[idx])
}

/// The weekday name for `dow` (0 = Monday … 6 = Sunday), in the language.
/// Covers all 24 EU official languages (CLDR stand-alone wide). `None` for an
/// out-of-range index.
pub fn weekday_name(lang: Lang, dow: u8) -> Option<&'static str> {
    use Lang::*;
    let idx = dow as usize;
    if idx >= 7 {
        return None;
    }
    let table: [&str; 7] = match lang {
        Nl => ["maandag", "dinsdag", "woensdag", "donderdag", "vrijdag", "zaterdag", "zondag"],
        En => ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
        De => ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"],
        Fr => ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"],
        Es => ["lunes", "martes", "miércoles", "jueves", "viernes", "sábado", "domingo"],
        It => ["lunedì", "martedì", "mercoledì", "giovedì", "venerdì", "sabato", "domenica"],
        Pt => ["segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado", "domingo"],
        Sv => ["måndag", "tisdag", "onsdag", "torsdag", "fredag", "lördag", "söndag"],
        Bg => ["понеделник", "вторник", "сряда", "четвъртък", "петък", "събота", "неделя"],
        Hr => ["ponedjeljak", "utorak", "srijeda", "četvrtak", "petak", "subota", "nedjelja"],
        Cs => ["pondělí", "úterý", "středa", "čtvrtek", "pátek", "sobota", "neděle"],
        Da => ["mandag", "tirsdag", "onsdag", "torsdag", "fredag", "lørdag", "søndag"],
        Et => ["esmaspäev", "teisipäev", "kolmapäev", "neljapäev", "reede", "laupäev", "pühapäev"],
        Fi => ["maanantai", "tiistai", "keskiviikko", "torstai", "perjantai", "lauantai", "sunnuntai"],
        El => ["Δευτέρα", "Τρίτη", "Τετάρτη", "Πέμπτη", "Παρασκευή", "Σάββατο", "Κυριακή"],
        Hu => ["hétfő", "kedd", "szerda", "csütörtök", "péntek", "szombat", "vasárnap"],
        Ga => ["Dé Luain", "Dé Máirt", "Dé Céadaoin", "Déardaoin", "Dé hAoine", "Dé Sathairn", "Dé Domhnaigh"],
        Lv => ["pirmdiena", "otrdiena", "trešdiena", "ceturtdiena", "piektdiena", "sestdiena", "svētdiena"],
        Lt => ["pirmadienis", "antradienis", "trečiadienis", "ketvirtadienis", "penktadienis", "šeštadienis", "sekmadienis"],
        Mt => ["It-Tnejn", "It-Tlieta", "L-Erbgħa", "Il-Ħamis", "Il-Ġimgħa", "Is-Sibt", "Il-Ħadd"],
        Pl => ["poniedziałek", "wtorek", "środa", "czwartek", "piątek", "sobota", "niedziela"],
        Ro => ["luni", "marți", "miercuri", "joi", "vineri", "sâmbătă", "duminică"],
        Sk => ["pondelok", "utorok", "streda", "štvrtok", "piatok", "sobota", "nedeľa"],
        Sl => ["ponedeljek", "torek", "sreda", "četrtek", "petek", "sobota", "nedelja"],
    };
    Some(table[idx])
}

/// Zeller-style day-of-week for a proleptic Gregorian date (0 = Monday … 6 =
/// Sunday), so `weekday_name` can be driven from a calendar date.
pub fn day_of_week(year: i32, month: u8, day: u8) -> u8 {
    // Sakamoto's algorithm (returns 0=Sunday); shift to 0=Monday.
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = year;
    if month < 3 {
        y -= 1;
    }
    let m = month as i32;
    let d = day as i32;
    let sun0 = (y + y / 4 - y / 100 + y / 400 + t[(m - 1) as usize] + d) % 7;
    ((sun0 + 6) % 7) as u8
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
    fn all_24_languages_have_month_and_weekday_names() {
        // 3F-4: every EU official language now has a complete month + weekday
        // table (no more numeric fallback). CLDR-sourced.
        for lang in Lang::ALL {
            for m in 1..=12u8 {
                assert!(month_name(lang, m).is_some(), "{lang:?} missing month {m}");
            }
            for d in 0..7u8 {
                assert!(weekday_name(lang, d).is_some(), "{lang:?} missing weekday {d}");
            }
        }
        // Out-of-range guarded.
        assert!(month_name(Lang::En, 0).is_none());
        assert!(month_name(Lang::En, 13).is_none());
        assert!(weekday_name(Lang::En, 7).is_none());
    }

    #[test]
    fn cldr_spot_checks_for_the_new_languages() {
        assert_eq!(month_name(Lang::Bg, 12), Some("декември"));
        assert_eq!(month_name(Lang::Pl, 1), Some("styczeń"));
        assert_eq!(month_name(Lang::El, 3), Some("Μάρτιος"));
        assert_eq!(month_name(Lang::Ga, 5), Some("Bealtaine"));
        assert_eq!(weekday_name(Lang::Fr, 0), Some("lundi"));
        assert_eq!(weekday_name(Lang::Pt, 0), Some("segunda-feira"));
        assert_eq!(weekday_name(Lang::El, 6), Some("Κυριακή"));
    }

    #[test]
    fn day_of_week_is_correct() {
        // 2026-06-06 is a Saturday (dow 5); 2000-01-01 was a Saturday.
        assert_eq!(day_of_week(2026, 6, 6), 5);
        assert_eq!(weekday_name(Lang::En, day_of_week(2026, 6, 6)), Some("Saturday"));
        assert_eq!(day_of_week(2000, 1, 1), 5);
        // 2026-12-31 is a Thursday (dow 3).
        assert_eq!(weekday_name(Lang::En, day_of_week(2026, 12, 31)), Some("Thursday"));
    }

    #[test]
    fn long_form_now_uses_names_for_every_language() {
        // Bulgarian used to fall back to numeric; now it names the month.
        assert_eq!(format_long(Lang::Bg, 2026, 12, 31), "31 декември 2026");
    }
}
