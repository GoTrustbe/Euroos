//! Currency formatting per language: the correct symbol, the correct placement and the
//! language-specific separators. Eurozone languages get €, the seven EU languages with
//! their own currency get that one (BGN/CZK/DKK/HUF/PLN/RON/SEK).

use crate::lang::Lang;
use crate::number::format_fixed;
use alloc::string::String;

/// The currency symbol of a language's main region.
pub fn symbol(lang: Lang) -> &'static str {
    use Lang::*;
    if lang.is_eurozone() {
        return "€";
    }
    match lang {
        Bg => "лв.",   // lev
        Cs => "Kč",    // koruna
        Da => "kr.",   // krone
        Hu => "Ft",    // forint
        Pl => "zł",    // złoty
        Ro => "lei",   // leu
        Sv => "kr",    // krona
        _ => "€",      // all eurozone languages (fallback)
    }
}

/// The ISO 4217 currency code of a language's main region.
pub fn iso_code(lang: Lang) -> &'static str {
    use Lang::*;
    if lang.is_eurozone() {
        return "EUR";
    }
    match lang {
        Bg => "BGN", Cs => "CZK", Da => "DKK", Hu => "HUF",
        Pl => "PLN", Ro => "RON", Sv => "SEK",
        _ => "EUR",
    }
}

/// Format a monetary amount in minor units (cents) with 2 decimals.
/// `1234,56 €` (nl) vs `€1,234.56` (en) vs `1 234,56 €` (fr).
pub fn format_minor(lang: Lang, minor: i64) -> String {
    format_amount(lang, minor, 2)
}

/// Format a monetary amount (`scaled` = amount × 10^`frac`) with currency symbol.
pub fn format_amount(lang: Lang, scaled: i64, frac: u32) -> String {
    let num = format_fixed(lang, scaled, frac);
    let sym = symbol(lang);
    let mut s = String::new();
    if lang.currency_before() {
        s.push_str(sym);
        s.push_str(&num);
    } else {
        s.push_str(&num);
        s.push('\u{00A0}'); // non-breaking space between amount and symbol
        s.push_str(sym);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nbsp(s: &str) -> alloc::string::String {
        s.replace('\u{00A0}', " ")
    }

    #[test]
    fn eurozone_symbols() {
        assert_eq!(symbol(Lang::Nl), "€");
        assert_eq!(symbol(Lang::De), "€");
        assert_eq!(symbol(Lang::Hr), "€"); // Croatia since 2023
        assert_eq!(iso_code(Lang::Fr), "EUR");
    }

    #[test]
    fn non_euro_currencies() {
        assert_eq!(symbol(Lang::Sv), "kr");
        assert_eq!(iso_code(Lang::Pl), "PLN");
        assert_eq!(iso_code(Lang::Cs), "CZK");
        assert_eq!(iso_code(Lang::Da), "DKK");
    }

    #[test]
    fn placement_and_separators() {
        // Dutch: symbol before, comma decimal, dot grouping.
        assert_eq!(format_minor(Lang::Nl, 123456), "€1.234,56");
        // English: symbol before, dot decimal, comma grouping.
        assert_eq!(format_minor(Lang::En, 123456), "€1,234.56");
        // German: symbol after (with non-breaking space).
        assert_eq!(nbsp(&format_minor(Lang::De, 123456)), "1.234,56 €");
        // French: symbol after, space grouping.
        assert_eq!(nbsp(&format_minor(Lang::Fr, 123456)), "1 234,56 €");
        // Swedish: krona after.
        assert_eq!(nbsp(&format_minor(Lang::Sv, 9900)), "99,00 kr");
    }
}
