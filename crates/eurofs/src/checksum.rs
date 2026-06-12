//! Integriteitschecksums voor EuroFS.
//!
//! We gebruiken XXH3 (64-bit) — snel, hoge kwaliteit, pure-Rust (`twox-hash`),
//! no_std. XXH3 detecteert *accidentele* corruptie (bit rot) betrouwbaar. Het
//! is NIET cryptografisch: tegen een actieve aanvaller heb je AEAD/HMAC nodig
//! (zie EuroFS-encryptielaag, Fase 3). Voor on-disk integriteit volstaat XXH3.

use twox_hash::xxh3::hash64;

/// 64-bit XXH3 checksum over een byte-slice.
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
        // Eén bit-flip moet de checksum veranderen (bit rot-detectie).
        let a = xxh3_64(b"sovereign");
        let b = xxh3_64(b"sovereigo"); // n -> o
        assert_ne!(a, b);
    }

    #[test]
    fn lege_input_is_geldig() {
        // Mag niet paniekeren; XXH3 heeft een gedefinieerde waarde voor "".
        let _ = xxh3_64(&[]);
    }
}
