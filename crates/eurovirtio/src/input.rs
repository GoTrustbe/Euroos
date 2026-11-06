//! virtio-input (spec 5.8): the device sends Linux evdev events
//! `{le16 type, le16 code, le32 value}` on its event queue. This module turns
//! them into what the EuroOS input layer already consumes: PS/2 set-1
//! scancodes for keys and one pointer update per `SYN_REPORT` (absolute
//! 0..=0x7FFF like the USB tablet, or relative).

/// virtio_input_config select values (spec 5.8.4).
pub const CFG_ID_NAME: u8 = 0x01;
pub const CFG_EV_BITS: u8 = 0x11;
pub const CFG_ABS_INFO: u8 = 0x12;
/// Config layout: select @0, subsel @1, size @2, data @8.
pub const CFG_SELECT: u64 = 0;
pub const CFG_SUBSEL: u64 = 1;
pub const CFG_SIZE: u64 = 2;
pub const CFG_DATA: u64 = 8;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;
pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_WHEEL: u16 = 8;
pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_TOUCH: u16 = 0x14A;
pub const KEY_A: u16 = 30;

/// One evdev event as it lies in the device-written buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ty: u16,
    pub code: u16,
    pub value: i32,
}

impl Event {
    pub fn parse(b: &[u8]) -> Option<Event> {
        if b.len() < 8 {
            return None;
        }
        Some(Event {
            ty: u16::from_le_bytes([b[0], b[1]]),
            code: u16::from_le_bytes([b[2], b[3]]),
            value: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        })
    }
}

/// What a device is, from its EV_BITS bitmaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tablet,
    Mouse,
    Keyboard,
    Other,
}

/// Classify from the EV_ABS / EV_REL / EV_KEY bitmaps (byte arrays, bit n =
/// code n, as the device config returns them).
pub fn classify(abs_bits: &[u8], rel_bits: &[u8], key_bits: &[u8]) -> Kind {
    let bit = |m: &[u8], n: u16| m.get(n as usize / 8).map_or(false, |b| b & (1 << (n % 8)) != 0);
    if bit(abs_bits, ABS_X) && bit(abs_bits, ABS_Y) {
        Kind::Tablet
    } else if bit(rel_bits, REL_X) && bit(rel_bits, REL_Y) {
        Kind::Mouse
    } else if bit(key_bits, KEY_A) {
        Kind::Keyboard
    } else {
        Kind::Other
    }
}

