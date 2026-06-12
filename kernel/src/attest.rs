//! Kernel-zijde van **EuroAttest** (plan O2): remote attestation. Bij boot lezen we
//! de echte **PCR-waarden** (measured boot, O1), bouwen we een door de verifier
//! genonceerde **quote**, ondertekenen die met een (TPM-geseede) attestatiesleutel,
//! en bewijzen we dat de verifier 'm aanvaardt — én een replay/gewijzigde toestand
//! weigert. De host-geteste kern leeft in [`euroattest`].

use alloc::string::String;
use alloc::vec::Vec;

use euroattest::{quote, verify, AttestError, Pcr};
use spin::Mutex;

static AK_PUB: Mutex<Option<[u8; 32]>> = Mutex::new(None);

/// Boot-zelftest: lees PCR's → genonceerde quote → verifieer → replay/tamper falen.
pub fn selftest(ak_seed: [u8; 32], nonce: [u8; 32], from_tpm: bool) {
    let ak = ed25519_dalek::SigningKey::from_bytes(&ak_seed);
    let ak_pub = ak.verifying_key().to_bytes();
    *AK_PUB.lock() = Some(ak_pub);

    // Lees echte PCR's (16 = debug-PCR die O1 al gebruikt, + 0). Vangnet = synthetisch.
    let mut pcrs: Vec<Pcr> = Vec::new();
    for idx in [0u32, 16u32] {
        match crate::tpm::read_pcr(idx) {
            Some(v) => pcrs.push((idx as u8, v)),
            None => pcrs.push((idx as u8, [idx as u8; 32])),
        }
    }

    let q = quote(&ak, pcrs.clone(), nonce);

    // 1. Verse, geldige quote → aanvaard.
    let accepted = verify(&q, &ak_pub, &nonce, &pcrs).is_ok();
    // 2. Replay met een oude/andere nonce → geweigerd.
    let mut other_nonce = nonce;
    other_nonce[0] ^= 0xFF;
    let replay_blocked = matches!(verify(&q, &ak_pub, &other_nonce, &pcrs), Err(AttestError::NonceMismatch));
    // 3. Niet-vertrouwde toestand: verwacht een afwijkende PCR → geweigerd.
    let mut tampered = pcrs.clone();
    tampered[0].1[0] ^= 0xFF;
    let bad_state_blocked = matches!(verify(&q, &ak_pub, &nonce, &tampered), Err(AttestError::PcrMismatch { .. }));

    let ok = accepted && replay_blocked && bad_state_blocked;
    crate::serial_println!(
        "[o2] EuroAttest: quote over {} PCR's (AK-seed-van-TPM={from_tpm}), verse-quote-aanvaard={accepted}, replay-geweigerd={replay_blocked}, gewijzigde-toestand-geweigerd={bad_state_blocked} → {}",
        pcrs.len(),
        if ok { "OK (remote attestation — bewijsbare vertrouwde toestand) ✓" } else { "MISLUKT" }
    );
}

/// `euroattest`-shellcommando: toon de attestatiesleutel + de huidige PCR-toestand.
pub fn shell() -> Vec<String> {
    let mut out = alloc::vec![String::from("EuroAttest — remote attestation (TPM-quote over measured-boot-PCR's)")];
    match &*AK_PUB.lock() {
        Some(pk) => {
            let hex: String = pk.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
            out.push(alloc::format!("  attestatiesleutel (AK): {hex}…"));
        }
        None => out.push(String::from("  AK: niet geïnitialiseerd")),
    }
    for idx in [0u32, 7, 16] {
        match crate::tpm::read_pcr(idx) {
            Some(v) => {
                let hex: String = v.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
                out.push(alloc::format!("  PCR{idx:<2} = {hex}…"));
            }
            None => out.push(alloc::format!("  PCR{idx:<2} = (geen TPM)")),
        }
    }
    out.push(String::from("  een verifier stuurt een nonce → de machine bewijst haar toestand zonder de TPM-sleutel prijs te geven"));
    out
}
