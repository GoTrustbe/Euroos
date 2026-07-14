//! TPM 2.0 over MMIO at the fixed base `0xFED4_0000` — plan O1 / Metal M6-1,
//! hardware root of trust. Two transport interfaces, both at that base:
//!
//! - **TIS** (FIFO): the classic discrete-chip interface. Command goes into a
//!   FIFO with burst-count flow control; the chip signals data-available.
//! - **CRB** (Command Response Buffer): the interface that firmware TPMs —
//!   Intel PTT, AMD fTPM — present on modern machines. Command and response
//!   live in an MMIO buffer; a `start` bit runs the command.
//!
//! `init()` reads the interface-id register to pick the backend; `transact()`
//! dispatches to it. On top runs a boot self-test: `Startup`, `GetRandom` and
//! **PCR-extend** (measured boot; with K3 a disk key sealed to the boot state).

use alloc::vec::Vec;

const TIS_BASE: u64 = 0xFED4_0000; // locality 0 (both TIS and CRB)
const REG_ACCESS: u64 = 0x00;
const REG_STS: u64 = 0x18;
const REG_DATA_FIFO: u64 = 0x24;
const REG_INTF_ID: u64 = 0x30; // TPM_INTERFACE_ID: low nibble = interface type
const REG_DID_VID: u64 = 0xF00;

const ACCESS_ACTIVE_LOCALITY: u8 = 1 << 5;
const ACCESS_REQUEST_USE: u8 = 1 << 1;
const STS_VALID: u32 = 1 << 7;
const STS_COMMAND_READY: u32 = 1 << 6;
const STS_TPM_GO: u32 = 1 << 5;
const STS_DATA_AVAIL: u32 = 1 << 4;
const STS_EXPECT: u32 = 1 << 3;

// ── CRB interface registers (offsets from TIS_BASE) ───────────────────────────
const CRB_LOC_STATE: u64 = 0x00; // bit1 locAssigned, bits2-4 activeLocality, bit7 valid
const CRB_LOC_CTRL: u64 = 0x08; // bit0 requestAccess, bit1 relinquish
const CRB_LOC_STS: u64 = 0x0C; // bit0 Granted
const CRB_CTRL_REQ: u64 = 0x40; // bit0 cmdReady, bit1 goIdle
const CRB_CTRL_STS: u64 = 0x44; // bit0 tpmSts(error), bit1 tpmIdle
const CRB_CTRL_START: u64 = 0x4C; // bit0 start
const CRB_CTRL_CMD_SIZE: u64 = 0x58;
const CRB_CTRL_CMD_LADDR: u64 = 0x5C;
const CRB_CTRL_CMD_HADDR: u64 = 0x60;
const CRB_CTRL_RSP_SIZE: u64 = 0x64;
const CRB_CTRL_RSP_ADDR: u64 = 0x68; // 64-bit

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    None,
    Tis,
    Crb,
}

static mut PRESENT: bool = false;
static mut BACKEND: Backend = Backend::None;

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

#[inline]
unsafe fn r32(off: u64) -> u32 {
    ((TIS_BASE + off) as *const u32).read_volatile()
}
#[inline]
unsafe fn w32(off: u64, v: u32) {
    ((TIS_BASE + off) as *mut u32).write_volatile(v);
}

