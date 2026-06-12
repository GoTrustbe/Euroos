//! Taalbewuste collatie (sorteervolgorde). Twee lagen:
//!
//! 1. **Diacritiek-vouwing** — `é` sorteert vlak na `e`, niet als los teken.
//! 2. **Taal-tailoring** — sommige talen plaatsen letters op een eigen plek:
//!    Zweeds/Fins `å ä ö` ná `z`, Deens `æ ø å` ná `z`, Spaans `ñ` tussen `n` en
//!    `o`, Tsjechisch `č` ná `c` / `š` ná `s` / `ž` ná `z`, Duits `ä≈a` (DIN-1).
//!
//! Latijns schrift wordt op gewogen sleutels vergeleken; niet-Latijn (Grieks,
//! Cyrillisch/Bulgaars) valt terug op codepoint-volgorde (binnen-schrift correct).

use crate::lang::Lang;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// De primaire collatie-weging van een (kleine) letter. Basisletters a–z liggen op
/// veelvouden van 100 zodat er ruimte is om getailorde letters ertussen/erna te
/// plaatsen. Niet-Latijn → codepoint + offset (sorteert ná Latijn).
fn weight(lang: Lang, ch: char) -> u32 {
    let lower = ch.to_lowercase().next().unwrap_or(ch);

    // 1. Taal-specifieke tailoring (heeft voorrang op de generieke vouwing).
    if let Some(w) = tailored(lang, lower) {
        return w;
    }
    // 2. Generieke diacritiek-vouwing naar (basisletter, sub-offset).
    if let Some((base, sub)) = fold(lower) {
        return base_rank(base) + sub;
    }
    // 3. Gewone ASCII-letter.
    if lower.is_ascii_lowercase() {
        return base_rank(lower);
    }
    // 4. Niet-letter / niet-Latijn → codepoint, ruim ná het Latijnse bereik.
    100_000 + lower as u32
}

/// Basisrang van een a–z-letter (a=0, b=100, …, z=2500).
fn base_rank(c: char) -> u32 {
    debug_assert!(c.is_ascii_lowercase());
    (c as u32 - 'a' as u32) * 100
}

/// Vouw een geaccentueerde Latijnse letter naar zijn basisletter + diacriet-offset
/// (zodat `e < é < ê < f`). `None` = geen bekende vouwing.
fn fold(c: char) -> Option<(char, u32)> {
    let r = match c {
        'á' | 'à' | 'â' | 'ã' | 'ā' | 'ă' => ('a', 1),
        'ä' => ('a', 2), // umlaut → basisletter (DIN-1, default voor de/nl)
        'å' => ('a', 3),
        'æ' => ('a', 4),
        'ç' | 'ć' | 'č' | 'ĉ' => ('c', 1),
        'ď' => ('d', 1),
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ě' | 'ę' => ('e', 1),
        'ğ' => ('g', 1),
        'í' | 'ì' | 'î' | 'ï' | 'ī' => ('i', 1),
        'ĺ' | 'ľ' | 'ł' => ('l', 1),
        'ñ' | 'ń' | 'ň' => ('n', 1),
        'ó' | 'ò' | 'ô' | 'õ' | 'ō' => ('o', 1),
        'ö' => ('o', 2),
        'ø' => ('o', 3),
        'ŕ' | 'ř' => ('r', 1),
        'ś' | 'š' | 'ş' => ('s', 1),
        'ť' | 'ţ' => ('t', 1),
        'ú' | 'ù' | 'û' | 'ū' | 'ů' => ('u', 1),
        'ü' => ('u', 2),
        'ý' | 'ÿ' => ('y', 1),
        'ź' | 'ž' | 'ż' => ('z', 1),
        'ß' => ('s', 2), // scharfes S ~ "ss"
        _ => return None,
    };
    Some(r)
}

