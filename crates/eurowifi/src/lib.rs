//! EuroWiFi — the 802.11 protocol core (plan N1).
//!
//! A sovereign WiFi stack: this `no_std` core parses **802.11 frames**
//! (management/data, beacons with SSID + security), models **scan results**, and
//! derives **WPA2/3 session keys** via the IEEE PRF (HMAC-SHA-256). The radio driver
//! itself (PHY/MAC on an AX200/210) is hardware work; the logic here is host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

// ── 802.11 frame parsing ─────────────────────────────────────────────────────

/// The type of an 802.11 frame (from the Frame-Control bytes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameType {
    Management,
    Control,
    Data,
    Reserved,
}

/// The management subtypes we recognize.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MgmtSubtype {
    Beacon,
    ProbeRequest,
    ProbeResponse,
    Authentication,
    AssociationRequest,
    AssociationResponse,
    Deauthentication,
    Other(u8),
}

/// The parsed header of an 802.11 frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub mgmt_subtype: Option<MgmtSubtype>,
    /// Address 1/2/3 (destination/source/BSSID, depending on direction).
    pub addr1: [u8; 6],
    pub addr2: [u8; 6],
    pub addr3: [u8; 6],
}

/// Parse the 24-byte management/data frame header.
pub fn parse_header(frame: &[u8]) -> Option<FrameHeader> {
    if frame.len() < 24 {
        return None;
    }
    let fc = frame[0];
    let frame_type = match (fc >> 2) & 0x3 {
        0 => FrameType::Management,
        1 => FrameType::Control,
        2 => FrameType::Data,
        _ => FrameType::Reserved,
    };
    let subtype = (fc >> 4) & 0xF;
    let mgmt_subtype = if frame_type == FrameType::Management {
        Some(match subtype {
            0x8 => MgmtSubtype::Beacon,
            0x4 => MgmtSubtype::ProbeRequest,
            0x5 => MgmtSubtype::ProbeResponse,
            0xB => MgmtSubtype::Authentication,
            0x0 => MgmtSubtype::AssociationRequest,
            0x1 => MgmtSubtype::AssociationResponse,
            0xC => MgmtSubtype::Deauthentication,
            o => MgmtSubtype::Other(o),
        })
    } else {
        None
    };
    let mut a1 = [0u8; 6];
    let mut a2 = [0u8; 6];
    let mut a3 = [0u8; 6];
    a1.copy_from_slice(&frame[4..10]);
    a2.copy_from_slice(&frame[10..16]);
    a3.copy_from_slice(&frame[16..22]);
    Some(FrameHeader { frame_type, mgmt_subtype, addr1: a1, addr2: a2, addr3: a3 })
}

/// The security type of a network (from the RSN/privacy info).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    Open,
    Wpa2,
    /// WPA3 (SAE) — mandatory for modern, sovereign deployments.
    Wpa3,
}

/// A scan result (one network found).
#[derive(Clone, Debug, PartialEq)]
pub struct ScanResult {
    pub bssid: [u8; 6],
    pub ssid: String,
    pub channel: u8,
    pub security: Security,
}

/// Parse a **beacon** frame into a scan result. Walks the tagged information
/// elements (SSID = id 0, DS-Parameter/channel = id 3, RSN = id 48).
pub fn parse_beacon(frame: &[u8]) -> Option<ScanResult> {
    let hdr = parse_header(frame)?;
    if hdr.mgmt_subtype != Some(MgmtSubtype::Beacon) {
        return None;
    }
    // 24-byte header + 12-byte fixed (timestamp/interval/capabilities). A valid
    // beacon is ≥ 36 bytes; a shorter (possibly malicious) frame → reject,
    // otherwise the capabilities read would index outside the buffer (audit C4).
    if frame.len() < 36 {
        return None;
    }
    // 24-byte header + 12-byte fixed (timestamp/interval/capabilities) → IEs.
    let mut i = 36;
    let mut ssid = String::new();
    let mut channel = 0u8;
    let mut security = Security::Open;
    // Privacy bit in the capabilities (offset 34..36, little-endian) → at least WEP/WPA.
    let caps = u16::from_le_bytes([frame[34], frame[35]]);
    let privacy = caps & 0x10 != 0;

    while i + 2 <= frame.len() {
        let id = frame[i];
        let len = frame[i + 1] as usize;
        let body_start = i + 2;
        if body_start + len > frame.len() {
            break;
        }
        let body = &frame[body_start..body_start + len];
        match id {
            0 => ssid = String::from_utf8_lossy(body).into_owned(),
            3 if !body.is_empty() => channel = body[0],
            48 => security = Security::Wpa2, // RSN present
            221
                // Vendor-specific; WPA3-SAE advertises AKM 8 in the RSN, simplified here.
                if body.windows(1).any(|_| false) => {}
            _ => {}
        }
        i = body_start + len;
    }
    // WPA3 is advertised via the RSN-AKM suite 00-0F-AC:8 (SAE).
    if has_sae_akm(frame) {
        security = Security::Wpa3;
    } else if privacy && security == Security::Open {
        security = Security::Wpa2;
    }
    Some(ScanResult { bssid: hdr.addr3, ssid, channel, security })
}

/// Detect the SAE-AKM suite (00-0F-AC-08) anywhere in the frame → WPA3.
fn has_sae_akm(frame: &[u8]) -> bool {
    frame.windows(4).any(|w| w == [0x00, 0x0F, 0xAC, 0x08])
}

