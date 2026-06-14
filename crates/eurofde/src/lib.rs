//! EuroFDE — **full-disk encryption** as a transparent block layer (plan K3).
//!
//! A sovereign OS encrypts the disk so that data-at-rest is protected on
//! loss/theft. EuroFDE encrypts **per block**, length-preserving, with the
//! **ChaCha20** stream cipher (a European/IETF standard, no dependency on
//! AES hardware): the nonce is derived from the block number (LBA), so that the same
//! plaintext block at different LBAs yields different ciphertext. The 256-bit
//! key comes (with K3 complete) from the **TPM** ([`eurotpm`]) — preferably
//! sealed to the boot-PCR state, so that the disk only decrypts on an
//! untampered system.
//!
//! ## Known limitation (audit #10): nonce = f(LBA), not per write action
//!
//! Because the nonce depends only on (volume-salt, LBA), writing TWICE
//! to the SAME physical block reuses the same keystream. An attacker who intercepts
//! both ciphertext versions can XOR them into `P₁ ⊕ P₂` (classic
//! "two-time pad"). This is inherent to length-preserving stream-cipher FDE: there is
//! no room to store a fresh random IV per write.
//!
//! **Mitigation in this stack:** EuroFS is copy-on-write — a logical overwrite
//! usually allocates a NEW physical block instead of rewriting the same one, so
//! physical-LBA reuse with different content is rare in practice.
//! **Production upgrade path:** a wide-block mode — **Adiantum** (ChaCha-based,
//! no AES hardware needed) or XTS — which diffuses every write without extra storage.
//! Until then: document this; do NOT claim full IND-CPA per write.
//!
//! As [`EncryptedBlockDevice`] it wraps any [`eurofs::BlockDevice`] → the whole
//! EuroFS runs transparently on top of it. Pure `no_std` logic → host-tested.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;

use eurofs::{BlockDevice, BlockError, BlockResult};

/// An FDE key (256-bit ChaCha20 key + 32-bit volume-salt against
/// cross-volume nonce reuse).
#[derive(Clone)]
pub struct FdeKey {
    key: [u8; 32],
    salt: u32,
}

impl FdeKey {
    pub fn new(key: [u8; 32], salt: u32) -> Self {
        FdeKey { key, salt }
    }

    /// The 12-byte ChaCha20 nonce for block `lba`: [salt(4) | lba(8)]. Unique per
    /// (volume, block) — avoids keystream reuse BETWEEN different blocks.
    /// NOTE: NOT unique per write action; rewriting the same physical block
    /// reuses the keystream (see the module doc "Known limitation", audit #10).
    fn nonce(&self, lba: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0..4].copy_from_slice(&self.salt.to_le_bytes());
        n[4..12].copy_from_slice(&lba.to_le_bytes());
        n
    }

    /// Encrypt/decrypt `buf` in-place for block `lba` (ChaCha20 is a
    /// stream cipher → encrypt == decrypt: XOR with the same keystream).
    pub fn xcrypt_block(&self, lba: u64, buf: &mut [u8]) {
        let mut cipher = ChaCha20::new((&self.key).into(), (&self.nonce(lba)).into());
        cipher.apply_keystream(buf);
    }
}

/// A transparent FDE layer over a [`BlockDevice`]: writing encrypts, reading
/// decrypts — the overlying FS sees only plaintext, the disk only ciphertext.
pub struct EncryptedBlockDevice<D: BlockDevice> {
    inner: D,
    key: FdeKey,
}

impl<D: BlockDevice> EncryptedBlockDevice<D> {
    pub fn new(inner: D, key: FdeKey) -> Self {
        EncryptedBlockDevice { inner, key }
    }
}

impl<D: BlockDevice> BlockDevice for EncryptedBlockDevice<D> {
    fn block_size(&self) -> u32 {
        self.inner.block_size()
    }
    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }

    fn read_blocks(&self, start_block: u64, count: u32, buffer: &mut [u8]) -> BlockResult<()> {
        self.inner.read_blocks(start_block, count, buffer)?;
        let bs = self.block_size() as usize;
        if buffer.len() != count as usize * bs {
            return Err(BlockError::NotAligned);
        }
        for i in 0..count as u64 {
            let o = (i as usize) * bs;
            self.key.xcrypt_block(start_block + i, &mut buffer[o..o + bs]);
        }
        Ok(())
    }

    fn write_blocks(&mut self, start_block: u64, count: u32, buffer: &[u8]) -> BlockResult<()> {
        let bs = self.block_size() as usize;
        if buffer.len() != count as usize * bs {
            return Err(BlockError::NotAligned);
        }
        // Encrypt into a temporary buffer (the caller's plaintext stays intact).
        let mut enc = alloc::vec![0u8; buffer.len()];
        enc.copy_from_slice(buffer);
        for i in 0..count as u64 {
            let o = (i as usize) * bs;
            self.key.xcrypt_block(start_block + i, &mut enc[o..o + bs]);
        }
        self.inner.write_blocks(start_block, count, &enc)
    }

    fn flush(&mut self) -> BlockResult<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};

    #[test]
    fn block_roundtrips_and_is_position_dependent() {
        let key = FdeKey::new([7u8; 32], 0xABCD);
        let plain = [0x11u8; 64];
        let mut a = plain;
        key.xcrypt_block(5, &mut a);
        assert_ne!(a, plain); // encrypted ≠ plaintext
        // Decrypt (same XOR) → back to plaintext.
        let mut b = a;
        key.xcrypt_block(5, &mut b);
        assert_eq!(b, plain);
        // Same plaintext on a DIFFERENT block → different ciphertext (nonce = LBA).
        let mut c = plain;
        key.xcrypt_block(6, &mut c);
        assert_ne!(a, c);
    }

    #[test]
    fn disk_stores_ciphertext_not_plaintext() {
        let key = FdeKey::new([0x42u8; 32], 1);
        let mut enc = EncryptedBlockDevice::new(MemoryBlockDevice::new(64, 4096), key.clone());
        let plain = alloc::vec![0xCDu8; 4096];
        enc.write_blocks(10, 1, &plain).unwrap();
        // Read via the FDE layer → plaintext back.
        let mut back = alloc::vec![0u8; 4096];
        enc.read_blocks(10, 1, &mut back).unwrap();
        assert_eq!(back, plain);
        // But the UNDERLYING disk contains ciphertext (a wrong key yields garbage).
        let wrong = EncryptedBlockDevice::new(enc.inner, FdeKey::new([0u8; 32], 1));
        let mut garbage = alloc::vec![0u8; 4096];
        wrong.read_blocks(10, 1, &mut garbage).unwrap();
        assert_ne!(garbage, plain); // without the correct key: unreadable
    }

    #[test]
    fn eurofs_mounts_on_encrypted_volume() {
        // A real EuroFS on top of the encrypted block layer (transparent FDE).
        let key = FdeKey::new([0x5Au8; 32], 0x1234);
        let mut dev = EncryptedBlockDevice::new(MemoryBlockDevice::new(1024, 4096), key.clone());
        EuroFs::format(&mut dev, [9u8; 16], 1).unwrap();
        assert!(EuroFs::mount(&mut dev, 2).is_ok());
        // With a wrong key the same physical volume is NOT mountable.
        let raw = dev.inner;
        let mut wrong = EncryptedBlockDevice::new(raw, FdeKey::new([0u8; 32], 0x1234));
        assert!(EuroFs::mount(&mut wrong, 3).is_err());
    }
}
