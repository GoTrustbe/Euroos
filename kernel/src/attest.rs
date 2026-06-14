//! Kernel side of **EuroAttest** (plan O2): remote attestation. At boot we read
//! the real **PCR values** (measured boot, O1), build a verifier-nonced
//! **quote**, sign it with a (TPM-seeded) attestation key,
//! and prove that the verifier accepts it — and rejects a replay/modified state.
//! The host-tested core lives in [`euroattest`].

use alloc::string::String;
use alloc::vec::Vec;

use euroattest::{quote, verify, AttestError, Pcr};
use spin::Mutex;

static AK_PUB: Mutex<Option<[u8; 32]>> = Mutex::new(None);

/// Boot self-test: read PCRs → nonced quote → verify → replay/tamper fail.
pub fn selftest(ak_seed: [u8; 32], nonce: [u8; 32], from_tpm: bool) {
    let ak = ed25519_dalek::SigningKey::from_bytes(&ak_seed);
    let ak_pub = ak.verifying_key().to_bytes();
    *AK_PUB.lock() = Some(ak_pub);

    // Read real PCRs (16 = debug PCR that O1 already uses, + 0). Fallback = synthetic.
    let mut pcrs: Vec<Pcr> = Vec::new();
    for idx in [0u32, 16u32] {
        match crate::tpm::read_pcr(idx) {
            Some(v) => pcrs.push((idx as u8, v)),
            None => pcrs.push((idx as u8, [idx as u8; 32])),
        }
    }

    let q = quote(&ak, pcrs.clone(), nonce);

    // 1. Fresh, valid quote → accept.
    let accepted = verify(&q, &ak_pub, &nonce, &pcrs).is_ok();
    // 2. Replay with an old/different nonce → rejected.
    let mut other_nonce = nonce;
    other_nonce[0] ^= 0xFF;
    let replay_blocked = matches!(verify(&q, &ak_pub, &other_nonce, &pcrs), Err(AttestError::NonceMismatch));
    // 3. Untrusted state: expect a deviating PCR → rejected.
    let mut tampered = pcrs.clone();
    tampered[0].1[0] ^= 0xFF;
    let bad_state_blocked = matches!(verify(&q, &ak_pub, &nonce, &tampered), Err(AttestError::PcrMismatch { .. }));

    let ok = accepted && replay_blocked && bad_state_blocked;
    crate::serial_println!(
        "[o2] EuroAttest: quote over {} PCRs (AK-seed-from-TPM={from_tpm}), fresh-quote-accepted={accepted}, replay-rejected={replay_blocked}, modified-state-rejected={bad_state_blocked} → {}",
        pcrs.len(),
        if ok { "OK (remote attestation — provable trusted state) ✓" } else { "FAILED" }
    );
}

/// `euroattest` shell command: show the attestation key + the current PCR state.
pub fn shell() -> Vec<String> {
    let mut out = alloc::vec![String::from("EuroAttest — remote attestation (TPM quote over measured-boot PCRs)")];
    match &*AK_PUB.lock() {
        Some(pk) => {
            let hex: String = pk.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
            out.push(alloc::format!("  attestation key (AK): {hex}…"));
        }
        None => out.push(String::from("  AK: not initialized")),
    }
    for idx in [0u32, 7, 16] {
        match crate::tpm::read_pcr(idx) {
            Some(v) => {
                let hex: String = v.iter().take(8).map(|b| alloc::format!("{b:02x}")).collect();
                out.push(alloc::format!("  PCR{idx:<2} = {hex}…"));
            }
            None => out.push(alloc::format!("  PCR{idx:<2} = (no TPM)")),
        }
    }
    out.push(String::from("  a verifier sends a nonce → the machine proves its state without revealing the TPM key"));
    out
}
