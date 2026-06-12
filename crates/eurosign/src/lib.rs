//! EuroSign — soevereine documentondertekening voor EuroOS (Sprint AC-4).
//!
//! Bestanden ondertekenen met Ed25519 (sleutel uit EuroVault), handtekeningen
//! verifiëren, en een visuele handtekening in een document plaatsen — zonder
//! externe cloud of betaaldienst. Deze crate levert het **canonieke manifest**
//! (doc-hash + ondertekenaar + tijd + doel), een **`.eurosig`-envelopformaat**
//! (tekstueel, parse/serialiseer) en **bindings-verificatie** (klopt de doc-hash
//! met de envelop?). De Ed25519-bewerking zelf blijft in [`eurotls`]/EuroVault;
//! deze crate is crypto-vrij en host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Het canonieke manifest dat ondertekend wordt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignManifest {
    pub doc_name: String,
    /// Hex-gecodeerde document-hash (bv. SHA-256, door de kernel berekend).
    pub doc_hash: String,
    pub signer: String,
    pub signed_at: u64,
    /// Doel/strekking (bv. "akkoord", "ontvangst", "auteur").
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

    /// De **canonieke bytes** die exact ondertekend/geverifieerd worden. Stabiel
    /// formaat (sleutel=waarde, vaste volgorde) zodat verificatie elders
    /// bit-voor-bit reproduceerbaar is.
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

/// Een visuele handtekening-plaatsing in een document (voor PDF-weergave).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisualAnchor {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Een complete `.eurosig`-envelop: manifest + handtekening (+ optionele plaatsing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignEnvelope {
    pub manifest: SignManifest,
    /// Hex-gecodeerde Ed25519-handtekening over `manifest.canonical_bytes()`.
    pub signature_hex: String,
    /// Hex-gecodeerde publieke sleutel van de ondertekenaar.
    pub pubkey_hex: String,
    pub anchor: Option<VisualAnchor>,
}

impl SignEnvelope {
    /// Serialiseer naar het tekstuele `.eurosig`-formaat.
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

    /// Parse het `.eurosig`-formaat. Geeft `None` bij ontbrekende verplichte velden.
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

/// Het resultaat van een verificatie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Geldig: doc-hash bindt én de handtekening klopt.
    Valid,
    /// De handtekening is geldig maar het document is gewijzigd (hash wijkt af).
    DocumentTampered,
    /// De handtekening zelf is ongeldig.
    BadSignature,
}

/// Verifieer een envelop tegen de actuele document-hash en een Ed25519-checker.
///
/// `actual_doc_hash` = hex-hash van het document zoals het nú is. `verify_sig` is
/// een door de aanroeper geleverde Ed25519-verificatie over de canonieke bytes
/// (zo blijft deze crate crypto-vrij; de kernel levert eurotls).
pub fn verify<F>(env: &SignEnvelope, actual_doc_hash: &str, mut verify_sig: F) -> Verdict
where
    F: FnMut(&[u8], &str, &str) -> bool, // (canoniek, sig_hex, pubkey_hex) -> ok
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
            manifest: SignManifest::new("contract.pdf", "ABCDEF01", "Jan Vandenberg", 1_700_000_000, "akkoord"),
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
        assert!(s.contains("hash=abcd\n")); // hash genormaliseerd naar lowercase
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
        assert!(SignEnvelope::from_text(bad).is_none()); // geen hash/at/sig
    }

    #[test]
    fn verify_valid() {
        let env = sample();
        // Nep-checker: handtekening "klopt" als pubkey niet leeg is.
        let v = verify(&env, "abcdef01", |_canon, sig, pk| !sig.is_empty() && !pk.is_empty());
        assert_eq!(v, Verdict::Valid);
    }

    #[test]
    fn verify_detects_tamper() {
        let env = sample();
        // Document gewijzigd → andere hash, maar handtekening "geldig".
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
        // Twee manifesten met verschillende inhoud → verschillende canonieke bytes.
        let a = SignManifest::new("d", "aa", "s", 1, "p");
        let b = SignManifest::new("d", "aa", "s", 2, "p"); // ander tijdstip
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
    }
}
