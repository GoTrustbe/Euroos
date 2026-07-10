//! EuroTPM — **TPM 2.0 command encoding + response parsing** (plan O1).
//!
//! A TPM (Trusted Platform Module) is the hardware trust anchor: a separate
//! chip that keeps measurement values (PCRs), can **seal** secrets to a
//! system state, and provides randomness. EuroOS uses it for measured boot and
//! (with K3) for a disk encryption key sealed to the boot state.
//!
//! The TPM speaks a binary command/response protocol (TPM 2.0, big-endian). This
//! crate is the architecture-independent **encode/parse layer**: it builds valid
//! command bytes (`Startup`, `GetRandom`, `PCR_Read`, `PCR_Extend`) and parses the
//! responses. The TIS-MMIO transport layer (actually talking to the chip) lives in the kernel
//! (`kernel/src/tpm.rs`). Pure `no_std` logic → the byte-exact encoding is fully
//! tested on the host.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

// ── TPM 2.0 constants ─────────────────────────────────────────────────────
const TPM_ST_NO_SESSIONS: u16 = 0x8001;
const TPM_ST_SESSIONS: u16 = 0x8002;
const TPM_SU_CLEAR: u16 = 0x0000;
const TPM_RS_PW: u32 = 0x4000_0009; // password (auth) session handle
pub const TPM_ALG_SHA256: u16 = 0x000B;
pub const SHA256_LEN: usize = 32;

// Permanent handles.
pub const TPM_RH_OWNER: u32 = 0x4000_0001; // the storage (owner) hierarchy
pub const TPM_RH_NULL: u32 = 0x4000_0007; // the null hierarchy (unsalted sessions)

// Algorithms / curves / session types (for real seal/unseal, 3D-1).
const TPM_ALG_NULL: u16 = 0x0010;
const TPM_ALG_ECC: u16 = 0x0023;
const TPM_ALG_KEYEDHASH: u16 = 0x0008;
const TPM_ALG_AES: u16 = 0x0006;
const TPM_ALG_CFB: u16 = 0x0043;
const TPM_ECC_NIST_P256: u16 = 0x0003;
const TPM_SE_POLICY: u8 = 0x01;
const TPM_SE_TRIAL: u8 = 0x03;

// Command codes.
const CC_STARTUP: u32 = 0x0000_0144;
const CC_GET_RANDOM: u32 = 0x0000_017B;
const CC_PCR_READ: u32 = 0x0000_017E;
const CC_PCR_EXTEND: u32 = 0x0000_0182;
// 3D-1: real seal/unseal command set.
const CC_CREATE_PRIMARY: u32 = 0x0000_0131;
const CC_CREATE: u32 = 0x0000_0153;
const CC_LOAD: u32 = 0x0000_0157;
const CC_UNSEAL: u32 = 0x0000_015E;
const CC_FLUSH_CONTEXT: u32 = 0x0000_0165;
const CC_START_AUTH_SESSION: u32 = 0x0000_0176;
const CC_POLICY_PCR: u32 = 0x0000_017F;
const CC_POLICY_GET_DIGEST: u32 = 0x0000_0189;
/// TPM_RC_POLICY_FAIL (RC_FMT1 | 0x1D) — the response code a real TPM returns
/// when an `Unseal` is attempted under a policy session whose PCR state does not
/// match the sealed object's policy (i.e. a tampered/changed boot). Hardware-enforced.
pub const TPM_RC_POLICY_FAIL: u32 = 0x0000_099D;

/// Build a command header + body. `tag`/`cc` + the payload → complete bytes with
/// the correct `commandSize` field filled in.
fn command(tag: u16, cc: u32, body: &[u8]) -> Vec<u8> {
    let size = 10 + body.len() as u32; // header (2+4+4) + body
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&cc.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// TPM2_Startup(CLEAR) — mandatory after a TPM reset before any other command.
pub fn startup() -> Vec<u8> {
    command(TPM_ST_NO_SESSIONS, CC_STARTUP, &TPM_SU_CLEAR.to_be_bytes())
}

/// TPM2_GetRandom — request `bytes` random bytes (proves the TPM is alive).
pub fn get_random(bytes: u16) -> Vec<u8> {
    command(TPM_ST_NO_SESSIONS, CC_GET_RANDOM, &bytes.to_be_bytes())
}

/// A PCR selection (SHA-256 bank, one PCR index) → a TPML_PCR_SELECTION block.
fn pcr_selection(pcr: u32) -> Vec<u8> {
    let mut sel = [0u8; 3];
    if pcr < 24 {
        sel[(pcr / 8) as usize] |= 1 << (pcr % 8);
    }
    let mut b = Vec::new();
    b.extend_from_slice(&1u32.to_be_bytes()); // count = 1 bank
    b.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes()); // hashAlg
    b.push(3); // sizeofSelect
    b.extend_from_slice(&sel);
    b
}

