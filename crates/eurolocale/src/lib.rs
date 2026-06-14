//! EuroLocale — sovereign localization for the 24 official EU languages (plan P1).
//!
//! An OS for Europe must speak every EU language, not just English. This crate
//! provides the CLDR core, `no_std` and fully host-tested, without external data blobs:
//! - [`lang`]      — the 24 languages + their rules (separators, date pattern, …);
//! - [`number`]    — number formatting (grouping + decimal separator per language);
//! - [`currency`]  — currency formatting (€ or the local currency, correct placement);
//! - [`datefmt`]   — date formatting (short numeric + long with month names);
//! - [`plural`]    — CLDR plural rules (one/two/few/many/other per language);
//! - [`collation`] — language-aware sort order (diacritics + language tailoring).
//!
//! The [`Locale`] bundles a language and offers the formatting as methods.

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

/// A bound locale: a language + the formatting API on it.
#[derive(Clone, Copy)]
pub struct Locale {
    pub lang: Lang,
}

impl Locale {
    /// Create a locale from a language.
    pub fn new(lang: Lang) -> Self {
        Locale { lang }
    }

    /// Parse a BCP-47-like tag (`"nl-BE"`, `"de_DE"`) → locale.
    pub fn parse(tag: &str) -> Option<Locale> {
        Lang::parse(tag).map(Locale::new)
    }

    /// An integer, grouped per language.
    pub fn int(&self, value: i64) -> String {
        number::format_int(self.lang, value)
    }

    /// A monetary amount in cents (2 decimals), with currency symbol.
    pub fn money(&self, minor: i64) -> String {
        currency::format_minor(self.lang, minor)
    }

    /// The short numeric date.
    pub fn date(&self, year: i32, month: u8, day: u8) -> String {
        datefmt::format_short(self.lang, year, month, day)
    }

    /// The long date (with month name where available).
    pub fn date_long(&self, year: i32, month: u8, day: u8) -> String {
        datefmt::format_long(self.lang, year, month, day)
    }

    /// The plural category of `n`.
    pub fn plural(&self, n: u64) -> Plural {
        plural::category(self.lang, n)
    }

    /// Sort strings in the collation of this language.
    pub fn sort(&self, items: &mut [String]) {
        collation::sort(self.lang, items);
    }

    /// The ISO currency code of the main region.
    pub fn currency_code(&self) -> &'static str {
        currency::iso_code(self.lang)
    }
}

/// All 24 EU locales (handy for a language picker).
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
