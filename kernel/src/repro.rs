//! Kernel side of **EuroRepro** (plan M3/Q2): reproducible builds. At boot
//! we prove the chain — deterministic spec id, signed attestation, bit-for-
//! bit reproduction, and independent-builder consensus. Host-tested core:
//! [`eurorepro`].

use alloc::string::String;
use alloc::vec::Vec;

use eurorepro::{attest, consensus, sha256, BuildSpec, Reproduction};

fn demo_spec() -> BuildSpec {
    BuildSpec {
        source_hash: sha256(b"// EuroSuite Writer source"),
        toolchain: String::from("eurorustc-1.0"),
        flags: alloc::vec![String::from("-O2"), String::from("--target=x86_64-euro")],
        env: alloc::vec![(String::from("LANG"), String::from("C"))],
    }
}

/// Boot self-test: spec id, attestation+reproduction, consensus, tamper-/volatile detection.
pub fn selftest() {
    let spec = demo_spec();
    let id = spec.id();
    let output = b"\x7fELF EuroSuite-Writer canonical binary";
    let out_hash = sha256(output);

    let b1 = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let b2 = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    let b3 = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);

    // Two independent builders reproduce the same output; one is compromised.
    let atts = alloc::vec![
        attest(&b1, id, out_hash),
        attest(&b2, id, out_hash),
        attest(&b3, id, sha256(b"compromised binary")),
    ];

    let reproduced = atts[0].verify() && atts[0].reproduce(output) == Reproduction::Reproducible;
    let consensus_ok = consensus(&id, &atts, 2) == Some(out_hash);
    // Tampering: claim a different output without re-signing → verification fails.
    let mut tampered = atts[0].clone();
    tampered.output_hash[0] ^= 0xFF;
    let tamper_blocked = !tampered.verify();
    // Non-determinism: a volatile env var makes the build non-reproducible.
    let mut volatile = demo_spec();
    volatile.env.push((String::from("SOURCE_DATE_EPOCH"), String::from("1700000000")));
    let volatile_flagged = !volatile.is_deterministic();

    let ok = reproduced && consensus_ok && tamper_blocked && volatile_flagged;
    crate::serial_println!(
        "[m3] EuroRepro: bit-for-bit-reproduction={reproduced}, 2-builder-consensus={consensus_ok}, forged-attestation-rejected={tamper_blocked}, volatile-input-flagged={volatile_flagged} → {}",
        if ok { "OK (verifiable source→binary, independently confirmed) ✓" } else { "FAILED" }
    );
}

/// `eurorepro` shell command: show the build spec id + reproduction status.
pub fn shell() -> Vec<String> {
    let spec = demo_spec();
    let id = spec.id();
    let hex: String = id.iter().take(12).map(|b| alloc::format!("{b:02x}")).collect();
    alloc::vec![
        String::from("EuroRepro — reproducible builds (deterministic spec → signed attestation → consensus)"),
        alloc::format!("  example build spec id: {hex}…"),
        alloc::format!("  toolchain: {} · flags normalized · env sorted → stable id", spec.toolchain),
        alloc::format!("  deterministic: {} (no volatile inputs)", spec.is_deterministic()),
        String::from("  ≥2 independent builders with the same output → reproducibly confirmed (verifiable sovereignty)"),
    ]
}
