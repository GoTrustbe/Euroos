//! Signature verification for X.509 certificate chains (plan A1, phase 2).
//!
//! One function, [`verify`], that checks a signature given the
//! algorithm, the signer's public key, the message (the raw
//! `tbsCertificate`) and the signature value. Builds on the RustCrypto family
//! that the kernel already compiles. No panic on garbage — everything returns `false`.

use crate::x509::{PubKeyAlg, SigAlg};

/// Constant-time equality of two byte slices: no early exit, so the
/// runtime does not depend on the content. (Audit #9 — the RSA verification does
/// compare public data, but here we avoid every timing side channel as
/// defense-in-depth and for uniform style with the rest of the crypto layer.)
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify `sig` over `msg` with the public key `signer_key` of type
/// `signer_alg`, according to signature algorithm `sig_alg`. Returns `false` on
/// any mismatch, invalid encoding or wrong signature.
pub fn verify(sig_alg: SigAlg, signer_alg: PubKeyAlg, signer_key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    match (sig_alg, signer_alg) {
        (SigAlg::EcdsaSha256, PubKeyAlg::EcP256) => verify_ecdsa_p256(signer_key, msg, sig),
        (SigAlg::EcdsaSha384, PubKeyAlg::EcP384) => verify_ecdsa_p384(signer_key, msg, sig),
        (SigAlg::Ed25519, PubKeyAlg::Ed25519) => verify_ed25519(signer_key, msg, sig),
        (SigAlg::RsaSha256, PubKeyAlg::Rsa) => verify_rsa_pkcs1_sha256(signer_key, msg, sig),
        (SigAlg::RsaSha384, PubKeyAlg::Rsa) => verify_rsa_pkcs1_sha384(signer_key, msg, sig),
        // The algorithm and key type must match each other.
        _ => false,
    }
}

/// ECDSA P-256 with SHA-256. `key` = SEC1 uncompressed point (0x04‖X‖Y),
/// `sig` = DER SEQUENCE { r INTEGER, s INTEGER } as in an X.509 cert.
pub fn verify_ecdsa_p256(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    let vk = match VerifyingKey::from_sec1_bytes(key) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let signature = match Signature::from_der(sig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    // `verify` hashes `msg` itself with SHA-256 (the curve default digest).
    vk.verify(msg, &signature).is_ok()
}

/// ECDSA P-384 with SHA-384. `key` = SEC1 uncompressed point (0x04‖X‖Y, 97
/// bytes), `sig` = DER SEQUENCE { r, s }. Many CA intermediate certificates use
/// this combination.
pub fn verify_ecdsa_p384(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use p384::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    let vk = match VerifyingKey::from_sec1_bytes(key) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let signature = match Signature::from_der(sig) {
        Ok(s) => s,
        Err(_) => return false,
    };
    vk.verify(msg, &signature).is_ok()
}

/// Ed25519 (PureEdDSA). `key` = 32-byte public key, `sig` = 64 bytes.
pub fn verify_ed25519(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let key_arr: [u8; 32] = match key.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let vk = match VerifyingKey::from_bytes(&key_arr) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let sig_arr: [u8; 64] = match sig.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    vk.verify(msg, &Signature::from_bytes(&sig_arr)).is_ok()
}

/// RSASSA-PKCS1-v1_5 with SHA-256 (RFC 8017 §8.2.2). `key` = `RSAPublicKey`
/// DER (SEQUENCE { modulus, exponent }). Supports moduli up to 4096 bits.
pub fn verify_rsa_pkcs1_sha256(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    // DigestInfo prefix for SHA-256.
    const PREFIX: [u8; 19] = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04,
        0x20,
    ];
    match rsa_pkcs1_recover_di(key, sig) {
        Some(di) if di.len() == PREFIX.len() + 32 && di[..19] == PREFIX => ct_eq(&di[19..], &sha256(msg)),
        _ => false,
    }
}

/// RSASSA-PKCS1-v1_5 with SHA-384 (occurs in CA intermediate certificates).
pub fn verify_rsa_pkcs1_sha384(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    // DigestInfo prefix for SHA-384.
    const PREFIX: [u8; 19] = [
        0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05, 0x00, 0x04,
        0x30,
    ];
    match rsa_pkcs1_recover_di(key, sig) {
        Some(di) if di.len() == PREFIX.len() + 48 && di[..19] == PREFIX => ct_eq(&di[19..], &sha384(msg)),
        _ => false,
    }
}

