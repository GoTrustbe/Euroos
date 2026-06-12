//! EuroFDE — **full-disk-encryptie** als transparante blok-laag (plan K3).
//!
//! Een soeverein OS versleutelt de schijf zodat data-at-rest beschermd is bij
//! verlies/diefstal. EuroFDE versleutelt **per blok**, lengte-behoudend, met het
//! **ChaCha20**-stroomcijfer (een Europese/IETF-standaard, geen afhankelijkheid van
//! AES-hardware): de nonce wordt afgeleid van het blok-nummer (LBA), zodat hetzelfde
//! plaintext-blok op verschillende LBA's verschillende ciphertext geeft. De 256-bit
//! sleutel komt (met K3 volledig) van de **TPM** ([`eurotpm`]) — bij voorkeur
//! gesealed aan de boot-PCR-toestand, zodat de schijf enkel ontsleutelt op een
//! niet-gemanipuleerd systeem.
//!
//! ## Bekende beperking (audit #10): nonce = f(LBA), niet per schrijf-actie
//!
//! Omdat de nonce enkel van (volume-salt, LBA) afhangt, hergebruikt twee KEER
//! schrijven naar HETZELFDE fysieke blok dezelfde keystream. Een aanvaller die
//! beide ciphertext-versies onderschept kan ze XOR-en tot `P₁ ⊕ P₂` (klassieke
//! "two-time pad"). Dit is inherent aan lengte-behoudende stroomcijfer-FDE: er is
//! geen ruimte om per schrijf een verse willekeurige IV op te slaan.
//!
//! **Mitigatie in deze stack:** EuroFS is copy-on-write — een logische overschrijving
//! alloceert doorgaans een NIEUW fysiek blok i.p.v. hetzelfde te herschrijven, dus
//! fysieke-LBA-hergebruik met andere inhoud is in de praktijk zeldzaam.
//! **Productie-upgradepad:** een wide-block-modus — **Adiantum** (ChaCha-gebaseerd,
//! geen AES-hardware nodig) of XTS — die elke schrijf diffundeert zonder extra opslag.
//! Tot dan: documenteer dit; claim GEEN volledige IND-CPA per schrijf.
//!
//! Als [`EncryptedBlockDevice`] wrapt het elk [`eurofs::BlockDevice`] → de hele
//! EuroFS draait er transparant bovenop. Pure `no_std`-logica → host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;

use eurofs::{BlockDevice, BlockError, BlockResult};

/// Een FDE-sleutel (256-bit ChaCha20-sleutel + 32-bit volume-salt tegen
/// cross-volume-nonce-hergebruik).
#[derive(Clone)]
pub struct FdeKey {
    key: [u8; 32],
    salt: u32,
}

impl FdeKey {
    pub fn new(key: [u8; 32], salt: u32) -> Self {
        FdeKey { key, salt }
    }

    /// De 12-byte ChaCha20-nonce voor blok `lba`: [salt(4) | lba(8)]. Uniek per
    /// (volume, blok) — vermijdt keystream-hergebruik TUSSEN verschillende blokken.
    /// LET OP: NIET uniek per schrijf-actie; herschrijven van hetzelfde fysieke blok
    /// hergebruikt de keystream (zie de module-doc "Bekende beperking", audit #10).
    fn nonce(&self, lba: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0..4].copy_from_slice(&self.salt.to_le_bytes());
        n[4..12].copy_from_slice(&lba.to_le_bytes());
        n
    }

    /// Versleutel/ontsleutel `buf` in-place voor blok `lba` (ChaCha20 is een
    /// stroomcijfer → encrypt == decrypt: XOR met dezelfde keystream).
    pub fn xcrypt_block(&self, lba: u64, buf: &mut [u8]) {
        let mut cipher = ChaCha20::new((&self.key).into(), (&self.nonce(lba)).into());
        cipher.apply_keystream(buf);
    }
}

/// Een transparante FDE-laag over een [`BlockDevice`]: schrijven versleutelt, lezen
/// ontsleutelt — de bovenliggende FS ziet enkel plaintext, de schijf enkel ciphertext.
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
        // Versleutel naar een tijdelijke buffer (de aanroeper z'n plaintext blijft heel).
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
        assert_ne!(a, plain); // versleuteld ≠ plaintext
        // Decrypt (zelfde XOR) → terug naar plaintext.
        let mut b = a;
        key.xcrypt_block(5, &mut b);
        assert_eq!(b, plain);
        // Zelfde plaintext op een ANDER blok → andere ciphertext (nonce = LBA).
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
        // Lees via de FDE-laag → plaintext terug.
        let mut back = alloc::vec![0u8; 4096];
        enc.read_blocks(10, 1, &mut back).unwrap();
        assert_eq!(back, plain);
        // Maar de ONDERLIGGENDE schijf bevat ciphertext (een verkeerde sleutel geeft rommel).
        let wrong = EncryptedBlockDevice::new(enc.inner, FdeKey::new([0u8; 32], 1));
        let mut garbage = alloc::vec![0u8; 4096];
        wrong.read_blocks(10, 1, &mut garbage).unwrap();
        assert_ne!(garbage, plain); // zonder de juiste sleutel: onleesbaar
    }

    #[test]
    fn eurofs_mounts_on_encrypted_volume() {
        // Een echte EuroFS bovenop de versleutelde blok-laag (transparante FDE).
        let key = FdeKey::new([0x5Au8; 32], 0x1234);
        let mut dev = EncryptedBlockDevice::new(MemoryBlockDevice::new(1024, 4096), key.clone());
        EuroFs::format(&mut dev, [9u8; 16], 1).unwrap();
        assert!(EuroFs::mount(&mut dev, 2).is_ok());
        // Met een verkeerde sleutel is hetzelfde fysieke volume NIET te mounten.
        let raw = dev.inner;
        let mut wrong = EncryptedBlockDevice::new(raw, FdeKey::new([0u8; 32], 0x1234));
        assert!(EuroFs::mount(&mut wrong, 3).is_err());
    }
}
