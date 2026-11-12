//! **EuroKeymap** — translate PS/2 **scancode set 1** into characters for the
//! keyboard layouts EU users actually have: US-QWERTY, BE/FR-AZERTY and
//! DE-QWERTZ. The active layout is chosen at install time (3F-4) and by the
//! `keymap` shell command; the kernel PS/2 driver calls [`translate`].
//!
//! Scope: the **alphanumeric block** (letters, the digit row, the punctuation
//! keys including the ISO key left of Z/W/Y, space/enter/tab/backspace) is
//! mapped per layout, with the AZERTY digit-row shift inversion, and the
//! **AltGr level** of each layout ([`translate_ex`]): `@ # { } [ ] \ | ~ €` and
//! the other symbols printed on the key caps. There is no dead-key
//! composition: a dead key (circumflex, diaeresis, acute, grave, tilde)
//! produces its own character at once. Shift+AltGr (the fourth level) is not
//! mapped.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// The keyboard layouts EuroOS ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// US QWERTY (the default / fallback).
    UsQwerty,
    /// Belgian AZERTY.
    BeAzerty,
    /// French AZERTY.
    FrAzerty,
    /// German QWERTZ.
    DeQwertz,
}

impl Layout {
    /// Parse a layout tag (`"us"`, `"be"`, `"fr"`, `"de"`, or the `be-azerty`
    /// style used by the installer). Case-insensitive.
    pub fn parse(tag: &str) -> Option<Layout> {
        let t = tag.trim();
        // Accept both the short code and the installer's "<lang>-<layout>" form.
        let short = t.split('-').next().unwrap_or(t);
        let is = |x: &str| short.eq_ignore_ascii_case(x);
        if is("us") || is("en") {
            Some(Layout::UsQwerty)
        } else if is("be") {
            Some(Layout::BeAzerty)
        } else if is("fr") {
            Some(Layout::FrAzerty)
        } else if is("de") {
            Some(Layout::DeQwertz)
        } else {
            None
        }
    }

    /// The installer/`keymap` tag for this layout.
    pub fn tag(self) -> &'static str {
        match self {
            Layout::UsQwerty => "us",
            Layout::BeAzerty => "be-azerty",
            Layout::FrAzerty => "fr-azerty",
            Layout::DeQwertz => "de-qwertz",
        }
    }

    /// Human name.
    pub fn name(self) -> &'static str {
        match self {
            Layout::UsQwerty => "US QWERTY",
            Layout::BeAzerty => "Belgian AZERTY",
            Layout::FrAzerty => "French AZERTY",
            Layout::DeQwertz => "German QWERTZ",
        }
    }

    /// All shipped layouts.
    pub const ALL: [Layout; 4] = [Layout::UsQwerty, Layout::BeAzerty, Layout::FrAzerty, Layout::DeQwertz];
}

/// Control keys shared by every layout (same physical scancode everywhere).
fn control_key(sc: u8) -> Option<char> {
    match sc {
        0x01 => Some('\u{1b}'), // escape (every consumer matches on it: launcher, dialogs, prompts)
        0x0E => Some('\u{8}'), // backspace
        0x0F => Some('\t'),
        0x1C => Some('\r'), // enter
        0x39 => Some(' '),
        _ => None,
    }
}

