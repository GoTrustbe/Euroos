//! EuroLocale — soevereine lokalisatie voor de 24 officiële EU-talen (plan P1).
//!
//! Een OS voor Europa moet élke EU-taal spreken, niet alleen Engels. Dit crate
//! levert de CLDR-kern, `no_std` en volledig host-getest, zonder externe data-blobs:
//! - [`lang`]      — de 24 talen + hun regels (scheidingstekens, datumpatroon, …);
//! - [`number`]    — getalopmaak (groepering + decimaalteken per taal);
//! - [`currency`]  — valuta-opmaak (€ of de eigen munt, juiste plaatsing);
//! - [`datefmt`]   — datumopmaak (kort numeriek + lang met maandnamen);
//! - [`plural`]    — CLDR-meervoudsregels (one/two/few/many/other per taal);
//! - [`collation`] — taalbewuste sorteervolgorde (diacritiek + taal-tailoring).
//!
//! De [`Locale`] bundelt een taal en biedt de opmaak als methodes.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod collation;
pub mod currency;
pub mod datefmt;
pub mod lang;
pub mod number;
pub mod plural;

pub use lang::Lang;
pub use plural::Plural;

use alloc::string::String;
use alloc::vec::Vec;

/// Een gebonden locale: een taal + de opmaak-API erop.
#[derive(Clone, Copy)]
pub struct Locale {
    pub lang: Lang,
}

impl Locale {
    /// Maak een locale uit een taal.
    pub fn new(lang: Lang) -> Self {
        Locale { lang }
    }

    /// Parse een BCP-47-achtige tag (`"nl-BE"`, `"de_DE"`) → locale.
    pub fn parse(tag: &str) -> Option<Locale> {
        Lang::parse(tag).map(Locale::new)
    }

    /// Een geheel getal, gegroepeerd per taal.
    pub fn int(&self, value: i64) -> String {
        number::format_int(self.lang, value)
    }

    /// Een geldbedrag in centen (2 decimalen), met valutasymbool.
    pub fn money(&self, minor: i64) -> String {
        currency::format_minor(self.lang, minor)
    }

    /// De korte numerieke datum.
    pub fn date(&self, year: i32, month: u8, day: u8) -> String {
        datefmt::format_short(self.lang, year, month, day)
    }

    /// De lange datum (met maandnaam waar beschikbaar).
    pub fn date_long(&self, year: i32, month: u8, day: u8) -> String {
        datefmt::format_long(self.lang, year, month, day)
    }

    /// De meervoudscategorie van `n`.
    pub fn plural(&self, n: u64) -> Plural {
        plural::category(self.lang, n)
    }

    /// Sorteer strings in de collatie van deze taal.
    pub fn sort(&self, items: &mut [String]) {
        collation::sort(self.lang, items);
    }

    /// De ISO-valutacode van de hoofdregio.
    pub fn currency_code(&self) -> &'static str {
        currency::iso_code(self.lang)
    }
}

/// Alle 24 EU-locales (handig voor een taalkiezer).
pub fn all_locales() -> Vec<Locale> {
    Lang::ALL.iter().map(|&l| Locale::new(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_facade() {
        let nl = Locale::parse("nl-BE").unwrap();
        assert_eq!(nl.int(1234567), "1.234.567");
        assert_eq!(nl.money(123456), "€1.234,56");
        assert_eq!(nl.date(2026, 6, 6), "06-06-2026");
        assert_eq!(nl.date_long(2026, 6, 6), "6 juni 2026");
        assert_eq!(nl.plural(1), Plural::One);
        assert_eq!(nl.currency_code(), "EUR");
    }

    #[test]
    fn all_24() {
        assert_eq!(all_locales().len(), 24);
    }
}
