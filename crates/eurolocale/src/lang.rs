//! The 24 official languages of the European Union + their localization rules.
//!
//! Each language carries the CLDR core data that the rest of the crate uses:
//! separators (number/grouping), currency placement, date pattern and the
//! plural family. Sovereignty at the language level: EuroOS speaks every EU language,
//! not just English.

/// An EU language (ISO 639-1). The 24 official languages of the Union.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Bg, // Bulgarian
    Hr, // Croatian
    Cs, // Czech
    Da, // Danish
    Nl, // Dutch
    En, // English
    Et, // Estonian
    Fi, // Finnish
    Fr, // French
    De, // German
    El, // Greek
    Hu, // Hungarian
    Ga, // Irish
    It, // Italian
    Lv, // Latvian
    Lt, // Lithuanian
    Mt, // Maltese
    Pl, // Polish
    Pt, // Portuguese
    Ro, // Romanian
    Sk, // Slovak
    Sl, // Slovenian
    Es, // Spanish
    Sv, // Swedish
}

/// The decimal and grouping separator of a language.
pub struct Separators {
    pub decimal: char,
    pub group: char,
}

/// The plural system (CLDR categories the language distinguishes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PluralSystem {
    /// No distinction (e.g. Hungarian, Turkish-style): always `other`.
    None,
    /// English-style: `one` (n=1) vs `other`.
    OneOther,
    /// French-style: `one` (n=0,1) vs `other`.
    FrenchOneOther,
    /// West Slavic (Czech/Slovak): one / few (2-4) / many (fractions) / other.
    WestSlavic,
    /// Polish: one / few / many.
    Polish,
    /// Croatian (BCS): one / few / other.
    Croatian,
    /// Latvian: zero / one / other.
    Latvian,
    /// Lithuanian: one / few / many.
    Lithuanian,
    /// Slovenian: one (n%100==1) / two (==2) / few (3,4) / other.
    Slovenian,
    /// Irish: one / two / few / many / other.
    Irish,
    /// Maltese: one / few / many / other.
    Maltese,
    /// Romanian: one / few / other.
    Romanian,
}

impl Lang {
    /// All 24 languages (fixed order).
    pub const ALL: [Lang; 24] = {
        use Lang::*;
        [Bg, Hr, Cs, Da, Nl, En, Et, Fi, Fr, De, El, Hu, Ga, It, Lv, Lt, Mt, Pl, Pt, Ro, Sk, Sl, Es, Sv]
    };

