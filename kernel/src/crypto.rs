//! Ed25519-handtekeningverificatie IN de kernel (Track: security).
//!
//! Verify-before-execute: vóór we een programma in ring 3 draaien, controleren we
//! een echte cryptografische handtekening (Ed25519) over de programmabytes tegen
//! de ingebakken EuroOS-developer publieke sleutel. Alleen gesigneerde, ongewijzigde
//! code draait. Dit vervangt de XXH3-integriteitscheck (die alleen toevallige
//! corruptie ving) door echte authenticiteit + integriteit.

use ed25519_dalek::{Signature, VerifyingKey};

/// De ingebakken EuroOS publieke sleutel (Ed25519, 32 bytes) — dezelfde sleutel
/// waarmee de eupkg-toolchain op de host ondertekent (toolchain/eupkg/keys/dev.pub).
pub static EUROOS_PUBKEY: [u8; 32] = *include_bytes!("../../toolchain/eupkg/keys/dev.pub");

/// Verifieer een Ed25519-handtekening (64 bytes) over `msg` met de ingebakken
/// publieke sleutel. Geeft `true` alleen als de handtekening geldig is.
pub fn verify(msg: &[u8], sig: &[u8]) -> bool {
    let vk = match VerifyingKey::from_bytes(&EUROOS_PUBKEY) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let bytes: [u8; 64] = match sig.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    // `verify_strict` weigert ook zwakke/niet-canonieke handtekeningen.
    vk.verify_strict(msg, &Signature::from_bytes(&bytes)).is_ok()
}

/// Korte hex-weergave (eerste 8 bytes) van de publieke sleutel, voor logging.
pub fn pubkey_fingerprint() -> [u8; 8] {
    let mut f = [0u8; 8];
    f.copy_from_slice(&EUROOS_PUBKEY[..8]);
    f
}
