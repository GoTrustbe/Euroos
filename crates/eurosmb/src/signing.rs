//! 3A-6 — **SMB2 message signing** (HMAC-SHA256, SMB 2.1).
//!
//! An unsigned SMB session is trivially tamperable by an on-path attacker; SMB
//! signing authenticates every message with a MAC keyed by the session key.
//! SMB 2.1 uses **HMAC-SHA256** (this module); SMB 3.x uses AES-CMAC / AES-GMAC,
//! which need an AES primitive the sovereign stack deliberately avoids — that
//! (and SMB3 encryption) is the honest remaining part.

use alloc::vec::Vec;
use sha2::{Digest, Sha256};

/// The SMB2 header signature field: 16 bytes at offset 48.
const SIG_OFFSET: usize = 48;
const SIG_LEN: usize = 16;
/// SMB2_FLAGS_SIGNED, in the 4-byte Flags field at header offset 16.
const FLAGS_OFFSET: usize = 16;
const FLAG_SIGNED: u32 = 0x0000_0008;

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    let mut r = [0u8; 32];
    r.copy_from_slice(&outer.finalize());
    r
}

/// Compute the SMB 2.1 signature of `msg` with `signing_key`: set the SIGNED
/// flag, zero the signature field, HMAC-SHA256 the whole message, take the first
/// 16 bytes.
pub fn compute(signing_key: &[u8; 16], msg: &[u8]) -> [u8; SIG_LEN] {
    let mut m: Vec<u8> = msg.to_vec();
    if m.len() >= SIG_OFFSET + SIG_LEN {
        // The signature is computed with SMB2_FLAGS_SIGNED set and the field zeroed.
        let mut flags = u32::from_le_bytes([m[FLAGS_OFFSET], m[FLAGS_OFFSET + 1], m[FLAGS_OFFSET + 2], m[FLAGS_OFFSET + 3]]);
        flags |= FLAG_SIGNED;
        m[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
        for b in &mut m[SIG_OFFSET..SIG_OFFSET + SIG_LEN] {
            *b = 0;
        }
    }
    let full = hmac_sha256(signing_key, &m);
    let mut sig = [0u8; SIG_LEN];
    sig.copy_from_slice(&full[..SIG_LEN]);
    sig
}

/// Sign `msg` in place (set the SIGNED flag + write the signature field).
pub fn sign(signing_key: &[u8; 16], msg: &mut [u8]) {
    if msg.len() < SIG_OFFSET + SIG_LEN {
        return;
    }
    let sig = compute(signing_key, msg);
    let mut flags = u32::from_le_bytes([msg[FLAGS_OFFSET], msg[FLAGS_OFFSET + 1], msg[FLAGS_OFFSET + 2], msg[FLAGS_OFFSET + 3]]);
    flags |= FLAG_SIGNED;
    msg[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&flags.to_le_bytes());
    msg[SIG_OFFSET..SIG_OFFSET + SIG_LEN].copy_from_slice(&sig);
}

/// Verify the signature of a received `msg` (constant-time compare). A message
/// with a wrong key, or one modified in flight, fails.
pub fn verify(signing_key: &[u8; 16], msg: &[u8]) -> bool {
    if msg.len() < SIG_OFFSET + SIG_LEN {
        return false;
    }
    let claimed = &msg[SIG_OFFSET..SIG_OFFSET + SIG_LEN];
    let computed = compute(signing_key, msg);
    let mut diff = 0u8;
    for i in 0..SIG_LEN {
        diff |= claimed[i] ^ computed[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Vec<u8> {
        // A 64-byte SMB2 header + a little payload (0xFE 'S' 'M' 'B' …).
        let mut m = alloc::vec![0u8; 80];
        m[0] = 0xFE;
        m[1] = b'S';
        m[2] = b'M';
        m[3] = b'B';
        for (i, b) in m.iter_mut().enumerate().skip(64) {
            *b = i as u8;
        }
        m
    }

    #[test]
    fn sign_then_verify() {
        let key = [0x11u8; 16];
        let mut m = message();
        sign(&key, &mut m);
        assert!(verify(&key, &m));
        // The SIGNED flag was set.
        assert_eq!(m[FLAGS_OFFSET] & 0x08, 0x08);
    }

    #[test]
    fn tampered_message_fails() {
        let key = [0x11u8; 16];
        let mut m = message();
        sign(&key, &mut m);
        m[70] ^= 0xFF; // modify the payload in flight
        assert!(!verify(&key, &m));
    }

    #[test]
    fn wrong_key_fails() {
        let mut m = message();
        sign(&[0x11u8; 16], &mut m);
        assert!(!verify(&[0x22u8; 16], &m));
    }
}