/// TPM2_PCR_Read of one PCR (SHA-256 bank).
pub fn pcr_read(pcr: u32) -> Vec<u8> {
    command(TPM_ST_NO_SESSIONS, CC_PCR_READ, &pcr_selection(pcr))
}

/// TPM2_PCR_Extend: extend PCR `pcr` with a SHA-256 `digest` (measured boot).
/// Uses an empty password auth session (TPM_RS_PW).
pub fn pcr_extend(pcr: u32, digest: &[u8; SHA256_LEN]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&pcr.to_be_bytes()); // pcrHandle
    // authArea: size + (TPM_RS_PW + nonce(0) + attrs(0) + hmac(0)) = 9 bytes.
    body.extend_from_slice(&9u32.to_be_bytes());
    body.extend_from_slice(&TPM_RS_PW.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // nonceSize
    body.push(0); // sessionAttributes
    body.extend_from_slice(&0u16.to_be_bytes()); // hmacSize
    // digests: TPML_DIGEST_VALUES = count + [hashAlg + digest].
    body.extend_from_slice(&1u32.to_be_bytes()); // count
    body.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes());
    body.extend_from_slice(digest);
    command(TPM_ST_SESSIONS, CC_PCR_EXTEND, &body)
}

/// The parsed response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RespHeader {
    pub tag: u16,
    pub size: u32,
    pub rc: u32, // responseCode (0 = TPM_RC_SUCCESS)
}

impl RespHeader {
    pub fn ok(&self) -> bool {
        self.rc == 0
    }
}

/// Parse the 10-byte response header.
pub fn parse_header(resp: &[u8]) -> Option<RespHeader> {
    if resp.len() < 10 {
        return None;
    }
    Some(RespHeader {
        tag: u16::from_be_bytes([resp[0], resp[1]]),
        size: u32::from_be_bytes([resp[2], resp[3], resp[4], resp[5]]),
        rc: u32::from_be_bytes([resp[6], resp[7], resp[8], resp[9]]),
    })
}

/// Parse the GetRandom response → the random bytes (after a `TPM2B_DIGEST` size).
pub fn parse_random(resp: &[u8]) -> Option<Vec<u8>> {
    let h = parse_header(resp)?;
    if !h.ok() || resp.len() < 12 {
        return None;
    }
    let n = u16::from_be_bytes([resp[10], resp[11]]) as usize;
    if resp.len() < 12 + n {
        return None;
    }
    Some(resp[12..12 + n].to_vec())
}

/// Parse the PCR_Read response → the first PCR digest (SHA-256, 32 bytes).
/// Layout: header + pcrUpdateCounter(4) + pcrSelectionOut(TPML) + pcrValues(TPML_DIGEST).
pub fn parse_pcr_read(resp: &[u8]) -> Option<Vec<u8>> {
    let h = parse_header(resp)?;
    if !h.ok() {
        return None;
    }
    let mut p = 10 + 4; // after header + pcrUpdateCounter
    // pcrSelectionOut: count(4) + count × (hashAlg(2) + sizeofSelect(1) + select[n]).
    if p + 4 > resp.len() {
        return None;
    }
    let sel_count = u32::from_be_bytes([resp[p], resp[p + 1], resp[p + 2], resp[p + 3]]) as usize;
    p += 4;
    for _ in 0..sel_count {
        if p + 3 > resp.len() {
            return None;
        }
        let sz = resp[p + 2] as usize;
        p += 3 + sz;
    }
    // pcrValues: count(4) + count × (size(2) + digest[size]).
    if p + 4 > resp.len() {
        return None;
    }
    let dcount = u32::from_be_bytes([resp[p], resp[p + 1], resp[p + 2], resp[p + 3]]) as usize;
    p += 4;
    if dcount == 0 || p + 2 > resp.len() {
        return None;
    }
    let dsz = u16::from_be_bytes([resp[p], resp[p + 1]]) as usize;
    p += 2;
    if p + dsz > resp.len() {
        return None;
    }
    Some(resp[p..p + dsz].to_vec())
}

