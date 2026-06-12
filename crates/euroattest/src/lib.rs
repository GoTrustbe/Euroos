//! EuroAttest — remote attestation (plan O2).
//!
//! Een verifier wil weten dat een EuroOS-machine in een *vertrouwde toestand* draait
//! vóór hij 'm toegang geeft (zero-trust). De machine bewijst dat met een **quote**:
//! de huidige PCR-waarden (measured boot, O1) + een door de verifier gekozen **nonce**,
//! ondertekend door de attestatiesleutel (AK) die in de TPM leeft. De verifier
//! controleert: handtekening (AK-pubkey), nonce-match (vers, geen replay) en of de
//! PCR's overeenkomen met de verwachte (goede) toestand.
//!
//! Host-getest; de echte AK + PCR-uitlezing koppelt de kernel via [`eurotpm`].

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

const DOMAIN: &[u8] = b"EuroAttest-quote-v1\0";

/// Eén PCR-meting: (index, 32-byte hash).
pub type Pcr = (u8, [u8; 32]);

/// Een ondertekende attestatie-quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
    /// De PCR-waarden op het moment van quoten (oplopend op index).
    pub pcrs: Vec<Pcr>,
    /// De door de verifier gekozen nonce (anti-replay).
    pub nonce: [u8; 32],
    /// Ed25519-handtekening van de AK over de TBS-bytes.
    pub signature: [u8; 64],
}

/// Waarom een quote afgewezen wordt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestError {
    /// De handtekening klopt niet voor de AK-pubkey.
    BadSignature,
    /// De nonce in de quote ≠ de verwachte (replay of fout).
    NonceMismatch,
    /// Een PCR wijkt af van de verwachte (gewijzigde/niet-vertrouwde toestand).
    PcrMismatch { index: u8 },
    /// Een verwachte PCR ontbreekt in de quote.
    PcrMissing { index: u8 },
    /// De AK-pubkey is geen geldig Ed25519-punt.
    BadKey,
}

/// De canonieke, domein-gescheiden TBS-bytes van een quote.
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

/// Produceer een quote: teken (PCR's ‖ nonce) met de AK. (Kernel-zijde gebruikt de
/// échte TPM-AK; host-tests een software-sleutel.)
pub fn quote(ak: &SigningKey, mut pcrs: Vec<Pcr>, nonce: [u8; 32]) -> Quote {
    pcrs.sort_by_key(|(i, _)| *i);
    let signature = ak.sign(&tbs(&pcrs, &nonce)).to_bytes();
    Quote { pcrs, nonce, signature }
}

/// Verifieer een quote: handtekening + nonce + alle verwachte PCR's.
///
/// - `ak_pubkey`     — de vertrouwde AK-publieke sleutel (out-of-band bekend);
/// - `expected_nonce`— de nonce die de verifier zelf koos;
/// - `expected_pcrs` — de verwachte (goede) PCR-waarden die *minstens* moeten kloppen.
pub fn verify(
    quote: &Quote,
    ak_pubkey: &[u8; 32],
    expected_nonce: &[u8; 32],
    expected_pcrs: &[Pcr],
) -> Result<(), AttestError> {
    // 1. Handtekening over de exacte TBS-bytes.
    let vk = VerifyingKey::from_bytes(ak_pubkey).map_err(|_| AttestError::BadKey)?;
    let sig = Signature::from_bytes(&quote.signature);
    vk.verify(&tbs(&quote.pcrs, &quote.nonce), &sig).map_err(|_| AttestError::BadSignature)?;
    // 2. Verse nonce (anti-replay).
    if &quote.nonce != expected_nonce {
        return Err(AttestError::NonceMismatch);
    }
    // 3. Elke verwachte PCR moet aanwezig zijn én exact kloppen.
    for (idx, want) in expected_pcrs {
        match quote.pcrs.iter().find(|(i, _)| i == idx) {
            None => return Err(AttestError::PcrMissing { index: *idx }),
            Some((_, got)) if got != want => return Err(AttestError::PcrMismatch { index: *idx }),
            Some(_) => {}
        }
    }
    Ok(())
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
        // Verifier verwacht een verse nonce [2;32].
        assert_eq!(
            verify(&q, &k.verifying_key().to_bytes(), &[2u8; 32], &good_pcrs()),
            Err(AttestError::NonceMismatch)
        );
    }

    #[test]
    fn untrusted_pcr_state_rejected() {
        let k = ak();
        let nonce = [9u8; 32];
        // De machine boette een gewijzigde PCR7 (bv. niet-getekende kernel).
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