/// Perform the RSA operation and strip the PKCS1-v1_5 padding; return the DigestInfo
/// (prefix ‖ hash), or `None` on wrong padding.
fn rsa_pkcs1_recover_di(key: &[u8], sig: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    let (full, k, _modbits) = rsa_public_op(key, sig)?;
    // The top (512-k) bytes must be zero (m fits in k bytes).
    if full[..512 - k].iter().any(|&b| b != 0) {
        return None;
    }
    let em = &full[512 - k..]; // the actual EM is k bytes
    // EM = 0x00 0x01 PS(0xFF…, ≥8) 0x00 DigestInfo.
    if em[0] != 0x00 || em[1] != 0x01 {
        return None;
    }
    let mut i = 2;
    while i < k && em[i] == 0xFF {
        i += 1;
    }
    if i < 10 || i >= k || em[i] != 0x00 {
        return None;
    }
    i += 1;
    Some(em[i..].to_vec())
}

/// RSASSA-PSS verification with SHA-256 + MGF1-SHA-256, salt length = 32 (as TLS
/// 1.3 `rsa_pss_rsae_sha256` requires; RFC 8017 §8.1.2 + §9.1.2). For the
/// CertificateVerify signature of an RSA server certificate.
pub fn verify_rsa_pss_sha256(key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    const HLEN: usize = 32;
    const SLEN: usize = 32;
    let (full, _k, modbits) = match rsa_public_op(key, sig) {
        Some(v) => v,
        None => return false,
    };
    let embits = modbits - 1;
    let emlen = embits.div_ceil(8);
    if !(HLEN + SLEN + 2..=512).contains(&emlen) {
        return false;
    }
    // The bytes above emLen must be zero (m fits in emLen bytes).
    if full[..512 - emlen].iter().any(|&b| b != 0) {
        return false;
    }
    let em = &full[512 - emlen..];
    // EMSA-PSS-VERIFY.
    if em[emlen - 1] != 0xbc {
        return false;
    }
    let masked_db = &em[..emlen - HLEN - 1];
    let h = &em[emlen - HLEN - 1..emlen - 1];
    // The unused leftmost bits of the first byte must be 0.
    let unused = 8 * emlen - embits;
    if unused > 0 && (masked_db[0] >> (8 - unused)) != 0 {
        return false;
    }
    // DB = maskedDB XOR MGF1(H, len(maskedDB)).
    let db_len = emlen - HLEN - 1;
    let db_mask = mgf1_sha256(h, db_len);
    let mut db = alloc::vec::Vec::with_capacity(db_len);
    for (a, b) in masked_db.iter().zip(db_mask.iter()) {
        db.push(a ^ b);
    }
    if unused > 0 {
        db[0] &= 0xFFu8 >> unused; // clear the same leftmost bits in DB
    }
    // DB must be: PS(0x00…) ‖ 0x01 ‖ salt.
    let ps_len = db_len - SLEN - 1;
    if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 0x01 {
        return false;
    }
    let salt = &db[ps_len + 1..];
    // M' = (0x00 ×8) ‖ mHash ‖ salt; H' = SHA-256(M') must equal H.
    let mhash = sha256(msg);
    let mut mprime = alloc::vec::Vec::with_capacity(8 + HLEN + SLEN);
    mprime.extend_from_slice(&[0u8; 8]);
    mprime.extend_from_slice(&mhash);
    mprime.extend_from_slice(salt);
    ct_eq(&sha256(&mprime), h)
}