/// The unshifted base character for a letter/punctuation scancode under `layout`.
/// Returns `None` for non-character scancodes (handled by [`control_key`]).
fn base(layout: Layout, sc: u8) -> Option<char> {
    // The letter positions differ between layouts; the digit row is the same
    // physically (its *shifted* meaning differs, handled below).
    use Layout::*;
    let c = match sc {
        // Digit row (unshifted digits on QWERTY/QWERTZ; on AZERTY these positions
        // are the accented/symbol characters, with the DIGITS on shift).
        0x02 => match layout {
            BeAzerty | FrAzerty => '&',
            _ => '1',
        },
        0x03 => match layout {
            BeAzerty => 'é',
            FrAzerty => 'é',
            _ => '2',
        },
        0x04 => match layout {
            BeAzerty | FrAzerty => '"',
            _ => '3',
        },
        0x05 => match layout {
            BeAzerty | FrAzerty => '\'',
            _ => '4',
        },
        0x06 => match layout {
            BeAzerty | FrAzerty => '(',
            _ => '5',
        },
        0x07 => match layout {
            BeAzerty => '§',
            FrAzerty => '-',
            _ => '6',
        },
        0x08 => match layout {
            BeAzerty | FrAzerty => 'è',
            _ => '7',
        },
        0x09 => match layout {
            BeAzerty | FrAzerty => '!',
            _ => '8',
        },
        0x0A => match layout {
            BeAzerty | FrAzerty => 'ç',
            _ => '9',
        },
        0x0B => match layout {
            BeAzerty | FrAzerty => 'à',
            _ => '0',
        },
        // Right of the digits: QWERTY - = ; Belgian AZERTY ) - ; French AZERTY ) = .
        0x0C => match layout {
            BeAzerty | FrAzerty => ')',
            _ => '-',
        },
        0x0D => match layout {
            BeAzerty => '-',
            _ => '=',
        },

        // Top letter row: QWERTY→ q w e r t y u i o p ; AZERTY→ a z e r t y u i o p ;
        // QWERTZ swaps y↔z.
        0x10 => match layout {
            BeAzerty | FrAzerty => 'a',
            _ => 'q',
        },
        0x11 => match layout {
            BeAzerty | FrAzerty => 'z',
            _ => 'w',
        },
        0x12 => 'e',
        0x13 => 'r',
        0x14 => 't',
        0x15 => match layout {
            DeQwertz => 'z',
            _ => 'y',
        },
        0x16 => 'u',
        0x17 => 'i',
        0x18 => 'o',
        0x19 => 'p',
        0x1A => '[',
        0x1B => ']',

        // Home row: QWERTY a s d f g h j k l ; AZERTY q s d f g h j k l m ; QWERTZ same as QWERTY.
        0x1E => match layout {
            BeAzerty | FrAzerty => 'q',
            _ => 'a',
        },
        0x1F => 's',
        0x20 => 'd',
        0x21 => 'f',
        0x22 => 'g',
        0x23 => 'h',
        0x24 => 'j',
        0x25 => 'k',
        0x26 => 'l',
        0x27 => match layout {
            BeAzerty | FrAzerty => 'm',
            _ => ';',
        },
        0x28 => '\'',
        0x29 => '`',
        0x2B => '\\',

        // Bottom row: QWERTY z x c v b n m ; AZERTY w x c v b n , ; QWERTZ y x c v b n m.
        0x2C => match layout {
            BeAzerty | FrAzerty => 'w',
            DeQwertz => 'y',
            _ => 'z',
        },
        0x2D => 'x',
        0x2E => 'c',
        0x2F => 'v',
        0x30 => 'b',
        0x31 => 'n',
        0x32 => match layout {
            BeAzerty | FrAzerty => ',',
            _ => 'm',
        },
        0x33 => match layout {
            BeAzerty | FrAzerty => ';',
            _ => ',',
        },
        0x34 => match layout {
            BeAzerty | FrAzerty => ':',
            _ => '.',
        },
        0x35 => match layout {
            BeAzerty => '=',
            FrAzerty => '!',
            _ => '/',
        },
        _ => return None,
    };
    Some(c)
}

