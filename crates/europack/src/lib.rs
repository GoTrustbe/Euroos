//! EuroPack v2 — a **signed** container for large files served from a second disk.
//!
//! v1 (`EUROPCK1`) was a bare table of contents: nothing proved that the bytes
//! a demand-paged binary faulted in from the disk were the bytes that were
//! packed. v2 closes that: the manifest carries, per file, the **root of a
//! SHA-256 Merkle tree over its 4 KiB pages**, and the whole manifest is
//! **Ed25519-signed**. The kernel verifies the signature at scan time, recomputes
//! every root from the file's leaf table (refusing the file on mismatch), keeps
//! the leaves, and then checks **every page against its leaf as it is read**. A
//! flipped byte anywhere in a served file fails the page that carries it.
//!
//! Layout (little-endian, all offsets from the start of the disk):
//!
//! ```text
//! 0    "EUROPCK2"                     8 B
//! 8    count u32 | flags u32          8 B
//! 16   salt                          32 B   binds the hashes to this pack
//! 48   signature                     64 B   Ed25519 over TBS (below)
//! 112  entries, count x 248 B:
//!        path[192] NUL-padded | data_off u64 | size u64 | leaves_off u64 | root[32]
//! ...  leaf tables, one per file, 4 KiB-aligned: ceil(size/4096) x 32 B
//! ...  file data, 4 KiB-aligned
//! ```
//!
//! TBS (to-be-signed) = bytes 0..48 ++ bytes 112..(112 + 248*count): the header
//! and every entry, with the signature field left out, so the roots, sizes and
//! paths are all under the signature. The leaf tables are not signed directly;
//! each is anchored by its root, which is.
//!
//! Hashing (same construction as EuroVerity): leaf = SHA-256(salt || 0x00 || page),
//! the last page zero-padded to 4096 B; node = SHA-256(salt || 0x01 || left ||
//! right), an odd node at any level pairs with itself; a file of size 0 has the
//! all-zero root. `scripts/mkeuropack.py` produces this format and must match.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

pub const MAGIC_V1: &[u8; 8] = b"EUROPCK1";
pub const MAGIC_V2: &[u8; 8] = b"EUROPCK2";
pub const PAGE: usize = 4096;
pub const HEADER_LEN: usize = 112;
pub const ENTRY_LEN: usize = 248;
pub const PATH_LEN: usize = 192;
pub const SIG_OFF: usize = 48;
pub const MAX_COUNT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A v1 pack: unsigned, refused.
    UnsignedV1,
    /// Not a EuroPack at all.
    BadMagic,
    /// A count or length that does not fit.
    BadLength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub count: usize,
    pub flags: u32,
    pub salt: [u8; 32],
    pub sig: [u8; 64],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<u8>,
    pub data_off: u64,
    pub size: u64,
    pub leaves_off: u64,
    pub root: [u8; 32],
}

fn u32_at(b: &[u8], o: usize) -> u32 { u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) }
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Parse the fixed header (needs at least `HEADER_LEN` bytes).
pub fn parse_header(b: &[u8]) -> Result<Header, Error> {
    if b.len() < HEADER_LEN { return Err(Error::BadLength); }
    if &b[0..8] == MAGIC_V1 { return Err(Error::UnsignedV1); }
    if &b[0..8] != MAGIC_V2 { return Err(Error::BadMagic); }
    let count = u32_at(b, 8) as usize;
    if count > MAX_COUNT { return Err(Error::BadLength); }
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&b[16..48]);
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&b[48..112]);
    Ok(Header { count, flags: u32_at(b, 12), salt, sig })
}

/// Total manifest length (header + entries) for `count` files.
pub fn manifest_len(count: usize) -> usize { HEADER_LEN + ENTRY_LEN * count }

