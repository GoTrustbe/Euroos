//! Boot-zelftest voor **EuroSign** (AC-4): documentondertekening-envelop + binding.
//! Kern: [`eurosign`].

use crate::serial_println;
use eurosign::{verify, SignEnvelope, SignManifest, Verdict, VisualAnchor};

pub fn selftest() {
    let env = SignEnvelope {
        manifest: SignManifest::new("contract.pdf", "ABCDEF01", "Jan Vandenberg", 1_700_000_000, "akkoord"),
        signature_hex: alloc::string::String::from("deadbeef"),
        pubkey_hex: alloc::string::String::from("0011aa"),
        anchor: Some(VisualAnchor { page: 1, x: 100, y: 700, w: 180, h: 60 }),
    };

    // .eurosig round-trip.
    let text = env.to_text();
    let roundtrip = SignEnvelope::from_text(&text).as_ref() == Some(&env);

    // Verificatie (nep-Ed25519-checker; de echte komt van eurotls in de kernel).
    let valid = verify(&env, "abcdef01", |_c, s, p| !s.is_empty() && !p.is_empty()) == Verdict::Valid;
    let tampered = verify(&env, "ffffffff", |_c, _s, _p| true) == Verdict::DocumentTampered;
    let bad_sig = verify(&env, "abcdef01", |_c, _s, _p| false) == Verdict::BadSignature;

    let ok = roundtrip && valid && tampered && bad_sig;
    serial_println!(
        "[sg] EuroSign: .eurosig-round-trip={}, geldig={}, wijziging-gedetecteerd={}, slechte-handtekening={} {}",
        roundtrip, valid, tampered, bad_sig,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
