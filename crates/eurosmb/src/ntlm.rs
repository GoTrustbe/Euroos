//! NTLMv2 authentication + the NTLMSSP message types (1=Negotiate, 2=Challenge,
//! 3=Authenticate) carried inside SMB2 SESSION_SETUP. Enough for username/password
//! (and anonymous) auth against Samba/Windows. No signing/sealing.

use crate::crypto::{hmac_md5, md4};
use alloc::vec::Vec;

const SIG: &[u8] = b"NTLMSSP\0";

// NTLMSSP negotiate flags we advertise (Unicode + NTLM + extended session security).
const F_UNICODE: u32 = 0x0000_0001;
const F_REQUEST_TARGET: u32 = 0x0000_0004;
const F_NTLM: u32 = 0x0000_0200;
const F_ALWAYS_SIGN: u32 = 0x0000_8000;
const F_EXT_SEC: u32 = 0x0008_0000;
const F_128: u32 = 0x2000_0000;
const F_56: u32 = 0x8000_0000;
const FLAGS: u32 = F_UNICODE | F_REQUEST_TARGET | F_NTLM | F_ALWAYS_SIGN | F_EXT_SEC | F_128 | F_56;

fn utf16le(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() * 2);
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// NTOWFv2 = HMAC-MD5( MD4(UTF16LE(password)), UTF16LE(UPPER(user) + domain) ).
pub fn ntowf_v2(user: &str, domain: &str, password: &str) -> [u8; 16] {
    let nt_hash = md4(&utf16le(password));
    let mut id = utf16le(&user.to_uppercase());
    id.extend_from_slice(&utf16le(domain));
    hmac_md5(&nt_hash, &id)
}

/// The NTLMSSP NEGOTIATE (type 1) message.
pub fn negotiate() -> Vec<u8> {
    let mut m = Vec::with_capacity(40);
    m.extend_from_slice(SIG);
    m.extend_from_slice(&1u32.to_le_bytes()); // MessageType = 1
    m.extend_from_slice(&FLAGS.to_le_bytes());
    // DomainNameFields (len, maxlen, offset) + WorkstationFields — both empty.
    m.extend_from_slice(&[0u8; 8]);
    m.extend_from_slice(&[0u8; 8]);
    m
}

/// Parsed NTLMSSP CHALLENGE (type 2).
pub struct Challenge {
    pub server_challenge: [u8; 8],
    pub target_info: Vec<u8>,
}

/// Parse an NTLMSSP CHALLENGE (type 2): the 8-byte server challenge (offset 24) and
/// the TargetInfo blob (from the fields at offset 40).
pub fn parse_challenge(buf: &[u8]) -> Option<Challenge> {
    if buf.len() < 32 || &buf[0..8] != SIG || u32::from_le_bytes(buf[8..12].try_into().ok()?) != 2 {
        return None;
    }
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(&buf[24..32]);
    let mut target_info = Vec::new();
    if buf.len() >= 48 {
        let ti_len = u16::from_le_bytes(buf[40..42].try_into().ok()?) as usize;
        let ti_off = u32::from_le_bytes(buf[44..48].try_into().ok()?) as usize;
        if ti_off + ti_len <= buf.len() {
            target_info.extend_from_slice(&buf[ti_off..ti_off + ti_len]);
        }
    }
    Some(Challenge { server_challenge, target_info })
}

/// Extract the server timestamp (MsvAvTimestamp, AV id 7) from a TargetInfo blob, or 0.
fn av_timestamp(ti: &[u8]) -> u64 {
    let mut i = 0;
    while i + 4 <= ti.len() {
        let id = u16::from_le_bytes([ti[i], ti[i + 1]]);
        let len = u16::from_le_bytes([ti[i + 2], ti[i + 3]]) as usize;
        if id == 0 {
            break; // MsvAvEOL
        }
        if id == 7 && i + 4 + 8 <= ti.len() {
            return u64::from_le_bytes(ti[i + 4..i + 12].try_into().unwrap());
        }
        i += 4 + len;
    }
    0
}

/// Compute the NTLMv2 NtChallengeResponse = NTProofStr(16) || blob.
fn ntlmv2_response(ntowf: &[u8; 16], server_chal: &[u8; 8], blob: &[u8]) -> Vec<u8> {
    let mut tmp = Vec::with_capacity(8 + blob.len());
    tmp.extend_from_slice(server_chal);
    tmp.extend_from_slice(blob);
    let proof = hmac_md5(ntowf, &tmp);
    let mut out = Vec::with_capacity(16 + blob.len());
    out.extend_from_slice(&proof);
    out.extend_from_slice(blob);
    out
}

