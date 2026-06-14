//! EuroCA — a sovereign, local certificate authority (plan O3).
//!
//! EuroOS does not trust a foreign CA hierarchy as its sole anchor: an organization
//! can run its own root CA and use it to sign services, users and agents.
//! This crate is the host-tested core: a **root CA** (self-signed),
//! the **issuance** of certificates from a CSR, **chain verification** (signature +
//! validity window + CA flag) and **revocation**. Crypto = Ed25519 (`ed25519-dalek`)
//! + SHA-256 fingerprints (`sha2`); no clock dependency (time is passed in).
//!
//! It is deliberately not X.509/ASN.1 (that is a compat layer in EuroTLS): it is a
//! compact, custom, unambiguously encodable format — sovereign by design.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"EuroCA-cert-v1\0";

/// An issued certificate: identity + key + validity + signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub serial: u64,
    pub subject: String,
    /// The public key of the subject (Ed25519, 32 bytes).
    pub subject_key: [u8; 32],
    pub issuer: String,
    /// Validity window (seconds since epoch).
    pub not_before: u64,
    pub not_after: u64,
    /// May this certificate itself sign certificates (a (sub-)CA)?
    pub is_ca: bool,
    /// Ed25519 signature of the **issuer** over the TBS bytes.
    pub signature: [u8; 64],
}

/// A certificate request (Certificate Signing Request).
pub struct Csr {
    pub subject: String,
    pub subject_key: [u8; 32],
    pub is_ca: bool,
}

/// Why a certificate is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertError {
    /// The signature does not match the issuer key.
    BadSignature,
    /// `now` is before `not_before`.
    NotYetValid,
    /// `now` is after `not_after`.
    Expired,
    /// The issuer is not a CA (not allowed to sign).
    IssuerNotCa,
    /// The certificate is on the revocation list.
    Revoked,
    /// The issuer key is not a valid Ed25519 point.
    BadIssuerKey,
}

/// The "to-be-signed" bytes: a canonical, length-prefixed encoding of all
/// fields except the signature. Domain-separated against reuse.
fn tbs(serial: u64, subject: &str, key: &[u8; 32], issuer: &str, nb: u64, na: u64, is_ca: bool) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(DOMAIN);
    b.extend_from_slice(&serial.to_le_bytes());
    push_str(&mut b, subject);
    b.extend_from_slice(key);
    push_str(&mut b, issuer);
    b.extend_from_slice(&nb.to_le_bytes());
    b.extend_from_slice(&na.to_le_bytes());
    b.push(is_ca as u8);
    b
}

fn push_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

impl Certificate {
    /// The TBS bytes of this certificate (for (re)verification).
    fn tbs_bytes(&self) -> Vec<u8> {
        tbs(self.serial, &self.subject, &self.subject_key, &self.issuer, self.not_before, self.not_after, self.is_ca)
    }

    /// The SHA-256 fingerprint (hex) of the complete certificate — a stable id.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.tbs_bytes());
        h.update(self.signature);
        let digest = h.finalize();
        let mut s = String::with_capacity(64);
        for byte in digest {
            s.push_str(&alloc::format!("{byte:02x}"));
        }
        s
    }

    /// Verify the signature + validity window against an issuer key at `now`.
    pub fn verify(&self, issuer_key: &[u8; 32], now: u64) -> Result<(), CertError> {
        let vk = VerifyingKey::from_bytes(issuer_key).map_err(|_| CertError::BadIssuerKey)?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(&self.tbs_bytes(), &sig).map_err(|_| CertError::BadSignature)?;
        if now < self.not_before {
            return Err(CertError::NotYetValid);
        }
        if now > self.not_after {
            return Err(CertError::Expired);
        }
        Ok(())
    }
}

/// A (root or sub-)certificate authority: a key + its own certificate.
pub struct CertAuthority {
    key: SigningKey,
    pub cert: Certificate,
    next_serial: u64,
    revoked: Vec<u64>,
}

impl CertAuthority {
    /// Create a **root CA**: a self-signed CA certificate from a 32-byte seed.
    pub fn new_root(name: &str, seed: [u8; 32], not_before: u64, not_after: u64) -> CertAuthority {
        let key = SigningKey::from_bytes(&seed);
        let pubkey = key.verifying_key().to_bytes();
        let body = tbs(0, name, &pubkey, name, not_before, not_after, true);
        let signature = key.sign(&body).to_bytes();
        let cert = Certificate {
            serial: 0,
            subject: String::from(name),
            subject_key: pubkey,
            issuer: String::from(name),
            not_before,
            not_after,
            is_ca: true,
            signature,
        };
        CertAuthority { key, cert, next_serial: 1, revoked: Vec::new() }
    }

    /// The public key of this CA.
    pub fn public_key(&self) -> [u8; 32] {
        self.cert.subject_key
    }

