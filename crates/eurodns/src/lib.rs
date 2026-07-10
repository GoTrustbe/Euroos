//! EuroDNS — a DNS message model + **DNSSEC validation** (Ed25519, RFC 8080).
//!
//! A sovereign resolver must not just fetch answers, it must be able to *prove*
//! they were not tampered with in transit or by a hostile resolver. DNSSEC does
//! that: a zone signs its records (RRSIG) with a key (DNSKEY) that a parent zone
//! vouches for (DS), forming a chain to a trust anchor. This crate implements:
//!
//! - a canonical DNS name + RR / RRset model (A and AAAA),
//! - **RRSIG verification** for algorithm 15 (**Ed25519**, RFC 8080) over the
//!   RFC 4034 §3.1.8.1 canonical signed form,
//! - **DS verification** (SHA-256 digest of a DNSKEY, RFC 4509) to link a key to
//!   its parent's delegation.
//!
//! Ed25519 zones validate from scratch here; RSA/ECDSA DNSSEC algorithms and the
//! DoT/DoH transport (over `eurotls`) are the remaining pieces. Pure `no_std`,
//! host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_DS: u16 = 43;
pub const TYPE_RRSIG: u16 = 46;
pub const TYPE_DNSKEY: u16 = 48;
pub const CLASS_IN: u16 = 1;
pub const ALG_ED25519: u8 = 15; // RFC 8080
pub const DIGEST_SHA256: u8 = 2; // RFC 4509

/// A DNS name in **canonical** wire form (RFC 4034 §6.1: all labels
/// lowercased, length-prefixed, root-terminated, no compression).
pub fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        out.push(label.len() as u8);
        for b in label.bytes() {
            out.push(b.to_ascii_lowercase());
        }
    }
    out.push(0); // root
    out
}

/// A resource record (owner + type + class + ttl + rdata).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rr {
    pub name: alloc::string::String,
    pub rtype: u16,
    pub class: u16,
    pub ttl: u32,
    pub rdata: Vec<u8>,
}

/// An RRSIG record's fields (RFC 4034 §3.1), signature separate.
#[derive(Clone, Debug)]
pub struct Rrsig {
    pub type_covered: u16,
    pub algorithm: u8,
    pub labels: u8,
    pub original_ttl: u32,
    pub sig_expiration: u32,
    pub sig_inception: u32,
    pub key_tag: u16,
    pub signer_name: alloc::string::String,
    pub signature: Vec<u8>,
}

impl Rrsig {
    /// The RRSIG RDATA up to (but excluding) the signature — the prefix of the
    /// signed data (RFC 4034 §3.1.8.1).
    fn rdata_prefix(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.type_covered.to_be_bytes());
        b.push(self.algorithm);
        b.push(self.labels);
        b.extend_from_slice(&self.original_ttl.to_be_bytes());
        b.extend_from_slice(&self.sig_expiration.to_be_bytes());
        b.extend_from_slice(&self.sig_inception.to_be_bytes());
        b.extend_from_slice(&self.key_tag.to_be_bytes());
        b.extend_from_slice(&encode_name(&self.signer_name));
        b
    }
}

/// The canonical signed data over an RRset (RFC 4034 §3.1.8.1): the RRSIG RDATA
/// prefix followed by each RR in canonical form, sorted by RDATA.
pub fn signed_data(rrsig: &Rrsig, rrset: &[Rr]) -> Vec<u8> {
    let mut records: Vec<Vec<u8>> = rrset
        .iter()
        .map(|rr| {
            let mut b = Vec::new();
            b.extend_from_slice(&encode_name(&rr.name));
            b.extend_from_slice(&rrsig.type_covered.to_be_bytes());
            b.extend_from_slice(&rr.class.to_be_bytes());
            b.extend_from_slice(&rrsig.original_ttl.to_be_bytes()); // RRSIG's TTL, not the RR's
            b.extend_from_slice(&(rr.rdata.len() as u16).to_be_bytes());
            b.extend_from_slice(&rr.rdata);
            b
        })
        .collect();
    records.sort(); // canonical RRset ordering (RFC 4034 §6.3, by full canonical RR)
    let mut out = rrsig.rdata_prefix();
    for r in records {
        out.extend_from_slice(&r);
    }
    out
}