// ── Real TPM2 seal/unseal to the boot-PCR state (3D-1) ────────────────────
//
// The sealing flow keeps the secret INSIDE the TPM: a storage primary (owner
// hierarchy, deterministic per TPM seed) is the parent; the FDE key / vault
// master is sealed as a `keyedhash` object whose `authPolicy` is the digest of
// a `PolicyPCR` over the measured-boot PCR. Unsealing requires a policy session
// whose live PCR state reproduces that digest — on a tampered boot the TPM
// itself refuses (`TPM_RC_POLICY_FAIL`). This replaces the earlier software KDF
// "PCR-seal" (which only kept the key in kernel RAM).

/// A TPM2B_* length-prefixed field (2-byte big-endian size + data).
fn tpm2b(data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + data.len());
    v.extend_from_slice(&(data.len() as u16).to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// An empty-password authorization area (`TPM_RS_PW`, no nonce/attrs/hmac) for a
/// hierarchy/parent whose auth value is empty.
fn pw_auth_area() -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&TPM_RS_PW.to_be_bytes());
    inner.extend_from_slice(&0u16.to_be_bytes()); // nonce (empty)
    inner.push(0); // sessionAttributes
    inner.extend_from_slice(&0u16.to_be_bytes()); // hmac (empty)
    let mut v = Vec::new();
    v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    v.extend_from_slice(&inner);
    v
}

/// An authorization area that uses a policy `session` handle (empty hmac) — how
/// `Unseal` proves the PCR policy is satisfied.
fn session_auth_area(session: u32) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&session.to_be_bytes());
    inner.extend_from_slice(&0u16.to_be_bytes()); // nonce (empty)
    inner.push(0); // sessionAttributes (no continueSession — one-shot)
    inner.extend_from_slice(&0u16.to_be_bytes()); // hmac (empty)
    let mut v = Vec::new();
    v.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    v.extend_from_slice(&inner);
    v
}

/// The TPMT_PUBLIC of a standard ECC-P256 restricted **storage parent** (SRK-like,
/// attributes `0x00030472`). Deterministic per owner seed with an empty `unique`,
/// so `CreatePrimary` reproduces the same parent every boot → a blob sealed under
/// it loads again after a reboot.
fn ecc_storage_parent_template() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&TPM_ALG_ECC.to_be_bytes()); // type
    t.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes()); // nameAlg
    t.extend_from_slice(&0x0003_0472u32.to_be_bytes()); // objectAttributes (SRK)
    t.extend_from_slice(&tpm2b(&[])); // authPolicy (empty)
    // TPMS_ECC_PARMS: symmetric AES-128-CFB, scheme NULL, curve P256, kdf NULL.
    t.extend_from_slice(&TPM_ALG_AES.to_be_bytes());
    t.extend_from_slice(&128u16.to_be_bytes());
    t.extend_from_slice(&TPM_ALG_CFB.to_be_bytes());
    t.extend_from_slice(&TPM_ALG_NULL.to_be_bytes()); // scheme
    t.extend_from_slice(&TPM_ECC_NIST_P256.to_be_bytes());
    t.extend_from_slice(&TPM_ALG_NULL.to_be_bytes()); // kdf
    // TPMS_ECC_POINT unique: empty x, empty y.
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t
}

/// The TPMT_PUBLIC of a `keyedhash` **sealed data** object: no user auth
/// (`fixedTPM|fixedParent` only) so the only way to release it is a policy
/// session matching `auth_policy`.
fn sealed_data_template(auth_policy: &[u8]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&TPM_ALG_KEYEDHASH.to_be_bytes()); // type
    t.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes()); // nameAlg
    t.extend_from_slice(&0x0000_0012u32.to_be_bytes()); // fixedTPM | fixedParent
    t.extend_from_slice(&tpm2b(auth_policy)); // authPolicy = PolicyPCR digest
    t.extend_from_slice(&TPM_ALG_NULL.to_be_bytes()); // TPMT_KEYEDHASH_SCHEME = NULL
    t.extend_from_slice(&tpm2b(&[])); // unique (empty)
    t
}