/// The shifted form of a base character.
fn shifted(layout: Layout, sc: u8, base_ch: char) -> char {
    use Layout::*;
    if base_ch.is_ascii_alphabetic() {
        return base_ch.to_ascii_uppercase();
    }
    // On AZERTY the digit-row positions yield the DIGIT when shifted.
    if matches!(layout, BeAzerty | FrAzerty) {
        if let Some(d) = azerty_shift_digit(sc) {
            return d;
        }
        // The AZERTY punctuation keys have their own shifted forms: on the bottom
        // row , ; : = (BE) become ? . / + and right of the digits ) - become ° _.
        // These are what a Belgian keyboard types for a URL's dot and slash; the
        // US table below gave ':' for shift+';' and nothing usable for '.' or '/'.
        let be = match (layout, sc) {
            (_, 0x32) => Some('?'),
            (_, 0x33) => Some('.'),
            (_, 0x34) => Some('/'),
            (BeAzerty, 0x35) => Some('+'),
            (FrAzerty, 0x35) => Some('§'),
            (_, 0x0C) => Some('°'),
            (BeAzerty, 0x0D) => Some('_'),
            (FrAzerty, 0x0D) => Some('+'),
            _ => None,
        };
        if let Some(c) = be {
            return c;
        }
    }
    // Common US-ish shifted punctuation (also fine for QWERTZ letters).
    match base_ch {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '/' => '?',
        '.' => '>',
        ',' => '<',
        ';' => ':',
        '\'' => '"',
        '\\' => '|',
        '[' => '{',
        ']' => '}',
        '`' => '~',
        other => other,
    }
}

/// AZERTY digit-row: the digit is produced with Shift (1..9,0 on the top row).
fn azerty_shift_digit(sc: u8) -> Option<char> {
    let d = match sc {
        0x02 => '1',
        0x03 => '2',
        0x04 => '3',
        0x05 => '4',
        0x06 => '5',
        0x07 => '6',
        0x08 => '7',
        0x09 => '8',
        0x0A => '9',
        0x0B => '0',
        _ => return None,
    };
    Some(d)
}

/// Keys whose two levels are specific to one national layout: (unshifted,
/// shifted). Looked up before the shared tables; the US layout has none.
fn national(layout: Layout, sc: u8) -> Option<(char, char)> {
    use Layout::*;
    let p = match (layout, sc) {
        // The ISO key between left Shift and the first letter of the bottom row.
        (UsQwerty, 0x56) => ('\\', '|'),
        (_, 0x56) => ('<', '>'),
        (UsQwerty, _) => return None,

        // Belgian AZERTY (period variant).
        (BeAzerty, 0x29) => ('²', '³'),
        (BeAzerty, 0x1A) => ('^', '¨'),
        (BeAzerty, 0x1B) => ('$', '*'),
        (BeAzerty, 0x28) => ('ù', '%'),
        (BeAzerty, 0x2B) => ('µ', '£'),

        // French AZERTY.
        (FrAzerty, 0x29) => ('²', '²'),
        (FrAzerty, 0x09) => ('_', '8'),
        (FrAzerty, 0x1A) => ('^', '¨'),
        (FrAzerty, 0x1B) => ('$', '£'),
        (FrAzerty, 0x28) => ('ù', '%'),
        (FrAzerty, 0x2B) => ('*', 'µ'),

        // German QWERTZ.
        (DeQwertz, 0x29) => ('^', '°'),
        (DeQwertz, 0x02) => ('1', '!'),
        (DeQwertz, 0x03) => ('2', '"'),
        (DeQwertz, 0x04) => ('3', '§'),
        (DeQwertz, 0x05) => ('4', '$'),
        (DeQwertz, 0x06) => ('5', '%'),
        (DeQwertz, 0x07) => ('6', '&'),
        (DeQwertz, 0x08) => ('7', '/'),
        (DeQwertz, 0x09) => ('8', '('),
        (DeQwertz, 0x0A) => ('9', ')'),
        (DeQwertz, 0x0B) => ('0', '='),
        (DeQwertz, 0x0C) => ('ß', '?'),
        (DeQwertz, 0x0D) => ('´', '`'),
        (DeQwertz, 0x1A) => ('ü', 'Ü'),
        (DeQwertz, 0x1B) => ('+', '*'),
        (DeQwertz, 0x27) => ('ö', 'Ö'),
        (DeQwertz, 0x28) => ('ä', 'Ä'),
        (DeQwertz, 0x2B) => ('#', '\''),
        (DeQwertz, 0x33) => (',', ';'),
        (DeQwertz, 0x34) => ('.', ':'),
        (DeQwertz, 0x35) => ('-', '_'),
        _ => return None,
    };
    Some(p)
}