/// A DNSKEY record.
#[derive(Clone, Debug)]
pub struct Dnskey {
    pub flags: u16,
    pub protocol: u8,
    pub algorithm: u8,
    pub public_key: Vec<u8>,
}

impl Dnskey {
    /// The DNSKEY RDATA (flags ‖ protocol ‖ algorithm ‖ public key).
    pub fn rdata(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.flags.to_be_bytes());
        b.push(self.protocol);
        b.push(self.algorithm);
        b.extend_from_slice(&self.public_key);
        b
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnssecError {
    UnsupportedAlgorithm,
    BadKey,
    BadSignature,
    Expired,
    NotYetValid,
    DsMismatch,
}

/// Verify an RRSIG over an RRset with a DNSKEY at time `now` (Ed25519 only).
pub fn verify_rrsig(rrsig: &Rrsig, rrset: &[Rr], key: &Dnskey, now: u32) -> Result<(), DnssecError> {
    if rrsig.algorithm != ALG_ED25519 || key.algorithm != ALG_ED25519 {
        return Err(DnssecError::UnsupportedAlgorithm);
    }
    if now < rrsig.sig_inception {
        return Err(DnssecError::NotYetValid);
    }
    if now > rrsig.sig_expiration {
        return Err(DnssecError::Expired);
    }
    let pk: [u8; 32] = key.public_key.as_slice().try_into().map_err(|_| DnssecError::BadKey)?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| DnssecError::BadKey)?;
    let sig: [u8; 64] = rrsig.signature.as_slice().try_into().map_err(|_| DnssecError::BadSignature)?;
    vk.verify(&signed_data(rrsig, rrset), &Signature::from_bytes(&sig)).map_err(|_| DnssecError::BadSignature)
}

/// A DS (Delegation Signer) record from the parent zone.
#[derive(Clone, Debug)]
pub struct Ds {
    pub key_tag: u16,
    pub algorithm: u8,
    pub digest_type: u8,
    pub digest: Vec<u8>,
}

/// Verify that a DNSKEY matches a parent DS record (RFC 4509, SHA-256):
/// `digest == SHA-256(owner_name_canonical ‖ DNSKEY_RDATA)`.
pub fn verify_ds(ds: &Ds, key: &Dnskey, owner: &str) -> Result<(), DnssecError> {
    if ds.digest_type != DIGEST_SHA256 {
        return Err(DnssecError::UnsupportedAlgorithm);
    }
    let mut h = Sha256::new();
    h.update(encode_name(owner));
    h.update(key.rdata());
    let digest = h.finalize();
    if digest.as_slice() == ds.digest.as_slice() {
        Ok(())
    } else {
        Err(DnssecError::DsMismatch)
    }
}

/// Compute the DNSKEY key tag (RFC 4034 App. B) — the 16-bit id a DS/RRSIG uses
/// to point at a key.
pub fn key_tag(key: &Dnskey) -> u16 {
    let rd = key.rdata();
    let mut acc: u32 = 0;
    for (i, &b) in rd.iter().enumerate() {
        acc += if i % 2 == 0 { (b as u32) << 8 } else { b as u32 };
    }
    acc += (acc >> 16) & 0xFFFF;
    (acc & 0xFFFF) as u16
}

