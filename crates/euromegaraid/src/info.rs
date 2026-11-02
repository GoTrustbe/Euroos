//! Decoders for what the firmware hands back: the controller-info block, the
//! logical-drive list and the READ CAPACITY(16) answer. Everything is read
//! from untrusted device memory: fixed offsets, bounded strings, clamped counts.
//!
//! `mfi_ctrl_info` offsets (QEMU `hw/scsi/mfi.h`): pci ids at 0x00, the
//! 8 + 8 firmware components (72 bytes each) after the host and device port
//! blocks, then `max_lds` 0x53F, `product_name[80]` 0x540, `serial[32]` 0x590,
//! `max_cmds` 0x5B8, `max_sg_elements` 0x5BA, `max_request_size` 0x5BC,
//! `lds_present` 0x5C0, `lds_degraded` 0x5C2, `lds_offline` 0x5C4,
//! `pd_present` 0x5C6, `pd_disks_present` 0x5C8, `memory_size` 0x5D0,
//! `package_version[0x60]` 0x640; total 0x800 bytes.

use crate::frame::get32;

pub const CTRL_INFO_LEN: usize = 0x800;
pub const LD_LIST_LEN: usize = 8 + MAX_LD * 16;
pub const MAX_LD: usize = 64;

const OFF_PRODUCT: usize = 0x540;
const OFF_SERIAL: usize = 0x590;
const OFF_PACKAGE: usize = 0x640;
const OFF_COMPONENTS: usize = 0xB8;
const OFF_COMPONENT_COUNT: usize = 0xB4;

/// A fixed-size, NUL-padded ASCII field, cleaned for logging.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    b: [u8; N],
    n: usize,
}

impl<const N: usize> Text<N> {
    fn from(raw: &[u8]) -> Text<N> {
        let mut b = [0u8; N];
        let mut n = 0;
        for &c in raw.iter().take(N) {
            if c == 0 {
                break;
            }
            // Printable ASCII only: the device does not get to put escape
            // sequences into the kernel log.
            b[n] = if (0x20..0x7F).contains(&c) { c } else { b'?' };
            n += 1;
        }
        while n > 0 && b[n - 1] == b' ' {
            n -= 1;
        }
        Text { b, n }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.b[..self.n]).unwrap_or("?")
    }
}

impl<const N: usize> core::fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtrlInfo {
    pub vendor: u16,
    pub device: u16,
    pub subvendor: u16,
    pub subdevice: u16,
    pub product: Text<80>,
    pub serial: Text<32>,
    pub package: Text<96>,
    /// Name and version of the first active firmware component.
    pub fw_name: Text<8>,
    pub fw_version: Text<32>,
    pub max_lds: u8,
    pub max_cmds: u16,
    pub max_sge: u16,
    pub max_request_size: u32,
    pub lds_present: u16,
    pub lds_degraded: u16,
    pub lds_offline: u16,
    pub pd_present: u16,
    pub pd_disks_present: u16,
    pub memory_mib: u16,
}

impl CtrlInfo {
    /// Decode the 0x800-byte block. `None` when the buffer is short.
    pub fn parse(b: &[u8]) -> Option<CtrlInfo> {
        if b.len() < CTRL_INFO_LEN {
            return None;
        }
        let comps = get32(b, OFF_COMPONENT_COUNT);
        let (fw_name, fw_version) = if comps > 0 {
            (Text::from(&b[OFF_COMPONENTS..OFF_COMPONENTS + 8]), Text::from(&b[OFF_COMPONENTS + 8..OFF_COMPONENTS + 40]))
        } else {
            (Text::from(&[]), Text::from(&[]))
        };
        Some(CtrlInfo {
            vendor: u16_at(b, 0),
            device: u16_at(b, 2),
            subvendor: u16_at(b, 4),
            subdevice: u16_at(b, 6),
            product: Text::from(&b[OFF_PRODUCT..OFF_PRODUCT + 80]),
            serial: Text::from(&b[OFF_SERIAL..OFF_SERIAL + 32]),
            package: Text::from(&b[OFF_PACKAGE..OFF_PACKAGE + 96]),
            fw_name,
            fw_version,
            max_lds: b[0x53F],
            max_cmds: u16_at(b, 0x5B8),
            max_sge: u16_at(b, 0x5BA),
            max_request_size: get32(b, 0x5BC),
            lds_present: u16_at(b, 0x5C0),
            lds_degraded: u16_at(b, 0x5C2),
            lds_offline: u16_at(b, 0x5C4),
            pd_present: u16_at(b, 0x5C6),
            pd_disks_present: u16_at(b, 0x5C8),
            memory_mib: u16_at(b, 0x5D0),
        })
    }
}

