//! Boot self-test for **EuroSign** (AC-4): document signing envelope + binding.
//! Core: [`eurosign`].

use crate::serial_println;
use eurosign::{verify, SignEnvelope, SignManifest, Verdict, VisualAnchor};

pub fn selftest() {
    let env = SignEnvelope {
        manifest: SignManifest::new("contract.pdf", "ABCDEF01", "Jan Vandenberg", 1_700_000_000, "approved"),
        signature_hex: alloc::string::String::from("deadbeef"),
        pubkey_hex: alloc::string::String::from("0011aa"),
        anchor: Some(VisualAnchor { page: 1, x: 100, y: 700, w: 180, h: 60 }),
    };

    // .eurosig round-trip.
    let text = env.to_text();
    let roundtrip = SignEnvelope::from_text(&text).as_ref() == Some(&env);

    // Verification (fake Ed25519 checker; the real one comes from eurotls in the kernel).
    let valid = verify(&env, "abcdef01", |_c, s, p| !s.is_empty() && !p.is_empty()) == Verdict::Valid;
    let tampered = verify(&env, "ffffffff", |_c, _s, _p| true) == Verdict::DocumentTampered;
    let bad_sig = verify(&env, "abcdef01", |_c, _s, _p| false) == Verdict::BadSignature;

    let ok = roundtrip && valid && tampered && bad_sig;
    serial_println!(
        "[sg] EuroSign: .eurosig-round-trip={}, valid={}, tampering-detected={}, bad-signature={} {}",
        roundtrip, valid, tampered, bad_sig,
        if ok { "✓" } else { "✗ ERROR" }
    );
}
