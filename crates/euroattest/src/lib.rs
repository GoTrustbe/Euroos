//! EuroAttest — remote attestation (plan O2).
//!
//! A verifier wants to know that a EuroOS machine is running in a *trusted state*
//! before granting it access (zero-trust). The machine proves this with a **quote**:
//! the current PCR values (measured boot, O1) + a **nonce** chosen by the verifier,
//! signed by the attestation key (AK) that lives in the TPM. The verifier
//! checks: signature (AK pubkey), nonce match (fresh, no replay) and whether the
//! PCRs match the expected (good) state.
//!
//! Host-tested; the real AK + PCR readout is wired by the kernel via [`eurotpm`].

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

const DOMAIN: &[u8] = b"EuroAttest-quote-v1\0";

/// A single PCR measurement: (index, 32-byte hash).
pub type Pcr = (u8, [u8; 32]);

/// A signed attestation quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    /// The PCR values at the moment of quoting (ascending by index).
    pub pcrs: Vec<Pcr>,
    /// The nonce chosen by the verifier (anti-replay).
    pub nonce: [u8; 32],
    /// Ed25519 signature of the AK over the TBS bytes.
    pub signature: [u8; 64],
}

/// Why a quote is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestError {
    /// The signature does not match the AK pubkey.
    BadSignature,
    /// The nonce in the quote ≠ the expected one (replay or error).
    NonceMismatch,
    /// A PCR deviates from the expected one (modified/untrusted state).
    PcrMismatch { index: u8 },
    /// An expected PCR is missing from the quote.
    PcrMissing { index: u8 },
    /// The AK pubkey is not a valid Ed25519 point.
    BadKey,
}