/// RSA public operation: parse the key, compute m = s^e mod n; return the
/// 512-byte big-endian `m`, k (= modulus byte length) and modBits.
fn rsa_public_op(key: &[u8], sig: &[u8]) -> Option<([u8; 512], usize, usize)> {
    use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
    use crypto_bigint::Encoding;
    let (n_raw, e_raw) = crate::x509::parse_rsa_public_key(key).ok()?;
    let n_trim = strip_leading_zeros(n_raw);
    let k = n_trim.len();
    if !(62..=512).contains(&k) || sig.len() != k {
        return None;
    }
    // A valid RSA modulus is odd (DynResidueParams requires that too).
    if n_trim[k - 1] & 1 == 0 {
        return None;
    }
    let n = to_u4096(n_trim)?;
    let s = to_u4096(sig)?;
    let e = to_u4096(strip_leading_zeros(e_raw))?;
    let params = DynResidueParams::new(&n);
    let m = DynResidue::new(&s, params).pow(&e).retrieve();
    let top = n_trim[0];
    let modbits = (k - 1) * 8 + (8 - top.leading_zeros() as usize);
    Some((m.to_be_bytes(), k, modbits))
}

/// MGF1 with SHA-256 (RFC 8017 §B.2.1): generate `len` bytes from `seed`.
fn mgf1_sha256(seed: &[u8], len: usize) -> alloc::vec::Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut out = alloc::vec::Vec::with_capacity(len + 32);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut h = Sha256::new();
        h.update(seed);
        h.update(counter.to_be_bytes());
        out.extend_from_slice(&h.finalize());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// SHA-256 of `msg` as a 32-byte array.
fn sha256(msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(msg);
    h.finalize().into()
}

/// SHA-384 of `msg` as a 48-byte array.
fn sha384(msg: &[u8]) -> [u8; 48] {
    use sha2::{Digest, Sha384};
    let mut h = Sha384::new();
    h.update(msg);
    h.finalize().into()
}

/// Strip leading 0x00 bytes (the ASN.1 INTEGER sign byte etc.).
fn strip_leading_zeros(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    &b[start..]
}

