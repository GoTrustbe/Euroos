//! EuroRepro — reproduceerbare builds (plan M3/Q2).
//!
//! Soevereiniteit vereist *verifieerbaarheid*: een derde moet kunnen aantonen dat
//! een binary écht uit de gepubliceerde broncode komt. Dit crate levert de kern:
//! een **deterministische build-spec** (genormaliseerde inputs → stabiele hash), een
//! door de bouwer **Ed25519-getekende attestatie** (`spec_id` + `output_hash`), en
//! **onafhankelijke-reproductie-consensus** — als ≥2 losse bouwers dezelfde output
//! voor dezelfde spec attesteren, is de build *reproduceerbaar bevestigd*.
//!
//! Pure, host-geteste `no_std`-logica (`sha2` + `ed25519-dalek`).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"EuroRepro-attest-v1\0";

/// Een SHA-256-hash.
pub type Hash = [u8; 32];

/// SHA-256 van wat bytes.
pub fn sha256(data: &[u8]) -> Hash {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// De genormaliseerde inputs van een build. De *spec-id* is hun deterministische
/// hash: dezelfde inputs → dezelfde id, ongeacht volgorde van env/flags.
pub struct BuildSpec {
    pub source_hash: Hash,
    pub toolchain: String,
    pub flags: Vec<String>,
    /// Omgevingsvariabelen (worden gesorteerd → volgorde-onafhankelijk).
    pub env: Vec<(String, String)>,
}

/// Volatiele env-vars die een build niet-reproduceerbaar maken (tijd, willekeur, paden).
const VOLATILE_ENV: &[&str] = &["SOURCE_DATE_EPOCH", "RANDOM", "HOSTNAME", "PWD", "BUILD_ID", "TIMESTAMP"];

impl BuildSpec {
    /// De deterministische spec-id (hash van de canoniek-geëncodeerde inputs).
    pub fn id(&self) -> Hash {
        let mut flags = self.flags.clone();
        flags.sort();
        let mut env = self.env.clone();
        env.sort();

        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update(self.source_hash);
        push(&mut h, self.toolchain.as_bytes());
        h.update((flags.len() as u32).to_le_bytes());
        for f in &flags {
            push(&mut h, f.as_bytes());
        }
        h.update((env.len() as u32).to_le_bytes());
        for (k, v) in &env {
            push(&mut h, k.as_bytes());
            push(&mut h, v.as_bytes());
        }
        h.finalize().into()
    }

    /// Welke (genormaliseerde) env-keys zijn volatiel? Hun aanwezigheid betekent dat
    /// de build niet bit-voor-bit reproduceerbaar zal zijn — een waarschuwing.
    pub fn volatile_inputs(&self) -> Vec<String> {
        self.env
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| VOLATILE_ENV.contains(k))
            .map(|k| k.to_string())
            .collect()
    }

    /// Is deze spec deterministisch (geen volatiele inputs)?
    pub fn is_deterministic(&self) -> bool {
        self.volatile_inputs().is_empty()
    }
}

fn push(h: &mut Sha256, b: &[u8]) {
    h.update((b.len() as u32).to_le_bytes());
    h.update(b);
}

/// Een door één bouwer getekende build-attestatie: "spec `spec_id` produceerde
/// een binary met hash `output_hash`".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attestation {
    pub spec_id: Hash,
    pub output_hash: Hash,
    pub builder: [u8; 32],
    pub signature: [u8; 64],
}

/// De uitkomst van een reproductie-poging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reproduction {
    /// Onze herbouwde output komt bit-voor-bit overeen met de attestatie.
    Reproducible,
    /// De output wijkt af — de attestatie klopt niet bij deze bron/toolchain.
    Mismatch,
}

fn attest_tbs(spec_id: &Hash, output_hash: &Hash) -> Vec<u8> {
    let mut b = Vec::with_capacity(DOMAIN.len() + 64);
    b.extend_from_slice(DOMAIN);
    b.extend_from_slice(spec_id);
    b.extend_from_slice(output_hash);
    b
}

/// Maak een getekende attestatie (de bouwer ondertekent spec_id ‖ output_hash).
pub fn attest(builder: &SigningKey, spec_id: Hash, output_hash: Hash) -> Attestation {
    let signature = builder.sign(&attest_tbs(&spec_id, &output_hash)).to_bytes();
    Attestation { spec_id, output_hash, builder: builder.verifying_key().to_bytes(), signature }
}

