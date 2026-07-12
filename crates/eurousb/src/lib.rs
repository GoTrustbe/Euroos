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
}
