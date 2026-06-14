//! Encoding commands (CU-6): base64, base32 (encode/decode) + POSIX `cksum` (CRC).

use alloc::string::String;
use alloc::vec::Vec;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// `base64` (encode, default) / `base64 -d` (decode). The encoder wraps at 76 columns
/// like GNU `base64`.
pub fn base64(decode: bool, input: &[u8]) -> Vec<u8> {
    if decode {
        b64_decode(input)
    } else {
        wrap76(&b64_encode(input))
    }
}

fn b64_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 0x3F) as usize]);
        out.push(B64[((n >> 12) & 0x3F) as usize]);
        out.push(if chunk.len() > 1 { B64[((n >> 6) & 0x3F) as usize] } else { b'=' });
        out.push(if chunk.len() > 2 { B64[(n & 0x3F) as usize] } else { b'=' });
    }
    out
}

fn b64_decode(data: &[u8]) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let clean: Vec<u8> = data.iter().copied().filter(|&c| c != b'\n' && c != b'\r' && c != b' ').collect();
    let mut out = Vec::new();
    for chunk in clean.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
            } else if let Some(v) = val(c) {
                n |= v << (18 - i * 6);
            }
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    out
}

/// `base32` (encode) / `base32 -d` (decode).
pub fn base32(decode: bool, input: &[u8]) -> Vec<u8> {
    if decode {
        b32_decode(input)
    } else {
        wrap76(&b32_encode(input))
    }
}

fn b32_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = ((buf[0] as u64) << 32) | ((buf[1] as u64) << 24) | ((buf[2] as u64) << 16) | ((buf[3] as u64) << 8) | buf[4] as u64;
        let chars = [(n >> 35) & 31, (n >> 30) & 31, (n >> 25) & 31, (n >> 20) & 31, (n >> 15) & 31, (n >> 10) & 31, (n >> 5) & 31, n & 31];
        // Number of valid output characters per input-block length (RFC 4648).
        let valid = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for (i, &c) in chars.iter().enumerate() {
            out.push(if i < valid { B32[c as usize] } else { b'=' });
        }
    }
    out
}

fn b32_decode(data: &[u8]) -> Vec<u8> {
    let val = |c: u8| -> Option<u64> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u64),
            b'2'..=b'7' => Some((c - b'2' + 26) as u64),
            _ => None,
        }
    };
    let clean: Vec<u8> = data.iter().copied().filter(|&c| c != b'\n' && c != b'\r' && c != b' ' && c != b'=').collect();
    let mut out = Vec::new();
    for chunk in clean.chunks(8) {
        let mut n = 0u64;
        for (i, &c) in chunk.iter().enumerate() {
            if let Some(v) = val(c) {
                n |= v << (35 - i * 5);
            }
        }
        let bytes = ((chunk.len() * 5) / 8).min(5);
        for k in 0..bytes {
            out.push((n >> (32 - k * 8)) as u8);
        }
    }
    out
}

fn wrap76(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 76 + 1);
    for chunk in data.chunks(76) {
        out.extend_from_slice(chunk);
        out.push(b'\n');
    }
    if out.is_empty() {
        out.push(b'\n');
    }
    out
}

/// `cksum` — the POSIX CRC + byte count (GNU-compatible). Output: `<crc> <len> <name>`.
pub fn cksum(input: &[u8], name: &str) -> Vec<u8> {
    let crc = posix_cksum_crc(input);
    let mut s = String::new();
    s.push_str(&crc.to_string());
    s.push(' ');
    s.push_str(&input.len().to_string());
    if !name.is_empty() {
        s.push(' ');
        s.push_str(name);
    }
    s.push('\n');
    s.into_bytes()
}

fn posix_cksum_crc(data: &[u8]) -> u32 {
    // CRC-32/CKSUM: poly 0x04C11DB7, non-reflected, init 0, length bytes included,
    // final XOR 0xFFFFFFFF.
    let mut crc: u32 = 0;
    let step = |crc: u32, byte: u8| -> u32 {
        let mut c = crc ^ ((byte as u32) << 24);
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
        }
        c
    };
    for &b in data {
        crc = step(crc, b);
    }
    let mut len = data.len() as u64;
    while len != 0 {
        crc = step(crc, (len & 0xFF) as u8);
        len >>= 8;
    }
    !crc
}

use alloc::string::ToString;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn s(v: Vec<u8>) -> String {
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn base64_roundtrip_and_vectors() {
        assert_eq!(s(base64(false, b"")).trim(), "");
        assert_eq!(s(base64(false, b"f")).trim(), "Zg==");
        assert_eq!(s(base64(false, b"foobar")).trim(), "Zm9vYmFy");
        let enc = base64(false, b"hallo soeverein");
        assert_eq!(base64(true, &enc), b"hallo soeverein");
    }

    #[test]
    fn base32_roundtrip_and_vectors() {
        assert_eq!(s(base32(false, b"f")).trim(), "MY======");
        assert_eq!(s(base32(false, b"foobar")).trim(), "MZXW6YTBOI======");
        let enc = base32(false, b"EuroOS");
        assert_eq!(base32(true, &enc), b"EuroOS");
    }

    #[test]
    fn cksum_posix_vector() {
        // GNU: `printf '' | cksum` → "4294967295 0", `printf 'a' | cksum` → "1220704766 1".
        assert_eq!(s(cksum(b"", "")).trim(), "4294967295 0");
        assert_eq!(s(cksum(b"a", "")).trim(), "1220704766 1");
    }
}
