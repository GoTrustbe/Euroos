//! EuroUSB — USB descriptor parsing + HID boot protocol core (plan I1).
//!
//! Modern machines have no PS/2; without USB HID, EuroOS cannot receive input from
//! them. The xHCI controller driver (the hardware layer) delivers raw USB transfers;
//! this module is the architecture-independent core above it: the **parsing of
//! USB descriptors** (device/configuration/interface/endpoint) to recognize a device,
//! and the **HID boot protocol** that translates the 8-byte keyboard and mouse
//! reports into input events. Pure `no_std` logic → host-tested, independent of
//! any controller.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

// Descriptor types.
const DT_DEVICE: u8 = 1;
const DT_CONFIG: u8 = 2;
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;

// USB classes.
pub const CLASS_HID: u8 = 0x03;
pub const HID_SUBCLASS_BOOT: u8 = 0x01;
pub const HID_PROTOCOL_KEYBOARD: u8 = 0x01;
pub const HID_PROTOCOL_MOUSE: u8 = 0x02;

/// The USB device descriptor (18 bytes) — identity + class of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<DeviceDescriptor> {
        if b.len() < 18 || b[0] < 18 || b[1] != DT_DEVICE {
            return None;
        }
        Some(DeviceDescriptor {
            usb_version: u16::from_le_bytes([b[2], b[3]]),
            class: b[4],
            subclass: b[5],
            protocol: b[6],
            max_packet0: b[7],
            vendor: u16::from_le_bytes([b[8], b[9]]),
            product: u16::from_le_bytes([b[10], b[11]]),
            num_configurations: b[17],
        })
    }
}

/// An endpoint descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8, // bit7 = IN direction
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }
}

/// An interface + its endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
}

impl Interface {
    /// Is this a HID boot keyboard? (class 3, subclass boot, protocol 1)
    pub fn is_boot_keyboard(&self) -> bool {
        self.class == CLASS_HID && self.subclass == HID_SUBCLASS_BOOT && self.protocol == HID_PROTOCOL_KEYBOARD
    }
    /// Is this a HID boot mouse?
    pub fn is_boot_mouse(&self) -> bool {
        self.class == CLASS_HID && self.subclass == HID_SUBCLASS_BOOT && self.protocol == HID_PROTOCOL_MOUSE
    }
    /// Is this a HID absolute pointer (usb-tablet / touchscreen)? These report
    /// under the report protocol (not boot), so subclass/protocol are 0. Used to
    /// get exact, drift-free cursor tracking (e.g. over VNC).
    pub fn is_hid_absolute_pointer(&self) -> bool {
        self.class == CLASS_HID && self.subclass == 0 && self.protocol == 0
    }
}

/// A parsed configuration: all interfaces + endpoints from the config block
/// (config descriptor followed by interface/endpoint descriptors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

impl Configuration {
    /// Parse a complete configuration block (walks the chained descriptors).
    pub fn parse(b: &[u8]) -> Option<Configuration> {
        if b.len() < 9 || b[1] != DT_CONFIG {
            return None;
        }
        let total = u16::from_le_bytes([b[2], b[3]]) as usize;
        let value = b[5];
        let mut interfaces: Vec<Interface> = Vec::new();
        let mut p = b[0] as usize; // after the config descriptor itself
        let end = total.min(b.len());
        while p + 2 <= end {
            let len = b[p] as usize;
            let dtype = b[p + 1];
            if len == 0 || p + len > end {
                break;
            }
            match dtype {
                DT_INTERFACE if len >= 9 => {
                    interfaces.push(Interface {
                        number: b[p + 2],
                        class: b[p + 5],
                        subclass: b[p + 6],
                        protocol: b[p + 7],
                        endpoints: Vec::new(),
                    });
                }
                DT_ENDPOINT if len >= 7 => {
                    if let Some(iface) = interfaces.last_mut() {
                        iface.endpoints.push(Endpoint {
                            address: b[p + 2],
                            attributes: b[p + 3],
                            max_packet: u16::from_le_bytes([b[p + 4], b[p + 5]]),
                            interval: b[p + 6],
                        });
                    }
                }
                _ => {} // skip class-specific descriptors (e.g. HID)
            }
            p += len;
        }
        Some(Configuration { value, interfaces })
    }
}

/// A keyboard input event from a HID boot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub keycode: u8, // USB HID usage id
    pub pressed: bool,
    pub modifiers: u8, // bit0=LCtrl,1=LShift,2=LAlt,3=LGui,4=RCtrl,…
}

