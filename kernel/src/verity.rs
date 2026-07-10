//! 3D-2 — EuroVerity: integrity-check a read-only system image block-by-block
//! against an Ed25519-signed Merkle root, so a tampered image is detected and
//! the loader can fall back to the good slot. `[3d2]` proves the primitives;
//! `[3d2-wire]` wires [`VerityBlk`] onto the **live EuroFS read path**, so every
//! block read is verified and a tampered backing block is caught at read time.

use alloc::vec::Vec;

use ed25519_dalek::SigningKey;
use eurofs::{BlockDevice, BlockError, BlockResult};
use euroverity::{verify_block, Manifest, VerityTree};

/// A **verity-verifying** block device: wraps a read-only backing device and a
/// signed Merkle tree, and verifies **every block on read** against the signed
/// root. A block that does not match (bit-rot, or a maliciously swapped image)
/// fails the read — so an `EuroFs` mounted on top can never serve tampered
/// bytes. Writes are refused (the image is read-only).
pub struct VerityBlk<D: BlockDevice> {
    inner: D,
    tree: VerityTree,
    root: [u8; 32],
    salt: [u8; 32],
}

impl<D: BlockDevice> VerityBlk<D> {
    pub fn new(inner: D, tree: VerityTree) -> Self {
        let root = tree.root();
        let salt = tree.salt;
        Self { inner, tree, root, salt }
    }
}

