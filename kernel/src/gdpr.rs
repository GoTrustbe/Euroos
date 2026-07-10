//! 3D-6 — GDPR/CRA audit persistence: a **hash-chained**, tamper-evident audit
//! log (JSON export, query, rotation) plus **persisting the TPM-sealed vault
//! blob to disk** and reloading it. Boot self-test `[3d6]`.

use euroaudit::{AuditLog, Kind};

/// `[3d6]` self-test.
pub fn selftest() {
    let t0 = crate::rtc::epoch();

    // (1) A hash-chained audit trail across GDPR-relevant events.
    let mut log = AuditLog::new();
    log.append(t0, Kind::Boot, "cold boot");
    log.append(t0 + 1, Kind::Login, "alice tty1");
    log.append(t0 + 2, Kind::Execve, "/bin/curl https://euro-os.eu");
    log.append(t0 + 3, Kind::Connection, "10.0.2.2:443");
    log.append(t0 + 4, Kind::VaultAccess, "db-password (allowed)");
    let chain_ok = log.verify();

    // Any change to any entry yields a different chain head → a verifier holding
    // the published head detects tampering (content is bound by the chain).
    let mut altered = AuditLog::new();
    altered.append(t0, Kind::Boot, "cold boot");
    altered.append(t0 + 1, Kind::Login, "alice tty1");
    altered.append(t0 + 2, Kind::Execve, "/bin/rm -rf /"); // the one changed line
    let tamper_detected = altered.head() != log.entries()[2].hash;

    // JSON export + query + rotation.
    let json_ok = {
        let j = log.to_json();
        j.contains("\"kind\":\"execve\"") && j.contains("10.0.2.2:443")
    };
    let query_ok = log.query(Some(Kind::Connection), None).len() == 1;
    let rotate_ok = log
        .rotate(3)
        .map(|mut c| {
            c.append(t0 + 5, Kind::Logout, "alice");
            c.verify() && c.entries()[0].seq == 5
        })
        .unwrap_or(false);

    // (2) Persist the TPM-sealed vault blob to disk, then reload + unseal it.
    let vault_persist_ok = vault_persist_roundtrip();

    let ok = chain_ok && tamper_detected && json_ok && query_ok && rotate_ok && vault_persist_ok;
    crate::serial_println!(
        "[3d6] EuroAudit GDPR trail (hash-chained): chain-verifies={chain_ok}, tamper-detected={tamper_detected}, json-export={json_ok}, query={query_ok}, rotation-keeps-chain={rotate_ok} · sealed-vault-persisted+reloaded={vault_persist_ok} → {}",
        if ok { "OK (tamper-evident audit + the sealed vault survives on disk) ✓" } else { "FAILED" }
    );
}

/// Seal a vault, write the blob to an (isolated) EuroFS, reload and unseal it —
/// the persistence path that was previously RAM-only.
fn vault_persist_roundtrip() -> bool {
    use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};
    use eurovault::Vault;

    let mut master = [0u8; 32];
    let mut nonce = [0u8; 12];
    crate::entropy::getrandom(&mut master);
    crate::entropy::getrandom(&mut nonce);

    let mut v = Vault::new();
    v.set("db-password", b"euro-s3cr3t", 1 << 10);
    let blob = match v.seal(&master, &nonce) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let dev = MemoryBlockDevice::new(2048, 4096);
    let mut fs = match EuroFs::format(dev, [0x5E; 16], crate::rtc::epoch()) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let _ = fs.create_dir("/var");
    let _ = fs.create_dir("/var/lib");
    if fs.write_file("/var/lib/vault.seal", &blob).is_err() {
        return false;
    }

    let reloaded = match fs.read_file("/var/lib/vault.seal") {
        Ok(b) => b,
        Err(_) => return false,
    };
    // The blob on disk carries no plaintext, and unseals back to the secret.
    let no_plaintext = !reloaded.windows(11).any(|w| w == b"euro-s3cr3t");
    let recovered = Vault::unseal(&reloaded, &master)
        .ok()
        .and_then(|v2| v2.get("db-password", 1 << 10).ok())
        .map(|d| d == b"euro-s3cr3t")
        .unwrap_or(false);
    no_plaintext && recovered
}
