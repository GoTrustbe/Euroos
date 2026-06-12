//! Getalopmaak per taal: groeperings- en decimaalteken volgens de CLDR-regels.

use crate::lang::Lang;
use alloc::string::{String, ToString};

/// Formatteer een geheel getal met groepering per duizendtal (`1234567` → `nl: "1.234.567"`).
pub fn format_int(lang: Lang, value: i64) -> String {
    let sep = lang.separators();
    let neg = value < 0;
    let digits = abs_to_string(value);
    let grouped = group_digits(&digits, sep.group);
    if neg {
        let mut s = String::from("-");
        s.push_str(&grouped);
        s
    } else {
        grouped
    }
}

/// Formatteer een vast-komma-getal: `value` is de waarde × 10^`frac_digits`
/// (bv. 123456 met `frac_digits=2` = 1234,56). Vermijdt drijvende komma.
pub fn format_fixed(lang: Lang, scaled: i64, frac_digits: u32) -> String {
    let sep = lang.separators();
    let neg = scaled < 0;
    let mut mag = (scaled as i128).unsigned_abs();
    // Begrens de schaal zodat 10^frac_digits niet overloopt (audit M6); 38 decimalen
    // is ruim meer dan een i64-bedrag ooit nodig heeft.
    let frac_digits = frac_digits.min(38);
    let divisor = 10u128.pow(frac_digits);
    let frac = (mag % divisor) as u64;
    mag /= divisor;
    let int_digits = u128_to_string(mag);
    let grouped = group_digits(&int_digits, sep.group);

    let mut out = String::new();
    if neg && (mag != 0 || frac != 0) {
        out.push('-');
    }
    out.push_str(&grouped);
    if frac_digits > 0 {
        out.push(sep.decimal);
        // Voeg de fractie met leidende nullen toe.
        let fs = frac.to_string();
        for _ in 0..(frac_digits as usize - fs.len()) {
            out.push('0');
        }
        out.push_str(&fs);
    }
    out
}

fn abs_to_string(v: i64) -> String {
    // i64::MIN veilig afhandelen via i128.
    u128_to_string((v as i128).unsigned_abs())
}

fn u128_to_string(v: u128) -> String {
    use alloc::string::ToString;
    v.to_string()
}

/// Plaats `group` om de drie cijfers in een (teken-loze) cijferreeks.
fn group_digits(digits: &str, group: char) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(group);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert_eq!(format_int(Lang::Nl, 1234567), "1.234.567");
        assert_eq!(format_int(Lang::En, 1234567), "1,234,567");
        assert_eq!(format_int(Lang::Fr, 1234567), "1 234 567");
        assert_eq!(format_int(Lang::Nl, -1000), "-1.000");
        assert_eq!(format_int(Lang::En, 0), "0");
        assert_eq!(format_int(Lang::En, 42), "42");
    }

    #[test]
    fn fixed_point() {
        // 1234.56 met 2 decimalen.
        assert_eq!(format_fixed(Lang::Nl, 123456, 2), "1.234,56");
        assert_eq!(format_fixed(Lang::En, 123456, 2), "1,234.56");
        assert_eq!(format_fixed(Lang::Fr, 123456, 2), "1 234,56");
        // Leidende nul in de fractie.
        assert_eq!(format_fixed(Lang::Nl, 105, 2), "1,05");
        // Geen fractie.
        assert_eq!(format_fixed(Lang::En, 1500, 0), "1,500");
        // Negatief.
        assert_eq!(format_fixed(Lang::De, -150, 2), "-1,50");
    }

    #[test]
    fn extremes() {
        assert_eq!(format_int(Lang::En, i64::MIN), "-9,223,372,036,854,775,808");
    }
}
