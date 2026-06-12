//! Kernel-zijde van **EuroRepro** (plan M3/Q2): reproduceerbare builds. Bij boot
//! bewijzen we de keten — deterministische spec-id, getekende attestatie, bit-voor-
//! bit-reproductie, en onafhankelijke-bouwers-consensus. Host-geteste kern:
//! [`eurorepro`].

use alloc::string::String;
use alloc::vec::Vec;

use eurorepro::{attest, consensus, sha256, BuildSpec, Reproduction};

fn demo_spec() -> BuildSpec {
    BuildSpec {
        source_hash: sha256(b"// EuroSuite Writer bron"),
        toolchain: String::from("eurorustc-1.0"),
        flags: alloc::vec![String::from("-O2"), String::from("--target=x86_64-euro")],
        env: alloc::vec![(String::from("LANG"), String::from("C"))],
    }
}

/// Boot-zelftest: spec-id, attestatie+reproductie, consensus, tamper-/volatiel-detectie.
pub fn selftest() {
    let spec = demo_spec();
    let id = spec.id();
    let output = b"\x7fELF EuroSuite-Writer canonieke binary";
    let out_hash = sha256(output);

    let b1 = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let b2 = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    let b3 = ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]);

    // Twee onafhankelijke bouwers reproduceren dezelfde output; één is gecompromitteerd.
    let atts = alloc::vec![
        attest(&b1, id, out_hash),
        attest(&b2, id, out_hash),
        attest(&b3, id, sha256(b"gecompromitteerde binary")),
    ];

    let reproduced = atts[0].verify() && atts[0].reproduce(output) == Reproduction::Reproducible;
    let consensus_ok = consensus(&id, &atts, 2) == Some(out_hash);
    // Manipulatie: claim een andere output zonder her-tekenen → verificatie faalt.
    let mut tampered = atts[0].clone();
    tampered.output_hash[0] ^= 0xFF;
    let tamper_blocked = !tampered.verify();
    // Niet-determinisme: een volatiele env-var maakt de build niet-reproduceerbaar.
    let mut volatile = demo_spec();
    volatile.env.push((String::from("SOURCE_DATE_EPOCH"), String::from("1700000000")));
    let volatile_flagged = !volatile.is_deterministic();

    let ok = reproduced && consensus_ok && tamper_blocked && volatile_flagged;
    crate::serial_println!(
        "[m3] EuroRepro: bit-voor-bit-reproductie={reproduced}, 2-bouwer-consensus={consensus_ok}, vervalste-attestatie-geweigerd={tamper_blocked}, volatiele-input-gemarkeerd={volatile_flagged} → {}",
        if ok { "OK (verifieerbare bron→binary, onafhankelijk bevestigd) ✓" } else { "MISLUKT" }
    );
}

/// `eurorepro`-shellcommando: toon de build-spec-id + reproductie-status.
pub fn shell() -> Vec<String> {
    let spec = demo_spec();
    let id = spec.id();
    let hex: String = id.iter().take(12).map(|b| alloc::format!("{b:02x}")).collect();
    alloc::vec![
        String::from("EuroRepro — reproduceerbare builds (deterministische spec → getekende attestatie → consensus)"),
        alloc::format!("  voorbeeld build-spec-id: {hex}…"),
        alloc::format!("  toolchain: {} · flags genormaliseerd · env gesorteerd → stabiele id", spec.toolchain),
        alloc::format!("  deterministisch: {} (geen volatiele inputs)", spec.is_deterministic()),
        String::from("  ≥2 onafhankelijke bouwers met dezelfde output → reproduceerbaar bevestigd (verifieerbare soevereiniteit)"),
    ]
}