// ── minimal message building (A / AAAA queries) ────────────────────────────
/// Build a DNS query for `name` with `qtype` (A=1 / AAAA=28).
pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1
    b.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // an/ns/ar = 0
    b.extend_from_slice(&encode_name(name));
    b.extend_from_slice(&qtype.to_be_bytes());
    b.extend_from_slice(&CLASS_IN.to_be_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use ed25519_dalek::{Signer, SigningKey};

    fn zone_key() -> (SigningKey, Dnskey) {
        let sk = SigningKey::from_bytes(&[0x21; 32]);
        let dk = Dnskey { flags: 257, protocol: 3, algorithm: ALG_ED25519, public_key: sk.verifying_key().to_bytes().to_vec() };
        (sk, dk)
    }

    fn a_rrset() -> Vec<Rr> {
        alloc::vec![
            Rr { name: "www.euro-os.eu".to_string(), rtype: TYPE_A, class: CLASS_IN, ttl: 3600, rdata: alloc::vec![93, 184, 216, 34] },
            Rr { name: "www.euro-os.eu".to_string(), rtype: TYPE_A, class: CLASS_IN, ttl: 3600, rdata: alloc::vec![93, 184, 216, 35] },
        ]
    }

    fn signed(sk: &SigningKey) -> Rrsig {
        let mut rrsig = Rrsig {
            type_covered: TYPE_A,
            algorithm: ALG_ED25519,
            labels: 3,
            original_ttl: 3600,
            sig_inception: 1000,
            sig_expiration: 2_000_000,
            key_tag: 0,
            signer_name: "euro-os.eu".to_string(),
            signature: Vec::new(),
        };
        rrsig.signature = sk.sign(&signed_data(&rrsig, &a_rrset())).to_bytes().to_vec();
        rrsig
    }

    #[test]
    fn valid_rrsig_verifies() {
        let (sk, dk) = zone_key();
        let rrsig = signed(&sk);
        assert_eq!(verify_rrsig(&rrsig, &a_rrset(), &dk, 100_000), Ok(()));
    }

    #[test]
    fn tampered_record_fails() {
        let (sk, dk) = zone_key();
        let rrsig = signed(&sk);
        let mut set = a_rrset();
        set[0].rdata[3] = 99; // spoof the IP
        assert_eq!(verify_rrsig(&rrsig, &set, &dk, 100_000), Err(DnssecError::BadSignature));
    }

    #[test]
    fn wrong_key_and_expiry() {
        let (sk, _) = zone_key();
        let rrsig = signed(&sk);
        let other = Dnskey { flags: 257, protocol: 3, algorithm: ALG_ED25519, public_key: SigningKey::from_bytes(&[0x99; 32]).verifying_key().to_bytes().to_vec() };
        assert_eq!(verify_rrsig(&rrsig, &a_rrset(), &other, 100_000), Err(DnssecError::BadSignature));
        let (_, dk) = zone_key();
        assert_eq!(verify_rrsig(&rrsig, &a_rrset(), &dk, 3_000_000), Err(DnssecError::Expired));
        assert_eq!(verify_rrsig(&rrsig, &a_rrset(), &dk, 500), Err(DnssecError::NotYetValid));
    }

    #[test]
    fn ds_links_key_to_parent() {
        let (_, dk) = zone_key();
        // A genuine DS built from the key digests correctly.
        let mut h = Sha256::new();
        h.update(encode_name("euro-os.eu"));
        h.update(dk.rdata());
        let ds = Ds { key_tag: key_tag(&dk), algorithm: ALG_ED25519, digest_type: DIGEST_SHA256, digest: h.finalize().to_vec() };
        assert_eq!(verify_ds(&ds, &dk, "euro-os.eu"), Ok(()));
        // A DS with a flipped digest is rejected.
        let mut bad = ds.clone();
        bad.digest[0] ^= 0xFF;
        assert_eq!(verify_ds(&bad, &dk, "euro-os.eu"), Err(DnssecError::DsMismatch));
    }

    #[test]
    fn query_build_aaaa() {
        let q = build_query(0x1234, "euro-os.eu", TYPE_AAAA);
        assert_eq!(&q[0..2], &[0x12, 0x34]);
        assert_eq!(&q[q.len() - 4..], &[0, TYPE_AAAA as u8, 0, CLASS_IN as u8]);
    }
}
