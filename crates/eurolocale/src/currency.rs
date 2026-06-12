//! Valuta-opmaak per taal: het juiste symbool, de juiste plaatsing en de
//! taal-eigen scheidingstekens. Eurozone-talen krijgen €, de zeven EU-talen met
//! een eigen munt krijgen die (BGN/CZK/DKK/HUF/PLN/RON/SEK).

use crate::lang::Lang;
use crate::number::format_fixed;
use alloc::string::String;

/// Het valutasymbool van de hoofdregio van een taal.
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
        _ => "€",      // alle eurozone-talen (vangnet)
    }
}

/// De ISO-4217-valutacode van de hoofdregio van een taal.
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

/// Formatteer een geldbedrag in minor units (centen) met 2 decimalen.
/// `1234,56 €` (nl) vs `€1,234.56` (en) vs `1 234,56 €` (fr).
pub fn format_minor(lang: Lang, minor: i64) -> String {
    format_amount(lang, minor, 2)
}

/// Formatteer een geldbedrag (`scaled` = bedrag × 10^`frac`) met valutasymbool.
pub fn format_amount(lang: Lang, scaled: i64, frac: u32) -> String {
    let num = format_fixed(lang, scaled, frac);
    let sym = symbol(lang);
    let mut s = String::new();
    if lang.currency_before() {
        s.push_str(sym);
        s.push_str(&num);
    } else {
        s.push_str(&num);
        s.push('\u{00A0}'); // vaste spatie tussen bedrag en symbool
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
        assert_eq!(symbol(Lang::Hr), "€"); // Kroatië sinds 2023
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
        // Nederlands: symbool vóór, komma-decimaal, punt-groepering.
        assert_eq!(format_minor(Lang::Nl, 123456), "€1.234,56");
        // Engels: symbool vóór, punt-decimaal, komma-groepering.
        assert_eq!(format_minor(Lang::En, 123456), "€1,234.56");
        // Duits: symbool erna (met vaste spatie).
        assert_eq!(nbsp(&format_minor(Lang::De, 123456)), "1.234,56 €");
        // Frans: symbool erna, spatie-groepering.
        assert_eq!(nbsp(&format_minor(Lang::Fr, 123456)), "1 234,56 €");
        // Zweeds: krona erna.
        assert_eq!(nbsp(&format_minor(Lang::Sv, 9900)), "99,00 kr");
    }
}
