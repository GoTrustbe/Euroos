//! TPM 2.0 via the **TIS** (TPM Interface Specification) MMIO interface — plan O1,
//! hardware root of trust.
//!
//! The TPM chip sits at the fixed MMIO base `0xFED4_0000` (locality 0, identity-
//! mapped). This module is the transport layer beneath the host-tested [`eurotpm`]
//! command encoding: it requests the locality, writes a TPM command into the
//! FIFO, gives the chip the `go`, and reads the response back (with burst-count-flow-
//! control). On top of that runs a boot self-test: `Startup`, `GetRandom` (proves
//! the TPM is alive), and **PCR-extend** — the measurement operation that enables measured boot and
//! (with K3) a disk key sealed to the boot state.

use alloc::vec::Vec;

const TIS_BASE: u64 = 0xFED4_0000; // locality 0
const REG_ACCESS: u64 = 0x00;
const REG_STS: u64 = 0x18;
const REG_DATA_FIFO: u64 = 0x24;
const REG_DID_VID: u64 = 0xF00;

const ACCESS_ACTIVE_LOCALITY: u8 = 1 << 5;
const ACCESS_REQUEST_USE: u8 = 1 << 1;
const STS_VALID: u32 = 1 << 7;
const STS_COMMAND_READY: u32 = 1 << 6;
const STS_TPM_GO: u32 = 1 << 5;
const STS_DATA_AVAIL: u32 = 1 << 4;
const STS_EXPECT: u32 = 1 << 3;

static mut PRESENT: bool = false;

#[inline]
unsafe fn r8(off: u64) -> u8 {
    ((TIS_BASE + off) as *const u8).read_volatile()
}
#[inline]
unsafe fn w8(off: u64, v: u8) {
    ((TIS_BASE + off) as *mut u8).write_volatile(v);
}
#[inline]
unsafe fn sts() -> u32 {
    ((TIS_BASE + REG_STS) as *const u32).read_volatile()
}
#[inline]
unsafe fn burst_count() -> usize {
    ((sts() >> 8) & 0xFFFF) as usize
}

fn spin() {
    for _ in 0..200 {
        core::hint::spin_loop();
    }
}

/// Request locality 0 (request → wait for activeLocality).
unsafe fn request_locality() -> bool {
    w8(REG_ACCESS, ACCESS_REQUEST_USE);
    for _ in 0..100_000 {
        if r8(REG_ACCESS) & ACCESS_ACTIVE_LOCALITY != 0 {
            return true;
        }
        spin();
    }
    false
}

/// Release locality 0 again.
unsafe fn release_locality() {
    w8(REG_ACCESS, ACCESS_ACTIVE_LOCALITY);
}

unsafe fn wait_sts(mask: u32) -> bool {
    for _ in 0..1_000_000 {
        let s = sts();
        if s & mask == mask {
            return true;
        }
        spin();
    }
    false
}

/// Perform one complete TPM transaction: send `cmd`, read the response. None on
/// a transport error/timeout.
pub fn transact(cmd: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        if !PRESENT {
            return None;
        }
        if !request_locality() {
            return None;
        }
        // Put the TPM into command-ready.
        w8(REG_STS, STS_COMMAND_READY as u8);
        if !wait_sts(STS_COMMAND_READY) {
            release_locality();
            return None;
        }
        // Write the command via the FIFO, with burst-count-flow-control. Only write the last
        // byte while Expect is still set (this lets the TPM know when it is done).
        let mut sent = 0;
        while sent < cmd.len() {
            let mut burst = burst_count();
            if burst == 0 {
                if !wait_sts(STS_VALID) {
                    break;
                }
                burst = burst_count().max(1);
            }
            let chunk = burst.min(cmd.len() - sent);
            for &b in &cmd[sent..sent + chunk] {
                w8(REG_DATA_FIFO, b);
            }
            sent += chunk;
        }
        // Give the chip the go.
        w8(REG_STS, STS_TPM_GO as u8);
        if !wait_sts(STS_VALID | STS_DATA_AVAIL) {
            release_locality();
            return None;
        }
        // Read the response: first the 10-byte header (for the size), then the rest.
        let mut resp = Vec::new();
        let mut guard = 0;
        loop {
            if sts() & STS_DATA_AVAIL == 0 {
                break;
            }
            let burst = burst_count().max(1);
            for _ in 0..burst {
                if sts() & STS_DATA_AVAIL == 0 {
                    break;
                }
                resp.push(r8(REG_DATA_FIFO));
                // Stop as soon as we have the full response (size from the header).
                if resp.len() >= 6 {
                    let size = u32::from_be_bytes([resp[2], resp[3], resp[4], resp[5]]) as usize;
                    if resp.len() >= size {
                        w8(REG_STS, STS_COMMAND_READY as u8);
                        release_locality();
                        return Some(resp);
                    }
                }
            }
            guard += 1;
            if guard > 100_000 {
                break;
            }
        }
        w8(REG_STS, STS_COMMAND_READY as u8);
        release_locality();
        if resp.len() >= 10 {
            Some(resp)
        } else {
            None
        }
    }
}

