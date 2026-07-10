//! **EuroKeymap** — translate PS/2 **scancode set 1** into characters for the
//! keyboard layouts EU users actually have: US-QWERTY, BE/FR-AZERTY and
//! DE-QWERTZ. The active layout is chosen at install time (3F-4) and by the
//! `keymap` shell command; the kernel PS/2 driver calls [`translate`].
//!
//! Honest scope: the **alphanumeric block** (letters, the digit row, the common
//! punctuation keys, space/enter/tab/backspace) is mapped per layout, including
//! the AZERTY digit-row shift inversion. Full dead-key composition and the
//! complete AltGr symbol planes are simplified — documented as remaining.

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
        0x0C => '-',
        0x0D => '=',

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
            BeAzerty | FrAzerty => '=',
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

/// Translate a make-code scancode to a character under `layout` + `shift`.
/// Break codes (bit 7 set) and modifier/non-character keys return `None`.
pub fn translate(layout: Layout, sc: u8, shift: bool) -> Option<char> {
    if let Some(c) = control_key(sc) {
        return Some(c);
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

    #[test]
    fn control_keys_layout_independent() {
        for l in Layout::ALL {
            assert_eq!(translate(l, 0x39, false), Some(' '));
            assert_eq!(translate(l, 0x1C, false), Some('\r'));
            assert_eq!(translate(l, 0x0E, false), Some('\u{8}'));
        }
    }
}