/// The AltGr (third) level of a key: the character printed bottom-right on the
/// key cap. `None` when the key has none; the US layout has no AltGr level.
pub fn level3(layout: Layout, sc: u8) -> Option<char> {
    use Layout::*;
    let c = match (layout, sc) {
        (UsQwerty, _) => return None,
        // The euro sign is on E in all three.
        (_, 0x12) => '€',

        (BeAzerty, 0x02) => '|',
        (BeAzerty, 0x03) => '@',
        (BeAzerty, 0x04) => '#',
        (BeAzerty, 0x07) => '^',
        (BeAzerty, 0x0A) => '{',
        (BeAzerty, 0x0B) => '}',
        (BeAzerty, 0x1A) => '[',
        (BeAzerty, 0x1B) => ']',
        (BeAzerty, 0x28) => '´',
        (BeAzerty, 0x2B) => '`',
        (BeAzerty, 0x35) => '~',
        (BeAzerty, 0x56) => '\\',

        (FrAzerty, 0x03) => '~',
        (FrAzerty, 0x04) => '#',
        (FrAzerty, 0x05) => '{',
        (FrAzerty, 0x06) => '[',
        (FrAzerty, 0x07) => '|',
        (FrAzerty, 0x08) => '`',
        (FrAzerty, 0x09) => '\\',
        (FrAzerty, 0x0A) => '^',
        (FrAzerty, 0x0B) => '@',
        (FrAzerty, 0x0C) => ']',
        (FrAzerty, 0x0D) => '}',
        (FrAzerty, 0x1B) => '¤',

        (DeQwertz, 0x03) => '²',
        (DeQwertz, 0x04) => '³',
        (DeQwertz, 0x08) => '{',
        (DeQwertz, 0x09) => '[',
        (DeQwertz, 0x0A) => ']',
        (DeQwertz, 0x0B) => '}',
        (DeQwertz, 0x0C) => '\\',
        (DeQwertz, 0x10) => '@',
        (DeQwertz, 0x1B) => '~',
        (DeQwertz, 0x32) => 'µ',
        (DeQwertz, 0x56) => '|',
        _ => return None,
    };
    Some(c)
}

/// [`translate`] with the AltGr level: while AltGr is held a key gives its
/// third-level character or nothing at all (never the base character), and
/// Shift is not looked at. Control keys (Esc, Tab, Enter, Backspace, Space)
/// are the same with or without AltGr.
pub fn translate_ex(layout: Layout, sc: u8, shift: bool, altgr: bool) -> Option<char> {
    if altgr {
        return control_key(sc).or_else(|| level3(layout, sc));
    }
    translate(layout, sc, shift)
}