// ── WPA2/3 key derivation (IEEE 802.11 PRF on HMAC-SHA-256) ────────────────

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// The IEEE 802.11 PRF: derive `bits/8` bytes from `key`, `label` and `data`.
pub fn prf(key: &[u8], label: &str, data: &[u8], out_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(out_len);
    // The counter is u16 so that out_len > 255·32 does not overflow (audit M4); the PRF
    // delivers ≤ 64 bytes (PTK) in practice, so this is more than enough.
    let mut counter: u16 = 0;
    while out.len() < out_len {
        let mut msg = Vec::new();
        msg.extend_from_slice(label.as_bytes());
        msg.push(0);
        msg.extend_from_slice(data);
        msg.push(counter as u8);
        out.extend_from_slice(&hmac_sha256(key, &msg));
        counter += 1;
    }
    out.truncate(out_len);
    out
}

/// Derive the **Pairwise Transient Key** (PTK) from the PMK + the two nonces + MACs
/// (the heart of the WPA 4-way handshake). Returns 48 bytes (KCK‖KEK‖TK for CCMP).
pub fn derive_ptk(pmk: &[u8], aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Vec<u8> {
    // PRF data = min(AA,SPA) ‖ max(AA,SPA) ‖ min(ANonce,SNonce) ‖ max(ANonce,SNonce).
    let mut data = Vec::new();
    let (min_mac, max_mac) = if aa <= spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(min_mac);
    data.extend_from_slice(max_mac);
    let (min_n, max_n): (&[u8], &[u8]) = if anonce <= snonce { (anonce, snonce) } else { (snonce, anonce) };
    data.extend_from_slice(min_n);
    data.extend_from_slice(max_n);
    prf(pmk, "Pairwise key expansion", &data, 48)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal beacon frame with an SSID-IE + channel-IE + RSN-IE.
    fn beacon(ssid: &str, channel: u8, with_rsn: bool, sae: bool) -> Vec<u8> {
        let mut f = Vec::new();
        f.push(0x80); // FC: management, subtype beacon (0x8 << 4)
        f.push(0x00);
        f.extend_from_slice(&[0; 2]); // duration
        f.extend_from_slice(&[0xff; 6]); // addr1 (broadcast)
        f.extend_from_slice(&[0x11; 6]); // addr2 (source)
        f.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]); // addr3 (BSSID)
        f.extend_from_slice(&[0; 2]); // seq
        f.extend_from_slice(&[0; 8]); // timestamp
        f.extend_from_slice(&[0x64, 0x00]); // beacon interval
        let caps: u16 = if with_rsn { 0x0011 } else { 0x0001 }; // privacy bit when RSN
        f.extend_from_slice(&caps.to_le_bytes());
        // SSID-IE.
        f.push(0);
        f.push(ssid.len() as u8);
        f.extend_from_slice(ssid.as_bytes());
        // DS-parameter (channel).
        f.push(3);
        f.push(1);
        f.push(channel);
        // RSN-IE (id 48) with optionally the SAE-AKM suite.
        if with_rsn {
            let akm = if sae { [0x00, 0x0F, 0xAC, 0x08] } else { [0x00, 0x0F, 0xAC, 0x02] };
            f.push(48);
            f.push(akm.len() as u8);
            f.extend_from_slice(&akm);
        }
        f
    }

    #[test]
    fn header_parsing() {
        let f = beacon("EuroNet", 6, false, false);
        let h = parse_header(&f).unwrap();
        assert_eq!(h.frame_type, FrameType::Management);
        assert_eq!(h.mgmt_subtype, Some(MgmtSubtype::Beacon));
        assert_eq!(h.addr3, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn beacon_scan_open() {
        let r = parse_beacon(&beacon("EuroOpen", 11, false, false)).unwrap();
        assert_eq!(r.ssid, "EuroOpen");
        assert_eq!(r.channel, 11);
        assert_eq!(r.security, Security::Open);
        assert_eq!(r.bssid, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn beacon_scan_wpa2_and_wpa3() {
        let r2 = parse_beacon(&beacon("EuroSecure", 6, true, false)).unwrap();
        assert_eq!(r2.security, Security::Wpa2);
        let r3 = parse_beacon(&beacon("EuroGov", 36, true, true)).unwrap();
        assert_eq!(r3.security, Security::Wpa3);
        assert_eq!(r3.ssid, "EuroGov");
    }

    #[test]
    fn short_beacon_rejected_no_panic() {
        // C4: a beacon of 24–35 bytes must not OOB-index on the capabilities.
        for len in 24..36 {
            let frame = alloc::vec![0u8; len];
            let mut f = frame;
            f[0] = 0x80; // beacon subtype
            assert!(parse_beacon(&f).is_none());
        }
    }

    #[test]
    fn non_beacon_rejected() {
        let mut data = beacon("x", 1, false, false);
        data[0] = 0x08; // data frame
        assert!(parse_beacon(&data).is_none());
    }

    #[test]
    fn ptk_is_deterministic_and_symmetric() {
        let pmk = [0x20u8; 32];
        let aa = [1u8; 6];
        let spa = [2u8; 6];
        let an = [3u8; 32];
        let sn = [4u8; 32];
        let ptk1 = derive_ptk(&pmk, &aa, &spa, &an, &sn);
        // The same inputs → the same PTK; the min/max ordering makes it direction-symmetric.
        let ptk2 = derive_ptk(&pmk, &spa, &aa, &sn, &an);
        assert_eq!(ptk1.len(), 48);
        assert_eq!(ptk1, ptk2);
        // Different PMK → different PTK.
        let ptk3 = derive_ptk(&[0x21u8; 32], &aa, &spa, &an, &sn);
        assert_ne!(ptk1, ptk3);
    }

    #[test]
    fn prf_known_length() {
        let out = prf(b"key", "test label", b"data", 48);
        assert_eq!(out.len(), 48);
        assert_ne!(out, alloc::vec![0u8; 48]);
    }
}
