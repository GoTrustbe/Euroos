//! EuroACPI — ACPI-tabel-parsing-kern (plan I3, power management).
//!
//! Voor echt power-management (nette shutdown/reboot, sleep-states, thermal/battery)
//! moet EuroOS de ACPI-tabellen lezen: de **SDT-headers** (met checksum-validatie),
//! de **RSDT/XSDT** (de lijst tabel-pointers), en de **FADT** (FACP) met de power-
//! management-registers. Deze module is de architectuur-onafhankelijke parser-kern
//! (de kernel levert de fysieke geheugentoegang erboven). Pure `no_std`-logica →
//! de checksum- + offset-gevoelige parsing is volledig op de host getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

/// De gemeenschappelijke 36-byte ACPI-SDT-header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub oem_id: [u8; 6],
}

impl SdtHeader {
    pub fn parse(b: &[u8]) -> Option<SdtHeader> {
        if b.len() < 36 {
            return None;
        }
        let length = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        Some(SdtHeader {
            signature: [b[0], b[1], b[2], b[3]],
            length,
            revision: b[8],
            oem_id: [b[16], b[17], b[18], b[19], b[20], b[21]],
        })
    }

    pub fn signature_is(&self, sig: &[u8; 4]) -> bool {
        &self.signature == sig
    }
}

/// Een geldige ACPI-tabel heeft een byte-som van 0 (over `length` bytes). Beschermt
/// tegen corrupte/onvolledige tabellen vóór we de velden vertrouwen.
pub fn checksum_ok(table: &[u8]) -> bool {
    let len = match SdtHeader::parse(table) {
        Some(h) => h.length as usize,
        None => return false,
    };
    if len == 0 || len > table.len() {
        return false;
    }
    table[..len].iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
}

/// Bereken de checksum-byte die `table` (met dat byte op 0) sluitend maakt — handig
/// om geldige test-tabellen te bouwen.
pub fn fix_checksum(table: &mut [u8], checksum_offset: usize) {
    table[checksum_offset] = 0;
    let sum = table.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    table[checksum_offset] = sum.wrapping_neg();
}

/// Lees de tabel-pointers uit een RSDT (4-byte entries) of XSDT (8-byte entries).
/// `xsdt=true` → 64-bit pointers. De header (36 bytes) wordt overgeslagen.
pub fn table_pointers(rsdt: &[u8], xsdt: bool) -> Vec<u64> {
    let header = match SdtHeader::parse(rsdt) {
        Some(h) => h,
        None => return Vec::new(),
    };
    let len = (header.length as usize).min(rsdt.len());
    let entry = if xsdt { 8 } else { 4 };
    let mut out = Vec::new();
    let mut p = 36;
    while p + entry <= len {
        let ptr = if xsdt {
            u64::from_le_bytes([rsdt[p], rsdt[p + 1], rsdt[p + 2], rsdt[p + 3], rsdt[p + 4], rsdt[p + 5], rsdt[p + 6], rsdt[p + 7]])
        } else {
            u32::from_le_bytes([rsdt[p], rsdt[p + 1], rsdt[p + 2], rsdt[p + 3]]) as u64
        };
        out.push(ptr);
        p += entry;
    }
    out
}

/// De power-management-velden uit de FADT (FACP). Genoeg voor nette ACPI-shutdown
/// (S5 via PM1a_CNT) en reboot (RESET_REG/RESET_VALUE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fadt {
    pub pm1a_evt_blk: u32,
    pub pm1a_cnt_blk: u32,
    pub reset_reg_addr: u64,
    pub reset_value: u8,
    pub flags: u32,
}

impl Fadt {
    /// Parse de FADT-power-velden (FADT ≥ ACPI 2.0 voor RESET_REG). Valideert de
    /// signature ("FACP") + checksum.
    pub fn parse(b: &[u8]) -> Option<Fadt> {
        let h = SdtHeader::parse(b)?;
        if !h.signature_is(b"FACP") || !checksum_ok(b) {
            return None;
        }
        if b.len() < 129 {
            return None;
        }
        Some(Fadt {
            pm1a_evt_blk: u32::from_le_bytes([b[56], b[57], b[58], b[59]]),
            pm1a_cnt_blk: u32::from_le_bytes([b[64], b[65], b[66], b[67]]),
            // RESET_REG is een 12-byte GAS op offset 116; het 64-bit adres staat op +4.
            reset_reg_addr: u64::from_le_bytes([
                b[120], b[121], b[122], b[123], b[124], b[125], b[126], b[127],
            ]),
            reset_value: b[128],
            flags: u32::from_le_bytes([b[112], b[113], b[114], b[115]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_table(sig: &[u8; 4], len: usize) -> Vec<u8> {
        let mut t = alloc::vec![0u8; len];
        t[..4].copy_from_slice(sig);
        t[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        t[8] = 2; // revision
        t[16..22].copy_from_slice(b"EUROOS");
        fix_checksum(&mut t, 9); // checksum-byte op offset 9
        t
    }

    #[test]
    fn parse_header_and_checksum() {
        let t = make_table(b"APIC", 60);
        let h = SdtHeader::parse(&t).unwrap();
        assert!(h.signature_is(b"APIC"));
        assert_eq!(h.length, 60);
        assert_eq!(&h.oem_id, b"EUROOS");
        assert!(checksum_ok(&t));
    }

    #[test]
    fn checksum_detects_corruption() {
        let mut t = make_table(b"APIC", 60);
        t[40] ^= 0xFF; // corrupt een byte
        assert!(!checksum_ok(&t));
    }

    #[test]
    fn rsdt_pointers() {
        // RSDT-header (36) + 3 × u32-pointers.
        let mut t = make_table(b"RSDT", 36 + 12);
        t[36..40].copy_from_slice(&0x1000u32.to_le_bytes());
        t[40..44].copy_from_slice(&0x2000u32.to_le_bytes());
        t[44..48].copy_from_slice(&0x3000u32.to_le_bytes());
        fix_checksum(&mut t, 9);
        let ptrs = table_pointers(&t, false);
        assert_eq!(ptrs, alloc::vec![0x1000, 0x2000, 0x3000]);
    }

    #[test]
    fn xsdt_64bit_pointers() {
        let mut t = make_table(b"XSDT", 36 + 16);
        t[36..44].copy_from_slice(&0xABCD_0000u64.to_le_bytes());
        t[44..52].copy_from_slice(&0x1_0000_0000u64.to_le_bytes()); // > 4 GiB
        fix_checksum(&mut t, 9);
        let ptrs = table_pointers(&t, true);
        assert_eq!(ptrs, alloc::vec![0xABCD_0000, 0x1_0000_0000]);
    }

    #[test]
    fn fadt_power_fields() {
        let mut t = make_table(b"FACP", 132);
        t[64..68].copy_from_slice(&0x604u32.to_le_bytes()); // PM1a_CNT_BLK (QEMU q35)
        t[120..128].copy_from_slice(&0xCF9u64.to_le_bytes()); // RESET_REG addr (0xCF9)
        t[128] = 0x06; // RESET_VALUE
        fix_checksum(&mut t, 9);
        let f = Fadt::parse(&t).unwrap();
        assert_eq!(f.pm1a_cnt_blk, 0x604);
        assert_eq!(f.reset_reg_addr, 0xCF9);
        assert_eq!(f.reset_value, 0x06);
    }

    #[test]
    fn fadt_rejects_wrong_signature() {
        let t = make_table(b"APIC", 132);
        assert!(Fadt::parse(&t).is_none()); // geen FACP
    }
}
