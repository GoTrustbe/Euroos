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

// Command codes.
const CC_STARTUP: u32 = 0x0000_0144;
const CC_GET_RANDOM: u32 = 0x0000_017B;
const CC_PCR_READ: u32 = 0x0000_017E;
const CC_PCR_EXTEND: u32 = 0x0000_0182;

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
}