    /// The ISO 639-1 code (`"nl"`, `"de"`, …).
    pub fn code(self) -> &'static str {
        use Lang::*;
        match self {
            Bg => "bg", Hr => "hr", Cs => "cs", Da => "da", Nl => "nl", En => "en",
            Et => "et", Fi => "fi", Fr => "fr", De => "de", El => "el", Hu => "hu",
            Ga => "ga", It => "it", Lv => "lv", Lt => "lt", Mt => "mt", Pl => "pl",
            Pt => "pt", Ro => "ro", Sk => "sk", Sl => "sl", Es => "es", Sv => "sv",
        }
    }

    /// The proper name of the language (endonym).
    pub fn native_name(self) -> &'static str {
        use Lang::*;
        match self {
            Bg => "български", Hr => "hrvatski", Cs => "čeština", Da => "dansk",
            Nl => "Nederlands", En => "English", Et => "eesti", Fi => "suomi",
            Fr => "français", De => "Deutsch", El => "ελληνικά", Hu => "magyar",
            Ga => "Gaeilge", It => "italiano", Lv => "latviešu", Lt => "lietuvių",
            Mt => "Malti", Pl => "polski", Pt => "português", Ro => "română",
            Sk => "slovenčina", Sl => "slovenščina", Es => "español", Sv => "svenska",
        }
    }

    /// Parse a language tag such as `"nl"`, `"nl-BE"`, `"de_DE"` → the language (region ignored).
    pub fn parse(tag: &str) -> Option<Lang> {
        let lang = tag
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == lang)
    }

    /// Does this language (in its main region) use the euro?
    pub fn is_eurozone(self) -> bool {
        use Lang::*;
        // Eurozone languages (incl. Croatia since 2023, and English = Ireland/Malta);
        // the non-euro EU currencies (BGN/CZK/DKK/HUF/PLN/RON/SEK) are in [`crate::currency`].
        matches!(
            self,
            En | Nl | De | Fr | It | Es | Pt | Fi | El | Ga | Mt | Sk | Sl | Et | Lv | Lt | Hr
        )
    }

    /// Decimal and grouping separator.
    pub fn separators(self) -> Separators {
        use Lang::*;
        // English/Irish/Maltese: dot decimal, comma grouping.
        // Most continental languages: comma decimal, dot or space grouping.
        match self {
            En | Ga | Mt => Separators { decimal: '.', group: ',' },
            // Space grouping (narrow no-break space simplified to a regular space).
            Fr | Cs | Sk | Pl | Hu | Sv | Fi | Et | Lv | Lt | Bg => {
                Separators { decimal: ',', group: ' ' }
            }
            // Swiss German uses ', but EU German (de-DE) uses dot grouping.
            De | Nl | It | Es | Pt | Da | El | Hr | Ro | Sl => {
                Separators { decimal: ',', group: '.' }
            }
        }
    }

    /// Is the currency symbol placed before the amount? (otherwise after)
    pub fn currency_before(self) -> bool {
        use Lang::*;
        // English/Irish/Maltese/Dutch: symbol before ("€1,00"); most others after ("1,00 €").
        matches!(self, En | Ga | Mt | Nl)
    }

    /// The date pattern (short numeric form) of the language.
    pub fn date_pattern(self) -> DatePattern {
        use Lang::*;
        match self {
            // ISO-like (year first): Swedish, Lithuanian, Hungarian.
            Sv | Lt | Hu => DatePattern::Ymd,
            // The rest of the EU: day-month-year.
            _ => DatePattern::Dmy,
        }
    }

    /// The plural system.
    pub fn plural_system(self) -> PluralSystem {
        use Lang::*;
        use PluralSystem::*;
        match self {
            En | Nl | De | Sv | Da | Et | Fi | El | It | Es | Pt | Bg | Hu => OneOther,
            Fr => FrenchOneOther,
            Cs | Sk => WestSlavic,
            Pl => Polish,
            Hr => Croatian,
            Lv => Latvian,
            Lt => Lithuanian,
            Sl => Slovenian,
            Ga => Irish,
            Mt => Maltese,
            Ro => Romanian,
        }
    }
}

/// Numeric date pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DatePattern {
    /// day/month/year (e.g. 31/12/2026).
    Dmy,
    /// year-month-day (e.g. 2026-12-31).
    Ymd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tags() {
        assert_eq!(Lang::parse("nl"), Some(Lang::Nl));
        assert_eq!(Lang::parse("nl-BE"), Some(Lang::Nl));
        assert_eq!(Lang::parse("de_DE"), Some(Lang::De));
        assert_eq!(Lang::parse("EN-gb"), Some(Lang::En));
        assert_eq!(Lang::parse("xx"), None);
    }

    #[test]
    fn all_24_distinct_codes() {
        let mut codes: alloc::vec::Vec<&str> = Lang::ALL.iter().map(|l| l.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 24);
    }

    #[test]
    fn separators_make_sense() {
        assert_eq!(Lang::En.separators().decimal, '.');
        assert_eq!(Lang::Nl.separators().decimal, ',');
        assert_eq!(Lang::Fr.separators().group, ' ');
        assert_eq!(Lang::De.separators().group, '.');
    }

    #[test]
    fn date_patterns() {
        assert_eq!(Lang::Sv.date_pattern(), DatePattern::Ymd);
        assert_eq!(Lang::Nl.date_pattern(), DatePattern::Dmy);
    }
}