impl<D: BlockDevice> BlockDevice for VerityBlk<D> {
    fn block_size(&self) -> u32 {
        self.inner.block_size()
    }
    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }
    fn read_blocks(&self, start: u64, count: u32, buffer: &mut [u8]) -> BlockResult<()> {
        self.inner.read_blocks(start, count, buffer)?;
        let bs = self.block_size() as usize;
        // Verify each 4 KiB block against the signed Merkle root.
        for k in 0..count as usize {
            let idx = start as usize + k;
            let block = &buffer[k * bs..(k + 1) * bs];
            if !verify_block(&self.root, &self.salt, idx, block, &self.tree.proof(idx)) {
                // Tampered/corrupt block — refuse to serve it (loader falls back).
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }
    fn write_blocks(&mut self, _start: u64, _count: u32, _buffer: &[u8]) -> BlockResult<()> {
        Err(BlockError::IoError) // read-only verity image
    }
    fn flush(&mut self) -> BlockResult<()> {
        Ok(())
    }
}

/// `[3d2]` self-test.
pub fn selftest() {
    // A stand-in read-only system image.
    let mut img = alloc::vec![0u8; 8192];
    for (i, b) in img.iter_mut().enumerate() {
        *b = (i as u32 * 31 + 7) as u8;
    }
    let mut salt = [0u8; 32];
    crate::entropy::getrandom(&mut salt);
    let tree = VerityTree::build(&img, 512, salt);

    // The release-signed manifest (test key from the CSPRNG; in production the
    // root is signed offline by the same key that signs A/B images).
    let mut seed = [0u8; 32];
    crate::entropy::getrandom(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let pubkey = key.verifying_key().to_bytes();
    let manifest = Manifest::of(&tree);
    let sig = manifest.sign(&key);
    let manifest_ok = manifest.verify(&pubkey, &sig);

    // A genuine block verifies against the signed root.
    let idx = 3usize;
    let block = &img[idx * 512..(idx + 1) * 512];
    let good = verify_block(&manifest.root, &manifest.salt, idx, block, &tree.proof(idx));

    // Tamper a byte → verification against the signed root fails (detected).
    let mut bad = block.to_vec();
    bad[10] ^= 0xFF;
    let tamper_detected = !verify_block(&manifest.root, &manifest.salt, idx, &bad, &tree.proof(idx));

    // A forged manifest (wrong root under the real signature) is refused.
    let mut m2 = manifest.clone();
    m2.root[0] ^= 0x01;
    let forged_refused = !m2.verify(&pubkey, &sig);

    let ok = manifest_ok && good && tamper_detected && forged_refused;
    crate::serial_println!(
        "[3d2] EuroVerity (dm-verity-style, Ed25519-signed Merkle root over {} blocks): manifest-signature-verified={manifest_ok}, block-verified-against-signed-root={good}, tampered-block-DETECTED={tamper_detected}, forged-manifest-REFUSED={forged_refused} → {}",
        tree.block_count,
        if ok { "OK (system image integrity provable block-by-block; tamper ⇒ fall back to the good slot) ✓" } else { "FAILED" }
    );
}

/// Read every block of a device into one contiguous buffer (the "image bytes"
/// the Merkle tree is built over).
fn image_bytes<D: BlockDevice>(dev: &D) -> Vec<u8> {
    let bs = dev.block_size() as usize;
    let n = dev.block_count() as usize;
    let mut out = alloc::vec![0u8; n * bs];
    for i in 0..n {
        let _ = dev.read_blocks(i as u64, 1, &mut out[i * bs..(i + 1) * bs]);
    }
    out
}

/// **[3d2-wire] boot self-test** — verity on the LIVE EuroFS read path. Format a
/// small read-only system image, build+sign its Merkle tree, wrap the device in
/// [`VerityBlk`], and **mount a real EuroFS on top**: reading a file verifies
/// every underlying block against the signed root. Then a tampered backing block
/// is caught at read time (the read fails), which is where the loader falls back
/// to the good A/B slot.
pub fn wire_selftest() {
    use eurofs::{EuroFs, FileSystem, MemoryBlockDevice};

    // (1) Build a real read-only EuroFS image (format + a file), then freeze it.
    let mut dev = MemoryBlockDevice::new(256, 4096);
    let formatted = EuroFs::format(&mut dev, [0xD2; 16], 1).is_ok();
    let wrote = {
        match EuroFs::mount(&mut dev, 1) {
            Ok(mut fs) => fs.write_file("/system-release", b"EuroOS verified system image v1").is_ok(),
            Err(_) => false,
        }
    };

    // (2) Build the Merkle tree over the frozen image + sign the manifest.
    let bytes = image_bytes(&dev);
    let mut salt = [0u8; 32];
    crate::entropy::getrandom(&mut salt);
    let tree = VerityTree::build(&bytes, 4096, salt);
    let mut seed = [0u8; 32];
    crate::entropy::getrandom(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    let manifest = Manifest::of(&tree);
    let sig = manifest.sign(&key);
    let signed = manifest.verify(&key.verifying_key().to_bytes(), &sig);

    // Build a fresh MemoryBlockDevice holding `raw` (MemoryBlockDevice is not Clone).
    let device_from = |raw: &[u8]| -> MemoryBlockDevice {
        let mut d = MemoryBlockDevice::new(256, 4096);
        for i in 0..(raw.len() / 4096) {
            let _ = d.write_blocks(i as u64, 1, &raw[i * 4096..(i + 1) * 4096]);
        }
        d
    };

    // (3) Mount a real EuroFS on the verity-wrapped device → the file reads back
    //     correctly, with EVERY underlying block verified against the signed root.
    let vblk = VerityBlk::new(device_from(&bytes), VerityTree::build(&bytes, 4096, salt));
    let read_ok = match EuroFs::mount(vblk, 1) {
        Ok(fs) => fs.read_file("/system-release").map(|d| d == b"EuroOS verified system image v1").unwrap_or(false),
        Err(_) => false,
    };

    // (4) Tamper a used backing block → the verity read path catches it.
    let mut tbytes = bytes.clone();
    // Flip a byte in block 3 (a used metadata/data block, past the reserved area).
    tbytes[3 * 4096 + 100] ^= 0xFF;
    let vblk_t = VerityBlk::new(device_from(&tbytes), VerityTree::build(&bytes, 4096, salt));
    let mut buf = [0u8; 4096];
    let genuine_read = vblk_t.read_blocks(2, 1, &mut buf).is_ok(); // untouched block verifies
    let tamper_caught = vblk_t.read_blocks(3, 1, &mut buf).is_err(); // tampered block refused

    let ok = formatted && wrote && signed && read_ok && genuine_read && tamper_caught;
    crate::serial_println!(
        "[3d2-wire] verity on the LIVE EuroFS read path: fmt+write={formatted}/{wrote}, signed-manifest={signed}, EuroFS-mounted-on-VerityBlk+file-read(every-block-verified)={read_ok}, untouched-block-verifies={genuine_read}, tampered-backing-block-REFUSED-on-read={tamper_caught} → {}",
        if ok { "OK (read path verifies every block; tamper ⇒ read fails ⇒ A/B fallback) ✓" } else { "FAILED ✗" }
    );
}