/// Build a 4096-bit number from big-endian bytes (left padded). `None` if
/// the number is larger than 512 bytes.
fn to_u4096(bytes: &[u8]) -> Option<crypto_bigint::U4096> {
    use crypto_bigint::U4096;
    let trimmed = strip_leading_zeros(bytes);
    if trimmed.len() > 512 {
        return None;
    }
    let mut buf = [0u8; 512];
    buf[512 - trimmed.len()..].copy_from_slice(trimmed);
    Some(U4096::from_be_slice(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x509::Certificate;

    const EC_ROOT: &[u8] = include_bytes!("../testdata/ec_root.der");
    const EC_LEAF: &[u8] = include_bytes!("../testdata/ec_leaf.der");
    const RSA: &[u8] = include_bytes!("../testdata/rsa.der");

    #[test]
    fn rsa_self_signed_verifies() {
        let c = Certificate::parse(RSA).unwrap();
        // The RSA cert is self-signed: verify its signature over its
        // own tbsCertificate with its own public key.
        assert!(verify(c.sig_alg, c.pubkey_alg, c.pubkey, c.tbs_der, c.signature));
    }

    #[test]
    fn rsa_rejects_tampered_message() {
        let c = Certificate::parse(RSA).unwrap();
        let mut tbs = c.tbs_der.to_vec();
        tbs[30] ^= 0x01;
        assert!(!verify(c.sig_alg, c.pubkey_alg, c.pubkey, &tbs, c.signature));
    }

    #[test]
    fn rsa_rejects_tampered_signature() {
        let c = Certificate::parse(RSA).unwrap();
        let mut s = c.signature.to_vec();
        s[10] ^= 0x80;
        assert!(!verify(c.sig_alg, c.pubkey_alg, c.pubkey, c.tbs_der, &s));
    }

    const PSS_MSG: &[u8] = include_bytes!("../testdata/pss_msg.bin");
    const PSS_SIG: &[u8] = include_bytes!("../testdata/pss_sig.bin");

    const P384_ROOT: &[u8] = include_bytes!("../testdata/p384_root.der");
    const P384_LEAF: &[u8] = include_bytes!("../testdata/p384_leaf.der");
    const RSA384: &[u8] = include_bytes!("../testdata/rsa384.der");

    #[test]
    fn ecdsa_p384_sha384_chain_verifies() {
        let root = Certificate::parse(P384_ROOT).unwrap();
        let leaf = Certificate::parse(P384_LEAF).unwrap();
        assert_eq!(root.pubkey_alg, PubKeyAlg::EcP384);
        assert_eq!(leaf.sig_alg, SigAlg::EcdsaSha384);
        assert_eq!(leaf.pubkey.len(), 97); // uncompressed P-384 point
        assert!(verify(leaf.sig_alg, root.pubkey_alg, root.pubkey, leaf.tbs_der, leaf.signature));
        // Tampering is rejected.
        let mut tbs = leaf.tbs_der.to_vec();
        tbs[25] ^= 0x01;
        assert!(!verify(leaf.sig_alg, root.pubkey_alg, root.pubkey, &tbs, leaf.signature));
    }

    #[test]
    fn rsa_sha384_self_signed_verifies() {
        let c = Certificate::parse(RSA384).unwrap();
        assert_eq!(c.sig_alg, SigAlg::RsaSha384);
        assert!(verify(c.sig_alg, c.pubkey_alg, c.pubkey, c.tbs_der, c.signature));
        let mut s = c.signature.to_vec();
        s[20] ^= 0x01;
        assert!(!verify(c.sig_alg, c.pubkey_alg, c.pubkey, c.tbs_der, &s));
    }

    #[test]
    fn rsa_pss_verifies_openssl_signature() {
        // PSS signature (SHA-256, MGF1-SHA-256, salt=32) made with openssl
        // over PSS_MSG, to be verified with the public key from rsa.der.
        let c = Certificate::parse(RSA).unwrap();
        assert!(verify_rsa_pss_sha256(c.pubkey, PSS_MSG, PSS_SIG));
    }

    #[test]
    fn rsa_pss_rejects_tampered() {
        let c = Certificate::parse(RSA).unwrap();
        // Wrong message.
        let mut m = PSS_MSG.to_vec();
        m[0] ^= 0x01;
        assert!(!verify_rsa_pss_sha256(c.pubkey, &m, PSS_SIG));
        // Wrong signature.
        let mut s = PSS_SIG.to_vec();
        s[100] ^= 0x01;
        assert!(!verify_rsa_pss_sha256(c.pubkey, PSS_MSG, &s));
        // A PKCS1 signature is not a valid PSS signature.
        assert!(!verify_rsa_pss_sha256(c.pubkey, c.tbs_der, c.signature));
    }

    #[test]
    fn ecdsa_leaf_verifies_against_root_key() {
        let root = Certificate::parse(EC_ROOT).unwrap();
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        // The leaf is signed by the root: verify leaf.signature over
        // leaf.tbs_der with the public key of the root.
        assert!(verify(leaf.sig_alg, root.pubkey_alg, root.pubkey, leaf.tbs_der, leaf.signature));
    }

    #[test]
    fn ecdsa_root_is_self_signed() {
        let root = Certificate::parse(EC_ROOT).unwrap();
        // Self-signed: the root verifies against its own key.
        assert!(verify(root.sig_alg, root.pubkey_alg, root.pubkey, root.tbs_der, root.signature));
    }

    #[test]
    fn ecdsa_rejects_tampered_message() {
        let root = Certificate::parse(EC_ROOT).unwrap();
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        let mut tbs = leaf.tbs_der.to_vec();
        tbs[20] ^= 0x01; // flip one bit in the signed message
        assert!(!verify(leaf.sig_alg, root.pubkey_alg, root.pubkey, &tbs, leaf.signature));
    }

    #[test]
    fn ecdsa_rejects_wrong_key() {
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        // Verifying with the leaf key (instead of the root) must fail.
        assert!(!verify(leaf.sig_alg, leaf.pubkey_alg, leaf.pubkey, leaf.tbs_der, leaf.signature));
    }

    #[test]
    fn mismatched_alg_and_key_rejected() {
        let root = Certificate::parse(EC_ROOT).unwrap();
        let leaf = Certificate::parse(EC_LEAF).unwrap();
        // ECDSA signature but we claim an RSA key type → false.
        assert!(!verify(SigAlg::RsaSha256, root.pubkey_alg, root.pubkey, leaf.tbs_der, leaf.signature));
        assert!(!verify(leaf.sig_alg, PubKeyAlg::Unsupported, root.pubkey, leaf.tbs_der, leaf.signature));
    }
}
