//! AEAD-recordbescherming voor TLS 1.3 met ChaCha20-Poly1305 (RFC 8446 §5.2,
//! RFC 8439). De per-record nonce is de statische IV ge-XOR'd met het
//! 64-bits record-sequentienummer (rechts uitgelijnd in de 12-byte nonce).

use alloc::vec::Vec;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::keyschedule::IV_LEN;

/// Bouw de per-record nonce: iv XOR seq (RFC 8446 §5.3).
fn nonce(iv: &[u8; IV_LEN], seq: u64) -> [u8; IV_LEN] {
    let mut n = *iv;
    let s = seq.to_be_bytes(); // 8 bytes
    for i in 0..8 {
        n[IV_LEN - 8 + i] ^= s[i];
    }
    n
}

/// Versleutel `plaintext` met geassocieerde data `aad`. Geeft ciphertext||tag.
pub fn seal(key: &[u8; 32], iv: &[u8; IV_LEN], seq: u64, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let n = nonce(iv, seq);
    cipher
        .encrypt(Nonce::from_slice(&n), Payload { msg: plaintext, aad })
        .expect("aead seal")
}

/// Ontsleutel `ciphertext` (incl. tag) met geassocieerde data `aad`. None bij
/// een ongeldige tag (authenticatiefout).
pub fn open(key: &[u8; 32], iv: &[u8; IV_LEN], seq: u64, aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let n = nonce(iv, seq);
    cipher.decrypt(Nonce::from_slice(&n), Payload { msg: ciphertext, aad }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = [7u8; 32];
        let iv = [3u8; IV_LEN];
        let aad = b"\x17\x03\x03\x00\x20";
        let pt = b"hallo euroos";
        let ct = seal(&key, &iv, 0, aad, pt);
        assert_ne!(&ct[..pt.len()], &pt[..]); // echt versleuteld
        assert_eq!(ct.len(), pt.len() + 16); // + Poly1305-tag
        assert_eq!(open(&key, &iv, 0, aad, &ct).as_deref(), Some(&pt[..]));
    }

    #[test]
    fn wrong_seq_or_aad_fails() {
        let key = [7u8; 32];
        let iv = [3u8; IV_LEN];
        let ct = seal(&key, &iv, 0, b"aad", b"data");
        assert!(open(&key, &iv, 1, b"aad", &ct).is_none()); // verkeerd seq
        assert!(open(&key, &iv, 0, b"bad", &ct).is_none()); // verkeerde aad
    }

    #[test]
    fn tampered_ciphertext_or_tag_fails() {
        let key = [7u8; 32];
        let iv = [3u8; IV_LEN];
        let aad = b"\x17\x03\x03\x00\x16";
        // Eén geflipte ciphertext-byte → Poly1305-tag klopt niet meer → None.
        let mut ct = seal(&key, &iv, 0, aad, b"geheim bericht");
        ct[0] ^= 0x01;
        assert!(open(&key, &iv, 0, aad, &ct).is_none());
        // Eén geflipte tag-byte → None.
        let mut ct2 = seal(&key, &iv, 0, aad, b"geheim bericht");
        let last = ct2.len() - 1;
        ct2[last] ^= 0x01;
        assert!(open(&key, &iv, 0, aad, &ct2).is_none());
    }

    #[test]
    fn nonce_xor() {
        let iv = [0u8; IV_LEN];
        // seq 1 zet alleen de laatste byte.
        let n = nonce(&iv, 1);
        assert_eq!(n[IV_LEN - 1], 1);
        assert_eq!(n[0], 0);
    }
}