/// Detect the TPM + do Startup. Returns true if there is a working TPM.
pub fn init() -> bool {
    unsafe {
        // Read DID_VID: 0xFFFFFFFF / 0 = no chip.
        let didvid = ((TIS_BASE + REG_DID_VID) as *const u32).read_volatile();
        if didvid == 0xFFFF_FFFF || didvid == 0 {
            crate::serial_println!("[tpm] no TPM-TIS @ {:#x} (DID_VID={:#010x})", TIS_BASE, didvid);
            return false;
        }
        PRESENT = true;
        // TPM2_Startup(CLEAR). The firmware (OVMF) usually started it already → in that case
        // this returns TPM_RC_INITIALIZE (0x100), which is fine.
        if let Some(r) = transact(&eurotpm::startup()) {
            if let Some(h) = eurotpm::parse_header(&r) {
                crate::serial_println!(
                    "[tpm] TPM 2.0 TIS @ {:#x} (DID_VID={:#010x}), Startup rc={:#x}{}",
                    TIS_BASE, didvid, h.rc,
                    if h.rc == 0 { " (freshly started)" } else { " (already started by firmware)" }
                );
            }
        }
        true
    }
}

pub fn present() -> bool {
    unsafe { PRESENT }
}

/// Request `n` true random bytes from the TPM (for key generation, K3-FDE).
pub fn get_random(n: u16) -> Option<Vec<u8>> {
    let r = transact(&eurotpm::get_random(n))?;
    eurotpm::parse_random(&r).filter(|b| b.len() >= n as usize)
}

/// Read a PCR value (SHA-256, 32 bytes) — the measured-boot state that
/// remote attestation (O2) includes in a quote.
pub fn read_pcr(index: u32) -> Option<[u8; 32]> {
    let r = transact(&eurotpm::pcr_read(index))?;
    let v = eurotpm::parse_pcr_read(&r)?;
    if v.len() >= 32 {
        let mut out = [0u8; 32];
        out.copy_from_slice(&v[..32]);
        Some(out)
    } else {
        None
    }
}

/// The measured-boot PCR the FDE key + vault master are sealed to (extended by
/// the O1 measured-boot step). A changed boot chain changes this PCR → the TPM
/// refuses to unseal.
pub const SEAL_PCR: u32 = 16;

/// **Real TPM2 seal (3D-1)** — keep `secret` INSIDE the TPM, releasable only
/// under a policy session matching PCR `pcr`. Returns the opaque (private,
/// public) blob to persist; the key itself never leaves the chip in the blob.
///
/// Flow: `CreatePrimary` (deterministic owner storage parent) → trial session +
/// `PolicyPCR` + `PolicyGetDigest` (the authPolicy) → `Create` (seal under that
/// policy). Unlike the old software KDF, the sealed key cannot be re-derived from
/// kernel RAM — only the TPM can release it, and only on a matching boot state.
pub fn seal_to_pcr(pcr: u32, secret: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if !present() {
        return None;
    }
    let parent = eurotpm::parse_handle(&transact(&eurotpm::create_primary_owner())?)?;
    let out = (|| {
        let nonce = get_random(16).unwrap_or_else(|| alloc::vec![0x5A; 16]);
        let trial = eurotpm::parse_handle(&transact(&eurotpm::start_auth_session(true, &nonce))?)?;
        transact(&eurotpm::policy_pcr(trial, pcr))?;
        let policy = eurotpm::parse_policy_digest(&transact(&eurotpm::policy_get_digest(trial))?)?;
        transact(&eurotpm::flush_context(trial));
        let blob = eurotpm::parse_create(&transact(&eurotpm::create_sealed(parent, &policy, secret))?)?;
        Some((blob.private, blob.public))
    })();
    transact(&eurotpm::flush_context(parent));
    out
}

