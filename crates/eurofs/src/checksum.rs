//! Integrity checksums for EuroFS.
//!
//! We use XXH3 (64-bit) — fast, high quality, pure-Rust (`twox-hash`),
//! no_std. XXH3 reliably detects *accidental* corruption (bit rot). It
//! is NOT cryptographic: against an active attacker you need AEAD/HMAC
//! (see EuroFS encryption layer, Phase 3). For on-disk integrity XXH3 suffices.

use twox_hash::xxh3::hash64;

/// 64-bit XXH3 checksum over a byte slice.
#[inline]
pub fn xxh3_64(data: &[u8]) -> u64 {
    hash64(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministisch() {
        assert_eq!(xxh3_64(b"EuroKernel"), xxh3_64(b"EuroKernel"));
    }

    #[test]
    fn gevoelig_voor_wijziging() {
        // A single bit flip must change the checksum (bit rot detection).
        let a = xxh3_64(b"sovereign");
        let b = xxh3_64(b"sovereigo"); // n -> o
        assert_ne!(a, b);
    }

    #[test]
    fn lege_input_is_geldig() {
        // Must not panic; XXH3 has a defined value for "".
        let _ = xxh3_64(&[]);
    }
}
