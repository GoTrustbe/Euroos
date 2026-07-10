//! 3D-2 — EuroVerity boot self-test: prove the read-only system image can be
//! integrity-checked block-by-block against an Ed25519-signed Merkle root, so a
//! tampered image is detected and the loader can fall back to the good slot.

use ed25519_dalek::SigningKey;
use euroverity::{verify_block, Manifest, VerityTree};

/// `[3d2]` self-test.
pub fn selftest() {
    // A stand-in read-only system image.
    let mut img = alloc::vec![0u8; 8192];
    for (i, b) in img.iter_mut().enumerate() {
        *b = (i as u32 * 31 + 7) as u8;
    }
    let mut salt = [0u8; 32];
    crate::entropy::getrandom(&mut salt);
    let tree = VerityTree::build(&img, 512, salt);

    // The release-signed manifest (test key from the CSPRNG; in production the
    // root is signed offline by the same key that signs A/B images).
    let mut seed = [0u8; 32];
    crate::entropy::getrandom(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let pubkey = key.verifying_key().to_bytes();
    let manifest = Manifest::of(&tree);
    let sig = manifest.sign(&key);
    let manifest_ok = manifest.verify(&pubkey, &sig);

    // A genuine block verifies against the signed root.
    let idx = 3usize;
    let block = &img[idx * 512..(idx + 1) * 512];
    let good = verify_block(&manifest.root, &manifest.salt, idx, block, &tree.proof(idx));

    // Tamper a byte → verification against the signed root fails (detected).
    let mut bad = block.to_vec();
    bad[10] ^= 0xFF;
    let tamper_detected = !verify_block(&manifest.root, &manifest.salt, idx, &bad, &tree.proof(idx));

    // A forged manifest (wrong root under the real signature) is refused.
    let mut m2 = manifest.clone();
    m2.root[0] ^= 0x01;
    let forged_refused = !m2.verify(&pubkey, &sig);

    let ok = manifest_ok && good && tamper_detected && forged_refused;
    crate::serial_println!(
        "[3d2] EuroVerity (dm-verity-style, Ed25519-signed Merkle root over {} blocks): manifest-signature-verified={manifest_ok}, block-verified-against-signed-root={good}, tampered-block-DETECTED={tamper_detected}, forged-manifest-REFUSED={forged_refused} → {}",
        tree.block_count,
        if ok { "OK (system image integrity provable block-by-block; tamper ⇒ fall back to the good slot) ✓" } else { "FAILED" }
    );
}