/// Parse entry `i` out of a buffer holding the whole manifest.
pub fn parse_entry(manifest: &[u8], i: usize) -> Result<Entry, Error> {
    let o = HEADER_LEN + i * ENTRY_LEN;
    if manifest.len() < o + ENTRY_LEN { return Err(Error::BadLength); }
    let e = &manifest[o..o + ENTRY_LEN];
    let plen = e[..PATH_LEN].iter().position(|&c| c == 0).unwrap_or(PATH_LEN);
    let mut root = [0u8; 32];
    root.copy_from_slice(&e[216..248]);
    Ok(Entry {
        path: e[..plen].to_vec(),
        data_off: u64_at(e, 192),
        size: u64_at(e, 200),
        leaves_off: u64_at(e, 208),
        root,
    })
}

/// The bytes the signature covers: the manifest with the signature field left out.
pub fn tbs(manifest: &[u8], count: usize) -> Result<Vec<u8>, Error> {
    let len = manifest_len(count);
    if manifest.len() < len { return Err(Error::BadLength); }
    let mut v = Vec::with_capacity(len - 64);
    v.extend_from_slice(&manifest[..SIG_OFF]);
    v.extend_from_slice(&manifest[HEADER_LEN..len]);
    Ok(v)
}

/// Number of 4 KiB pages (leaves) a file of `size` bytes has.
pub fn page_count(size: u64) -> usize { ((size + PAGE as u64 - 1) / PAGE as u64) as usize }

