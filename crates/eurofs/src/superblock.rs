//! EuroFS on-disk superblok (Track 2, Fase 2).
//!
//! 512 bytes, little-endian (native op x86-64; expliciete conversies houden de
//! latere ARM64-port correct). Bevat een XXH3-checksum over alle velden vóór
//! de checksum zelf, en wordt redundant weggeschreven (blok 1 + back-up blok 2).
//!
//! `#[repr(C, packed)]`: velden liggen mogelijk niet uitgelijnd. We nemen NOOIT
//! een referentie naar een veld; we kopiëren Copy-velden naar locals en
//! (de)serialiseren via `read_unaligned`. Dit is de meest gemaakte fout met
//! on-disk structs — hier expliciet vermeden.

use core::mem::{offset_of, size_of};

use crate::block::{BlockDevice, BlockResult};
use crate::checksum::xxh3_64;
use crate::fs::{FsError, FsResult};

pub const EUROFS_MAGIC: [u8; 8] = *b"EUROFS01";
pub const EUROFS_VERSION_MAJOR: u16 = 0;
pub const EUROFS_VERSION_MINOR: u16 = 1;
pub const SUPERBLOCK_BLOCK: u64 = 1;
pub const SUPERBLOCK_BACKUP_BLOCK: u64 = 2;
pub const DEFAULT_BLOCK_SIZE: u32 = 4096;
/// Eerste blokken (boot, super, back-up, checkpoint-zone, b-tree roots).
pub const RESERVED_BLOCKS: u64 = 16;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EuroFsSuperblock {
    pub magic: [u8; 8],
    pub version_major: u16,
    pub version_minor: u16,
    pub uuid: [u8; 16],
    pub block_size: u32,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub reserved_blocks: u64,
    pub created_at: u64,
    pub last_mounted: u64,
    pub last_written: u64,
    pub checkpoint_id: u64,
    pub checkpoint_block: u64,
    pub object_map_root: u64,
    pub extent_tree_root: u64,
    pub root_dir_oid: u64,
    pub encryption: u8,
    pub kdf_params: [u8; 64],
    pub wrapped_key: [u8; 48],
    pub checksum: u64,
    pub _padding: [u8; 271],
}

const _: () = assert!(size_of::<EuroFsSuperblock>() == 512);

impl EuroFsSuperblock {
    /// Nieuw superblok voor een vers geformatteerd volume.
    pub fn new_empty(total_blocks: u64, uuid: [u8; 16], created_at: u64) -> Self {
        let mut sb = EuroFsSuperblock {
            magic: EUROFS_MAGIC,
            version_major: EUROFS_VERSION_MAJOR,
            version_minor: EUROFS_VERSION_MINOR,
            uuid,
            block_size: DEFAULT_BLOCK_SIZE,
            total_blocks,
            free_blocks: total_blocks.saturating_sub(RESERVED_BLOCKS),
            reserved_blocks: RESERVED_BLOCKS,
            created_at,
            last_mounted: 0,
            last_written: created_at,
            checkpoint_id: 1,
            checkpoint_block: 3,
            object_map_root: 11,
            extent_tree_root: 12,
            root_dir_oid: 1,
            encryption: 0,
            kdf_params: [0; 64],
            wrapped_key: [0; 48],
            checksum: 0,
            _padding: [0; 271],
        };
        sb.checksum = sb.compute_checksum();
        sb
    }