/// Translate a make-code scancode to a character under `layout` + `shift`.
/// Break codes (bit 7 set) and modifier/non-character keys return `None`.
pub fn translate(layout: Layout, sc: u8, shift: bool) -> Option<char> {
    if let Some(c) = control_key(sc) {
        return Some(c);
    }
    if let Some((b, s)) = national(layout, sc) {
        return Some(if shift { s } else { b });
    }
    let b = base(layout, sc)?;
    if shift {
        Some(shifted(layout, sc, b))
    } else {
        Some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_tags() {
        assert_eq!(Layout::parse("be-azerty"), Some(Layout::BeAzerty));
        assert_eq!(Layout::parse("DE"), Some(Layout::DeQwertz));
        assert_eq!(Layout::parse("us"), Some(Layout::UsQwerty));
        assert_eq!(Layout::parse("xx"), None);
        assert_eq!(Layout::BeAzerty.tag(), "be-azerty");
    }

    #[test]
    fn qwerty_baseline() {
        // 0x10=Q 0x1E=A 0x2C=Z 0x15=Y
        assert_eq!(translate(Layout::UsQwerty, 0x10, false), Some('q'));
        assert_eq!(translate(Layout::UsQwerty, 0x1E, false), Some('a'));
        assert_eq!(translate(Layout::UsQwerty, 0x2C, false), Some('z'));
        assert_eq!(translate(Layout::UsQwerty, 0x15, false), Some('y'));
        assert_eq!(translate(Layout::UsQwerty, 0x10, true), Some('Q'));
        assert_eq!(translate(Layout::UsQwerty, 0x02, false), Some('1'));
        assert_eq!(translate(Layout::UsQwerty, 0x02, true), Some('!'));
    }

    #[test]
    fn belgian_azerty_punctuation() {
        // What a Belgian keyboard types for the characters of a URL.
        assert_eq!(translate(Layout::BeAzerty, 0x33, true), Some('.'));
        assert_eq!(translate(Layout::BeAzerty, 0x34, false), Some(':'));
        assert_eq!(translate(Layout::BeAzerty, 0x34, true), Some('/'));
        assert_eq!(translate(Layout::BeAzerty, 0x0D, false), Some('-'));
        assert_eq!(translate(Layout::BeAzerty, 0x0D, true), Some('_'));
        assert_eq!(translate(Layout::BeAzerty, 0x32, true), Some('?'));
        assert_eq!(translate(Layout::BeAzerty, 0x35, false), Some('='));
        assert_eq!(translate(Layout::BeAzerty, 0x0C, false), Some(')'));
        assert_eq!(translate(Layout::UsQwerty, 0x34, false), Some('.'));
        assert_eq!(translate(Layout::UsQwerty, 0x0C, false), Some('-'));
    }

    #[test]
    fn azerty_letter_transposition() {
        // The three signature AZERTY swaps: Q-pos→a, W-pos→z, A-pos→q, Z-pos→w, M-pos→,
        assert_eq!(translate(Layout::BeAzerty, 0x10, false), Some('a'));
        assert_eq!(translate(Layout::BeAzerty, 0x11, false), Some('z'));
        assert_eq!(translate(Layout::BeAzerty, 0x1E, false), Some('q'));
        assert_eq!(translate(Layout::BeAzerty, 0x2C, false), Some('w'));
        assert_eq!(translate(Layout::BeAzerty, 0x32, false), Some(','));
        // The 'm' key sits where QWERTY has ';'.
        assert_eq!(translate(Layout::FrAzerty, 0x27, false), Some('m'));
        assert_eq!(translate(Layout::FrAzerty, 0x27, true), Some('M'));
    }

    #[test]
    fn azerty_digit_row_needs_shift() {
        // Unshifted top row = symbols/accents; shift = the digit.
        assert_eq!(translate(Layout::BeAzerty, 0x02, false), Some('&'));
        assert_eq!(translate(Layout::BeAzerty, 0x02, true), Some('1'));
        assert_eq!(translate(Layout::BeAzerty, 0x03, false), Some('é'));
        assert_eq!(translate(Layout::BeAzerty, 0x03, true), Some('2'));
        assert_eq!(translate(Layout::BeAzerty, 0x0B, false), Some('à'));
        assert_eq!(translate(Layout::BeAzerty, 0x0B, true), Some('0'));
    }

    #[test]
    fn qwertz_swaps_y_and_z() {
        // German QWERTZ: Y-pos→z, Z-pos→y; letters otherwise QWERTY.
        assert_eq!(translate(Layout::DeQwertz, 0x15, false), Some('z'));
        assert_eq!(translate(Layout::DeQwertz, 0x2C, false), Some('y'));
        assert_eq!(translate(Layout::DeQwertz, 0x2C, true), Some('Y'));
        assert_eq!(translate(Layout::DeQwertz, 0x10, false), Some('q'));
    }

    /// Every scancode of a layout that gives `want` with AltGr.
    fn altgr_keys(l: Layout, want: char) -> Vec<u8> {
        (0u8..0x80).filter(|&sc| translate_ex(l, sc, false, true) == Some(want)).collect()
    }

    #[test]
    fn altgr_belgian_azerty() {
        let l = Layout::BeAzerty;
        // The key caps of a Belgian keyboard: & e-acute " section c-cedilla
        // a-grave on the digit row, ^ $ right of P, = left of right Shift,
        // < left of W, and E.
        for (sc, c) in [(0x02, '|'), (0x03, '@'), (0x04, '#'), (0x07, '^'), (0x0A, '{'), (0x0B, '}'),
                        (0x1A, '['), (0x1B, ']'), (0x56, '\\'), (0x35, '~'), (0x12, '€'), (0x2B, '`')] {
            assert_eq!(translate_ex(l, sc, false, true), Some(c), "be AltGr+{sc:#x}");
        }
        // Each of the ten required characters is on exactly one key.
        for c in ['@', '#', '{', '}', '[', ']', '\\', '|', '€', '~'] {
            assert_eq!(altgr_keys(l, c).len(), 1, "be: {c}");
        }
    }

    #[test]
    fn altgr_french_azerty() {
        let l = Layout::FrAzerty;
        for (sc, c) in [(0x03, '~'), (0x04, '#'), (0x05, '{'), (0x06, '['), (0x07, '|'), (0x08, '`'),
                        (0x09, '\\'), (0x0A, '^'), (0x0B, '@'), (0x0C, ']'), (0x0D, '}'), (0x12, '€')] {
            assert_eq!(translate_ex(l, sc, false, true), Some(c), "fr AltGr+{sc:#x}");
        }
        for c in ['@', '#', '{', '}', '[', ']', '\\', '|', '€', '~'] {
            assert_eq!(altgr_keys(l, c).len(), 1, "fr: {c}");
        }
    }

    #[test]
    fn altgr_german_qwertz() {
        let l = Layout::DeQwertz;
        for (sc, c) in [(0x10, '@'), (0x08, '{'), (0x09, '['), (0x0A, ']'), (0x0B, '}'), (0x0C, '\\'),
                        (0x56, '|'), (0x1B, '~'), (0x12, '€'), (0x03, '²'), (0x04, '³'), (0x32, 'µ')] {
            assert_eq!(translate_ex(l, sc, false, true), Some(c), "de AltGr+{sc:#x}");
        }
        for c in ['@', '{', '}', '[', ']', '\\', '|', '€', '~'] {
            assert_eq!(altgr_keys(l, c).len(), 1, "de: {c}");
        }
        // '#' is a key of its own on QWERTZ (left of Enter), not an AltGr character.
        assert_eq!(altgr_keys(l, '#'), Vec::<u8>::new());
        assert_eq!(translate(l, 0x2B, false), Some('#'));
        assert_eq!(translate(l, 0x2B, true), Some('\''));
    }

    #[test]
    fn altgr_never_gives_the_base_character() {
        for l in Layout::ALL {
            for sc in 0u8..0x80 {
                let plain = translate(l, sc, false);
                let third = translate_ex(l, sc, false, true);
                if control_key(sc).is_some() {
                    assert_eq!(third, plain, "{l:?} control key {sc:#x}");
                    continue;
                }
                // A key without a third level is silent under AltGr.
                if let Some(c) = third {
                    assert_ne!(Some(c), plain, "{l:?} {sc:#x}: AltGr repeats the base character");
                    assert_eq!(level3(l, sc), Some(c));
                }
                // translate_ex without AltGr is translate.
                assert_eq!(translate_ex(l, sc, false, false), plain);
                assert_eq!(translate_ex(l, sc, true, false), translate(l, sc, true));
            }
        }
        // The US layout has no AltGr level at all.
        assert!((0u8..0x80).all(|sc| level3(Layout::UsQwerty, sc).is_none()));
        assert_eq!(translate_ex(Layout::UsQwerty, 0x1E, false, true), None);
    }

    #[test]
    fn iso_key_left_of_the_bottom_row() {
        assert_eq!(translate(Layout::BeAzerty, 0x56, false), Some('<'));
        assert_eq!(translate(Layout::BeAzerty, 0x56, true), Some('>'));
        assert_eq!(translate(Layout::FrAzerty, 0x56, false), Some('<'));
        assert_eq!(translate(Layout::DeQwertz, 0x56, true), Some('>'));
        assert_eq!(translate(Layout::UsQwerty, 0x56, false), Some('\\'));
        assert_eq!(translate(Layout::UsQwerty, 0x56, true), Some('|'));
    }

    #[test]
    fn french_digit_row_and_right_hand_keys() {
        let l = Layout::FrAzerty;
        assert_eq!(translate(l, 0x07, false), Some('-'));
        assert_eq!(translate(l, 0x09, false), Some('_'));
        assert_eq!(translate(l, 0x09, true), Some('8'));
        assert_eq!(translate(l, 0x1B, false), Some('$'));
        assert_eq!(translate(l, 0x2B, false), Some('*'));
        assert_eq!(translate(l, 0x28, true), Some('%'));
        assert_eq!(translate(l, 0x35, false), Some('!'));
    }

    #[test]
    fn belgian_right_hand_keys() {
        let l = Layout::BeAzerty;
        assert_eq!(translate(l, 0x09, false), Some('!'));
        assert_eq!(translate(l, 0x09, true), Some('8'));
        assert_eq!(translate(l, 0x1A, false), Some('^'));
        assert_eq!(translate(l, 0x1B, false), Some('$'));
        assert_eq!(translate(l, 0x1B, true), Some('*'));
        assert_eq!(translate(l, 0x28, false), Some('ù'));
        assert_eq!(translate(l, 0x28, true), Some('%'));
        assert_eq!(translate(l, 0x2B, false), Some('µ'));
    }

    #[test]
    fn german_punctuation() {
        let l = Layout::DeQwertz;
        for (sc, b, s) in [(0x02, '1', '!'), (0x03, '2', '"'), (0x04, '3', '§'), (0x05, '4', '$'), (0x07, '6', '&'),
                           (0x08, '7', '/'), (0x09, '8', '('), (0x0A, '9', ')'), (0x0B, '0', '='), (0x0C, 'ß', '?'),
                           (0x1A, 'ü', 'Ü'), (0x1B, '+', '*'), (0x27, 'ö', 'Ö'), (0x28, 'ä', 'Ä'),
                           (0x33, ',', ';'), (0x34, '.', ':'), (0x35, '-', '_'), (0x29, '^', '°')] {
            assert_eq!(translate(l, sc, false), Some(b), "de {sc:#x}");
            assert_eq!(translate(l, sc, true), Some(s), "de shift+{sc:#x}");
        }
    }

    #[test]
    fn control_keys_layout_independent() {
        for l in Layout::ALL {
            assert_eq!(translate(l, 0x39, false), Some(' '));
            assert_eq!(translate(l, 0x1C, false), Some('\r'));
            assert_eq!(translate(l, 0x0E, false), Some('\u{8}'));
            assert_eq!(translate(l, 0x01, false), Some('\u{1b}'));
            assert_eq!(translate(l, 0x01, true), Some('\u{1b}'));
            // Function keys are not characters in any layout.
            assert_eq!(translate(l, 0x3B, false), None);
            assert_eq!(translate(l, 0x3C, false), None);
        }
    }
}