/// Leaf hash of one page. A short final page is zero-padded to 4096 bytes.
pub fn leaf_of(salt: &[u8; 32], page: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update([0x00]);
    h.update(page);
    if page.len() < PAGE {
        let pad = [0u8; PAGE];
        h.update(&pad[..PAGE - page.len()]);
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

fn node_of(salt: &[u8; 32], l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update([0x01]);
    h.update(l);
    h.update(r);
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

/// The Merkle root of a leaf table. An odd node pairs with itself; no leaves
/// (an empty file) gives the all-zero root.
pub fn root_of_leaves(salt: &[u8; 32], leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() { return [0u8; 32]; }
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity((level.len() + 1) / 2);
        for pair in level.chunks(2) {
            let r = if pair.len() == 2 { &pair[1] } else { &pair[0] };
            next.push(node_of(salt, &pair[0], r));
        }
        level = next;
    }
    level[0]
}

/// Parse a raw leaf table (`page_count(size) * 32` bytes).
pub fn parse_leaves(raw: &[u8], size: u64) -> Result<Vec<[u8; 32]>, Error> {
    let n = page_count(size);
    if raw.len() < n * 32 { return Err(Error::BadLength); }
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let mut l = [0u8; 32];
        l.copy_from_slice(&raw[i * 32..i * 32 + 32]);
        v.push(l);
    }
    Ok(v)
}

/// Does `page` (page number `index` of the file) match its leaf?
pub fn page_ok(salt: &[u8; 32], leaves: &[[u8; 32]], index: usize, page: &[u8]) -> bool {
    match leaves.get(index) {
        Some(l) => leaf_of(salt, page) == *l,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

    /// Build a v2 pack in memory exactly as mkeuropack.py does.
    fn build_pack(files: &[(&str, &[u8])], salt: [u8; 32], key: &SigningKey) -> Vec<u8> {
        let count = files.len();
        let mlen = manifest_len(count);
        let mut off = (mlen + PAGE - 1) / PAGE * PAGE;
        // leaf tables first, then data
        let mut leaf_tables: Vec<(usize, Vec<[u8; 32]>)> = Vec::new();
        for (_, data) in files {
            let leaves: Vec<[u8; 32]> = data.chunks(PAGE).map(|p| leaf_of(&salt, p)).collect();
            leaf_tables.push((off, leaves.clone()));
            off = (off + leaves.len() * 32 + PAGE - 1) / PAGE * PAGE;
        }
        let mut data_offs = Vec::new();
        for (_, data) in files {
            data_offs.push(off);
            off = (off + data.len() + PAGE - 1) / PAGE * PAGE;
        }
        let mut img = alloc::vec![0u8; off.max(PAGE)];
        img[0..8].copy_from_slice(MAGIC_V2);
        img[8..12].copy_from_slice(&(count as u32).to_le_bytes());
        img[16..48].copy_from_slice(&salt);
        for (i, (path, data)) in files.iter().enumerate() {
            let o = HEADER_LEN + i * ENTRY_LEN;
            img[o..o + path.len()].copy_from_slice(path.as_bytes());
            img[o + 192..o + 200].copy_from_slice(&(data_offs[i] as u64).to_le_bytes());
            img[o + 200..o + 208].copy_from_slice(&(data.len() as u64).to_le_bytes());
            img[o + 208..o + 216].copy_from_slice(&(leaf_tables[i].0 as u64).to_le_bytes());
            img[o + 216..o + 248].copy_from_slice(&root_of_leaves(&salt, &leaf_tables[i].1));
            for (j, l) in leaf_tables[i].1.iter().enumerate() {
                let lo = leaf_tables[i].0 + j * 32;
                img[lo..lo + 32].copy_from_slice(l);
            }
            img[data_offs[i]..data_offs[i] + data.len()].copy_from_slice(data);
        }
        let t = tbs(&img, count).unwrap();
        let sig = key.sign(&t).to_bytes();
        img[SIG_OFF..SIG_OFF + 64].copy_from_slice(&sig);
        img
    }

    fn key() -> SigningKey { SigningKey::from_bytes(&[7u8; 32]) }

    #[test]
    fn roundtrip_parses_and_verifies() {
        let a = alloc::vec![0xabu8; PAGE * 3 + 17];
        let b = b"tiny".to_vec();
        let img = build_pack(&[("/pack/a", &a), ("/pack/b", &b)], [3u8; 32], &key());
        let h = parse_header(&img).unwrap();
        assert_eq!(h.count, 2);
        let vk: VerifyingKey = key().verifying_key();
        let t = tbs(&img, h.count).unwrap();
        assert!(vk.verify(&t, &ed25519_dalek::Signature::from_bytes(&h.sig)).is_ok());
        let e = parse_entry(&img, 0).unwrap();
        assert_eq!(e.path, b"/pack/a");
        assert_eq!(e.size, a.len() as u64);
        let leaves = parse_leaves(&img[e.leaves_off as usize..], e.size).unwrap();
        assert_eq!(leaves.len(), 4);
        assert_eq!(root_of_leaves(&h.salt, &leaves), e.root);
        for (i, p) in a.chunks(PAGE).enumerate() {
            let d = &img[e.data_off as usize + i * PAGE..];
            assert!(page_ok(&h.salt, &leaves, i, &d[..p.len()]));
        }
    }

    #[test]
    fn v1_is_refused() {
        let mut img = alloc::vec![0u8; PAGE];
        img[0..8].copy_from_slice(MAGIC_V1);
        assert_eq!(parse_header(&img), Err(Error::UnsignedV1));
        img[0..8].copy_from_slice(b"NOTAPACK");
        assert_eq!(parse_header(&img), Err(Error::BadMagic));
    }

    #[test]
    fn tampered_entry_breaks_the_signature() {
        let a = alloc::vec![1u8; PAGE * 2];
        let mut img = build_pack(&[("/pack/a", &a)], [9u8; 32], &key());
        let h = parse_header(&img).unwrap();
        let vk = key().verifying_key();
        let sig = ed25519_dalek::Signature::from_bytes(&h.sig);
        assert!(vk.verify(&tbs(&img, 1).unwrap(), &sig).is_ok());
        img[HEADER_LEN + 216] ^= 0x01; // one bit of the root
        assert!(vk.verify(&tbs(&img, 1).unwrap(), &sig).is_err());
    }

    #[test]
    fn tampered_leaf_table_breaks_the_root() {
        let a = alloc::vec![2u8; PAGE * 5];
        let mut img = build_pack(&[("/pack/a", &a)], [1u8; 32], &key());
        let h = parse_header(&img).unwrap();
        let e = parse_entry(&img, 0).unwrap();
        img[e.leaves_off as usize + 32 * 3] ^= 0x80;
        let leaves = parse_leaves(&img[e.leaves_off as usize..], e.size).unwrap();
        assert_ne!(root_of_leaves(&h.salt, &leaves), e.root);
    }

    #[test]
    fn tampered_page_fails_its_leaf_only() {
        let a: Vec<u8> = (0..PAGE * 4 + 100).map(|i| (i % 251) as u8).collect();
        let mut img = build_pack(&[("/pack/a", &a)], [5u8; 32], &key());
        let h = parse_header(&img).unwrap();
        let e = parse_entry(&img, 0).unwrap();
        let leaves = parse_leaves(&img[e.leaves_off as usize..], e.size).unwrap();
        assert_eq!(root_of_leaves(&h.salt, &leaves), e.root);
        let d0 = e.data_off as usize;
        img[d0 + PAGE * 2 + 7] ^= 0x10;
        assert!(page_ok(&h.salt, &leaves, 1, &img[d0 + PAGE..d0 + PAGE * 2]));
        assert!(!page_ok(&h.salt, &leaves, 2, &img[d0 + PAGE * 2..d0 + PAGE * 3]));
        // the short last page is checked zero-padded
        assert!(page_ok(&h.salt, &leaves, 4, &img[d0 + PAGE * 4..d0 + PAGE * 4 + 100]));
        assert!(!page_ok(&h.salt, &leaves, 9, &img[d0..d0 + PAGE]));
    }

    #[test]
    fn empty_file_has_zero_root_and_odd_levels_pair_with_self() {
        let salt = [0u8; 32];
        assert_eq!(root_of_leaves(&salt, &[]), [0u8; 32]);
        let l = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let n01 = node_of(&salt, &l[0], &l[1]);
        let n22 = node_of(&salt, &l[2], &l[2]);
        assert_eq!(root_of_leaves(&salt, &l), node_of(&salt, &n01, &n22));
    }

    /// A pack produced by scripts/mkeuropack.py (checked in) must parse and verify
    /// here with the real public key: this pins the Python writer and the Rust
    /// reader to one format. Regenerate with:
    ///   mkeuropack.py tiny-signed.img a.txt:/pack/a.txt b.bin:/pack/b.bin
    #[test]
    fn pack_from_the_python_tool_verifies_with_dev_pub() {
        let img: &[u8] = include_bytes!("../testdata/tiny-signed.img");
        let pub_bytes: &[u8; 32] = include_bytes!("../testdata/dev.pub");
        let h = parse_header(img).unwrap();
        assert_eq!(h.count, 2);
        let vk = VerifyingKey::from_bytes(pub_bytes).unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(&h.sig);
        assert!(vk.verify_strict(&tbs(img, h.count).unwrap(), &sig).is_ok(), "manifest signature");
        for i in 0..h.count {
            let e = parse_entry(img, i).unwrap();
            let leaves = parse_leaves(&img[e.leaves_off as usize..], e.size).unwrap();
            assert_eq!(root_of_leaves(&h.salt, &leaves), e.root, "root of {:?}", core::str::from_utf8(&e.path));
            let d = &img[e.data_off as usize..e.data_off as usize + e.size as usize];
            for (j, page) in d.chunks(PAGE).enumerate() {
                assert!(page_ok(&h.salt, &leaves, j, page), "page {j} of {:?}", core::str::from_utf8(&e.path));
            }
        }
        let b = parse_entry(img, 1).unwrap();
        assert_eq!(b.path, b"/pack/b.bin");
        assert_eq!(b.size, 256 * 20);
        assert_eq!(page_count(b.size), 2);
    }
}
