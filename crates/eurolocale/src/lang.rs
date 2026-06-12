//! De 24 officiële talen van de Europese Unie + hun lokalisatieregels.
//!
//! Elke taal draagt de CLDR-kerngegevens die de rest van het crate gebruikt:
//! scheidingstekens (getal/groepering), valutaplaatsing, datumpatroon en de
//! meervouds-familie. Soevereiniteit op taalniveau: EuroOS spreekt élke EU-taal,
//! niet alleen Engels.

/// Een EU-taal (ISO 639-1). De 24 officiële talen van de Unie.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Bg, // Bulgaars
    Hr, // Kroatisch
    Cs, // Tsjechisch
    Da, // Deens
    Nl, // Nederlands
    En, // Engels
    Et, // Ests
    Fi, // Fins
    Fr, // Frans
    De, // Duits
    El, // Grieks
    Hu, // Hongaars
    Ga, // Iers
    It, // Italiaans
    Lv, // Lets
    Lt, // Litouws
    Mt, // Maltees
    Pl, // Pools
    Pt, // Portugees
    Ro, // Roemeens
    Sk, // Slowaaks
    Sl, // Sloveens
    Es, // Spaans
    Sv, // Zweeds
}

/// Het decimaal- en groeperingsteken van een taal.
pub struct Separators {
    pub decimal: char,
    pub group: char,
}

/// Het meervouds-systeem (CLDR-categorieën die de taal onderscheidt).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PluralSystem {
    /// Geen onderscheid (bv. Hongaars, Turks-stijl): altijd `other`.
    None,
    /// Engels-stijl: `one` (n=1) vs `other`.
    OneOther,
    /// Frans-stijl: `one` (n=0,1) vs `other`.
    FrenchOneOther,
    /// West-Slavisch (Tsjechisch/Slowaaks): one / few (2-4) / many (fracties) / other.
    WestSlavic,
    /// Pools: one / few / many.
    Polish,
    /// Kroatisch (BCS): one / few / other.
    Croatian,
    /// Lets: zero / one / other.
    Latvian,
    /// Litouws: one / few / many.
    Lithuanian,
    /// Sloveens: one (n%100==1) / two (==2) / few (3,4) / other.
    Slovenian,
    /// Iers: one / two / few / many / other.
    Irish,
    /// Maltees: one / few / many / other.
    Maltese,
    /// Roemeens: one / few / other.
    Romanian,
}

impl Lang {
    /// Alle 24 talen (vaste volgorde).
    pub const ALL: [Lang; 24] = {
        use Lang::*;
        [Bg, Hr, Cs, Da, Nl, En, Et, Fi, Fr, De, El, Hu, Ga, It, Lv, Lt, Mt, Pl, Pt, Ro, Sk, Sl, Es, Sv]
    };

    /// De ISO 639-1-code (`"nl"`, `"de"`, …).
    pub fn code(self) -> &'static str {
        use Lang::*;
        match self {
            Bg => "bg", Hr => "hr", Cs => "cs", Da => "da", Nl => "nl", En => "en",
            Et => "et", Fi => "fi", Fr => "fr", De => "de", El => "el", Hu => "hu",
            Ga => "ga", It => "it", Lv => "lv", Lt => "lt", Mt => "mt", Pl => "pl",
            Pt => "pt", Ro => "ro", Sk => "sk", Sl => "sl", Es => "es", Sv => "sv",
        }
    }

    /// De eigennaam van de taal (endoniem).
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

    /// Parse een taal-tag als `"nl"`, `"nl-BE"`, `"de_DE"` → de taal (regio genegeerd).
    pub fn parse(tag: &str) -> Option<Lang> {
        let lang = tag
            .split(|c| c == '-' || c == '_')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Lang::ALL.into_iter().find(|l| l.code() == lang)
    }

    /// Gebruikt deze taal (in zijn hoofdregio) de euro?
    pub fn is_eurozone(self) -> bool {
        use Lang::*;
        // Eurozone-talen (incl. Kroatië sinds 2023, en Engels = Ierland/Malta);
        // de niet-euro EU-munten (BGN/CZK/DKK/HUF/PLN/RON/SEK) staan in [`crate::currency`].
        matches!(
            self,
            En | Nl | De | Fr | It | Es | Pt | Fi | El | Ga | Mt | Sk | Sl | Et | Lv | Lt | Hr
        )
    }

    /// Decimaal- en groeperingsteken.
    pub fn separators(self) -> Separators {
        use Lang::*;
        // Engels/Iers/Maltees: punt-decimaal, komma-groepering.
        // De meeste continentale talen: komma-decimaal, punt of spatie-groepering.
        match self {
            En | Ga | Mt => Separators { decimal: '.', group: ',' },
            // Spatie-groepering (smal vaste spatie vereenvoudigd tot gewone spatie).
            Fr | Cs | Sk | Pl | Hu | Sv | Fi | Et | Lv | Lt | Bg => {
                Separators { decimal: ',', group: ' ' }
            }
            // Zwitsers-Duits gebruikt ', maar EU-Duits (de-DE) gebruikt punt-groepering.
            De | Nl | It | Es | Pt | Da | El | Hr | Ro | Sl => {
                Separators { decimal: ',', group: '.' }
            }
        }
    }

    /// Wordt het valutasymbool vóór het bedrag geplaatst? (anders erachter)
    pub fn currency_before(self) -> bool {
        use Lang::*;
        // Engels/Iers/Maltees/Nederlands: symbool vóór ("€1,00"); de meeste andere erna ("1,00 €").
        matches!(self, En | Ga | Mt | Nl)
    }

    /// Het datumpatroon (korte numerieke vorm) van de taal.
    pub fn date_pattern(self) -> DatePattern {
        use Lang::*;
        match self {
            // ISO-achtig (jaar eerst): Zweeds, Litouws, Hongaars.
            Sv | Lt | Hu => DatePattern::Ymd,
            // De rest van de EU: dag-maand-jaar.
            _ => DatePattern::Dmy,
        }
    }

    /// Het meervouds-systeem.
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

/// Numeriek datumpatroon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DatePattern {
    /// dag/maand/jaar (bv. 31/12/2026).
    Dmy,
    /// jaar-maand-dag (bv. 2026-12-31).
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