impl Attestation {
    /// Verifieer de handtekening van de bouwer over de attestatie.
    pub fn verify(&self) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.builder) else {
            return false;
        };
        let sig = Signature::from_bytes(&self.signature);
        vk.verify(&attest_tbs(&self.spec_id, &self.output_hash), &sig).is_ok()
    }

    /// Vergelijk onze eigen herbouwde binary met de geattesteerde output.
    pub fn reproduce(&self, rebuilt_output: &[u8]) -> Reproduction {
        if sha256(rebuilt_output) == self.output_hash {
            Reproduction::Reproducible
        } else {
            Reproduction::Mismatch
        }
    }
}

/// Onafhankelijke-reproductie-consensus: gegeven attestaties van losse bouwers voor
/// dezelfde `spec_id`, geef de `output_hash` waarover **minstens `quorum`** geldig-
/// getekende, *verschillende* bouwers het eens zijn. `None` = geen consensus.
pub fn consensus(spec_id: &Hash, attestations: &[Attestation], quorum: usize) -> Option<Hash> {
    // Tel per output_hash het aantal unieke, geldige bouwers.
    let mut tally: Vec<(Hash, Vec<[u8; 32]>)> = Vec::new();
    for a in attestations {
        if &a.spec_id != spec_id || !a.verify() {
            continue;
        }
        let entry = match tally.iter_mut().find(|(h, _)| h == &a.output_hash) {
            Some(e) => e,
            None => {
                tally.push((a.output_hash, Vec::new()));
                tally.last_mut().unwrap()
            }
        };
        if !entry.1.contains(&a.builder) {
            entry.1.push(a.builder);
        }
    }
    tally.into_iter().find(|(_, builders)| builders.len() >= quorum).map(|(h, _)| h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> BuildSpec {
        BuildSpec {
            source_hash: sha256(b"fn main() {}"),
            toolchain: "eurorustc-1.0".to_string(),
            flags: alloc::vec!["-O2".to_string(), "--target=x86_64-euro".to_string()],
            env: alloc::vec![("LANG".to_string(), "C".to_string())],
        }
    }

    #[test]
    fn spec_id_is_order_independent() {
        let a = spec();
        let mut b = spec();
        b.flags.reverse();
        b.env.push(("EXTRA".to_string(), "1".to_string()));
        b.env.reverse();
        // Zelfde flags (andere volgorde) → zelfde id; extra env → andere id.
        assert_eq!(a.id(), { let mut c = spec(); c.flags.reverse(); c.id() });
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn detects_volatile_inputs() {
        let mut s = spec();
        assert!(s.is_deterministic());
        s.env.push(("SOURCE_DATE_EPOCH".to_string(), "1700000000".to_string()));
        assert!(!s.is_deterministic());
        assert_eq!(s.volatile_inputs(), alloc::vec!["SOURCE_DATE_EPOCH".to_string()]);
    }

    #[test]
    fn attestation_signed_and_reproducible() {
        let builder = SigningKey::from_bytes(&[1u8; 32]);
        let output = b"\x7fELF...the binary...";
        let att = attest(&builder, spec().id(), sha256(output));
        assert!(att.verify());
        assert_eq!(att.reproduce(output), Reproduction::Reproducible);
        assert_eq!(att.reproduce(b"different binary"), Reproduction::Mismatch);
    }

    #[test]
    fn tampered_attestation_fails() {
        let builder = SigningKey::from_bytes(&[1u8; 32]);
        let mut att = attest(&builder, spec().id(), sha256(b"x"));
        att.output_hash[0] ^= 0xFF; // claim een andere output zonder her-tekenen
        assert!(!att.verify());
    }

    #[test]
    fn consensus_needs_independent_builders() {
        let id = spec().id();
        let out = sha256(b"the canonical binary");
        let b1 = SigningKey::from_bytes(&[1u8; 32]);
        let b2 = SigningKey::from_bytes(&[2u8; 32]);
        let b3 = SigningKey::from_bytes(&[3u8; 32]);
        // Twee bouwers eens over `out`, één bouwer een afwijkende output.
        let atts = alloc::vec![
            attest(&b1, id, out),
            attest(&b2, id, out),
            attest(&b3, id, sha256(b"compromised binary")),
        ];
        assert_eq!(consensus(&id, &atts, 2), Some(out));
        // Quorum 3 wordt niet gehaald.
        assert_eq!(consensus(&id, &atts, 3), None);
    }

    #[test]
    fn same_builder_twice_is_not_consensus() {
        let id = spec().id();
        let out = sha256(b"bin");
        let b1 = SigningKey::from_bytes(&[1u8; 32]);
        // Dezelfde bouwer twee keer telt als één.
        let atts = alloc::vec![attest(&b1, id, out), attest(&b1, id, out)];
        assert_eq!(consensus(&id, &atts, 2), None);
    }
}
