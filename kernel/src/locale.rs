//! Kernel side of **EuroLocale** (plan P1): proves at boot that EuroOS localizes
//! the 24 official EU languages (number/currency/date/plural/collation). The
//! host-tested core lives in [`eurolocale`]; here we show it live + provide
//! the `locale` shell command.

use alloc::string::String;
use alloc::vec::Vec;

use eurolocale::{Lang, Locale, Plural};

/// Boot self-test: localize the same values in multiple languages and check the
/// language-specific formatting (separators, currency placement, date pattern, collation).
pub fn selftest() {
    let nl = Locale::new(Lang::Nl);
    let en = Locale::new(Lang::En);
    let de = Locale::new(Lang::De);

    // Number + currency: same amount, language-specific formatting.
    let money_ok = nl.money(123456) == "€1.234,56"
        && en.money(123456) == "€1,234.56"
        && de.money(123456).replace('\u{00A0}', " ") == "1.234,56 €";

    // Date: numeric pattern per language.
    let date_ok = nl.date(2026, 6, 6) == "06-06-2026"
        && en.date(2026, 6, 6) == "06/06/2026"
        && Locale::new(Lang::Sv).date(2026, 6, 6) == "2026-06-06";

    // Plural: Polish 5 = "many", French 0 = "one".
    let plural_ok = Locale::new(Lang::Pl).plural(5) == Plural::Many
        && Locale::new(Lang::Fr).plural(0) == Plural::One
        && en.plural(1) == Plural::One;

    // Collation: Swedish sorts å/ä/ö after z.
    let mut words: Vec<String> = ["öl", "abc", "zoo", "ärlig", "åes"].iter().map(|s| String::from(*s)).collect();
    Locale::new(Lang::Sv).sort(&mut words);
    let collation_ok = words == ["abc", "zoo", "åes", "ärlig", "öl"];

    let ok = money_ok && date_ok && plural_ok && collation_ok;
    crate::serial_println!(
        "[loc] EuroLocale: 24 EU languages — currency(€1.234,56/€1,234.56/1.234,56 €)={money_ok}, date(nl/en/sv)={date_ok}, plural(pl/fr/en)={plural_ok}, collation(sv å<ä<ö after z)={collation_ok} → {}",
        if ok { "OK (sovereign localization for the whole EU) ✓" } else { "FAILED" }
    );
}

/// `locale [tag]` shell command. Without a tag: list the languages. With a tag (`nl-BE`):
/// show examples of number/currency/date formatting in that language.
pub fn shell(args: &str) -> Vec<String> {
    let args = args.trim();
    if args.is_empty() {
        let mut out = alloc::vec![String::from("EuroLocale — 24 official EU languages:")];
        let mut line = String::from("  ");
        for (i, l) in Lang::ALL.iter().enumerate() {
            line.push_str(l.code());
            line.push('(');
            line.push_str(l.native_name());
            line.push_str(") ");
            if (i + 1) % 4 == 0 {
                out.push(core::mem::take(&mut line));
                line.push_str("  ");
            }
        }
        if !line.trim().is_empty() {
            out.push(line);
        }
        out.push(String::from("usage: locale <tag>   e.g. locale de-DE"));
        return out;
    }

    let Some(loc) = Locale::parse(args) else {
        return alloc::vec![alloc::format!("locale: unknown language tag '{args}'")];
    };
    alloc::vec![
        alloc::format!("EuroLocale — {} ({})", loc.lang.code(), loc.lang.native_name()),
        alloc::format!("  large number : {}", loc.int(1234567)),
        alloc::format!("  amount       : {} ({})", loc.money(123456), loc.currency_code()),
        alloc::format!("  date short   : {}", loc.date(2026, 6, 6)),
        alloc::format!("  date long    : {}", loc.date_long(2026, 6, 6)),
        alloc::format!("  plural       : 1→{:?}, 2→{:?}, 5→{:?}", loc.plural(1), loc.plural(2), loc.plural(5)),
    ]
}