/// HID boot keyboard: compares two consecutive 8-byte reports and emits the
/// press/release events (report = `[modifiers][reserved][6× keycode]`). A
/// stateful decoder so repeated keys don't count twice.
#[derive(Default)]
pub struct BootKeyboard {
    prev: [u8; 6],
}

impl BootKeyboard {
    pub fn new() -> Self {
        BootKeyboard { prev: [0; 6] }
    }

    /// Feed a new 8-byte report; emit the press/release events since the previous one.
    pub fn feed(&mut self, report: &[u8]) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        if report.len() < 8 {
            return events;
        }
        let mods = report[0];
        let cur = [report[2], report[3], report[4], report[5], report[6], report[7]];
        // Pressed: in `cur` but not in `prev`.
        for &k in cur.iter() {
            if k != 0 && !self.prev.contains(&k) {
                events.push(KeyEvent { keycode: k, pressed: true, modifiers: mods });
            }
        }
        // Released: in `prev` but not in `cur`.
        for &k in self.prev.iter() {
            if k != 0 && !cur.contains(&k) {
                events.push(KeyEvent { keycode: k, pressed: false, modifiers: mods });
            }
        }
        self.prev = cur;
        events
    }
}

/// A mouse movement event from a HID boot report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub buttons: u8, // bit0=left,1=right,2=middle
    pub dx: i8,
    pub dy: i8,
    pub wheel: i8,
}

/// Decode a HID boot mouse report (`[buttons][dx][dy]` (+wheel)).
pub fn parse_mouse(report: &[u8]) -> Option<MouseEvent> {
    if report.len() < 3 {
        return None;
    }
    Some(MouseEvent {
        buttons: report[0],
        dx: report[1] as i8,
        dy: report[2] as i8,
        wheel: report.get(3).map(|&w| w as i8).unwrap_or(0),
    })
}

/// An absolute-pointer report: `buttons` + `x`/`y` in the device's logical range
/// (0..=0x7FFF for the QEMU usb-tablet and most touchscreens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsPointerEvent {
    pub buttons: u8,
    pub x: u16,
    pub y: u16,
}

/// Decode a usb-tablet / absolute-pointer report:
/// `[buttons, x_lo, x_hi, y_lo, y_hi, (wheel)]`, X/Y little-endian in 0..=0x7FFF.
pub fn parse_tablet(report: &[u8]) -> Option<AbsPointerEvent> {
    if report.len() < 5 {
        return None;
    }
    Some(AbsPointerEvent {
        buttons: report[0],
        x: (report[1] as u16) | ((report[2] as u16) << 8),
        y: (report[3] as u16) | ((report[4] as u16) << 8),
    })
}

// USB Mass Storage class (plan I1 — USB disk).
pub const CLASS_MASS_STORAGE: u8 = 0x08;
pub const MSC_SUBCLASS_SCSI: u8 = 0x06;
pub const MSC_PROTOCOL_BOT: u8 = 0x50; // Bulk-Only Transport

impl Interface {
    /// Is this a SCSI Bulk-Only-Transport mass-storage interface? (USB disk)
    pub fn is_mass_storage_bot(&self) -> bool {
        self.class == CLASS_MASS_STORAGE
            && self.subclass == MSC_SUBCLASS_SCSI
            && self.protocol == MSC_PROTOCOL_BOT
    }
}

/// Bulk-Only Transport (BOT) + SCSI — the byte layer of a USB disk. Pure
/// `no_std` builders/parsers (host-tested); the xHCI bulk transport drives them.
pub mod bot {
    /// Build a 31-byte Command Block Wrapper (CBW). `data_len` = expected data-
    /// phase length, `in_dir` = data device→host, `cdb` = the SCSI command block.
    pub fn cbw(tag: u32, data_len: u32, in_dir: bool, lun: u8, cdb: &[u8]) -> [u8; 31] {
        let mut b = [0u8; 31];
        b[0..4].copy_from_slice(&0x4342_5355u32.to_le_bytes()); // "USBC"
        b[4..8].copy_from_slice(&tag.to_le_bytes());
        b[8..12].copy_from_slice(&data_len.to_le_bytes());
        b[12] = if in_dir { 0x80 } else { 0x00 };
        b[13] = lun & 0x0F;
        b[14] = cdb.len().min(16) as u8;
        b[15..15 + cdb.len().min(16)].copy_from_slice(&cdb[..cdb.len().min(16)]);
        b
    }

