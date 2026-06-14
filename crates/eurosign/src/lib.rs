//! EuroSign — sovereign document signing for EuroOS (Sprint AC-4).
//!
//! Sign files with Ed25519 (key from EuroVault), verify signatures,
//! and place a visual signature in a document — without
//! an external cloud or paid service. This crate provides the **canonical manifest**
//! (doc hash + signer + time + purpose), a **`.eurosig` envelope format**
//! (textual, parse/serialize) and **binding verification** (does the doc hash
//! match the envelope?). The Ed25519 operation itself stays in [`eurotls`]/EuroVault;
//! this crate is crypto-free and host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The canonical manifest that gets signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignManifest {
    pub doc_name: String,
    /// Hex-encoded document hash (e.g. SHA-256, computed by the kernel).
    pub doc_hash: String,
    pub signer: String,
    pub signed_at: u64,
    /// Purpose/intent (e.g. "agreement", "receipt", "author").
    pub purpose: String,
}

impl SignManifest {
    pub fn new(doc_name: &str, doc_hash: &str, signer: &str, signed_at: u64, purpose: &str) -> Self {
        SignManifest {
            doc_name: doc_name.to_string(),
            doc_hash: doc_hash.to_ascii_lowercase(),
            signer: signer.to_string(),
            signed_at,
            purpose: purpose.to_string(),
        }
    }

    /// The **canonical bytes** that are signed/verified exactly. Stable
    /// format (key=value, fixed order) so verification elsewhere is
    /// bit-for-bit reproducible.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str("EuroSign-v1\n");
        s.push_str(&alloc::format!("doc={}\n", self.doc_name));
        s.push_str(&alloc::format!("hash={}\n", self.doc_hash));
        s.push_str(&alloc::format!("signer={}\n", self.signer));
        s.push_str(&alloc::format!("at={}\n", self.signed_at));
        s.push_str(&alloc::format!("purpose={}\n", self.purpose));
        s.into_bytes()
    }
}

/// A visual signature placement in a document (for PDF rendering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualAnchor {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A complete `.eurosig` envelope: manifest + signature (+ optional placement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignEnvelope {
    pub manifest: SignManifest,
    /// Hex-encoded Ed25519 signature over `manifest.canonical_bytes()`.
    pub signature_hex: String,
    /// Hex-encoded public key of the signer.
    pub pubkey_hex: String,
    pub anchor: Option<VisualAnchor>,
}

impl SignEnvelope {
    /// Serialize to the textual `.eurosig` format.
    pub fn to_text(&self) -> String {
        let m = &self.manifest;
        let mut s = String::new();
        s.push_str("-----BEGIN EUROSIG-----\n");
        s.push_str(&alloc::format!("doc: {}\n", m.doc_name));
        s.push_str(&alloc::format!("hash: {}\n", m.doc_hash));
        s.push_str(&alloc::format!("signer: {}\n", m.signer));
        s.push_str(&alloc::format!("at: {}\n", m.signed_at));
        s.push_str(&alloc::format!("purpose: {}\n", m.purpose));
        s.push_str(&alloc::format!("pubkey: {}\n", self.pubkey_hex));
        s.push_str(&alloc::format!("sig: {}\n", self.signature_hex));
        if let Some(a) = self.anchor {
            s.push_str(&alloc::format!("anchor: {} {} {} {} {}\n", a.page, a.x, a.y, a.w, a.h));
        }
        s.push_str("-----END EUROSIG-----\n");
        s
    }

    /// Parse the `.eurosig` format. Returns `None` on missing required fields.
    pub fn from_text(text: &str) -> Option<SignEnvelope> {
        let mut doc = None;
        let mut hash = None;
        let mut signer = None;
        let mut at = None;
        let mut purpose = String::new();
        let mut pubkey = String::new();
        let mut sig = None;
        let mut anchor = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("-----") {
                continue;
            }
            let (k, v) = match line.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            match k {
                "doc" => doc = Some(v.to_string()),
                "hash" => hash = Some(v.to_ascii_lowercase()),
                "signer" => signer = Some(v.to_string()),
                "at" => at = v.parse::<u64>().ok(),
                "purpose" => purpose = v.to_string(),
                "pubkey" => pubkey = v.to_string(),
                "sig" => sig = Some(v.to_string()),
                "anchor" => {
                    let nums: Vec<u32> = v.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                    if nums.len() == 5 {
                        anchor = Some(VisualAnchor {
                            page: nums[0],
                            x: nums[1],
                            y: nums[2],
                            w: nums[3],
                            h: nums[4],
                        });
                    }
                }
                _ => {}
            }
        }
        Some(SignEnvelope {
            manifest: SignManifest::new(&doc?, &hash?, &signer?, at?, &purpose),
            signature_hex: sig?,
            pubkey_hex: pubkey,
            anchor,
        })
    }
}

