//! Language-aware collation (sort order). Two layers:
//!
//! 1. **Diacritic folding** — `é` sorts right after `e`, not as a separate character.
//! 2. **Language tailoring** — some languages place letters in their own position:
//!    Swedish/Finnish `å ä ö` after `z`, Danish `æ ø å` after `z`, Spanish `ñ` between `n` and
//!    `o`, Czech `č` after `c` / `š` after `s` / `ž` after `z`, German `ä≈a` (DIN-1).
//!
//! Latin script is compared on weighted keys; non-Latin (Greek,
//! Cyrillic/Bulgarian) falls back to codepoint order (correct within-script).

use crate::lang::Lang;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// The primary collation weight of a (lowercase) letter. Base letters a–z sit on
/// multiples of 100 so there is room to place tailored letters between/after them.
/// Non-Latin → codepoint + offset (sorts after Latin).
fn weight(lang: Lang, ch: char) -> u32 {
    let lower = ch.to_lowercase().next().unwrap_or(ch);

    // 1. Language-specific tailoring (takes precedence over the generic folding).
    if let Some(w) = tailored(lang, lower) {
        return w;
    }
    // 2. Generic diacritic folding to (base letter, sub-offset).
    if let Some((base, sub)) = fold(lower) {
        return base_rank(base) + sub;
    }
    // 3. Ordinary ASCII letter.
    if lower.is_ascii_lowercase() {
        return base_rank(lower);
    }
    // 4. Non-letter / non-Latin → codepoint, well after the Latin range.
    100_000 + lower as u32
}

/// Base rank of an a–z letter (a=0, b=100, …, z=2500).
fn base_rank(c: char) -> u32 {
    debug_assert!(c.is_ascii_lowercase());
    (c as u32 - 'a' as u32) * 100
}

/// Fold an accented Latin letter to its base letter + diacritic offset
/// (so that `e < é < ê < f`). `None` = no known folding.
fn fold(c: char) -> Option<(char, u32)> {
    let r = match c {
        'á' | 'à' | 'â' | 'ã' | 'ā' | 'ă' => ('a', 1),
        'ä' => ('a', 2), // umlaut → base letter (DIN-1, default for de/nl)
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

/// Language-specific letter placement. Returns an explicit weight that overrides
/// the generic folding.
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
            // ñ as its own letter between n and o.
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
            // Estonian: õ ä ö ü after w (before x).
            'õ' => Some(base_rank('w') + 25),
            'ä' => Some(base_rank('w') + 50),
            'ö' => Some(base_rank('w') + 75),
            'ü' => Some(base_rank('w') + 90),
            _ => None,
        },
        // German (de) + Dutch: ä/ö/ü fold to a/o/u (the default fold does this).
        _ => None,
    }
}

/// The collation key of a whole string (sequence of primary weights).
fn key(lang: Lang, s: &str) -> Vec<u32> {
    s.chars().map(|c| weight(lang, c)).collect()
}

/// Compare two strings in the collation of `lang`. On a primary tie
/// the codepoint decides (so the order is total + stable, uppercase after).
pub fn collate(lang: Lang, a: &str, b: &str) -> Ordering {
    let (ka, kb) = (key(lang, a), key(lang, b));
    match ka.cmp(&kb) {
        Ordering::Equal => a.cmp(b), // secondary: raw codepoint
        ord => ord,
    }
}

/// Sort a list of strings in the collation of `lang`.
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
        // 'é' belongs between 'e' and 'f'.
        assert_eq!(sorted(Lang::Fr, &["f", "é", "e"]), vec!["e", "é", "f"]);
    }

    #[test]
    fn swedish_aa_after_z() {
        // Swedish: å ä ö after z.
        assert_eq!(
            sorted(Lang::Sv, &["ö", "a", "z", "å", "ä"]),
            vec!["a", "z", "å", "ä", "ö"]
        );
    }

    #[test]
    fn german_umlaut_as_base() {
        // German (DIN-1): ä sorts as a → "ä" before "b".
        assert_eq!(sorted(Lang::De, &["b", "ä", "a"]), vec!["a", "ä", "b"]);
    }

    #[test]
    fn spanish_enye_after_n() {
        // ñ between n and o.
        assert_eq!(sorted(Lang::Es, &["o", "ñ", "n"]), vec!["n", "ñ", "o"]);
    }

    #[test]
    fn czech_c_hacek_after_c() {
        assert_eq!(sorted(Lang::Cs, &["d", "č", "c"]), vec!["c", "č", "d"]);
    }

    #[test]
    fn case_insensitive_primary() {
        // Uppercase counts as primary-equal; "Appel" right next to "appel".
        let v = sorted(Lang::Nl, &["banaan", "Appel", "appel"]);
        assert_eq!(v, vec!["Appel", "appel", "banaan"]);
    }
}