    /// Parse a 13-byte Command Status Wrapper (CSW). Returns (tag, residue, status)
    /// if the signature matches (0 = success, 1 = failure, 2 = phase-error).
    pub fn parse_csw(b: &[u8]) -> Option<(u32, u32, u8)> {
        if b.len() < 13 || u32::from_le_bytes([b[0], b[1], b[2], b[3]]) != 0x5342_5355 {
            return None; // "USBS"
        }
        Some((
            u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            b[12],
        ))
    }

    /// SCSI INQUIRY (6-byte CDB) — requests 36 bytes of device identity.
    pub fn inquiry() -> [u8; 6] {
        [0x12, 0, 0, 0, 36, 0]
    }

    /// SCSI TEST UNIT READY (6-byte CDB) — polls whether the medium is ready.
    pub fn test_unit_ready() -> [u8; 6] {
        [0x00, 0, 0, 0, 0, 0]
    }

    /// SCSI READ CAPACITY(10) (10-byte CDB) — returns (last LBA, block size).
    pub fn read_capacity10() -> [u8; 10] {
        [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    }

    /// Decode an 8-byte READ CAPACITY(10) response: (last LBA, block size), big-endian.
    pub fn parse_capacity(b: &[u8]) -> Option<(u32, u32)> {
        if b.len() < 8 {
            return None;
        }
        Some((
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            u32::from_be_bytes([b[4], b[5], b[6], b[7]]),
        ))
    }

    /// SCSI READ(10) (10-byte CDB) — read `count` blocks from `lba` (big-endian fields).
    pub fn read10(lba: u32, count: u16) -> [u8; 10] {
        let l = lba.to_be_bytes();
        let c = count.to_be_bytes();
        [0x28, 0, l[0], l[1], l[2], l[3], 0, c[0], c[1], 0]
    }

    /// SCSI WRITE(10) (10-byte CDB) — write `count` blocks from `lba`.
    pub fn write10(lba: u32, count: u16) -> [u8; 10] {
        let l = lba.to_be_bytes();
        let c = count.to_be_bytes();
        [0x2A, 0, l[0], l[1], l[2], l[3], 0, c[0], c[1], 0]
    }
}

// ── HID report-descriptor parsing (Metal M4-2) ───────────────────────────────
//
// Real keyboards, mice, touchpads and tablets describe their input reports in
// a HID *report descriptor*; only keyboards/mice guarantee the fixed "boot
// protocol" layout. This parser walks the descriptor's short items and builds
// an [`InputMap`]: where X, Y, wheel and the buttons live (bit offset/size),
// whether X/Y are relative or absolute, the logical maximum (for scaling
// absolute coordinates) and the report id — so the driver can decode input
// from arbitrary pointing devices instead of assuming a layout.

/// Location of one numeric field inside an input report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidField {
    pub bit_off: u16,
    pub bit_size: u8,
    pub relative: bool,
    pub logical_max: i32,
}

/// Decoded layout of one pointing-device input report.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputMap {
    pub report_id: Option<u8>, // reports start with this id byte when Some
    pub x: Option<HidField>,
    pub y: Option<HidField>,
    pub wheel: Option<HidField>,
    pub buttons_off: u16, // bit offset of button 1
    pub buttons_n: u8,    // number of button bits (0 = none found)
}

/// Extract an unsigned little-endian bit field from a report.
pub fn extract_bits(report: &[u8], bit_off: u16, bit_size: u8) -> u32 {
    let mut v: u32 = 0;
    for i in 0..bit_size as u16 {
        let bit = bit_off + i;
        let byte = (bit / 8) as usize;
        if byte >= report.len() {
            break;
        }
        if report[byte] >> (bit % 8) & 1 != 0 {
            v |= 1 << i;
        }
    }
    v
}

/// Extract a field with sign extension (relative deltas are signed).
pub fn extract_signed(report: &[u8], f: &HidField) -> i32 {
    let raw = extract_bits(report, f.bit_off, f.bit_size);
    if f.bit_size < 32 && raw & (1 << (f.bit_size - 1)) != 0 {
        (raw as i32) - (1i32 << f.bit_size)
    } else {
        raw as i32
    }
}

