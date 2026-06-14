//! CLDR plural rules for whole numbers. Returns the correct category so that a
//! UI picks the correct form ("1 file" vs "2 files" vs Polish "5 plików").
//!
//! Limited to integer input (`n`), which covers the vast majority of UI cases;
//! the fractional categories (`many` for decimals etc.) are deliberately omitted.

use crate::lang::{Lang, PluralSystem};

/// A CLDR plural category.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plural {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// Determine the plural category of `n` in `lang`.
pub fn category(lang: Lang, n: u64) -> Plural {
    use Plural::*;
    let m10 = n % 10;
    let m100 = n % 100;
    match lang.plural_system() {
        PluralSystem::None => Other,
        PluralSystem::OneOther => {
            if n == 1 {
                One
            } else {
                Other
            }
        }
        PluralSystem::FrenchOneOther => {
            if n == 0 || n == 1 {
                One
            } else {
                Other
            }
        }
        PluralSystem::WestSlavic => {
            // cs/sk: one=1, few=2..4, otherwise other (many = fractions only).
            if n == 1 {
                One
            } else if (2..=4).contains(&n) {
                Few
            } else {
                Other
            }
        }
        PluralSystem::Polish => {
            if n == 1 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Many
            }
        }
        PluralSystem::Croatian => {
            // BCS: one = n%10==1 & n%100!=11; few = n%10 2..4 & n%100 not 12..14.
            if m10 == 1 && m100 != 11 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Other
            }
        }
        PluralSystem::Latvian => {
            // zero = n%10==0 or n%100 in 11..19; one = n%10==1 & n%100!=11.
            if m10 == 0 || (11..=19).contains(&m100) {
                Zero
            } else if m10 == 1 && m100 != 11 {
                One
            } else {
                Other
            }
        }
        PluralSystem::Lithuanian => {
            // one = n%10==1 & n%100 not 11..19; few = n%10 2..9 & n%100 not 11..19.
            if m10 == 1 && !(11..=19).contains(&m100) {
                One
            } else if (2..=9).contains(&m10) && !(11..=19).contains(&m100) {
                Few
            } else {
                Many // (actually fractions; integer fallback)
            }
        }
        PluralSystem::Slovenian => {
            // one = n%100==1; two = n%100==2; few = n%100 in 3..4.
            if m100 == 1 {
                One
            } else if m100 == 2 {
                Two
            } else if (3..=4).contains(&m100) {
                Few
            } else {
                Other
            }
        }
        PluralSystem::Irish => {
            if n == 1 {
                One
            } else if n == 2 {
                Two
            } else if (3..=6).contains(&n) {
                Few
            } else if (7..=10).contains(&n) {
                Many
            } else {
                Other
            }
        }
        PluralSystem::Maltese => {
            if n == 1 {
                One
            } else if n == 0 || (2..=10).contains(&m100) {
                Few
            } else if (11..=19).contains(&m100) {
                Many
            } else {
                Other
            }
        }
        PluralSystem::Romanian => {
            // one=1; few = n==0 or (n%100 in 1..19 & n!=1); otherwise other.
            if n == 1 {
                One
            } else if n == 0 || (1..=19).contains(&m100) {
                Few
            } else {
                Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Plural::*;

    #[test]
    fn english_one_other() {
        assert_eq!(category(Lang::En, 1), One);
        assert_eq!(category(Lang::En, 0), Other);
        assert_eq!(category(Lang::En, 2), Other);
    }

    #[test]
    fn french_zero_is_one() {
        assert_eq!(category(Lang::Fr, 0), One);
        assert_eq!(category(Lang::Fr, 1), One);
        assert_eq!(category(Lang::Fr, 2), Other);
    }

    #[test]
    fn polish() {
        assert_eq!(category(Lang::Pl, 1), One);
        assert_eq!(category(Lang::Pl, 2), Few);
        assert_eq!(category(Lang::Pl, 3), Few);
        assert_eq!(category(Lang::Pl, 4), Few);
        assert_eq!(category(Lang::Pl, 5), Many);
        assert_eq!(category(Lang::Pl, 12), Many); // 12..14 exception
        assert_eq!(category(Lang::Pl, 22), Few);
    }

    #[test]
    fn czech_west_slavic() {
        assert_eq!(category(Lang::Cs, 1), One);
        assert_eq!(category(Lang::Cs, 3), Few);
        assert_eq!(category(Lang::Cs, 5), Other);
    }

    #[test]
    fn croatian() {
        assert_eq!(category(Lang::Hr, 1), One);
        assert_eq!(category(Lang::Hr, 21), One); // %10==1, %100!=11
        assert_eq!(category(Lang::Hr, 11), Other);
        assert_eq!(category(Lang::Hr, 3), Few);
    }

    #[test]
    fn latvian_has_zero() {
        assert_eq!(category(Lang::Lv, 0), Zero);
        assert_eq!(category(Lang::Lv, 10), Zero);
        assert_eq!(category(Lang::Lv, 11), Zero);
        assert_eq!(category(Lang::Lv, 1), One);
        assert_eq!(category(Lang::Lv, 21), One);
        assert_eq!(category(Lang::Lv, 2), Other);
    }

    #[test]
    fn slovenian_dual() {
        assert_eq!(category(Lang::Sl, 1), One);
        assert_eq!(category(Lang::Sl, 2), Two);
        assert_eq!(category(Lang::Sl, 3), Few);
        assert_eq!(category(Lang::Sl, 5), Other);
    }
}