/// TPM2_CreatePrimary in the owner hierarchy → the storage parent for sealing.
pub fn create_primary_owner() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&TPM_RH_OWNER.to_be_bytes()); // primaryHandle
    body.extend_from_slice(&pw_auth_area());
    // inSensitive TPM2B_SENSITIVE_CREATE: empty userAuth + empty data.
    let mut sens = Vec::new();
    sens.extend_from_slice(&tpm2b(&[])); // userAuth
    sens.extend_from_slice(&tpm2b(&[])); // data
    body.extend_from_slice(&(sens.len() as u16).to_be_bytes());
    body.extend_from_slice(&sens);
    body.extend_from_slice(&tpm2b(&ecc_storage_parent_template())); // inPublic
    body.extend_from_slice(&tpm2b(&[])); // outsideInfo
    body.extend_from_slice(&0u32.to_be_bytes()); // creationPCR (count 0)
    command(TPM_ST_SESSIONS, CC_CREATE_PRIMARY, &body)
}

/// TPM2_StartAuthSession — an unsalted, unbound policy (or trial) session.
pub fn start_auth_session(trial: bool, nonce_caller: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&TPM_RH_NULL.to_be_bytes()); // tpmKey
    body.extend_from_slice(&TPM_RH_NULL.to_be_bytes()); // bind
    body.extend_from_slice(&tpm2b(nonce_caller)); // nonceCaller (>=16 bytes)
    body.extend_from_slice(&tpm2b(&[])); // encryptedSalt (empty)
    body.push(if trial { TPM_SE_TRIAL } else { TPM_SE_POLICY });
    body.extend_from_slice(&TPM_ALG_NULL.to_be_bytes()); // symmetric = NULL
    body.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes()); // authHash
    command(TPM_ST_NO_SESSIONS, CC_START_AUTH_SESSION, &body)
}

/// TPM2_PolicyPCR — bind `session` to the current value of PCR `pcr`.
pub fn policy_pcr(session: u32, pcr: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&session.to_be_bytes()); // policySession
    body.extend_from_slice(&tpm2b(&[])); // pcrDigest (empty → bind to current)
    body.extend_from_slice(&pcr_selection(pcr));
    command(TPM_ST_NO_SESSIONS, CC_POLICY_PCR, &body)
}

/// TPM2_PolicyGetDigest — read a (trial) session's accumulated policy digest,
/// which becomes the sealed object's `authPolicy`.
pub fn policy_get_digest(session: u32) -> Vec<u8> {
    command(TPM_ST_NO_SESSIONS, CC_POLICY_GET_DIGEST, &session.to_be_bytes())
}

/// TPM2_Create — seal `secret` under `parent`, gated by `auth_policy`.
pub fn create_sealed(parent: u32, auth_policy: &[u8], secret: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&parent.to_be_bytes());
    body.extend_from_slice(&pw_auth_area());
    // inSensitive TPM2B_SENSITIVE_CREATE: empty userAuth + the secret as data.
    let mut sens = Vec::new();
    sens.extend_from_slice(&tpm2b(&[])); // userAuth
    sens.extend_from_slice(&tpm2b(secret)); // data
    body.extend_from_slice(&(sens.len() as u16).to_be_bytes());
    body.extend_from_slice(&sens);
    body.extend_from_slice(&tpm2b(&sealed_data_template(auth_policy))); // inPublic
    body.extend_from_slice(&tpm2b(&[])); // outsideInfo
    body.extend_from_slice(&0u32.to_be_bytes()); // creationPCR
    command(TPM_ST_SESSIONS, CC_CREATE, &body)
}

/// TPM2_Load — load a sealed (private, public) blob under `parent`.
pub fn load(parent: u32, private: &[u8], public: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&parent.to_be_bytes());
    body.extend_from_slice(&pw_auth_area());
    body.extend_from_slice(&tpm2b(private)); // inPrivate
    body.extend_from_slice(&tpm2b(public)); // inPublic
    command(TPM_ST_SESSIONS, CC_LOAD, &body)
}

/// TPM2_Unseal — release the sealed data, authorized by a policy `session`.
pub fn unseal(item: u32, session: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&item.to_be_bytes());
    body.extend_from_slice(&session_auth_area(session));
    command(TPM_ST_SESSIONS, CC_UNSEAL, &body)
}

/// TPM2_FlushContext — free a transient object/session handle.
pub fn flush_context(handle: u32) -> Vec<u8> {
    command(TPM_ST_NO_SESSIONS, CC_FLUSH_CONTEXT, &handle.to_be_bytes())
}