/// evdev key code → PS/2 set-1 make code; `true` = 0xE0-extended.
/// Codes 1..=83 are numerically the set-1 make codes (Linux took them from it).
pub fn key_to_set1(code: u16) -> Option<(bool, u8)> {
    Some(match code {
        1..=83 => (false, code as u8),
        86 => (false, 0x56), // 102nd key
        87 => (false, 0x57), // F11
        88 => (false, 0x58), // F12
        96 => (true, 0x1C),  // keypad Enter
        97 => (true, 0x1D),  // right Ctrl
        98 => (true, 0x35),  // keypad /
        100 => (true, 0x38), // right Alt
        102 => (true, 0x47), // Home
        103 => (true, 0x48), // Up
        104 => (true, 0x49), // PageUp
        105 => (true, 0x4B), // Left
        106 => (true, 0x4D), // Right
        107 => (true, 0x4F), // End
        108 => (true, 0x50), // Down
        109 => (true, 0x51), // PageDown
        110 => (true, 0x52), // Insert
        111 => (true, 0x53), // Delete
        125 => (true, 0x5B), // left Meta
        126 => (true, 0x5C), // right Meta
        127 => (true, 0x5D), // Menu
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A key: set-1 make code (break = code | 0x80), extended = 0xE0 prefix.
    Key { extended: bool, code: u8, pressed: bool },
    /// One pointer report. `abs` is scaled to 0..=0x7FFF; buttons in HID order
    /// (bit0 left, bit1 right, bit2 middle).
    Pointer { abs: Option<(u16, u16)>, dx: i32, dy: i32, wheel: i32, buttons: u8 },
}

/// Axis range from CFG_ABS_INFO (min, max).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbsRange {
    pub min: i32,
    pub max: i32,
}

impl AbsRange {
    pub const TABLET: AbsRange = AbsRange { min: 0, max: 0x7FFF };
    pub fn scale(&self, v: i32) -> u16 {
        let span = (self.max as i64 - self.min as i64).max(1);
        let v = (v as i64).clamp(self.min as i64, self.max as i64) - self.min as i64;
        (v * 0x7FFF / span) as u16
    }
}

/// Per-device event decoder (keeps the state between SYN_REPORTs).
#[derive(Clone, Copy, Debug)]
pub struct Decoder {
    pub x_range: AbsRange,
    pub y_range: AbsRange,
    abs_x: Option<i32>,
    abs_y: Option<i32>,
    last_x: i32,
    last_y: i32,
    dx: i32,
    dy: i32,
    wheel: i32,
    buttons: u8,
    dirty: bool,
}

impl Decoder {
    pub const fn new(x_range: AbsRange, y_range: AbsRange) -> Decoder {
        Decoder { x_range, y_range, abs_x: None, abs_y: None, last_x: 0, last_y: 0, dx: 0, dy: 0, wheel: 0, buttons: 0, dirty: false }
    }

    pub fn feed(&mut self, ev: Event) -> Option<Action> {
        match ev.ty {
            EV_KEY => {
                let bit = match ev.code {
                    BTN_LEFT | BTN_TOUCH => Some(1u8),
                    BTN_RIGHT => Some(2),
                    BTN_MIDDLE => Some(4),
                    _ => None,
                };
                if let Some(b) = bit {
                    if ev.value != 0 {
                        self.buttons |= b;
                    } else {
                        self.buttons &= !b;
                    }
                    self.dirty = true;
                    return None;
                }
                let (extended, code) = key_to_set1(ev.code)?;
                // value: 0 release, 1 press, 2 autorepeat (a repeated make).
                Some(Action::Key { extended, code, pressed: ev.value != 0 })
            }
            EV_REL => {
                match ev.code {
                    REL_X => self.dx += ev.value,
                    REL_Y => self.dy += ev.value,
                    REL_WHEEL => self.wheel += ev.value,
                    _ => return None,
                }
                self.dirty = true;
                None
            }
            EV_ABS => {
                match ev.code {
                    ABS_X => self.abs_x = Some(ev.value),
                    ABS_Y => self.abs_y = Some(ev.value),
                    _ => return None,
                }
                self.dirty = true;
                None
            }
            EV_SYN if ev.code == SYN_REPORT => {
                if !self.dirty {
                    return None;
                }
                let abs = if self.abs_x.is_some() || self.abs_y.is_some() {
                    // An axis that did not move keeps its last position.
                    let x = self.abs_x.unwrap_or(self.last_x);
                    let y = self.abs_y.unwrap_or(self.last_y);
                    self.last_x = x;
                    self.last_y = y;
                    Some((self.x_range.scale(x), self.y_range.scale(y)))
                } else {
                    None
                };
                let a = Action::Pointer { abs, dx: self.dx, dy: self.dy, wheel: self.wheel, buttons: self.buttons };
                self.abs_x = None;
                self.abs_y = None;
                self.dx = 0;
                self.dy = 0;
                self.wheel = 0;
                self.dirty = false;
                Some(a)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ty: u16, code: u16, value: i32) -> Event {
        Event { ty, code, value }
    }

    #[test]
    fn parse_event_bytes() {
        let b = [3, 0, 1, 0, 0xFF, 0x3F, 0, 0];
        assert_eq!(Event::parse(&b), Some(ev(EV_ABS, ABS_Y, 0x3FFF)));
        assert_eq!(Event::parse(&b[..7]), None);
    }

    #[test]
    fn tablet_move_and_click() {
        let mut d = Decoder::new(AbsRange::TABLET, AbsRange::TABLET);
        assert_eq!(d.feed(ev(EV_ABS, ABS_X, 16384)), None);
        assert_eq!(d.feed(ev(EV_ABS, ABS_Y, 32767)), None);
        assert_eq!(
            d.feed(ev(EV_SYN, SYN_REPORT, 0)),
            Some(Action::Pointer { abs: Some((16384, 0x7FFF)), dx: 0, dy: 0, wheel: 0, buttons: 0 })
        );
        // Only Y moves: X stays where it was.
        d.feed(ev(EV_ABS, ABS_Y, 0));
        d.feed(ev(EV_KEY, BTN_LEFT, 1));
        assert_eq!(
            d.feed(ev(EV_SYN, SYN_REPORT, 0)),
            Some(Action::Pointer { abs: Some((16384, 0)), dx: 0, dy: 0, wheel: 0, buttons: 1 })
        );
        // Button release alone still reports the position.
        d.feed(ev(EV_KEY, BTN_LEFT, 0));
        assert_eq!(
            d.feed(ev(EV_SYN, SYN_REPORT, 0)),
            Some(Action::Pointer { abs: None, dx: 0, dy: 0, wheel: 0, buttons: 0 })
        );
        // An empty report yields nothing.
        assert_eq!(d.feed(ev(EV_SYN, SYN_REPORT, 0)), None);
    }

    #[test]
    fn scaled_range() {
        let r = AbsRange { min: 0, max: 4095 };
        assert_eq!(r.scale(4095), 0x7FFF);
        assert_eq!(r.scale(0), 0);
        assert_eq!(r.scale(99999), 0x7FFF);
        assert_eq!(r.scale(-5), 0);
        assert_eq!(AbsRange::TABLET.scale(32767), 32767);
    }

    #[test]
    fn relative_mouse() {
        let mut d = Decoder::new(AbsRange::TABLET, AbsRange::TABLET);
        d.feed(ev(EV_REL, REL_X, 3));
        d.feed(ev(EV_REL, REL_X, 2));
        d.feed(ev(EV_REL, REL_Y, -1));
        d.feed(ev(EV_KEY, BTN_RIGHT, 1));
        assert_eq!(
            d.feed(ev(EV_SYN, SYN_REPORT, 0)),
            Some(Action::Pointer { abs: None, dx: 5, dy: -1, wheel: 0, buttons: 2 })
        );
    }

    #[test]
    fn keyboard_codes() {
        let mut d = Decoder::new(AbsRange::TABLET, AbsRange::TABLET);
        assert_eq!(d.feed(ev(EV_KEY, KEY_A, 1)), Some(Action::Key { extended: false, code: 0x1E, pressed: true }));
        assert_eq!(d.feed(ev(EV_KEY, KEY_A, 0)), Some(Action::Key { extended: false, code: 0x1E, pressed: false }));
        assert_eq!(d.feed(ev(EV_KEY, 28, 2)), Some(Action::Key { extended: false, code: 0x1C, pressed: true })); // Enter repeat
        assert_eq!(d.feed(ev(EV_KEY, 103, 1)), Some(Action::Key { extended: true, code: 0x48, pressed: true })); // Up
        assert_eq!(d.feed(ev(EV_KEY, 0x2FF, 1)), None);
        assert_eq!(key_to_set1(1), Some((false, 0x01)));
        assert_eq!(key_to_set1(57), Some((false, 0x39))); // space
        assert_eq!(key_to_set1(42), Some((false, 0x2A))); // left shift
        assert_eq!(key_to_set1(29), Some((false, 0x1D))); // left ctrl
    }

    #[test]
    fn classification() {
        let abs = [0b11u8];
        let none = [0u8; 16];
        let mut keys = [0u8; 16];
        keys[KEY_A as usize / 8] |= 1 << (KEY_A % 8);
        assert_eq!(classify(&abs, &none, &none), Kind::Tablet);
        assert_eq!(classify(&none, &[0b11], &none), Kind::Mouse);
        assert_eq!(classify(&none, &none, &keys), Kind::Keyboard);
        assert_eq!(classify(&[], &[], &[]), Kind::Other);
    }
}