/// Logical-drive states (`MFI_LD_STATE_*`).
pub const LD_OFFLINE: u8 = 0;
pub const LD_PARTIALLY_DEGRADED: u8 = 1;
pub const LD_DEGRADED: u8 = 2;
pub const LD_OPTIMAL: u8 = 3;

pub fn ld_state_name(s: u8) -> &'static str {
    match s {
        LD_OFFLINE => "offline",
        LD_PARTIALLY_DEGRADED => "partially degraded",
        LD_DEGRADED => "degraded",
        LD_OPTIMAL => "optimal",
        _ => "unknown",
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ld {
    pub target: u8,
    pub state: u8,
    /// Size in 512-byte sectors.
    pub sectors: u64,
}

/// The LD list, at most [`MAX_LD`] entries (more would be a lying device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LdList {
    pub count: usize,
    pub lds: [Ld; MAX_LD],
    /// The device claimed more entries than fit: clamped.
    pub clamped: bool,
}

impl LdList {
    pub fn parse(b: &[u8]) -> Option<LdList> {
        if b.len() < 8 {
            return None;
        }
        let claimed = get32(b, 0) as usize;
        let fit = (b.len() - 8) / 16;
        let count = claimed.min(MAX_LD).min(fit);
        let mut lds = [Ld::default(); MAX_LD];
        for (i, ld) in lds.iter_mut().enumerate().take(count) {
            let e = 8 + i * 16;
            let mut s = [0u8; 8];
            s.copy_from_slice(&b[e + 8..e + 16]);
            *ld = Ld { target: b[e], state: b[e + 4], sectors: u64::from_le_bytes(s) };
        }
        Some(LdList { count, lds, clamped: claimed != count })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Ld> {
        self.lds[..self.count].iter()
    }
}

/// READ CAPACITY(16): (number of logical blocks, block length in bytes).
pub fn read_capacity16(b: &[u8]) -> Option<(u64, u32)> {
    if b.len() < 12 {
        return None;
    }
    let mut last = [0u8; 8];
    last.copy_from_slice(&b[0..8]);
    let last = u64::from_be_bytes(last);
    let bs = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
    Some((last.checked_add(1)?, bs))
}

#[cfg(test)]
pub(crate) fn encode_ctrl_info(product: &str, serial: &str, package: &str, max_cmds: u16, lds: u16) -> [u8; CTRL_INFO_LEN] {
    let mut b = [0u8; CTRL_INFO_LEN];
    b[0..2].copy_from_slice(&0x1000u16.to_le_bytes());
    b[2..4].copy_from_slice(&0x0079u16.to_le_bytes());
    b[OFF_COMPONENT_COUNT..OFF_COMPONENT_COUNT + 4].copy_from_slice(&1u32.to_le_bytes());
    b[OFF_COMPONENTS..OFF_COMPONENTS + 4].copy_from_slice(b"APP\0");
    b[OFF_COMPONENTS + 8..OFF_COMPONENTS + 12].copy_from_slice(b"1.80");
    b[OFF_PRODUCT..OFF_PRODUCT + product.len()].copy_from_slice(product.as_bytes());
    b[OFF_SERIAL..OFF_SERIAL + serial.len()].copy_from_slice(serial.as_bytes());
    b[OFF_PACKAGE..OFF_PACKAGE + package.len()].copy_from_slice(package.as_bytes());
    b[0x53F] = 64;
    b[0x5B8..0x5BA].copy_from_slice(&max_cmds.to_le_bytes());
    b[0x5BA..0x5BC].copy_from_slice(&80u16.to_le_bytes());
    b[0x5C0..0x5C2].copy_from_slice(&lds.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_info_fields() {
        let b = encode_ctrl_info("LSI MegaRAID SAS 9260-8i", "QEMU123456", "1.80.0", 1008, 2);
        let i = CtrlInfo::parse(&b).unwrap();
        assert_eq!(i.product.as_str(), "LSI MegaRAID SAS 9260-8i");
        assert_eq!(i.serial.as_str(), "QEMU123456");
        assert_eq!(i.package.as_str(), "1.80.0");
        assert_eq!(i.fw_name.as_str(), "APP");
        assert_eq!(i.fw_version.as_str(), "1.80");
        assert_eq!((i.vendor, i.device), (0x1000, 0x0079));
        assert_eq!(i.max_cmds, 1008);
        assert_eq!(i.max_sge, 80);
        assert_eq!(i.lds_present, 2);
        assert_eq!(i.max_lds, 64);
        assert!(CtrlInfo::parse(&b[..CTRL_INFO_LEN - 1]).is_none());
    }

    #[test]
    fn hostile_strings_are_cleaned() {
        let mut b = encode_ctrl_info("ok", "", "", 1, 0);
        b[OFF_PRODUCT..OFF_PRODUCT + 6].copy_from_slice(b"A\x1b[2JB");
        let i = CtrlInfo::parse(&b).unwrap();
        assert_eq!(i.product.as_str(), "A?[2JB");
        // No NUL anywhere: bounded by the field, not by the buffer.
        let mut b = encode_ctrl_info("", "", "", 1, 0);
        for x in &mut b[OFF_SERIAL..OFF_SERIAL + 32] {
            *x = b'S';
        }
        b[OFF_SERIAL + 32] = b'X';
        let i = CtrlInfo::parse(&b).unwrap();
        assert_eq!(i.serial.as_str().len(), 32);
        assert!(!i.serial.as_str().contains('X'));
    }

    #[test]
    fn ld_list_parse_and_clamp() {
        let mut b = [0u8; LD_LIST_LEN];
        b[0..4].copy_from_slice(&2u32.to_le_bytes());
        b[8] = 0;
        b[12] = LD_OPTIMAL;
        b[16..24].copy_from_slice(&131072u64.to_le_bytes());
        b[24] = 1;
        b[28] = LD_DEGRADED;
        b[32..40].copy_from_slice(&524288u64.to_le_bytes());
        let l = LdList::parse(&b).unwrap();
        assert_eq!(l.count, 2);
        assert!(!l.clamped);
        assert_eq!(l.lds[0], Ld { target: 0, state: LD_OPTIMAL, sectors: 131072 });
        assert_eq!(l.lds[1].target, 1);
        assert_eq!(ld_state_name(l.lds[1].state), "degraded");
        // A device claiming 4 billion LDs gets what fits in the buffer, no more.
        b[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        let l = LdList::parse(&b).unwrap();
        assert_eq!(l.count, MAX_LD);
        assert!(l.clamped);
        let l = LdList::parse(&b[..8 + 16 * 3]).unwrap();
        assert_eq!(l.count, 3);
        assert!(LdList::parse(&b[..7]).is_none());
    }

    #[test]
    fn read_capacity() {
        let mut b = [0u8; 32];
        b[0..8].copy_from_slice(&131071u64.to_be_bytes());
        b[8..12].copy_from_slice(&512u32.to_be_bytes());
        assert_eq!(read_capacity16(&b), Some((131072, 512)));
        b[0..8].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(read_capacity16(&b), None); // overflow, not a wrap to 0
        assert_eq!(read_capacity16(&b[..11]), None);
    }
}