/// Parse a response whose handle area carries a single handle (CreatePrimary,
/// Load, StartAuthSession) → that handle.
pub fn parse_handle(resp: &[u8]) -> Option<u32> {
    let h = parse_header(resp)?;
    if !h.ok() || resp.len() < 14 {
        return None;
    }
    Some(u32::from_be_bytes([resp[10], resp[11], resp[12], resp[13]]))
}

/// A sealed blob as returned by TPM2_Create: the (opaque) private + public
/// halves that must both be persisted to later `Load` the object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedBlob {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

/// Parse a TPM2_Create response → the outPrivate/outPublic blob to persist.
pub fn parse_create(resp: &[u8]) -> Option<SealedBlob> {
    let h = parse_header(resp)?;
    if !h.ok() {
        return None;
    }
    let mut p = 10usize;
    if h.tag == TPM_ST_SESSIONS {
        p += 4; // parameterSize
    }
    let plen = u16::from_be_bytes([*resp.get(p)?, *resp.get(p + 1)?]) as usize;
    p += 2;
    let private = resp.get(p..p + plen)?.to_vec();
    p += plen;
    let publen = u16::from_be_bytes([*resp.get(p)?, *resp.get(p + 1)?]) as usize;
    p += 2;
    let public = resp.get(p..p + publen)?.to_vec();
    Some(SealedBlob { private, public })
}

/// Parse a TPM2_PolicyGetDigest response → the policy digest.
pub fn parse_policy_digest(resp: &[u8]) -> Option<Vec<u8>> {
    let h = parse_header(resp)?;
    if !h.ok() {
        return None;
    }
    let n = u16::from_be_bytes([*resp.get(10)?, *resp.get(11)?]) as usize;
    resp.get(12..12 + n).map(|s| s.to_vec())
}