/// Parse a HID report descriptor into an [`InputMap`]. Returns `None` when no
/// X/Y usage pair is found (not a pointing device). Handles report ids (the
/// map binds to the report that carries X); usages we don't track only
/// advance the bit cursor.
pub fn parse_report_descriptor(d: &[u8]) -> Option<InputMap> {
    const UP_GENERIC_DESKTOP: u32 = 0x01;
    const UP_BUTTON: u32 = 0x09;
    const U_X: u32 = 0x30;
    const U_Y: u32 = 0x31;
    const U_WHEEL: u32 = 0x38;

    let mut map = InputMap::default();
    let mut usage_page: u32 = 0;
    let mut report_size: u32 = 0;
    let mut report_count: u32 = 0;
    let mut logical_max: i32 = 0;
    let mut cur_id: Option<u8> = None;
    // Per-report-id bit cursors — descriptors may interleave report ids.
    let mut cursors: heapless_cursors::Cursors = Default::default();
    let mut usages: [u32; 16] = [0; 16]; // full usage: page<<16 | usage
    let mut n_usages = 0usize;
    // Local Usage Minimum/Maximum: how ranges (e.g. Button 1..N) are declared.
    let mut usage_min: Option<u32> = None;

    let mut i = 0usize;
    while i < d.len() {
        let prefix = d[i];
        if prefix == 0xFE {
            // Long item: skip over its payload.
            let sz = *d.get(i + 1)? as usize;
            i += 3 + sz;
            continue;
        }
        let bsize = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let btype = (prefix >> 2) & 0x03;
        let btag = prefix >> 4;
        if i + 1 + bsize > d.len() {
            return None;
        }
        let mut data: u32 = 0;
        for k in 0..bsize {
            data |= (d[i + 1 + k] as u32) << (8 * k);
        }
        let sdata = match bsize {
            1 => d[i + 1] as i8 as i32,
            2 => i16::from_le_bytes([d[i + 1], d[i + 2]]) as i32,
            4 => data as i32,
            _ => 0,
        };
        match (btype, btag) {
            (1, 0) => usage_page = data,         // Global: Usage Page
            (1, 2) => logical_max = sdata,       // Global: Logical Maximum
            // Global: Report Size. Clamp to a sane HID bound so a hostile
            // descriptor can't overflow the bit-offset arithmetic below.
            (1, 7) => report_size = data.min(64),
            (1, 8) => cur_id = Some(data as u8), // Global: Report ID
            // Global: Report Count. Clamped for the same overflow reason (a real
            // report is at most a few thousand fields).
            (1, 9) => report_count = data.min(4096),
            (2, 0) => {
                // Local: Usage. The 4-byte form carries its own page.
                let full = if bsize == 4 { data } else { (usage_page << 16) | data };
                if n_usages < usages.len() {
                    usages[n_usages] = full;
                    n_usages += 1;
                }
            }
            (2, 1) => usage_min = Some((usage_page << 16) | data), // Local: Usage Minimum
            (2, 2) => {
                // Local: Usage Maximum — expand the min..=max range into usages.
                if let Some(mn) = usage_min {
                    let mx = (usage_page << 16) | data;
                    let mut u = mn;
                    while u <= mx && n_usages < usages.len() {
                        usages[n_usages] = u;
                        n_usages += 1;
                        u += 1;
                    }
                }
            }
            (0, 8) => {
                // Main: Input item — report_count fields of report_size bits.
                let relative = data & (1 << 2) != 0;
                let constant = data & 1 != 0;
                let base = cursors.get(cur_id);
                if !constant && n_usages > 0 {
                    for slot in 0..report_count {
                        let u = usages[(slot as usize).min(n_usages - 1)];
                        // Bit offsets are u16 (a report is ≤ 8 KiB); saturate so
                        // a crafted descriptor can't overflow (fuzz-found).
                        let off = (base as u32).saturating_add(slot.saturating_mul(report_size));
                        let field = HidField {
                            bit_off: off.min(u16::MAX as u32) as u16,
                            bit_size: report_size as u8,
                            relative,
                            logical_max,
                        };
                        let (page, usage) = (u >> 16, u & 0xFFFF);
                        if page == UP_GENERIC_DESKTOP && usage == U_X && map.x.is_none() {
                            map.x = Some(field);
                            map.report_id = cur_id;
                        } else if page == UP_GENERIC_DESKTOP && usage == U_Y && map.y.is_none() {
                            map.y = Some(field);
                        } else if page == UP_GENERIC_DESKTOP && usage == U_WHEEL && map.wheel.is_none() {
                            map.wheel = Some(field);
                        } else if page == UP_BUTTON && map.buttons_n == 0 {
                            map.buttons_off = field.bit_off;
                            map.buttons_n = report_count.min(8) as u8;
                        }
                    }
                }
                cursors.advance(cur_id, report_size.saturating_mul(report_count).min(u16::MAX as u32) as u16);
                n_usages = 0;
                usage_min = None;
            }
            (0, _) => {
                // Other Main items (Output/Feature/Collection): clear locals.
                n_usages = 0;
                usage_min = None;
            }
            _ => {}
        }
        i += 1 + bsize;
    }
    if map.x.is_some() && map.y.is_some() {
        Some(map)
    } else {
        None
    }
}