    /// Serialiseer naar 512 bytes (raw on-disk representatie).
    pub fn to_bytes(&self) -> [u8; 512] {
        let mut out = [0u8; 512];
        // SAFETY: repr(C, packed), size == 512; we lezen `self` als bytes.
        let raw = unsafe {
            core::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>())
        };
        out.copy_from_slice(raw);
        out
    }

    /// Deserialiseer uit 512 bytes (zonder validatie).
    pub fn from_bytes(buf: &[u8; 512]) -> Self {
        // SAFETY: elke bitpatroon is een geldige EuroFsSuperblock (alle velden
        // zijn POD), en we lezen unaligned uit de buffer.
        unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) }
    }

    /// XXH3 over alle bytes vóór het checksum-veld (checksum + padding excl.).
    pub fn compute_checksum(&self) -> u64 {
        let bytes = self.to_bytes();
        let end = offset_of!(EuroFsSuperblock, checksum);
        xxh3_64(&bytes[..end])
    }

    /// Volledige validatie: magic, versie, blokgrootte én checksum.
    pub fn is_valid(&self) -> bool {
        let magic = self.magic;
        let major = self.version_major;
        let bs = self.block_size;
        let stored = self.checksum;
        magic == EUROFS_MAGIC
            && major == EUROFS_VERSION_MAJOR
            && bs >= 512
            && bs.is_power_of_two()
            && stored == self.compute_checksum()
    }

    /// Lees + valideer één superblok-slot (None = afwezig/corrupt).
    fn read_slot<D: BlockDevice>(dev: &D, loc: u64) -> Option<Self> {
        let bs = dev.block_size() as usize;
        let mut block = alloc::vec![0u8; bs];
        if dev.read_blocks(loc, 1, &mut block).is_err() {
            return None;
        }
        let mut raw = [0u8; 512];
        raw.copy_from_slice(&block[..512]);
        let sb = Self::from_bytes(&raw);
        if sb.is_valid() {
            Some(sb)
        } else {
            None
        }
    }

    /// A/B-COMMIT van de superblok (S7 crash-consistentie — fix voor de torn-write-race).
    ///
    /// De superblok bestaat in TWEE slots met een GENERATIENUMMER (`checkpoint_id`).
    /// We schrijven de nieuwe superblok ALTIJD naar het slot met de OUDSTE generatie,
    /// zodat het andere slot de vorige geldige staat behoudt: wordt deze commit door
    /// een stroomuitval halverwege afgekapt (torn write), dan blijft minstens één slot
    /// consistent en mount kiest die. Ordening:
    ///   1) flush → alle DATA/objmap-blokken staan durabel op schijf (I/O-barrier),
    ///      zodat de superblok nooit vóór de blokken landt waarnaar ze verwijst;
    ///   2) schrijf de superblok naar het oudste slot;
    ///   3) flush → de superblok is durabel vóór de commit als geslaagd geldt.
    pub fn write_to<D: BlockDevice>(&self, dev: &mut D) -> BlockResult<()> {
        // 1) Barrier: data + metadata eerst durabel maken.
        dev.flush()?;

        // 2) Bepaal het doelslot op basis van de generaties in beide slots.
        let ga = Self::read_slot(dev, SUPERBLOCK_BLOCK).map(|s| s.checkpoint_id);
        let gb = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK).map(|s| s.checkpoint_id);

        let bytes = self.to_bytes();
        let bs = dev.block_size() as usize;
        let mut block = alloc::vec![0u8; bs];
        block[..512].copy_from_slice(&bytes);

        match (ga, gb) {
            // FORMAT / beide slots leeg: vestig BEIDE slots — er is geen vorige
            // geldige staat om te verliezen, dus er is meteen een back-up.
            (None, None) => {
                dev.write_blocks(SUPERBLOCK_BLOCK, 1, &block)?;
                dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &block)?;
            }
            // Steady-state: overschrijf ALLEEN het oudste (of corrupte) slot; het
            // andere — nieuwere, geldige — slot blijft als fallback staan, zodat een
            // torn write deze commit nooit de laatste goede staat vernietigt.
            _ => {
                let target = match (ga, gb) {
                    (None, _) => SUPERBLOCK_BLOCK,
                    (_, None) => SUPERBLOCK_BACKUP_BLOCK,
                    (Some(a), Some(b)) if a <= b => SUPERBLOCK_BLOCK,
                    _ => SUPERBLOCK_BACKUP_BLOCK,
                };
                dev.write_blocks(target, 1, &block)?;
            }
        }

        // 3) De nieuwe superblok durabel maken.
        dev.flush()
    }

    /// Hoeveel van de twee superblok-slots zijn momenteel ONGELDIG (magic/checksum
    /// kapot)? 0 = beide intact, 1 = gedegradeerd maar mountbaar (één geldige kopie
    /// over), 2 = beide corrupt (niet te helen).
    pub fn degraded_slots<D: BlockDevice>(dev: &D) -> u8 {
        let a = Self::read_slot(dev, SUPERBLOCK_BLOCK).is_some();
        let b = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK).is_some();
        (!a as u8) + (!b as u8)
    }

    /// ZELF-HELING van de A/B-redundantie: staat één slot corrupt en het andere
    /// geldig, herschrijf dan het corrupte slot uit de geldige kopie (en flush).
    /// Geeft het aantal herstelde slots terug (0 als er niets te helen valt: beide
    /// geldig, of beide corrupt — dan is er geen goede bron). Dit is de reparatie-
    /// tegenhanger van de torn-write-bescherming: na een afgekapte commit herstelt
    /// dit de back-up zodat het filesysteem weer twee geldige kopieën heeft.
    pub fn heal_slots<D: BlockDevice>(dev: &mut D) -> BlockResult<usize> {
        let a = Self::read_slot(dev, SUPERBLOCK_BLOCK);
        let b = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK);
        let (loc, bytes) = match (a, b) {
            (Some(valid), None) => (SUPERBLOCK_BACKUP_BLOCK, valid.to_bytes()),
            (None, Some(valid)) => (SUPERBLOCK_BLOCK, valid.to_bytes()),
            _ => return Ok(0), // beide geldig, of beide corrupt → niets (veilig) te doen
        };
        let bs = dev.block_size() as usize;
        let mut block = alloc::vec![0u8; bs];
        block[..512].copy_from_slice(&bytes);
        dev.write_blocks(loc, 1, &block)?;
        dev.flush()?;
        Ok(1)
    }

    /// Lees + valideer de superblok: kies het slot met de HOOGSTE geldige generatie.
    /// Is de nieuwste door een torn write corrupt, dan valt dit automatisch terug op
    /// het oudere, nog-consistente slot.
    pub fn read_from<D: BlockDevice>(dev: &D) -> FsResult<Self> {
        let mut best: Option<Self> = None;
        for loc in [SUPERBLOCK_BLOCK, SUPERBLOCK_BACKUP_BLOCK] {
            if let Some(sb) = Self::read_slot(dev, loc) {
                if best.as_ref().is_none_or(|b| sb.checkpoint_id > b.checkpoint_id) {
                    best = Some(sb);
                }
            }
        }
        best.ok_or(FsError::Corruption)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemoryBlockDevice;

    #[test]
    fn precies_512_bytes() {
        assert_eq!(size_of::<EuroFsSuperblock>(), 512);
    }

    #[test]
    fn roundtrip_bytes() {
        let sb = EuroFsSuperblock::new_empty(1024, [7; 16], 1_700_000_000);
        let bytes = sb.to_bytes();
        let back = EuroFsSuperblock::from_bytes(&bytes);
        assert!(back.is_valid());
        let (a, b) = (back.total_blocks, sb.total_blocks);
        assert_eq!(a, b);
    }

    #[test]
    fn checksum_detecteert_corruptie() {
        let sb = EuroFsSuperblock::new_empty(1024, [1; 16], 42);
        let mut bytes = sb.to_bytes();
        bytes[40] ^= 0xFF; // flip een byte in free_blocks
        let corrupt = EuroFsSuperblock::from_bytes(&bytes);
        assert!(!corrupt.is_valid(), "bit-flip moet checksum breken");
    }

    #[test]
    fn format_en_mount_via_device() {
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        let sb = EuroFsSuperblock::new_empty(1024, [9; 16], 100);
        sb.write_to(&mut dev).unwrap();
        // A/B-commit flusht twee keer: een barrier (data eerst durabel) + de superblok.
        assert_eq!(dev.flush_count, 2, "A/B-commit: barrier-flush + superblok-flush");

        let mounted = EuroFsSuperblock::read_from(&dev).unwrap();
        assert!(mounted.is_valid());
        let blocks = mounted.total_blocks;
        assert_eq!(blocks, 1024);
    }

    #[test]
    fn ab_torn_write_valt_terug_op_vorige_generatie() {
        // Bewijs de A/B-garantie: een torn write op de NIEUWSTE superblok-slot mag de
        // vorige geldige generatie niet vernietigen — mount herstelt naar die staat.
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        // Generatie 1 (format) -> beide slots.
        let mut sb = EuroFsSuperblock::new_empty(1024, [5; 16], 100);
        sb.checkpoint_id = 1;
        sb.checksum = sb.compute_checksum();
        sb.write_to(&mut dev).unwrap();
        // Commit generatie 2 -> gaat naar het oudste slot; het andere houdt gen 1.
        sb.checkpoint_id = 2;
        sb.checksum = sb.compute_checksum();
        sb.write_to(&mut dev).unwrap();
        let g2 = EuroFsSuperblock::read_from(&dev).unwrap().checkpoint_id;
        assert_eq!(g2, 2);

        // Simuleer een TORN write van de volgende commit: het slot dat gen 2 droeg
        // raakt corrupt. (gen 2 zat in het slot dat bij commit 2 het oudste was.)
        let ga = EuroFsSuperblock::read_slot(&dev, SUPERBLOCK_BLOCK).map(|s| s.checkpoint_id);
        let newest = if ga == Some(2) { SUPERBLOCK_BLOCK } else { SUPERBLOCK_BACKUP_BLOCK };
        dev.write_blocks(newest, 1, &alloc::vec![0xCDu8; 4096]).unwrap();

        // Mount valt terug op de vorige geldige generatie (1) — geen verlies van een
        // CONSISTENTE staat, alleen van de halfgeschreven commit.
        let mounted = EuroFsSuperblock::read_from(&dev).unwrap();
        let gen = mounted.checkpoint_id;
        let blocks = mounted.total_blocks;
        assert_eq!(gen, 1, "moet terugvallen op de vorige generatie");
        assert_eq!(blocks, 1024);
    }

    #[test]
    fn valt_terug_op_backup_bij_corrupt_primair() {
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        let sb = EuroFsSuperblock::new_empty(1024, [3; 16], 100);
        sb.write_to(&mut dev).unwrap();

        // Verniel het primaire superblok (blok 1) volledig.
        let zero = alloc::vec![0xABu8; 4096];
        dev.write_blocks(SUPERBLOCK_BLOCK, 1, &zero).unwrap();

        // Mount moet slagen via de back-up op blok 2.
        let mounted = EuroFsSuperblock::read_from(&dev).unwrap();
        let blocks = mounted.total_blocks;
        assert_eq!(blocks, 1024);
    }

    #[test]
    fn beide_kapot_geeft_corruption() {
        let mut dev = MemoryBlockDevice::new(64, 4096);
        let junk = alloc::vec![0xFFu8; 4096];
        dev.write_blocks(SUPERBLOCK_BLOCK, 1, &junk).unwrap();
        dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &junk).unwrap();
        assert_eq!(
            EuroFsSuperblock::read_from(&dev).err(),
            Some(FsError::Corruption)
        );
    }
}
