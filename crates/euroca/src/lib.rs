//! EuroCA — een soevereine, lokale certificaatautoriteit (plan O3).
//!
//! EuroOS vertrouwt geen buitenlandse CA-hiërarchie als enige anker: een organisatie
//! kan haar eigen wortel-CA draaien en daarmee diensten, gebruikers en agents
//! ondertekenen. Dit crate is de host-geteste kern: een **wortel-CA** (zelf-getekend),
//! het **uitgeven** van certificaten op een CSR, **ketenverificatie** (handtekening +
//! geldigheidsvenster + CA-vlag) en **revocatie**. Crypto = Ed25519 (`ed25519-dalek`)
//! + SHA-256-vingerafdrukken (`sha2`); geen klok-afhankelijkheid (de tijd komt binnen).
//!
//! Het is bewust geen X.509/ASN.1 (dat is een compat-laag in EuroTLS): het is een
//! compact, eigen, eenduidig te encoderen formaat — soeverein by design.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"EuroCA-cert-v1\0";

/// Een uitgegeven certificaat: identiteit + sleutel + geldigheid + handtekening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub serial: u64,
    pub subject: String,
    /// De publieke sleutel van het subject (Ed25519, 32 bytes).
    pub subject_key: [u8; 32],
    pub issuer: String,
    /// Geldigheidsvenster (seconden sinds epoch).
    pub not_before: u64,
    pub not_after: u64,
    /// Mag dit certificaat zelf certificaten ondertekenen (een (sub-)CA)?
    pub is_ca: bool,
    /// Ed25519-handtekening van de **uitgever** over de TBS-bytes.
    pub signature: [u8; 64],
}

/// Een certificaataanvraag (Certificate Signing Request).
pub struct Csr {
    pub subject: String,
    pub subject_key: [u8; 32],
    pub is_ca: bool,
}

/// Waarom een certificaat ongeldig is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertError {
    /// De handtekening klopt niet voor de uitgever-sleutel.
    BadSignature,
    /// `now` ligt vóór `not_before`.
    NotYetValid,
    /// `now` ligt ná `not_after`.
    Expired,
    /// De uitgever is geen CA (mag niet tekenen).
    IssuerNotCa,
    /// Het certificaat staat op de revocatielijst.
    Revoked,
    /// De uitgever-sleutel is geen geldig Ed25519-punt.
    BadIssuerKey,
}

/// De "to-be-signed"-bytes: een canonieke, lengte-geprefixte encodering van alle
/// velden behalve de handtekening. Domein-gescheiden tegen hergebruik.
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
    /// De TBS-bytes van dit certificaat (voor (her)verificatie).
    fn tbs_bytes(&self) -> Vec<u8> {
        tbs(self.serial, &self.subject, &self.subject_key, &self.issuer, self.not_before, self.not_after, self.is_ca)
    }

    /// De SHA-256-vingerafdruk (hex) van het volledige certificaat — een stabiele id.
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

    /// Verifieer de handtekening + geldigheidsvenster tegen een uitgever-sleutel op `now`.
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

/// Een (wortel- of sub-)certificaatautoriteit: een sleutel + het eigen certificaat.
pub struct CertAuthority {
    key: SigningKey,
    pub cert: Certificate,
    next_serial: u64,
    revoked: Vec<u64>,
}

impl CertAuthority {
    /// Maak een **wortel-CA**: een zelf-getekend CA-certificaat uit een 32-byte seed.
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

    /// De publieke sleutel van deze CA.
    pub fn public_key(&self) -> [u8; 32] {
        self.cert.subject_key
    }

    /// Geef een certificaat uit op een CSR (de CA tekent het). Het geldigheidsvenster
    /// wordt geklemd binnen dat van de CA zelf.
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

    /// Trek een uitgegeven certificaat in (op serienummer).
    pub fn revoke(&mut self, serial: u64) {
        if !self.revoked.contains(&serial) {
            self.revoked.push(serial);
        }
    }

    pub fn is_revoked(&self, serial: u64) -> bool {
        self.revoked.contains(&serial)
    }

    /// Verifieer een door **deze** CA uitgegeven certificaat: handtekening + venster +
    /// dat de CA zelf geldig is + niet-ingetrokken. De volledige ketencheck.
    pub fn verify_issued(&self, cert: &Certificate, now: u64) -> Result<(), CertError> {
        // 1. De CA moet zelf een geldige CA zijn op `now`.
        if !self.cert.is_ca {
            return Err(CertError::IssuerNotCa);
        }
        self.cert.verify(&self.cert.subject_key, now)?; // zelf-getekend
        // 2. Niet ingetrokken.
        if self.is_revoked(cert.serial) {
            return Err(CertError::Revoked);
        }
        // 3. Het blad verifieert tegen onze sleutel.
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
        // Een dienst met zijn eigen sleutel.
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
        leaf.subject = "evil.example.com".into(); // na ondertekening gewijzigd
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
        let b = ca.issue(&leaf_csr(), T0, T0 + YEAR); // ander serienummer
        assert_eq!(a.fingerprint(), a.fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 64);
    }
}
