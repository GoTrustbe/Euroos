//! Bad-block-remapping (plan J2 — opslag-robuustheid).
//!
//! Wanneer de data-path-scrubber (G5) of een I/O-fout een onherstelbaar blok
//! detecteert, wordt dat blok als SLECHT gemarkeerd en transparant naar een
//! reserve-blok (spare) omgeleid: latere reads/writes naar die LBA gaan voortaan
//! naar de spare, zodat één kapotte sector niet het hele filesysteem fataal maakt.
//! De tabel persisteert (serialiseerbaar) zodat de remap een herstart overleeft.
//!
//! Pure `no_std`-logica (geen device-I/O), zodat de veiligheidskritische remap-
//! boekhouding volledig op de host getest is.

use alloc::vec::Vec;

const MAGIC: u32 = 0x4242_5400; // "BBT\0"
const ENTRY: usize = 16; // 8 bytes bad-LBA + 8 bytes spare-LBA

/// Eén remap: een slecht blok → zijn reserve-blok.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Remap {
    bad: u64,
    spare: u64,
}

/// De bad-block-tabel: een lijst remaps + een pool reserve-blokken (een aaneengesloten
/// reeks LBA's `[spare_base, spare_base + spare_count)` die niet door het filesysteem
/// wordt gebruikt).
#[derive(Debug, Clone)]
pub struct BadBlockTable {
    remaps: Vec<Remap>,
    spare_base: u64,
    spare_count: u64,
    spare_next: u64, // hoeveel spares al uitgegeven
}

impl BadBlockTable {
    /// Maak een lege tabel met een reserve-pool `[spare_base, spare_base+spare_count)`.
    pub fn new(spare_base: u64, spare_count: u64) -> Self {
        BadBlockTable {
            remaps: Vec::new(),
            spare_base,
            spare_count,
            spare_next: 0,
        }
    }

    /// Vertaal een LBA: als die geremapt is, geef het spare-blok; anders de LBA zelf.
    /// Dit is de hot-path die een block-device-wrapper bij elke read/write aanroept.
    pub fn translate(&self, lba: u64) -> u64 {
        self.remaps.iter().find(|r| r.bad == lba).map(|r| r.spare).unwrap_or(lba)
    }

    /// Is dit blok als slecht geregistreerd?
    pub fn is_bad(&self, lba: u64) -> bool {
        self.remaps.iter().any(|r| r.bad == lba)
    }

    /// Markeer `lba` als slecht en wijs een reserve-blok toe. Geeft de spare-LBA, of
    /// `None` als de reserve-pool op is (dan is het blok onherstelbaar verloren).
    /// Idempotent: een al-geremapt blok geeft zijn bestaande spare terug.
    pub fn mark_bad(&mut self, lba: u64) -> Option<u64> {
        if let Some(r) = self.remaps.iter().find(|r| r.bad == lba) {
            return Some(r.spare);
        }
        if self.spare_next >= self.spare_count {
            return None; // pool uitgeput
        }
        let spare = self.spare_base + self.spare_next;
        self.spare_next += 1;
        self.remaps.push(Remap { bad: lba, spare });
        Some(spare)
    }

    /// Aantal geremapte (slechte) blokken.
    pub fn bad_count(&self) -> usize {
        self.remaps.len()
    }

    /// Resterende reserve-blokken.
    pub fn spares_left(&self) -> u64 {
        self.spare_count - self.spare_next
    }