/// Tiny fixed-capacity map of per-report-id bit cursors (no_std, no alloc).
mod heapless_cursors {
    #[derive(Default)]
    pub struct Cursors {
        slots: [(u8, u16, bool); 8], // (report id, bit cursor, used)
    }
    impl Cursors {
        fn idx(&mut self, id: Option<u8>) -> usize {
            let key = id.unwrap_or(0);
            if let Some(i) = self.slots.iter().position(|s| s.2 && s.0 == key) {
                return i;
            }
            if let Some(i) = self.slots.iter().position(|s| !s.2) {
                self.slots[i] = (key, 0, true);
                return i;
            }
            7
        }
        pub fn get(&mut self, id: Option<u8>) -> u16 {
            let i = self.idx(id);
            self.slots[i].1
        }
        pub fn advance(&mut self, id: Option<u8>, bits: u16) {
            let i = self.idx(id);
            self.slots[i].1 = self.slots[i].1.saturating_add(bits);
        }
    }
}

impl InputMap {
    /// Decode a pointing-device input report using this map. Returns
    /// `(x, y, buttons, x_is_absolute)`; with a report id set, a report
    /// carrying a different id yields `None`.
    pub fn decode(&self, report: &[u8]) -> Option<(i32, i32, u8, bool)> {
        let body = match self.report_id {
            Some(id) => {
                if report.first() != Some(&id) {
                    return None;
                }
                &report[1..]
            }
            None => report,
        };
        let fx = self.x.as_ref()?;
        let fy = self.y.as_ref()?;
        let x = if fx.relative {
            extract_signed(body, fx)
        } else {
            extract_bits(body, fx.bit_off, fx.bit_size) as i32
        };
        let y = if fy.relative {
            extract_signed(body, fy)
        } else {
            extract_bits(body, fy.bit_off, fy.bit_size) as i32
        };
        let buttons = if self.buttons_n > 0 {
            extract_bits(body, self.buttons_off, self.buttons_n) as u8
        } else {
            0
        };
        Some((x, y, buttons, !fx.relative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard_device_descriptor() -> [u8; 18] {
        [
            18, DT_DEVICE, 0x00, 0x02, // len, type, bcdUSB 2.0
            0x00, 0x00, 0x00, 8, // class/sub/proto 0 (per-interface), maxpkt 8
            0x6d, 0x04, 0x00, 0xc0, // vendor 0x046d, product 0xc000
            0x00, 0x01, 1, 2, 0, // bcdDevice, iMan, iProd, iSerial
            1, // 1 configuration
        ]
    }

    #[test]
    fn parse_device_descriptor() {
        let d = DeviceDescriptor::parse(&keyboard_device_descriptor()).unwrap();
        assert_eq!(d.usb_version, 0x0200);
        assert_eq!(d.vendor, 0x046d);
        assert_eq!(d.product, 0xc000);
        assert_eq!(d.num_configurations, 1);
    }

    #[test]
    fn reject_bad_device_descriptor() {
        assert!(DeviceDescriptor::parse(&[18, 99]).is_none()); // wrong type/len
        assert!(DeviceDescriptor::parse(&[1, 2, 3]).is_none());
    }

    fn keyboard_config() -> Vec<u8> {
        // config(9) + interface(9, HID boot keyboard) + endpoint(7, IN interrupt)
        let mut v = alloc::vec![
            9, DT_CONFIG, 25, 0, 1, 1, 0, 0xa0, 50, // config: total=25, 1 iface, value 1
            9, DT_INTERFACE, 0, 0, 1, CLASS_HID, HID_SUBCLASS_BOOT, HID_PROTOCOL_KEYBOARD, 0,
            7, DT_ENDPOINT, 0x81, 0x03, 8, 0, 10, // ep 1 IN, interrupt, maxpkt 8
        ];
        v[2] = v.len() as u8; // update wTotalLength
        v
    }

    #[test]
    fn parse_configuration_with_hid_keyboard() {
        let c = Configuration::parse(&keyboard_config()).unwrap();
        assert_eq!(c.value, 1);
        assert_eq!(c.interfaces.len(), 1);
        let iface = &c.interfaces[0];
        assert!(iface.is_boot_keyboard());
        assert!(!iface.is_boot_mouse());
        assert_eq!(iface.endpoints.len(), 1);
        assert!(iface.endpoints[0].is_in());
        assert_eq!(iface.endpoints[0].number(), 1);
    }

    #[test]
    fn parse_tablet_absolute_report() {
        // buttons=left(0x01), X=0x1234, Y=0x5678 (little-endian), wheel=0.
        let a = parse_tablet(&[0x01, 0x34, 0x12, 0x78, 0x56, 0x00]).unwrap();
        assert_eq!(a.buttons, 0x01);
        assert_eq!(a.x, 0x1234);
        assert_eq!(a.y, 0x5678);
        // Full-scale corner and too-short guard.
        let hi = parse_tablet(&[0, 0xFF, 0x7F, 0xFF, 0x7F]).unwrap();
        assert_eq!((hi.x, hi.y), (0x7FFF, 0x7FFF));
        assert!(parse_tablet(&[0, 1, 2]).is_none());
    }

    #[test]
    fn keyboard_press_and_release() {
        let mut kb = BootKeyboard::new();
        // 'a' (0x04) pressed with Left-Shift (mod bit1).
        let e1 = kb.feed(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].keycode, 0x04);
        assert!(e1[0].pressed);
        assert_eq!(e1[0].modifiers, 0x02);
        // 'a' held down → no new event.
        let e2 = kb.feed(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(e2.len(), 0);
        // released.
        let e3 = kb.feed(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(e3.len(), 1);
        assert!(!e3[0].pressed);
        assert_eq!(e3[0].keycode, 0x04);
    }

    #[test]
    fn keyboard_multiple_keys() {
        let mut kb = BootKeyboard::new();
        let e = kb.feed(&[0, 0, 0x04, 0x05, 0x06, 0, 0, 0]);
        assert_eq!(e.len(), 3); // 3 keys pressed at once
        let down: Vec<u8> = e.iter().filter(|k| k.pressed).map(|k| k.keycode).collect();
        assert!(down.contains(&0x04) && down.contains(&0x05) && down.contains(&0x06));
    }

    #[test]
    fn mouse_report() {
        let m = parse_mouse(&[0x01, 5, 0xFB, 0]).unwrap(); // left, dx=+5, dy=-5
        assert_eq!(m.buttons, 0x01);
        assert_eq!(m.dx, 5);
        assert_eq!(m.dy, -5);
        assert!(parse_mouse(&[0x01]).is_none());
    }

    #[test]
    fn cbw_round_trip_and_csw() {
        let cdb = bot::read10(0x1234, 8);
        let w = bot::cbw(0xDEAD_BEEF, 8 * 512, true, 0, &cdb);
        assert_eq!(&w[0..4], b"USBC");
        assert_eq!(u32::from_le_bytes([w[4], w[5], w[6], w[7]]), 0xDEAD_BEEF);
        assert_eq!(u32::from_le_bytes([w[8], w[9], w[10], w[11]]), 8 * 512);
        assert_eq!(w[12], 0x80); // IN
        assert_eq!(w[14], 10); // READ(10) CDB length
        assert_eq!(w[15], 0x28); // opcode READ(10)

        // A valid CSW: status success, no residue.
        let mut csw = [0u8; 13];
        csw[0..4].copy_from_slice(b"USBS");
        csw[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        csw[12] = 0;
        let (tag, residue, status) = bot::parse_csw(&csw).unwrap();
        assert_eq!(tag, 0xDEAD_BEEF);
        assert_eq!(residue, 0);
        assert_eq!(status, 0);
        assert!(bot::parse_csw(&[0u8; 13]).is_none()); // wrong signature
    }

    #[test]
    fn scsi_read_capacity_decode() {
        // last LBA = 0x0001_0000 (65536 blocks), block size = 512.
        let resp = [0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00];
        let (last_lba, bs) = bot::parse_capacity(&resp).unwrap();
        assert_eq!(last_lba, 0x0001_0000);
        assert_eq!(bs, 512);
        assert_eq!(bot::read_capacity10()[0], 0x25);
        assert_eq!(bot::inquiry()[4], 36);
    }

    #[test]
    fn mass_storage_interface_detect() {
        let iface = Interface {
            number: 0,
            class: CLASS_MASS_STORAGE,
            subclass: MSC_SUBCLASS_SCSI,
            protocol: MSC_PROTOCOL_BOT,
            endpoints: Vec::new(),
        };
        assert!(iface.is_mass_storage_bot());
        assert!(!iface.is_boot_keyboard());
    }

    #[test]
    fn report_descriptor_boot_mouse() {
        // Classic 3-button relative mouse: 3 button bits + 5 pad, X/Y 8-bit rel.
        let d = [
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, // GD, Mouse, Coll, Pointer, Coll
            0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, // Buttons 1..3, logical 0..1
            0x95, 0x03, 0x75, 0x01, 0x81, 0x02, // count 3, size 1, Input(Data,Var)
            0x95, 0x01, 0x75, 0x05, 0x81, 0x01, // count 1, size 5, Input(Const) padding
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, // GD, X, Y, logical -127..127
            0x75, 0x08, 0x95, 0x02, 0x81, 0x06, // size 8, count 2, Input(Data,Var,Rel)
            0xC0, 0xC0,
        ];
        let m = parse_report_descriptor(&d).unwrap();
        assert_eq!((m.buttons_off, m.buttons_n), (0, 3));
        let x = m.x.unwrap();
        assert!(x.relative);
        assert_eq!((x.bit_off, x.bit_size), (8, 8));
        assert_eq!(m.y.unwrap().bit_off, 16);
        // Report: button 1 down, dx=-2, dy=+5.
        let (dx, dy, b, abs) = m.decode(&[0x01, 0xFE, 0x05]).unwrap();
        assert_eq!((dx, dy, b, abs), (-2, 5, 1, false));
    }

    #[test]
    fn report_descriptor_abs_tablet() {
        // Tablet-like: 3 buttons + pad, X/Y 16-bit absolute 0..32767, wheel 8-bit rel.
        let d = [
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00,
            0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01,
            0x95, 0x03, 0x75, 0x01, 0x81, 0x02,
            0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, // logical max 32767
            0x75, 0x10, 0x95, 0x02, 0x81, 0x02, // X/Y 16-bit absolute
            0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, // wheel 8-bit rel
            0xC0, 0xC0,
        ];
        let m = parse_report_descriptor(&d).unwrap();
        let x = m.x.unwrap();
        assert!(!x.relative);
        assert_eq!((x.bit_off, x.bit_size), (8, 16));
        assert_eq!(x.logical_max, 32767);
        assert_eq!(m.y.unwrap().bit_off, 24);
        assert_eq!(m.wheel.unwrap().bit_off, 40);
        // Report: buttons=0b101, x=0x1234, y=0x7FFF, wheel=-1.
        let rpt = [0x05, 0x34, 0x12, 0xFF, 0x7F, 0xFF];
        let (x, y, b, abs) = m.decode(&rpt).unwrap();
        assert_eq!((x, y, b, abs), (0x1234, 0x7FFF, 5, true));
    }

    #[test]
    fn report_descriptor_with_report_id() {
        // Same tablet but behind report id 7; decode must demand the id byte.
        let d = [
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x07, // Report ID 7
            0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01,
            0x95, 0x03, 0x75, 0x01, 0x81, 0x02,
            0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F,
            0x75, 0x10, 0x95, 0x02, 0x81, 0x02,
            0xC0,
        ];
        let m = parse_report_descriptor(&d).unwrap();
        assert_eq!(m.report_id, Some(7));
        assert!(m.decode(&[0x06, 0, 0, 0, 0, 0]).is_none()); // wrong id
        let (x, y, _, abs) = m.decode(&[0x07, 0x00, 0x00, 0x40, 0x00, 0x20]).unwrap();
        assert_eq!((x, y, abs), (0x4000, 0x2000, true));
    }

    #[test]
    fn report_descriptor_rejects_non_pointer() {
        // A keyboard-ish descriptor without X/Y yields no map.
        let d = [0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x08, 0x81, 0x02, 0xC0];
        assert!(parse_report_descriptor(&d).is_none());
    }
}