/// The canonical, domain-separated TBS bytes of a quote.
fn tbs(pcrs: &[Pcr], nonce: &[u8; 32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(DOMAIN.len() + 32 + pcrs.len() * 33);
    b.extend_from_slice(DOMAIN);
    b.extend_from_slice(nonce);
    b.extend_from_slice(&(pcrs.len() as u32).to_le_bytes());
    for (idx, hash) in pcrs {
        b.push(*idx);
        b.extend_from_slice(hash);
    }
    b
}

/// Produce a quote: sign (PCRs ‖ nonce) with the AK. (Kernel side uses the
/// real TPM AK; host tests use a software key.)
pub fn quote(ak: &SigningKey, mut pcrs: Vec<Pcr>, nonce: [u8; 32]) -> Quote {
    pcrs.sort_by_key(|(i, _)| *i);
    let signature = ak.sign(&tbs(&pcrs, &nonce)).to_bytes();
    Quote { pcrs, nonce, signature }
}

/// Verify a quote: signature + nonce + all expected PCRs.
///
/// - `ak_pubkey`     — the trusted AK public key (known out-of-band);
/// - `expected_nonce`— the nonce the verifier itself chose;
/// - `expected_pcrs` — the expected (good) PCR values that must *at least* match.
pub fn verify(
    quote: &Quote,
    ak_pubkey: &[u8; 32],
    expected_nonce: &[u8; 32],
    expected_pcrs: &[Pcr],
) -> Result<(), AttestError> {
    // 1. Signature over the exact TBS bytes.
    let vk = VerifyingKey::from_bytes(ak_pubkey).map_err(|_| AttestError::BadKey)?;
    let sig = Signature::from_bytes(&quote.signature);
    vk.verify(&tbs(&quote.pcrs, &quote.nonce), &sig).map_err(|_| AttestError::BadSignature)?;
    // 2. Fresh nonce (anti-replay).
    if &quote.nonce != expected_nonce {
        return Err(AttestError::NonceMismatch);
    }
    // 3. Every expected PCR must be present and match exactly.
    for (idx, want) in expected_pcrs {
        match quote.pcrs.iter().find(|(i, _)| i == idx) {
            None => return Err(AttestError::PcrMissing { index: *idx }),
            Some((_, got)) if got != want => return Err(AttestError::PcrMismatch { index: *idx }),
            Some(_) => {}
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> alloc::string::String {
    let mut s = alloc::string::String::with_capacity(b.len() * 2);
    for x in b {
        s.push(core::char::from_digit((x >> 4) as u32, 16).unwrap());
        s.push(core::char::from_digit((x & 0xf) as u32, 16).unwrap());
    }
    s
}

/// A full attestation report a verifier consumes over the network: the AK
/// public key + the signed quote. `to_json` serializes it for HTTPS transport;
/// the verifier deserializes it and calls [`verify`].
pub struct Report<'a> {
    pub ak_pubkey: &'a [u8; 32],
    pub quote: &'a Quote,
}
impl Report<'_> {
    /// Serialize the report as JSON (the on-the-wire attestation document).
    pub fn to_json(&self) -> alloc::string::String {
        use alloc::string::String;
        let mut s = String::from("{\"ak\":\"");
        s.push_str(&hex(self.ak_pubkey));
        s.push_str("\",\"nonce\":\"");
        s.push_str(&hex(&self.quote.nonce));
        s.push_str("\",\"sig\":\"");
        s.push_str(&hex(&self.quote.signature));
        s.push_str("\",\"pcrs\":[");
        for (i, (idx, h)) in self.quote.pcrs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&alloc::format!("{{\"index\":{},\"value\":\"{}\"}}", idx, hex(h)));
        }
        s.push_str("]}");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ak() -> SigningKey {
        SigningKey::from_bytes(&[5u8; 32])
    }

    fn good_pcrs() -> Vec<Pcr> {
        alloc::vec![(0u8, [0xAA; 32]), (7u8, [0xBB; 32])]
    }

    #[test]
    fn fresh_quote_verifies() {
        let k = ak();
        let nonce = [9u8; 32];
        let q = quote(&k, good_pcrs(), nonce);
        assert_eq!(verify(&q, &k.verifying_key().to_bytes(), &nonce, &good_pcrs()), Ok(()));
    }

    #[test]
    fn replay_with_old_nonce_rejected() {
        let k = ak();
        let q = quote(&k, good_pcrs(), [1u8; 32]);
        // Verifier expects a fresh nonce [2;32].
        assert_eq!(
            verify(&q, &k.verifying_key().to_bytes(), &[2u8; 32], &good_pcrs()),
            Err(AttestError::NonceMismatch)
        );
    }

    #[test]
    fn untrusted_pcr_state_rejected() {
        let k = ak();
        let nonce = [9u8; 32];
        // The machine reported a modified PCR7 (e.g. an unsigned kernel).
        let bad = alloc::vec![(0u8, [0xAA; 32]), (7u8, [0xEE; 32])];
        let q = quote(&k, bad, nonce);
        assert_eq!(
            verify(&q, &k.verifying_key().to_bytes(), &nonce, &good_pcrs()),
            Err(AttestError::PcrMismatch { index: 7 })
        );
    }

    #[test]
    fn missing_expected_pcr_rejected() {
        let k = ak();
        let nonce = [9u8; 32];
        let q = quote(&k, alloc::vec![(0u8, [0xAA; 32])], nonce);
        assert_eq!(
            verify(&q, &k.verifying_key().to_bytes(), &nonce, &good_pcrs()),
            Err(AttestError::PcrMissing { index: 7 })
        );
    }

    #[test]
    fn wrong_ak_rejected() {
        let k = ak();
        let nonce = [9u8; 32];
        let q = quote(&k, good_pcrs(), nonce);
        let other = SigningKey::from_bytes(&[6u8; 32]).verifying_key().to_bytes();
        assert_eq!(verify(&q, &other, &nonce, &good_pcrs()), Err(AttestError::BadSignature));
    }

    #[test]
    fn tampered_signature_rejected() {
        let k = ak();
        let nonce = [9u8; 32];
        let mut q = quote(&k, good_pcrs(), nonce);
        q.signature[0] ^= 0xFF;
        assert_eq!(
            verify(&q, &k.verifying_key().to_bytes(), &nonce, &good_pcrs()),
            Err(AttestError::BadSignature)
        );
    }
}