/// Parse a TPM2_Unseal response → the released secret bytes. `None` when the TPM
/// refused (e.g. `TPM_RC_POLICY_FAIL` on a mismatched/tampered boot state).
pub fn parse_unseal(resp: &[u8]) -> Option<Vec<u8>> {
    let h = parse_header(resp)?;
    if !h.ok() {
        return None;
    }
    let mut p = 10usize;
    if h.tag == TPM_ST_SESSIONS {
        p += 4; // parameterSize
    }
    let n = u16::from_be_bytes([*resp.get(p)?, *resp.get(p + 1)?]) as usize;
    p += 2;
    resp.get(p..p + n).map(|s| s.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(b: &[u8], o: usize) -> u32 {
        u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn startup_encoding() {
        let c = startup();
        assert_eq!(u16::from_be_bytes([c[0], c[1]]), TPM_ST_NO_SESSIONS);
        assert_eq!(be32(&c, 2), c.len() as u32); // commandSize == actual length
        assert_eq!(be32(&c, 6), CC_STARTUP);
        assert_eq!(&c[10..12], &TPM_SU_CLEAR.to_be_bytes());
        assert_eq!(c.len(), 12);
    }

    #[test]
    fn get_random_and_parse() {
        let c = get_random(16);
        assert_eq!(be32(&c, 6), CC_GET_RANDOM);
        assert_eq!(u16::from_be_bytes([c[10], c[11]]), 16);
        // Build a fake response with 8 random bytes.
        let mut r = alloc::vec![0x80, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8];
        r.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let size = r.len() as u32;
        r[2..6].copy_from_slice(&size.to_be_bytes());
        assert_eq!(parse_random(&r).unwrap(), alloc::vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn pcr_extend_encoding() {
        let digest = [0xABu8; 32];
        let c = pcr_extend(16, &digest);
        assert_eq!(u16::from_be_bytes([c[0], c[1]]), TPM_ST_SESSIONS);
        assert_eq!(be32(&c, 6), CC_PCR_EXTEND);
        assert_eq!(be32(&c, 10), 16); // pcrHandle
        assert_eq!(be32(&c, 14), 9); // authSize
        assert_eq!(be32(&c, 18), TPM_RS_PW);
        // digests: count=1, hashAlg SHA256, then the 32-byte digest.
        let dp = 14 + 4 + 9; // after pcrHandle+authSize+authArea
        assert_eq!(be32(&c, dp), 1); // count
        assert_eq!(u16::from_be_bytes([c[dp + 4], c[dp + 5]]), TPM_ALG_SHA256);
        assert_eq!(&c[dp + 6..dp + 6 + 32], &digest);
        assert_eq!(be32(&c, 2), c.len() as u32);
    }

    #[test]
    fn pcr_read_roundtrip() {
        let c = pcr_read(7);
        assert_eq!(be32(&c, 6), CC_PCR_READ);
        // selection bitmap: PCR 7 → byte 0 bit 7.
        // header(10) + count(4) + hashAlg(2) + sizeofSelect(1) + select(3)
        assert_eq!(c[10 + 4 + 2], 3); // sizeofSelect
        assert_eq!(c[10 + 4 + 2 + 1], 0x80); // bit 7 in byte 0

        // Build a plausible PCR_Read response with digest 0x11.. (32×).
        let digest = [0x11u8; 32];
        let mut r = alloc::vec![0x80, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]; // header
        r.extend_from_slice(&0u32.to_be_bytes()); // pcrUpdateCounter
        r.extend_from_slice(&1u32.to_be_bytes()); // selOut count
        r.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes());
        r.push(3);
        r.extend_from_slice(&[0x80, 0, 0]); // PCR 7
        r.extend_from_slice(&1u32.to_be_bytes()); // pcrValues count
        r.extend_from_slice(&(32u16).to_be_bytes());
        r.extend_from_slice(&digest);
        let size = r.len() as u32;
        r[2..6].copy_from_slice(&size.to_be_bytes());
        assert_eq!(parse_pcr_read(&r).unwrap(), digest.to_vec());
    }

    #[test]
    fn header_error_code() {
        let r = [0x80, 0x01, 0, 0, 0, 10, 0, 0, 0x01, 0x01]; // rc != 0
        let h = parse_header(&r).unwrap();
        assert!(!h.ok());
        assert!(parse_random(&r).is_none());
    }

    // ── 3D-1: real seal/unseal command encodings + response parsers ──────────

    #[test]
    fn create_primary_encoding() {
        let c = create_primary_owner();
        assert_eq!(u16::from_be_bytes([c[0], c[1]]), TPM_ST_SESSIONS);
        assert_eq!(be32(&c, 2), c.len() as u32); // commandSize correct
        assert_eq!(be32(&c, 6), CC_CREATE_PRIMARY);
        assert_eq!(be32(&c, 10), TPM_RH_OWNER); // primaryHandle
        // The ECC storage template must carry the canonical SRK attributes.
        let t = ecc_storage_parent_template();
        assert_eq!(u16::from_be_bytes([t[0], t[1]]), TPM_ALG_ECC);
        assert_eq!(u16::from_be_bytes([t[2], t[3]]), TPM_ALG_SHA256);
        assert_eq!(be32(&t, 4), 0x0003_0472);
    }

    #[test]
    fn sealed_template_carries_policy() {
        let policy = [0xC7u8; 32];
        let t = sealed_data_template(&policy);
        assert_eq!(u16::from_be_bytes([t[0], t[1]]), TPM_ALG_KEYEDHASH);
        assert_eq!(be32(&t, 4), 0x0000_0012); // fixedTPM|fixedParent, no user auth
        // authPolicy is a TPM2B holding exactly the 32-byte policy digest.
        assert_eq!(u16::from_be_bytes([t[8], t[9]]), 32);
        assert_eq!(&t[10..42], &policy);
    }

    #[test]
    fn start_session_policy_vs_trial() {
        let nonce = [0x5Au8; 16];
        let pol = start_auth_session(false, &nonce);
        let trial = start_auth_session(true, &nonce);
        assert_eq!(be32(&pol, 6), CC_START_AUTH_SESSION);
        assert_eq!(be32(&pol, 10), TPM_RH_NULL); // tpmKey
        assert_eq!(be32(&pol, 14), TPM_RH_NULL); // bind
        assert_eq!(u16::from_be_bytes([pol[18], pol[19]]), 16); // nonceCaller size
        // sessionType byte sits after nonceCaller(2+16) + encryptedSalt(2).
        let st = 18 + 2 + 16 + 2;
        assert_eq!(pol[st], TPM_SE_POLICY);
        assert_eq!(trial[st], TPM_SE_TRIAL);
        assert_eq!(u16::from_be_bytes([pol[st + 1], pol[st + 3]]), 0); // symmetric NULL hi/lo sane
    }

    #[test]
    fn policy_pcr_and_getdigest_encoding() {
        let c = policy_pcr(0x0300_0000, 16);
        assert_eq!(u16::from_be_bytes([c[0], c[1]]), TPM_ST_NO_SESSIONS);
        assert_eq!(be32(&c, 6), CC_POLICY_PCR);
        assert_eq!(be32(&c, 10), 0x0300_0000); // policySession handle
        // pcrDigest empty (size 0) then the PCR selection (PCR 16 → byte 2 bit 0).
        assert_eq!(u16::from_be_bytes([c[14], c[15]]), 0);
        let g = policy_get_digest(0x0300_0000);
        assert_eq!(be32(&g, 6), CC_POLICY_GET_DIGEST);
        assert_eq!(be32(&g, 10), 0x0300_0000);
    }

    #[test]
    fn create_sealed_and_load_roundtrip_shape() {
        let policy = [0x11u8; 32];
        let secret = b"disk-key-material-0123456789abcd";
        let c = create_sealed(0x8000_0001, &policy, secret);
        assert_eq!(u16::from_be_bytes([c[0], c[1]]), TPM_ST_SESSIONS);
        assert_eq!(be32(&c, 6), CC_CREATE);
        assert_eq!(be32(&c, 10), 0x8000_0001); // parentHandle
        // The secret is present in the marshalled inSensitive.data.
        assert!(c.windows(secret.len()).any(|w| w == secret));

        let l = load(0x8000_0001, &[1, 2, 3, 4], &policy);
        assert_eq!(be32(&l, 6), CC_LOAD);
        assert_eq!(be32(&l, 10), 0x8000_0001);
    }

    #[test]
    fn unseal_uses_policy_session_auth() {
        let c = unseal(0x8000_0002, 0x0300_0000);
        assert_eq!(be32(&c, 6), CC_UNSEAL);
        assert_eq!(be32(&c, 10), 0x8000_0002); // itemHandle
        assert_eq!(be32(&c, 14), 9); // authSize (handle+nonce+attrs+hmac)
        assert_eq!(be32(&c, 18), 0x0300_0000); // policy session handle in the auth area
    }

    #[test]
    fn parse_handle_and_create_and_unseal() {
        // Handle response (CreatePrimary/Load): header + handle.
        let mut r = alloc::vec![0x80, 0x02, 0, 0, 0, 0, 0, 0, 0, 0];
        r.extend_from_slice(&0x8000_0007u32.to_be_bytes());
        let size = r.len() as u32;
        r[2..6].copy_from_slice(&size.to_be_bytes());
        assert_eq!(parse_handle(&r), Some(0x8000_0007));

        // Create response (SESSIONS tag): header + paramSize + outPrivate + outPublic + trailing.
        let priv_blob = [0xAAu8; 20];
        let pub_blob = [0xBBu8; 14];
        let mut cr = alloc::vec![0x80, 0x02, 0, 0, 0, 0, 0, 0, 0, 0];
        cr.extend_from_slice(&0u32.to_be_bytes()); // paramSize (value irrelevant to parser)
        cr.extend_from_slice(&(priv_blob.len() as u16).to_be_bytes());
        cr.extend_from_slice(&priv_blob);
        cr.extend_from_slice(&(pub_blob.len() as u16).to_be_bytes());
        cr.extend_from_slice(&pub_blob);
        cr.extend_from_slice(&[0xEE; 8]); // creationData/hash/ticket tail (ignored)
        let sz = cr.len() as u32;
        cr[2..6].copy_from_slice(&sz.to_be_bytes());
        let blob = parse_create(&cr).unwrap();
        assert_eq!(blob.private, priv_blob.to_vec());
        assert_eq!(blob.public, pub_blob.to_vec());

        // Unseal response (SESSIONS tag): header + paramSize + outData TPM2B.
        let secret = b"unsealed!";
        let mut ur = alloc::vec![0x80, 0x02, 0, 0, 0, 0, 0, 0, 0, 0];
        ur.extend_from_slice(&0u32.to_be_bytes());
        ur.extend_from_slice(&(secret.len() as u16).to_be_bytes());
        ur.extend_from_slice(secret);
        let usz = ur.len() as u32;
        ur[2..6].copy_from_slice(&usz.to_be_bytes());
        assert_eq!(parse_unseal(&ur).unwrap(), secret.to_vec());

        // A policy-fail response (rc != 0) must parse to None (secret withheld).
        let fail = [0x80, 0x01, 0, 0, 0, 10, 0, 0, 0x09, 0x9D];
        assert_eq!(be32(&fail, 6), TPM_RC_POLICY_FAIL);
        assert!(parse_unseal(&fail).is_none());
    }
}