/// One complete CRB transaction (Metal M6-1): request locality, set the command
/// buffer, run `start`, read the response. None on a transport error/timeout.
unsafe fn crb_transact(cmd: &[u8]) -> Option<Vec<u8>> {
    // 1. Request locality 0.
    w32(CRB_LOC_CTRL, 1); // requestAccess
    let mut got = false;
    for _ in 0..100_000 {
        if r32(CRB_LOC_STS) & 1 != 0 || r32(CRB_LOC_STATE) & (1 << 1) != 0 {
            got = true;
            break;
        }
        spin();
    }
    if !got {
        return None;
    }
    // 2. Command-ready.
    w32(CRB_CTRL_REQ, 1); // cmdReady
    for _ in 0..100_000 {
        if r32(CRB_CTRL_REQ) & 1 == 0 {
            break;
        }
        spin();
    }
    // 3. Command + response buffer addresses (from the control registers).
    let cmd_addr = (r32(CRB_CTRL_CMD_LADDR) as u64) | ((r32(CRB_CTRL_CMD_HADDR) as u64) << 32);
    let cmd_size = r32(CRB_CTRL_CMD_SIZE) as usize;
    let rsp_addr = (r32(CRB_CTRL_RSP_ADDR) as u64) | ((r32(CRB_CTRL_RSP_ADDR + 4) as u64) << 32);
    let rsp_size = r32(CRB_CTRL_RSP_SIZE) as usize;
    if cmd_addr == 0 || rsp_addr == 0 || cmd.len() > cmd_size {
        w32(CRB_CTRL_REQ, 2); // goIdle
        w32(CRB_LOC_CTRL, 2); // relinquish
        return None;
    }
    // 4. Write the command into the MMIO command buffer (identity-mapped).
    for (i, &b) in cmd.iter().enumerate() {
        ((cmd_addr + i as u64) as *mut u8).write_volatile(b);
    }
    // 5. Run it: set start, wait for it to clear.
    w32(CRB_CTRL_START, 1);
    let mut done = false;
    for _ in 0..2_000_000 {
        if r32(CRB_CTRL_START) & 1 == 0 {
            done = true;
            break;
        }
        spin();
    }
    if !done || r32(CRB_CTRL_STS) & 1 != 0 {
        w32(CRB_CTRL_REQ, 2);
        w32(CRB_LOC_CTRL, 2);
        return None;
    }
    // 6. Read the response header (10 bytes) for its size, then the whole thing.
    let mut resp = Vec::new();
    for i in 0..6.min(rsp_size) {
        resp.push(((rsp_addr + i as u64) as *const u8).read_volatile());
    }
    let size = if resp.len() >= 6 {
        u32::from_be_bytes([resp[2], resp[3], resp[4], resp[5]]) as usize
    } else {
        0
    };
    let total = size.clamp(resp.len(), rsp_size);
    for i in resp.len()..total {
        resp.push(((rsp_addr + i as u64) as *const u8).read_volatile());
    }
    // 7. Back to idle + release the locality.
    w32(CRB_CTRL_REQ, 2); // goIdle
    w32(CRB_LOC_CTRL, 2); // relinquish
    if resp.len() >= 10 {
        Some(resp)
    } else {
        None
    }
}

/// Perform one complete TPM transaction: send `cmd`, read the response. None on
/// a transport error/timeout. Dispatches to the detected interface (TIS or CRB).
pub fn transact(cmd: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        if !PRESENT {
            return None;
        }
        if BACKEND == Backend::Crb {
            return crb_transact(cmd);
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

/// Detect the TPM + its interface + do Startup. Returns true if there is a
/// working TPM. Prefers CRB when the interface-id register reports it (firmware
/// TPMs), else falls back to the TIS FIFO (discrete chips / QEMU tpm-tis).
pub fn init() -> bool {
    unsafe {
        // TPM_INTERFACE_ID (0x30): low nibble = InterfaceType. 1 = CRB, 0 = FIFO
        // (TIS). QEMU's tpm-crb reports 1 here; tpm-tis reports 0/0xF.
        let intf = ((TIS_BASE + REG_INTF_ID) as *const u32).read_volatile();
        let intf_type = intf & 0xF;
        let is_crb = intf != 0xFFFF_FFFF && intf_type == 1;

        if is_crb {
            // A CRB TPM: probe the command-buffer registers to confirm it's real.
            let cmd_addr = (r32(CRB_CTRL_CMD_LADDR) as u64) | ((r32(CRB_CTRL_CMD_HADDR) as u64) << 32);
            if cmd_addr == 0 || cmd_addr == 0xFFFF_FFFF_FFFF_FFFF {
                crate::serial_println!("[tpm] CRB interface reported but no command buffer — no TPM");
                return false;
            }
            PRESENT = true;
            BACKEND = Backend::Crb;
            if let Some(r) = transact(&eurotpm::startup()) {
                if let Some(h) = eurotpm::parse_header(&r) {
                    crate::serial_println!(
                        "[tpm] TPM 2.0 CRB @ {:#x} (cmd-buf {:#x}), Startup rc={:#x}{} — firmware-TPM interface (fTPM/PTT)",
                        TIS_BASE, cmd_addr, h.rc,
                        if h.rc == 0 { " (freshly started)" } else { " (already started by firmware)" }
                    );
                }
            }
            return true;
        }

        // TIS FIFO path. DID_VID: 0xFFFFFFFF / 0 = no chip.
        let didvid = ((TIS_BASE + REG_DID_VID) as *const u32).read_volatile();
        if didvid == 0xFFFF_FFFF || didvid == 0 {
            crate::serial_println!("[tpm] no TPM @ {:#x} (intf-id={:#010x}, DID_VID={:#010x})", TIS_BASE, intf, didvid);
            return false;
        }
        PRESENT = true;
        BACKEND = Backend::Tis;
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