    /// Issue a certificate from a CSR (the CA signs it). The validity window
    /// is clamped within that of the CA itself.
    pub fn issue(&mut self, csr: &Csr, not_before: u64, not_after: u64) -> Certificate {
        let nb = not_before.max(self.cert.not_before);
        let na = not_after.min(self.cert.not_after);
        let serial = self.next_serial;
        self.next_serial += 1;
        let body = tbs(serial, &csr.subject, &csr.subject_key, &self.cert.subject, nb, na, csr.is_ca);
        let signature = self.key.sign(&body).to_bytes();
        Certificate {
            serial,
            subject: csr.subject.clone(),
            subject_key: csr.subject_key,
            issuer: self.cert.subject.clone(),
            not_before: nb,
            not_after: na,
            is_ca: csr.is_ca,
            signature,
        }
    }

    /// Revoke an issued certificate (by serial number).
    pub fn revoke(&mut self, serial: u64) {
        if !self.revoked.contains(&serial) {
            self.revoked.push(serial);
        }
    }

    pub fn is_revoked(&self, serial: u64) -> bool {
        self.revoked.contains(&serial)
    }

    /// Verify a certificate issued by **this** CA: signature + window +
    /// that the CA itself is valid + not revoked. The full chain check.
    pub fn verify_issued(&self, cert: &Certificate, now: u64) -> Result<(), CertError> {
        // 1. The CA must itself be a valid CA at `now`.
        if !self.cert.is_ca {
            return Err(CertError::IssuerNotCa);
        }
        self.cert.verify(&self.cert.subject_key, now)?; // self-signed
        // 2. Not revoked.
        if self.is_revoked(cert.serial) {
            return Err(CertError::Revoked);
        }
        // 3. The leaf verifies against our key.
        cert.verify(&self.public_key(), now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;
    const YEAR: u64 = 365 * 24 * 3600;

    fn root() -> CertAuthority {
        CertAuthority::new_root("EuroCA Root", [42u8; 32], T0, T0 + 10 * YEAR)
    }

    fn leaf_csr() -> Csr {
        // A service with its own key.
        let key = SigningKey::from_bytes(&[7u8; 32]);
        Csr { subject: "service.gov.eu".into(), subject_key: key.verifying_key().to_bytes(), is_ca: false }
    }

    #[test]
    fn root_self_signed_verifies() {
        let ca = root();
        assert_eq!(ca.cert.verify(&ca.public_key(), T0 + YEAR), Ok(()));
    }

    #[test]
    fn issued_leaf_verifies() {
        let mut ca = root();
        let leaf = ca.issue(&leaf_csr(), T0, T0 + YEAR);
        assert_eq!(ca.verify_issued(&leaf, T0 + 100), Ok(()));
        assert_eq!(leaf.issuer, "EuroCA Root");
        assert_eq!(leaf.serial, 1);
    }

    #[test]
    fn tampered_cert_rejected() {
        let mut ca = root();
        let mut leaf = ca.issue(&leaf_csr(), T0, T0 + YEAR);
        leaf.subject = "evil.example.com".into(); // modified after signing
        assert_eq!(ca.verify_issued(&leaf, T0 + 100), Err(CertError::BadSignature));
    }

    #[test]
    fn expired_and_not_yet_valid() {
        let mut ca = root();
        let leaf = ca.issue(&leaf_csr(), T0 + YEAR, T0 + 2 * YEAR);
        assert_eq!(ca.verify_issued(&leaf, T0 + 100), Err(CertError::NotYetValid));
        assert_eq!(ca.verify_issued(&leaf, T0 + 3 * YEAR), Err(CertError::Expired));
    }

    #[test]
    fn revocation() {
        let mut ca = root();
        let leaf = ca.issue(&leaf_csr(), T0, T0 + YEAR);
        assert_eq!(ca.verify_issued(&leaf, T0 + 100), Ok(()));
        ca.revoke(leaf.serial);
        assert_eq!(ca.verify_issued(&leaf, T0 + 100), Err(CertError::Revoked));
    }

    #[test]
    fn wrong_issuer_key_fails() {
        let mut ca = root();
        let leaf = ca.issue(&leaf_csr(), T0, T0 + YEAR);
        let other = SigningKey::from_bytes(&[99u8; 32]).verifying_key().to_bytes();
        assert_eq!(leaf.verify(&other, T0 + 100), Err(CertError::BadSignature));
    }

    #[test]
    fn fingerprint_stable_and_unique() {
        let mut ca = root();
        let a = ca.issue(&leaf_csr(), T0, T0 + YEAR);
        let b = ca.issue(&leaf_csr(), T0, T0 + YEAR); // different serial number
        assert_eq!(a.fingerprint(), a.fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 64);
    }
}
