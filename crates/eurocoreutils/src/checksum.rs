//! Checksum commands (CU-6): the SHA-2 family via the host-tested `sha2` crate.
//! GNU output format: `<hex>  <name>` (two spaces). `name` is usually `-`
//! (stdin) or the file name passed by the shell.

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

/// GNU `sha1sum` — SHA-1 (RFC 3174), via the dependency-free [`crate::hashes`] module.
pub fn sha1sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&crate::hashes::sha1(input)), name)
}
/// GNU `md5sum` — MD5 (RFC 1321), via the dependency-free [`crate::hashes`] module.
pub fn md5sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&crate::hashes::md5(input)), name)
}
/// GNU `b2sum` — BLAKE2b-512 (RFC 7693), via the dependency-free [`crate::hashes`] module.
pub fn b2sum(input: &[u8], name: &str) -> Vec<u8> {
    line(&hex(&crate::hashes::blake2b512(input)), name)
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

    #[test]
    fn sha1sum_format() {
        // sha1sum("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        assert_eq!(
            String::from_utf8(sha1sum(b"abc", "-")).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d  -\n"
        );
        assert_eq!(
            String::from_utf8(sha1sum(b"", "f")).unwrap(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709  f\n"
        );
    }

    #[test]
    fn md5sum_format() {
        // md5sum("abc") = 900150983cd24fb0d6963f7d28e17f72
        assert_eq!(
            String::from_utf8(md5sum(b"abc", "-")).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72  -\n"
        );
        assert_eq!(
            String::from_utf8(md5sum(b"", "-")).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e  -\n"
        );
    }

    #[test]
    fn b2sum_format() {
        let out = String::from_utf8(b2sum(b"abc", "-")).unwrap();
        assert_eq!(
            out,
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923  -\n"
        );
        let empty = String::from_utf8(b2sum(b"", "x")).unwrap();
        assert_eq!(
            empty,
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce  x\n"
        );
    }
}