/// The result of a verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Valid: doc hash binds AND the signature is correct.
    Valid,
    /// The signature is valid but the document was changed (hash differs).
    DocumentTampered,
    /// The signature itself is invalid.
    BadSignature,
}

/// Verify an envelope against the current document hash and an Ed25519 checker.
///
/// `actual_doc_hash` = hex hash of the document as it is now. `verify_sig` is
/// a caller-supplied Ed25519 verification over the canonical bytes
/// (so this crate stays crypto-free; the kernel provides eurotls).
pub fn verify<F>(env: &SignEnvelope, actual_doc_hash: &str, mut verify_sig: F) -> Verdict
where
    F: FnMut(&[u8], &str, &str) -> bool, // (canonical, sig_hex, pubkey_hex) -> ok
{
    let sig_ok = verify_sig(&env.manifest.canonical_bytes(), &env.signature_hex, &env.pubkey_hex);
    if !sig_ok {
        return Verdict::BadSignature;
    }
    if env.manifest.doc_hash != actual_doc_hash.to_ascii_lowercase() {
        return Verdict::DocumentTampered;
    }
    Verdict::Valid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SignEnvelope {
        SignEnvelope {
            manifest: SignManifest::new("contract.pdf", "ABCDEF01", "Jan Vandenberg", 1_700_000_000, "agreement"),
            signature_hex: "deadbeef".to_string(),
            pubkey_hex: "0011aa".to_string(),
            anchor: Some(VisualAnchor { page: 1, x: 100, y: 700, w: 180, h: 60 }),
        }
    }

    #[test]
    fn canonical_is_stable_and_lowercased_hash() {
        let m = SignManifest::new("d", "ABCD", "s", 5, "p");
        let s = alloc::string::String::from_utf8(m.canonical_bytes()).unwrap();
        assert!(s.starts_with("EuroSign-v1\n"));
        assert!(s.contains("hash=abcd\n")); // hash normalized to lowercase
    }

    #[test]
    fn envelope_roundtrip() {
        let env = sample();
        let text = env.to_text();
        assert!(text.contains("-----BEGIN EUROSIG-----"));
        let back = SignEnvelope::from_text(&text).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn parse_missing_field_fails() {
        let bad = "-----BEGIN EUROSIG-----\ndoc: x\nsigner: y\n-----END EUROSIG-----\n";
        assert!(SignEnvelope::from_text(bad).is_none()); // no hash/at/sig
    }

    #[test]
    fn verify_valid() {
        let env = sample();
        // Fake checker: signature "matches" if pubkey is not empty.
        let v = verify(&env, "abcdef01", |_canon, sig, pk| !sig.is_empty() && !pk.is_empty());
        assert_eq!(v, Verdict::Valid);
    }

    #[test]
    fn verify_detects_tamper() {
        let env = sample();
        // Document changed → different hash, but signature "valid".
        let v = verify(&env, "ffffffff", |_c, _s, _p| true);
        assert_eq!(v, Verdict::DocumentTampered);
    }

    #[test]
    fn verify_bad_signature() {
        let env = sample();
        let v = verify(&env, "abcdef01", |_c, _s, _p| false);
        assert_eq!(v, Verdict::BadSignature);
    }

    #[test]
    fn canonical_changes_break_signature() {
        // Two manifests with different content → different canonical bytes.
        let a = SignManifest::new("d", "aa", "s", 1, "p");
        let b = SignManifest::new("d", "aa", "s", 2, "p"); // different timestamp
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
    }
}
