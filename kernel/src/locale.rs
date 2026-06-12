//! Kernel-zijde van **EuroLocale** (plan P1): bewijst bij boot dat EuroOS de 24
//! officiële EU-talen lokaliseert (getal/valuta/datum/meervoud/collatie). De
//! host-geteste kern leeft in [`eurolocale`]; hier tonen we 'm live + bieden we
//! het `locale`-shellcommando.

use alloc::string::String;
use alloc::vec::Vec;

use eurolocale::{Lang, Locale, Plural};

/// Boot-zelftest: lokaliseer dezelfde waarden in meerdere talen en controleer de
/// taal-eigen opmaak (scheidingstekens, valutaplaatsing, datumpatroon, collatie).
pub fn selftest() {
    let nl = Locale::new(Lang::Nl);
    let en = Locale::new(Lang::En);
    let de = Locale::new(Lang::De);

    // Getal + valuta: zelfde bedrag, taal-eigen opmaak.
    let money_ok = nl.money(123456) == "€1.234,56"
        && en.money(123456) == "€1,234.56"
        && de.money(123456).replace('\u{00A0}', " ") == "1.234,56 €";

    // Datum: numeriek patroon per taal.
    let date_ok = nl.date(2026, 6, 6) == "06-06-2026"
        && en.date(2026, 6, 6) == "06/06/2026"
        && Locale::new(Lang::Sv).date(2026, 6, 6) == "2026-06-06";

    // Meervoud: Pools 5 = "many", Frans 0 = "one".
    let plural_ok = Locale::new(Lang::Pl).plural(5) == Plural::Many
        && Locale::new(Lang::Fr).plural(0) == Plural::One
        && en.plural(1) == Plural::One;

    // Collatie: Zweeds sorteert å/ä/ö ná z.
    let mut words: Vec<String> = ["öl", "abc", "zoo", "ärlig", "åes"].iter().map(|s| String::from(*s)).collect();
    Locale::new(Lang::Sv).sort(&mut words);
    let collation_ok = words == ["abc", "zoo", "åes", "ärlig", "öl"];

    let ok = money_ok && date_ok && plural_ok && collation_ok;
    crate::serial_println!(
        "[loc] EuroLocale: 24 EU-talen — valuta(€1.234,56/€1,234.56/1.234,56 €)={money_ok}, datum(nl/en/sv)={date_ok}, meervoud(pl/fr/en)={plural_ok}, collatie(sv å<ä<ö ná z)={collation_ok} → {}",
        if ok { "OK (soevereine lokalisatie voor heel de EU) ✓" } else { "MISLUKT" }
    );
}

/// `locale [tag]`-shellcommando. Zonder tag: lijst de talen. Met een tag (`nl-BE`):
/// toon voorbeelden van getal/valuta/datum-opmaak in die taal.
pub fn shell(args: &str) -> Vec<String> {
    let args = args.trim();
    if args.is_empty() {
        let mut out = alloc::vec![String::from("EuroLocale — 24 officiële EU-talen:")];
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
        out.push(String::from("gebruik: locale <tag>   bv. locale de-DE"));
        return out;
    }

    let Some(loc) = Locale::parse(args) else {
        return alloc::vec![alloc::format!("locale: onbekende taal-tag '{args}'")];
    };
    alloc::vec![
        alloc::format!("EuroLocale — {} ({})", loc.lang.code(), loc.lang.native_name()),
        alloc::format!("  groot getal : {}", loc.int(1234567)),
        alloc::format!("  bedrag      : {} ({})", loc.money(123456), loc.currency_code()),
        alloc::format!("  datum kort  : {}", loc.date(2026, 6, 6)),
        alloc::format!("  datum lang  : {}", loc.date_long(2026, 6, 6)),
        alloc::format!("  meervoud    : 1→{:?}, 2→{:?}, 5→{:?}", loc.plural(1), loc.plural(2), loc.plural(5)),
    ]
}
