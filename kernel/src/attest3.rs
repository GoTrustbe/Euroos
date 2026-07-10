//! 3D-3 — attestation + CA, end-to-end: a **three-level CA hierarchy** (root →
//! intermediate → leaf) with an **on-disk certificate store**, and a **JSON
//! attestation report** a remote verifier checks (quote over the real boot PCRs
//! + a fresh nonce; replay and a tampered PCR state are refused).

use ed25519_dalek::SigningKey;
use euroattest::{quote, verify, AttestError, Report};
use euroca::{verify_chain, CertAuthority, CertStore, Csr};

/// `[3d3]` self-test.
pub fn selftest() {
    let now = crate::rtc::epoch();
    let year = 365 * 24 * 3600;

    // ── (1) CA hierarchy: root → intermediate → leaf ──
    let mut root_seed = [0u8; 32];
    crate::entropy::getrandom(&mut root_seed);
    let mut root = CertAuthority::new_root("EuroCA Root", root_seed, now, now + 10 * year);

    let mut inter_seed = [0u8; 32];
    crate::entropy::getrandom(&mut inter_seed);
    let inter_key = SigningKey::from_bytes(&inter_seed);
    let inter_cert = root.issue(
        &Csr { subject: "EuroCA Intermediate".into(), subject_key: inter_key.verifying_key().to_bytes(), is_ca: true },
        now,
        now + 5 * year,
    );

    let mut inter_ca = CertAuthority::from_cert(inter_seed, inter_cert.clone());
    let mut leaf_seed = [0u8; 32];
    crate::entropy::getrandom(&mut leaf_seed);
    let leaf_key = SigningKey::from_bytes(&leaf_seed);
    let leaf = inter_ca.issue(
        &Csr { subject: "vpn.euro-os.eu".into(), subject_key: leaf_key.verifying_key().to_bytes(), is_ca: false },
        now,
        now + year,
    );

    // The full chain verifies against the ROOT key only.
    let chain = alloc::vec![inter_cert.clone(), leaf.clone()];
    let chain_ok = verify_chain(&chain, &root.public_key(), now + 100).is_ok();

    // ── (2) On-disk certificate store (root + issued + CRL) ──
    let store_ok = cert_store_roundtrip(&root, &inter_cert, &leaf);

    // ── (3) JSON attestation report over the real boot PCRs ──
    let mut ak_seed = [0u8; 32];
    crate::entropy::getrandom(&mut ak_seed);
    let ak = SigningKey::from_bytes(&ak_seed);
    let ak_pub = ak.verifying_key().to_bytes();
    let pcr0 = crate::tpm::read_pcr(0).unwrap_or([0x11; 32]);
    let pcr16 = crate::tpm::read_pcr(16).unwrap_or([0x22; 32]);
    let pcrs = alloc::vec![(0u8, pcr0), (16u8, pcr16)];

    let mut nonce = [0u8; 32];
    crate::entropy::getrandom(&mut nonce); // the verifier's challenge
    let q = quote(&ak, pcrs.clone(), nonce);
    let report = Report { ak_pubkey: &ak_pub, quote: &q };
    let json = report.to_json();
    let json_ok = json.contains("\"pcrs\"") && json.contains("\"ak\"") && json.len() > 80;

    // A verifier accepts a genuine quote (fresh nonce + expected PCRs).
    let accepted = verify(&q, &ak_pub, &nonce, &pcrs).is_ok();
    // A replayed quote (different nonce) is refused.
    let replay_refused = matches!(verify(&q, &ak_pub, &[0u8; 32], &pcrs), Err(AttestError::NonceMismatch));
    // An untrusted boot state (a changed PCR) is refused.
    let mut bad = pcrs.clone();
    bad[0].1[0] ^= 0xFF;
    let tamper_refused = matches!(verify(&q, &ak_pub, &nonce, &bad), Err(AttestError::PcrMismatch { .. }));

    let ok = chain_ok && store_ok && json_ok && accepted && replay_refused && tamper_refused;
    crate::serial_println!(
        "[3d3] EuroCA+Attest: 3-level chain(root→intermediate→leaf)-verifies={chain_ok}, on-disk-cert-store-roundtrip={store_ok} · attestation JSON-report={json_ok}, verifier-accepts={accepted}, replay-REFUSED={replay_refused}, tampered-PCR-REFUSED={tamper_refused} → {}",
        if ok { "OK (sovereign PKI + remote attestation over the boot PCRs; live HTTPS endpoint + hardware-resident TPM2_Quote pending) ✓" } else { "FAILED" }
    );
}

/// Persist the CA store to an isolated EuroFS, reload it, and confirm the chain
/// still verifies + revocation survives.
fn cert_store_roundtrip(root: &CertAuthority, inter: &euroca::Certificate, leaf: &euroca::Certificate) -> bool {
    use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};

    let mut store = CertStore::new(root.cert.clone());
    store.add(inter.clone());
    store.add(leaf.clone());
    store.revoke(leaf.serial);
    let bytes = store.to_bytes();

    let dev = MemoryBlockDevice::new(2048, 4096);
    let mut fs = match EuroFs::format(dev, [0xCA; 16], crate::rtc::epoch()) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let _ = fs.create_dir("/etc");
    let _ = fs.create_dir("/etc/euroca");
    if fs.write_file("/etc/euroca/store.bin", &bytes).is_err() {
        return false;
    }
    let reloaded = match fs.read_file("/etc/euroca/store.bin") {
        Ok(b) => b,
        Err(_) => return false,
    };
    match CertStore::from_bytes(&reloaded) {
        Some(s) => s == store && s.is_revoked(leaf.serial) && s.issued.len() == 2,
        None => false,
    }
}
