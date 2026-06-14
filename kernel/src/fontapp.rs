//! Boot self-test for **EuroFont** (AC-2): sfnt/TrueType parser.
//! Core: [`eurofont`].

use crate::serial_println;

pub fn selftest() {
    // Build a minimal font and parse it back.
    let font = eurofont::build_min_font("EuroSans", "Bold", "EuroSans Bold", 2048, 412);
    let info = eurofont::parse(&font);

    let parse_ok = info
        .as_ref()
        .map(|i| {
            i.family == "EuroSans"
                && i.subfamily == "Bold"
                && i.full_name == "EuroSans Bold"
                && i.units_per_em == 2048
                && i.num_glyphs == 412
        })
        .unwrap_or(false);

    // Non-font rejected; truncated input does not panic.
    let reject_ok = eurofont::parse(b"not a font").is_none();
    let _ = eurofont::parse(&font[..font.len() / 2]); // bounds-safe

    let ok = parse_ok && reject_ok;
    serial_println!(
        "[ft] EuroFont: parse(family/style/upem/glyphs)={}, non-font-rejected={}, bounds-safe=true {}",
        parse_ok, reject_ok,
        if ok { "✓" } else { "✗ FAIL" }
    );
}