/// Taal-specifieke letterplaatsing. Geeft een expliciete weging die de generieke
/// vouwing overschrijft.
fn tailored(lang: Lang, c: char) -> Option<u32> {
    use Lang::*;
    let after_z = base_rank('z') + 100; // 2600
    match lang {
        Sv | Fi => match c {
            'å' => Some(after_z),
            'ä' => Some(after_z + 100),
            'ö' => Some(after_z + 200),
            _ => None,
        },
        Da => match c {
            'æ' => Some(after_z),
            'ø' => Some(after_z + 100),
            'å' => Some(after_z + 200),
            _ => None,
        },
        Es => match c {
            // ñ als eigen letter tussen n en o.
            'ñ' => Some(base_rank('n') + 50),
            _ => None,
        },
        Cs | Sk => match c {
            'č' => Some(base_rank('c') + 50),
            'ř' => Some(base_rank('r') + 50),
            'š' => Some(base_rank('s') + 50),
            'ž' => Some(base_rank('z') + 50),
            _ => None,
        },
        Et => match c {
            // Ests: õ ä ö ü ná w (vóór x).
            'õ' => Some(base_rank('w') + 25),
            'ä' => Some(base_rank('w') + 50),
            'ö' => Some(base_rank('w') + 75),
            'ü' => Some(base_rank('w') + 90),
            _ => None,
        },
        // Duits (de) + Nederlands: ä/ö/ü vouwen naar a/o/u (default fold doet dit).
        _ => None,
    }
}

/// De collatie-sleutel van een hele string (rij primaire wegingen).
fn key(lang: Lang, s: &str) -> Vec<u32> {
    s.chars().map(|c| weight(lang, c)).collect()
}

/// Vergelijk twee strings in de collatie van `lang`. Bij een primaire gelijkstand
/// beslist de codepoint (zodat de orde totaal + stabiel is, hoofdletters ná).
pub fn collate(lang: Lang, a: &str, b: &str) -> Ordering {
    let (ka, kb) = (key(lang, a), key(lang, b));
    match ka.cmp(&kb) {
        Ordering::Equal => a.cmp(b), // secundair: ruwe codepoint
        ord => ord,
    }
}

/// Sorteer een lijst strings in de collatie van `lang`.
pub fn sort(lang: Lang, items: &mut [String]) {
    items.sort_by(|a, b| collate(lang, a, b));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn sorted(lang: Lang, v: &[&str]) -> Vec<String> {
        let mut s: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        sort(lang, &mut s);
        s
    }

    #[test]
    fn diacritic_folds_near_base() {
        // 'é' hoort tussen 'e' en 'f'.
        assert_eq!(sorted(Lang::Fr, &["f", "é", "e"]), vec!["e", "é", "f"]);
    }

    #[test]
    fn swedish_aa_after_z() {
        // Zweeds: å ä ö ná z.
        assert_eq!(
            sorted(Lang::Sv, &["ö", "a", "z", "å", "ä"]),
            vec!["a", "z", "å", "ä", "ö"]
        );
    }

    #[test]
    fn german_umlaut_as_base() {
        // Duits (DIN-1): ä sorteert als a → "ä" vóór "b".
        assert_eq!(sorted(Lang::De, &["b", "ä", "a"]), vec!["a", "ä", "b"]);
    }

    #[test]
    fn spanish_enye_after_n() {
        // ñ tussen n en o.
        assert_eq!(sorted(Lang::Es, &["o", "ñ", "n"]), vec!["n", "ñ", "o"]);
    }

    #[test]
    fn czech_c_hacek_after_c() {
        assert_eq!(sorted(Lang::Cs, &["d", "č", "c"]), vec!["c", "č", "d"]);
    }

    #[test]
    fn case_insensitive_primary() {
        // Hoofdletters tellen primair gelijk; "Appel" vlak bij "appel".
        let v = sorted(Lang::Nl, &["banaan", "Appel", "appel"]);
        assert_eq!(v, vec!["Appel", "appel", "banaan"]);
    }
}
