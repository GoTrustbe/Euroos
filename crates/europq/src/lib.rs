//! EuroPQ — sovereign **post-quantum cryptography, from scratch**.
//!
//! - [`keccak`] — Keccak-f[1600] + SHA3-256/512 + SHAKE-128/256 (FIPS 202).
//! - [`mlkem`] — **ML-KEM-768** (FIPS 203), the post-quantum KEM, verified
//!   byte-for-byte against the NIST ACVP known-answer vectors.
//!
//! Intended for **hybrid** use: an X25519 exchange combined with an ML-KEM-768
//! exchange via [`hybrid_combine`], so the session key stays secret as long as
//! *either* primitive is unbroken — classical security today, quantum security
//! against "harvest now, decrypt later". No external crates: the entire PQ stack
//! down to the Keccak permutation is EuroOS code.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod keccak;
pub mod mlkem;

pub use mlkem::{decaps, encaps, encaps_internal, keygen, CT_LEN, DK_LEN, EK_LEN, SS_LEN};

/// Combine a classical shared secret (e.g. X25519) with the ML-KEM shared secret
/// into one 32-byte session key: `SHA3-256("EuroPQ-hybrid-v1" ‖ classical ‖ pq)`.
/// The result is secret as long as **either** input is — the defining property
/// of a hybrid KEM.
pub fn hybrid_combine(classical: &[u8], pq: &[u8]) -> [u8; 32] {
    let mut inp = alloc::vec::Vec::with_capacity(16 + classical.len() + pq.len());
    inp.extend_from_slice(b"EuroPQ-hybrid-v1");
    inp.extend_from_slice(classical);
    inp.extend_from_slice(pq);
    keccak::sha3_256(&inp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_combine_depends_on_both() {
        let base = hybrid_combine(b"classical-ss-32-bytes-aaaaaaaaaa", b"pq-ss-32-bytes-bbbbbbbbbbbbbbbb");
        // Changing either half changes the session key.
        let diff_c = hybrid_combine(b"CLASSICAL-ss-32-bytes-aaaaaaaaaa", b"pq-ss-32-bytes-bbbbbbbbbbbbbbbb");
        let diff_p = hybrid_combine(b"classical-ss-32-bytes-aaaaaaaaaa", b"PQ-ss-32-bytes-bbbbbbbbbbbbbbbb");
        assert_ne!(base, diff_c);
        assert_ne!(base, diff_p);
    }
}
