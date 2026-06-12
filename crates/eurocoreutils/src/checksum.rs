//! Checksum-commando's (CU-6): de SHA-2-familie via de host-geteste `sha2`-crate.
//! GNU-uitvoer-formaat: `<hex>  <naam>` (twee spaties). `name` is doorgaans `-`
//! (stdin) of de bestandsnaam die de shell meegeeft.

use alloc::string::String;
use alloc::vec::Vec;

use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&alloc::format!("{b:02x}"));
    }
    s
}

fn line(hash_hex: &str, name: &str) -> Vec<u8> {
    let mut v = hash_hex.as_bytes().to_vec();
    v.extend_from_slice(b"  ");
    v.extend_from_slice(name.as_bytes());
    v.push(b'\n');
    v
}

pub fn sha256sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&Sha256::digest(input)), name)
}
pub fn sha512sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&Sha512::digest(input)), name)
}
pub fn sha224sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&Sha224::digest(input)), name)
}
pub fn sha384sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&Sha384::digest(input)), name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let out = String::from_utf8(sha256sum(b"abc", "-")).unwrap();
        assert_eq!(out, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -\n");
    }

    #[test]
    fn sha512_known_vector() {
        let out = String::from_utf8(sha512sum(b"abc", "test.txt")).unwrap();
        assert!(out.starts_with("ddaf35a193617aba"));
        assert!(out.ends_with("  test.txt\n"));
    }

    #[test]
    fn sha224_384_lengths() {
        // 224-bit = 56 hex chars, 384-bit = 96 hex chars.
        assert_eq!(String::from_utf8(sha224sum(b"x", "-")).unwrap().split("  ").next().unwrap().len(), 56);
        assert_eq!(String::from_utf8(sha384sum(b"x", "-")).unwrap().split("  ").next().unwrap().len(), 96);
    }
}