    /// Serialiseer de tabel naar bytes (magic + pool-info + remaps). Vast formaat,
    /// little-endian; een eenvoudige som-checksum sluit af.
    pub fn serialize(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32 + self.remaps.len() * ENTRY);
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.extend_from_slice(&(self.remaps.len() as u32).to_le_bytes());
        b.extend_from_slice(&self.spare_base.to_le_bytes());
        b.extend_from_slice(&self.spare_count.to_le_bytes());
        b.extend_from_slice(&self.spare_next.to_le_bytes());
        for r in &self.remaps {
            b.extend_from_slice(&r.bad.to_le_bytes());
            b.extend_from_slice(&r.spare.to_le_bytes());
        }
        let ck: u32 = b.iter().fold(0u32, |a, &x| a.wrapping_add(x as u32));
        b.extend_from_slice(&ck.to_le_bytes());
        b
    }

    /// Lees een tabel terug; `None` bij verkeerde magic, lengte of checksum.
    pub fn deserialize(data: &[u8]) -> Option<BadBlockTable> {
        if data.len() < 32 + 4 {
            return None;
        }
        if u32::from_le_bytes(data[0..4].try_into().ok()?) != MAGIC {
            return None;
        }
        let n = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
        let need = 32 + n * ENTRY + 4;
        if data.len() < need {
            return None;
        }
        let stored = u32::from_le_bytes(data[need - 4..need].try_into().ok()?);
        let ck: u32 = data[..need - 4].iter().fold(0u32, |a, &x| a.wrapping_add(x as u32));
        if ck != stored {
            return None;
        }
        let spare_base = u64::from_le_bytes(data[8..16].try_into().ok()?);
        let spare_count = u64::from_le_bytes(data[16..24].try_into().ok()?);
        let spare_next = u64::from_le_bytes(data[24..32].try_into().ok()?);
        let mut remaps = Vec::with_capacity(n);
        for i in 0..n {
            let o = 32 + i * ENTRY;
            remaps.push(Remap {
                bad: u64::from_le_bytes(data[o..o + 8].try_into().ok()?),
                spare: u64::from_le_bytes(data[o + 8..o + 16].try_into().ok()?),
            });
        }
        Some(BadBlockTable {
            remaps,
            spare_base,
            spare_count,
            spare_next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_passthrough_when_healthy() {
        let t = BadBlockTable::new(1000, 8);
        assert_eq!(t.translate(42), 42); // niet geremapt → zichzelf
        assert!(!t.is_bad(42));
    }

    #[test]
    fn mark_bad_remaps_to_spare() {
        let mut t = BadBlockTable::new(1000, 8);
        let spare = t.mark_bad(42).unwrap();
        assert_eq!(spare, 1000); // eerste spare
        assert!(t.is_bad(42));
        assert_eq!(t.translate(42), 1000); // reads/writes gaan nu naar de spare
        assert_eq!(t.translate(43), 43); // buurblok ongemoeid
        assert_eq!(t.bad_count(), 1);
        assert_eq!(t.spares_left(), 7);
    }

    #[test]
    fn mark_bad_is_idempotent() {
        let mut t = BadBlockTable::new(1000, 8);
        let s1 = t.mark_bad(42).unwrap();
        let s2 = t.mark_bad(42).unwrap();
        assert_eq!(s1, s2); // zelfde spare, geen tweede toewijzing
        assert_eq!(t.bad_count(), 1);
    }

    #[test]
    fn distinct_bad_blocks_get_distinct_spares() {
        let mut t = BadBlockTable::new(1000, 8);
        assert_eq!(t.mark_bad(10).unwrap(), 1000);
        assert_eq!(t.mark_bad(20).unwrap(), 1001);
        assert_eq!(t.mark_bad(30).unwrap(), 1002);
        assert_eq!(t.translate(20), 1001);
    }

    #[test]
    fn pool_exhaustion_returns_none() {
        let mut t = BadBlockTable::new(1000, 2);
        assert!(t.mark_bad(10).is_some());
        assert!(t.mark_bad(20).is_some());
        assert_eq!(t.mark_bad(30), None); // pool op → onherstelbaar
        assert_eq!(t.spares_left(), 0);
    }

    #[test]
    fn serialize_roundtrip() {
        let mut t = BadBlockTable::new(5000, 16);
        t.mark_bad(7);
        t.mark_bad(99);
        t.mark_bad(12345);
        let bytes = t.serialize();
        let back = BadBlockTable::deserialize(&bytes).unwrap();
        assert_eq!(back.bad_count(), 3);
        assert_eq!(back.translate(99), t.translate(99));
        assert_eq!(back.translate(12345), t.translate(12345));
        assert_eq!(back.spares_left(), t.spares_left());
    }

    #[test]
    fn deserialize_rejects_corruption() {
        let mut t = BadBlockTable::new(5000, 16);
        t.mark_bad(7);
        let mut bytes = t.serialize();
        let n = bytes.len();
        bytes[n - 6] ^= 0xFF; // corrupt een remap-byte → checksum-mismatch
        assert!(BadBlockTable::deserialize(&bytes).is_none());
        assert!(BadBlockTable::deserialize(&[1, 2, 3]).is_none()); // te kort/geen magic
    }
}