/// Build the NTLMSSP AUTHENTICATE (type 3) message for user/domain/password against the
/// given challenge. `client_chal` is 8 random bytes; `now_filetime` a Windows FILETIME
/// (or 0). For anonymous, pass an empty user — sends an anonymous authenticate.
pub fn authenticate(
    user: &str,
    domain: &str,
    password: &str,
    chal: &Challenge,
    client_chal: &[u8; 8],
    now_filetime: u64,
) -> Vec<u8> {
    let anonymous = user.is_empty();
    let (lm_resp, nt_resp): (Vec<u8>, Vec<u8>) = if anonymous {
        // Anonymous: LM = single 0 byte, NT = empty.
        (alloc::vec![0u8], Vec::new())
    } else {
        let ntowf = ntowf_v2(user, domain, password);
        // NTLMv2 temp blob.
        let ts = if now_filetime != 0 { now_filetime } else { av_timestamp(&chal.target_info) };
        let mut blob = Vec::new();
        blob.extend_from_slice(&[0x01, 0x01, 0x00, 0x00]); // RespType, HiRespType, reserved
        blob.extend_from_slice(&[0u8; 4]); // reserved
        blob.extend_from_slice(&ts.to_le_bytes());
        blob.extend_from_slice(client_chal);
        blob.extend_from_slice(&[0u8; 4]); // reserved
        blob.extend_from_slice(&chal.target_info);
        blob.extend_from_slice(&[0u8; 4]); // reserved
        let nt = ntlmv2_response(&ntowf, &chal.server_challenge, &blob);
        // LMv2 response = HMAC-MD5(ntowf, server_chal || client_chal) || client_chal.
        let mut lmtmp = Vec::new();
        lmtmp.extend_from_slice(&chal.server_challenge);
        lmtmp.extend_from_slice(client_chal);
        let mut lm = hmac_md5(&ntowf, &lmtmp).to_vec();
        lm.extend_from_slice(client_chal);
        (lm, nt)
    };

    let dom = if anonymous { Vec::new() } else { utf16le(domain) };
    let usr = if anonymous { Vec::new() } else { utf16le(user) };
    let ws: Vec<u8> = Vec::new();
    let sk: Vec<u8> = Vec::new();

    // Layout: 64-byte header (8 sig + 4 type + 6×8 field descriptors + 4 flags),
    // then the payload (LM, NT, domain, user, workstation, session key) appended.
    let header_len = 8 + 4 + 8 * 6 + 4; // = 72
    let mut payload = Vec::new();
    let field = |buf: &mut Vec<u8>, data: &[u8], payload: &mut Vec<u8>| {
        let off = header_len + payload.len();
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(off as u32).to_le_bytes());
        payload.extend_from_slice(data);
    };

    let mut m = Vec::with_capacity(header_len);
    m.extend_from_slice(SIG);
    m.extend_from_slice(&3u32.to_le_bytes()); // MessageType = 3
    field(&mut m, &lm_resp, &mut payload); // LmChallengeResponse
    field(&mut m, &nt_resp, &mut payload); // NtChallengeResponse
    field(&mut m, &dom, &mut payload); // DomainName
    field(&mut m, &usr, &mut payload); // UserName
    field(&mut m, &ws, &mut payload); // Workstation
    field(&mut m, &sk, &mut payload); // EncryptedRandomSessionKey
    m.extend_from_slice(&FLAGS.to_le_bytes());
    m.extend_from_slice(&payload);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntowfv2_known_vector() {
        // MS-NLMP §4.2.4.1.1: User "User", Domain "Domain", Password "Password".
        let r = ntowf_v2("User", "Domain", "Password");
        let hex: alloc::string::String = r.iter().map(|b| alloc::format!("{b:02x}")).collect();
        assert_eq!(hex, "0c868a403bfd7a93a3001ef22ef02e3f");
    }

    #[test]
    fn negotiate_has_signature_and_type() {
        let n = negotiate();
        assert_eq!(&n[0..8], SIG);
        assert_eq!(u32::from_le_bytes(n[8..12].try_into().unwrap()), 1);
    }

    #[test]
    fn authenticate_roundtrips_fields() {
        let chal = Challenge { server_challenge: [1; 8], target_info: alloc::vec![0, 0, 0, 0] };
        let m = authenticate("euro", "WORKGROUP", "pw", &chal, &[2; 8], 0);
        assert_eq!(&m[0..8], SIG);
        assert_eq!(u32::from_le_bytes(m[8..12].try_into().unwrap()), 3);
        // NT response field (offset 20: len/maxlen/off) must be non-empty (NTLMv2).
        let nt_len = u16::from_le_bytes(m[20..22].try_into().unwrap());
        assert!(nt_len >= 16 + 28);
    }
}