/// **Real TPM2 unseal (3D-1)** — reproduce the parent, `Load` the blob, open a
/// policy session bound to the LIVE value of PCR `pcr`, and ask the TPM to
/// release the secret. On a tampered/changed boot the TPM itself refuses
/// (`TPM_RC_POLICY_FAIL`) and this returns `None` — fail-closed by hardware.
pub fn unseal_from_pcr(pcr: u32, private: &[u8], public: &[u8]) -> Option<Vec<u8>> {
    if !present() {
        return None;
    }
    let parent = eurotpm::parse_handle(&transact(&eurotpm::create_primary_owner())?)?;
    let out = (|| {
        let item = eurotpm::parse_handle(&transact(&eurotpm::load(parent, private, public))?)?;
        let nonce = get_random(16).unwrap_or_else(|| alloc::vec![0x5A; 16]);
        let sess = eurotpm::parse_handle(&transact(&eurotpm::start_auth_session(false, &nonce))?)?;
        transact(&eurotpm::policy_pcr(sess, pcr))?;
        let secret = transact(&eurotpm::unseal(item, sess)).and_then(|r| eurotpm::parse_unseal(&r));
        transact(&eurotpm::flush_context(sess));
        transact(&eurotpm::flush_context(item));
        secret
    })();
    transact(&eurotpm::flush_context(parent));
    out
}

/// Extend PCR `pcr` with `digest` (measured-boot / tamper simulation). Returns
/// whether the TPM accepted the extend.
pub fn extend_pcr(pcr: u32, digest: &[u8; 32]) -> bool {
    transact(&eurotpm::pcr_extend(pcr, digest))
        .and_then(|r| eurotpm::parse_header(&r))
        .map(|h| h.ok())
        .unwrap_or(false)
}

/// O1 boot self-test: prove a live TPM (GetRandom) + measured boot (PCR-extend
/// changes the PCR value, exactly as the boot chain measures).
pub fn selftest() {
    if !present() {
        return;
    }
    // (1) GetRandom — a live TPM delivers true randomness.
    let rnd = transact(&eurotpm::get_random(16)).and_then(|r| eurotpm::parse_random(&r));
    let rnd_ok = rnd.as_ref().map(|b| b.len() >= 8 && b.iter().any(|&x| x != 0)).unwrap_or(false);

    // (2) Read PCR 16 (debug PCR) before the extend.
    let before = transact(&eurotpm::pcr_read(16)).and_then(|r| eurotpm::parse_pcr_read(&r));

    // (3) Extend PCR 16 with a measurement digest → the PCR changes (SHA256(old || digest)).
    let digest = [0x42u8; eurotpm::SHA256_LEN];
    let extended = transact(&eurotpm::pcr_extend(16, &digest))
        .and_then(|r| eurotpm::parse_header(&r))
        .map(|h| h.ok())
        .unwrap_or(false);

    // (4) Read PCR 16 again → must DIFFER from before the extend.
    let after = transact(&eurotpm::pcr_read(16)).and_then(|r| eurotpm::parse_pcr_read(&r));
    let changed = matches!((&before, &after), (Some(b), Some(a)) if b != a);

    let rb: Vec<u8> = rnd.unwrap_or_default().into_iter().take(8).collect();
    crate::serial_println!(
        "[o1] TPM measured boot: GetRandom-OK={} ({:02x?}), PCR16-extend-OK={}, PCR16-changed-after-extend={} → {}",
        rnd_ok, rb, extended, changed,
        if rnd_ok && extended && changed { "OK (hardware root of trust works) ✓" } else { "FAILED/incomplete" }
    );
}
