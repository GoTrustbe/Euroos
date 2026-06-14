//! Bad-block remapping (plan J2 — storage robustness).
//!
//! When the data-path scrubber (G5) or an I/O error detects an unrecoverable
//! block, that block is marked as BAD and transparently redirected to a
//! reserve block (spare): later reads/writes to that LBA go from then on
//! to the spare, so that one broken sector does not make the whole filesystem fatal.
//! The table persists (serializable) so the remap survives a restart.
//!
//! Pure `no_std` logic (no device I/O), so the safety-critical remap
//! bookkeeping is fully tested on the host.

use alloc::vec::Vec;

const MAGIC: u32 = 0x4242_5400; // "BBT\0"
const ENTRY: usize = 16; // 8 bytes bad-LBA + 8 bytes spare-LBA

/// One remap: a bad block → its reserve block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Remap {
    bad: u64,
    spare: u64,
}

/// The bad-block table: a list of remaps + a pool of reserve blocks (a contiguous
/// range of LBAs `[spare_base, spare_base + spare_count)` that is not used by the
/// filesystem).
#[derive(Debug, Clone)]
pub struct BadBlockTable {
    remaps: Vec<Remap>,
    spare_base: u64,
    spare_count: u64,
    spare_next: u64, // how many spares already handed out
}

impl BadBlockTable {
    /// Create an empty table with a reserve pool `[spare_base, spare_base+spare_count)`.
    pub fn new(spare_base: u64, spare_count: u64) -> Self {
        BadBlockTable {
            remaps: Vec::new(),
            spare_base,
            spare_count,
            spare_next: 0,
        }
    }

    /// Translate an LBA: if it is remapped, return the spare block; otherwise the LBA itself.
    /// This is the hot path that a block-device wrapper calls on every read/write.
    pub fn translate(&self, lba: u64) -> u64 {
        self.remaps.iter().find(|r| r.bad == lba).map(|r| r.spare).unwrap_or(lba)
    }

    /// Is this block registered as bad?
    pub fn is_bad(&self, lba: u64) -> bool {
        self.remaps.iter().any(|r| r.bad == lba)
    }

    /// Mark `lba` as bad and assign a reserve block. Returns the spare-LBA, or
    /// `None` if the reserve pool is exhausted (then the block is unrecoverably lost).
    /// Idempotent: an already-remapped block returns its existing spare.
    pub fn mark_bad(&mut self, lba: u64) -> Option<u64> {
        if let Some(r) = self.remaps.iter().find(|r| r.bad == lba) {
            return Some(r.spare);
        }
        if self.spare_next >= self.spare_count {
            return None; // pool exhausted
        }
        let spare = self.spare_base + self.spare_next;
        self.spare_next += 1;
        self.remaps.push(Remap { bad: lba, spare });
        Some(spare)
    }

    /// Number of remapped (bad) blocks.
    pub fn bad_count(&self) -> usize {
        self.remaps.len()
    }

    /// Remaining reserve blocks.
    pub fn spares_left(&self) -> u64 {
        self.spare_count - self.spare_next
    }

    /// Serialize the table to bytes (magic + pool info + remaps). Fixed format,
    /// little-endian; a simple sum checksum closes it off.
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

    /// Read a table back; `None` on wrong magic, length or checksum.
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
        assert_eq!(t.translate(42), 42); // not remapped → itself
        assert!(!t.is_bad(42));
    }

    #[test]
    fn mark_bad_remaps_to_spare() {
        let mut t = BadBlockTable::new(1000, 8);
        let spare = t.mark_bad(42).unwrap();
        assert_eq!(spare, 1000); // first spare
        assert!(t.is_bad(42));
        assert_eq!(t.translate(42), 1000); // reads/writes now go to the spare
        assert_eq!(t.translate(43), 43); // neighboring block untouched
        assert_eq!(t.bad_count(), 1);
        assert_eq!(t.spares_left(), 7);
    }

    #[test]
    fn mark_bad_is_idempotent() {
        let mut t = BadBlockTable::new(1000, 8);
        let s1 = t.mark_bad(42).unwrap();
        let s2 = t.mark_bad(42).unwrap();
        assert_eq!(s1, s2); // same spare, no second assignment
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
        assert_eq!(t.mark_bad(30), None); // pool exhausted → unrecoverable
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
        bytes[n - 6] ^= 0xFF; // corrupt a remap byte → checksum mismatch
        assert!(BadBlockTable::deserialize(&bytes).is_none());
        assert!(BadBlockTable::deserialize(&[1, 2, 3]).is_none()); // too short / no magic
    }
}
