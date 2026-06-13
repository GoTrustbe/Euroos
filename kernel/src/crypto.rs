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

/// Een ECHTE update-image + Ed25519-handtekening, op de host met de EuroOS-
/// developer-sleutel (`dev.key`) getekend (`toolchain/eupkg/sign-test-image.py`).
/// Beide artefacten zijn publiek (ze verifiëren tegen de ingebakken `dev.pub`) en
/// gecommit, zodat de build hermetisch is zónder de privésleutel.
static TEST_IMG: &[u8] = include_bytes!("testdata/update-test.img");
static TEST_SIG: &[u8] = include_bytes!("testdata/update-test.img.sig");

/// Een geldige handtekening van het testimage (voor de update-pijplijn-zelftest).
pub fn test_update_image() -> (&'static [u8], &'static [u8]) {
    (TEST_IMG, TEST_SIG)
}

/// **[upd3] — verify-before-activate bewezen met een ECHTE Ed25519-handtekening.**
/// Bewijst tegen de ingebakken `dev.pub` dat een geldige handtekening WORDT
/// aanvaard en dat élke wijziging (aan het image óf aan de handtekening) WORDT
/// geweigerd — de kern van "een gemanipuleerde update kan nooit geactiveerd worden".
pub fn selftest() {
    let genuine = verify(TEST_IMG, TEST_SIG);

    // 1 byte in het image flippen → handtekening moet ongeldig worden.
    let mut bad_img = TEST_IMG.to_vec();
    bad_img[100] ^= 0xFF;
    let tampered_image_refused = !verify(&bad_img, TEST_SIG);

    // 1 byte in de handtekening flippen → moet ongeldig worden.
    let mut bad_sig = TEST_SIG.to_vec();
    bad_sig[10] ^= 0xFF;
    let tampered_sig_refused = !verify(TEST_IMG, &bad_sig);

    // Verkeerde lengte → geweigerd (geen paniek).
    let short_sig_refused = !verify(TEST_IMG, &TEST_SIG[..63]);

    let ok = genuine && tampered_image_refused && tampered_sig_refused && short_sig_refused;
    let fp = pubkey_fingerprint();
    crate::serial_println!(
        "[upd3] Ed25519 verify-before-activate (dev.pub {:02x}{:02x}{:02x}{:02x}…): echt={} · image-tamper-geweigerd={} · sig-tamper-geweigerd={} · korte-sig-geweigerd={} → {}",
        fp[0], fp[1], fp[2], fp[3],
        genuine, tampered_image_refused, tampered_sig_refused, short_sig_refused,
        if ok { "OK ✓" } else { "MISLUKT ✗" }
    );
}
