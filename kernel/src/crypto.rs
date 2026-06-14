//! Ed25519 signature verification IN the kernel (Track: security).
//!
//! Verify-before-execute: before we run a program in ring 3, we check
//! a real cryptographic signature (Ed25519) over the program bytes against
//! the baked-in EuroOS developer public key. Only signed, unmodified
//! code runs. This replaces the XXH3 integrity check (which only caught accidental
//! corruption) with real authenticity + integrity.

use ed25519_dalek::{Signature, VerifyingKey};

/// The baked-in EuroOS public key (Ed25519, 32 bytes) — the same key
/// that the eupkg toolchain on the host signs with (toolchain/eupkg/keys/dev.pub).
pub static EUROOS_PUBKEY: [u8; 32] = *include_bytes!("../../toolchain/eupkg/keys/dev.pub");

/// Verify an Ed25519 signature (64 bytes) over `msg` with the baked-in
/// public key. Returns `true` only if the signature is valid.
pub fn verify(msg: &[u8], sig: &[u8]) -> bool {
    let vk = match VerifyingKey::from_bytes(&EUROOS_PUBKEY) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let bytes: [u8; 64] = match sig.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    // `verify_strict` also rejects weak/non-canonical signatures.
    vk.verify_strict(msg, &Signature::from_bytes(&bytes)).is_ok()
}

/// Short hex rendering (first 8 bytes) of the public key, for logging.
pub fn pubkey_fingerprint() -> [u8; 8] {
    let mut f = [0u8; 8];
    f.copy_from_slice(&EUROOS_PUBKEY[..8]);
    f
}

/// A REAL update image + Ed25519 signature, signed on the host with the EuroOS
/// developer key (`dev.key`) (`toolchain/eupkg/sign-test-image.py`).
/// Both artifacts are public (they verify against the baked-in `dev.pub`) and
/// committed, so the build is hermetic without the private key.
static TEST_IMG: &[u8] = include_bytes!("testdata/update-test.img");
static TEST_SIG: &[u8] = include_bytes!("testdata/update-test.img.sig");

/// A valid signature of the test image (for the update-pipeline self-test).
pub fn test_update_image() -> (&'static [u8], &'static [u8]) {
    (TEST_IMG, TEST_SIG)
}

/// **[upd3] — verify-before-activate proven with a REAL Ed25519 signature.**
/// Proves against the baked-in `dev.pub` that a valid signature IS
/// accepted and that ANY change (to the image OR to the signature) IS
/// rejected — the core of "a tampered update can never be activated".
pub fn selftest() {
    let genuine = verify(TEST_IMG, TEST_SIG);

    // Flip 1 byte in the image → signature must become invalid.
    let mut bad_img = TEST_IMG.to_vec();
    bad_img[100] ^= 0xFF;
    let tampered_image_refused = !verify(&bad_img, TEST_SIG);

    // Flip 1 byte in the signature → must become invalid.
    let mut bad_sig = TEST_SIG.to_vec();
    bad_sig[10] ^= 0xFF;
    let tampered_sig_refused = !verify(TEST_IMG, &bad_sig);

    // Wrong length → rejected (no panic).
    let short_sig_refused = !verify(TEST_IMG, &TEST_SIG[..63]);

    let ok = genuine && tampered_image_refused && tampered_sig_refused && short_sig_refused;
    let fp = pubkey_fingerprint();
    crate::serial_println!(
        "[upd3] Ed25519 verify-before-activate (dev.pub {:02x}{:02x}{:02x}{:02x}…): genuine={} · image-tamper-refused={} · sig-tamper-refused={} · short-sig-refused={} → {}",
        fp[0], fp[1], fp[2], fp[3],
        genuine, tampered_image_refused, tampered_sig_refused, short_sig_refused,
        if ok { "OK ✓" } else { "FAILED ✗" }
    );
}
