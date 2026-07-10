//! EuroVerity — **dm-verity-style integrity for the read-only system image**.
//!
//! A sovereign OS must be able to prove its own system partition has not been
//! tampered with — offline (evil-maid) or at runtime. EuroVerity builds a
//! **SHA-256 Merkle tree** over the fixed-size blocks of the system image; the
//! single **root hash** is placed in a **manifest signed with Ed25519**
//! (verify-before-trust, like the A/B update signature). Then:
//!
//! - any block can be verified against the signed root with a short Merkle proof
//!   (O(log n) hashes) — so a read path can check integrity block-by-block;
//! - flipping a single byte anywhere changes the root, so a tampered image fails
//!   verification and the system falls back to the known-good slot.
//!
//! Pure `no_std` logic, host-tested. The salt binds the tree to this image so a
//! block from a different image cannot be substituted.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// The hash of one data block: `SHA-256(salt ‖ 0x00 ‖ block)`.
pub fn hash_block(salt: &[u8; 32], block: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update([0x00]);
    h.update(block);
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

/// The hash of an interior node: `SHA-256(salt ‖ 0x01 ‖ left ‖ right)`.
fn hash_node(salt: &[u8; 32], left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update([0x01]);
    h.update(left);
    h.update(right);
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

/// A built Merkle tree over an image's blocks. Keeps every level so it can emit
/// a proof for any block.
pub struct VerityTree {
    pub salt: [u8; 32],
    pub block_size: usize,
    pub block_count: usize,
    /// levels[0] = leaf hashes; the last level is the single root.
    levels: Vec<Vec<[u8; 32]>>,
}

impl VerityTree {
    /// Build the tree over `data`, split into `block_size` blocks (the final
    /// block is zero-padded). `salt` binds the tree to this image.
    pub fn build(data: &[u8], block_size: usize, salt: [u8; 32]) -> VerityTree {
        assert!(block_size > 0);
        let block_count = data.len().div_ceil(block_size).max(1);
        let mut leaves = Vec::with_capacity(block_count);
        for i in 0..block_count {
            let start = i * block_size;
            let end = (start + block_size).min(data.len());
            let mut block = Vec::with_capacity(block_size);
            block.extend_from_slice(&data[start.min(data.len())..end]);
            block.resize(block_size, 0); // zero-pad the tail block
            leaves.push(hash_block(&salt, &block));
        }
        // Build interior levels until a single root remains. Odd nodes are
        // promoted (hashed against themselves) so the tree is well-defined.
        let mut levels = alloc::vec![leaves];
        while levels.last().unwrap().len() > 1 {
            let cur = levels.last().unwrap();
            let mut next = Vec::with_capacity(cur.len().div_ceil(2));
            let mut i = 0;
            while i < cur.len() {
                let left = cur[i];
                let right = if i + 1 < cur.len() { cur[i + 1] } else { cur[i] };
                next.push(hash_node(&salt, &left, &right));
                i += 2;
            }
            levels.push(next);
        }
        VerityTree { salt, block_size, block_count, levels }
    }

    /// The signed-into-the-manifest root hash.
    pub fn root(&self) -> [u8; 32] {
        *self.levels.last().unwrap().first().unwrap()
    }

    /// A Merkle proof (sibling hashes bottom-up) for block `index`.
    pub fn proof(&self, index: usize) -> Vec<[u8; 32]> {
        let mut proof = Vec::new();
        let mut idx = index;
        for level in &self.levels[..self.levels.len() - 1] {
            let sib = if idx.is_multiple_of(2) {
                if idx + 1 < level.len() {
                    level[idx + 1]
                } else {
                    level[idx] // promoted (self-paired)
                }
            } else {
                level[idx - 1]
            };
            proof.push(sib);
            idx /= 2;
        }
        proof
    }
}

/// Verify a single `block` at `index` against the trusted `root` using its
/// Merkle `proof`. Returns false on any mismatch (tampered block or wrong image).
pub fn verify_block(root: &[u8; 32], salt: &[u8; 32], index: usize, block: &[u8], proof: &[[u8; 32]]) -> bool {
    let mut h = hash_block(salt, block);
    let mut idx = index;
    for sib in proof {
        h = if idx.is_multiple_of(2) { hash_node(salt, &h, sib) } else { hash_node(salt, sib, &h) };
        idx /= 2;
    }
    &h == root
}

// ── The signed manifest ─────────────────────────────────────────────────────
/// A verity manifest: the image's root hash + geometry, signed by the release
/// key. The system trusts a slot only if this signature verifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub root: [u8; 32],
    pub salt: [u8; 32],
    pub block_size: u32,
    pub block_count: u32,
}

impl Manifest {
    pub fn of(tree: &VerityTree) -> Manifest {
        Manifest {
            root: tree.root(),
            salt: tree.salt,
            block_size: tree.block_size as u32,
            block_count: tree.block_count as u32,
        }
    }

    /// The canonical to-be-signed bytes (domain-separated).
    pub fn tbs(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(80);
        b.extend_from_slice(b"EuroVerity-manifest-v1\0");
        b.extend_from_slice(&self.root);
        b.extend_from_slice(&self.salt);
        b.extend_from_slice(&self.block_size.to_le_bytes());
        b.extend_from_slice(&self.block_count.to_le_bytes());
        b
    }

    /// Sign the manifest with the release key → a 64-byte Ed25519 signature.
    pub fn sign(&self, key: &SigningKey) -> [u8; 64] {
        key.sign(&self.tbs()).to_bytes()
    }

    /// Verify the manifest signature against the release public key.
    pub fn verify(&self, pubkey: &[u8; 32], sig: &[u8; 64]) -> bool {
        let vk = match VerifyingKey::from_bytes(pubkey) {
            Ok(v) => v,
            Err(_) => return false,
        };
        vk.verify(&self.tbs(), &Signature::from_bytes(sig)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        (0..10_000u32).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn every_block_verifies_against_the_root() {
        let data = image();
        let t = VerityTree::build(&data, 512, [0x5A; 32]);
        let root = t.root();
        for i in 0..t.block_count {
            let start = i * 512;
            let end = (start + 512).min(data.len());
            let mut block = data[start..end].to_vec();
            block.resize(512, 0);
            assert!(verify_block(&root, &t.salt, i, &block, &t.proof(i)), "block {i}");
        }
    }

    #[test]
    fn a_tampered_block_fails() {
        let data = image();
        let t = VerityTree::build(&data, 512, [0x5A; 32]);
        let root = t.root();
        let mut block = data[512..1024].to_vec();
        block[0] ^= 0x01; // flip one bit
        assert!(!verify_block(&root, &t.salt, 1, &block, &t.proof(1)));
    }

    #[test]
    fn root_changes_if_image_changes() {
        let a = VerityTree::build(&image(), 512, [1; 32]).root();
        let mut d2 = image();
        d2[9999] ^= 0xFF;
        let b = VerityTree::build(&d2, 512, [1; 32]).root();
        assert_ne!(a, b);
    }

    #[test]
    fn manifest_signature_roundtrip_and_rejection() {
        let t = VerityTree::build(&image(), 1024, [7; 32]);
        let m = Manifest::of(&t);
        let key = SigningKey::from_bytes(&[0x11; 32]);
        let sig = m.sign(&key);
        assert!(m.verify(&key.verifying_key().to_bytes(), &sig));
        // A different signer is rejected.
        let other = SigningKey::from_bytes(&[0x22; 32]).verifying_key().to_bytes();
        assert!(!m.verify(&other, &sig));
        // A manifest with a tampered root is rejected under the real signature.
        let mut m2 = m.clone();
        m2.root[0] ^= 0x01;
        assert!(!m2.verify(&key.verifying_key().to_bytes(), &sig));
    }
}
