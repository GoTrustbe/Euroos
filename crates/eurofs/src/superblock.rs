//! EuroFS on-disk superblock (Track 2, Phase 2).
//!
//! 512 bytes, little-endian (native on x86-64; explicit conversions keep the
//! later ARM64 port correct). Contains an XXH3 checksum over all fields before
//! the checksum itself, and is written redundantly (block 1 + backup block 2).
//!
//! `#[repr(C, packed)]`: fields may not be aligned. We NEVER take a reference to
//! a field; we copy Copy fields into locals and (de)serialize via
//! `read_unaligned`. This is the most common mistake with on-disk structs —
//! explicitly avoided here.

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
/// First blocks (boot, super, backup, checkpoint zone, b-tree roots).
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
    /// New superblock for a freshly formatted volume.
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

    /// Serialize to 512 bytes (raw on-disk representation).
    pub fn to_bytes(&self) -> [u8; 512] {
        let mut out = [0u8; 512];
        // SAFETY: repr(C, packed), size == 512; we read `self` as bytes.
        let raw = unsafe {
            core::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>())
        };
        out.copy_from_slice(raw);
        out
    }

    /// Deserialize from 512 bytes (without validation).
    pub fn from_bytes(buf: &[u8; 512]) -> Self {
        // SAFETY: every bit pattern is a valid EuroFsSuperblock (all fields
        // are POD), and we read unaligned from the buffer.
        unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Self) }
    }

    /// XXH3 over all bytes before the checksum field (checksum + padding excl.).
    pub fn compute_checksum(&self) -> u64 {
        let bytes = self.to_bytes();
        let end = offset_of!(EuroFsSuperblock, checksum);
        xxh3_64(&bytes[..end])
    }

    /// Full validation: magic, version, block size and checksum.
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

    /// Read + validate a single superblock slot (None = absent/corrupt).
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

    /// A/B COMMIT of the superblock (S7 crash consistency — fix for the torn-write race).
    ///
    /// The superblock exists in TWO slots with a GENERATION NUMBER (`checkpoint_id`).
    /// We ALWAYS write the new superblock to the slot with the OLDEST generation,
    /// so the other slot retains the previous valid state: if this commit is cut
    /// short by a power failure halfway through (torn write), then at least one slot
    /// stays consistent and mount picks it. Ordering:
    ///   1) flush → all DATA/objmap blocks are durably on disk (I/O barrier),
    ///      so the superblock never lands before the blocks it refers to;
    ///   2) write the superblock to the oldest slot;
    ///   3) flush → the superblock is durable before the commit counts as succeeded.
    pub fn write_to<D: BlockDevice>(&self, dev: &mut D) -> BlockResult<()> {
        // 1) Barrier: make data + metadata durable first.
        dev.flush()?;

        // 2) Determine the target slot based on the generations in both slots.
        let ga = Self::read_slot(dev, SUPERBLOCK_BLOCK).map(|s| s.checkpoint_id);
        let gb = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK).map(|s| s.checkpoint_id);

        let bytes = self.to_bytes();
        let bs = dev.block_size() as usize;
        let mut block = alloc::vec![0u8; bs];
        block[..512].copy_from_slice(&bytes);

        match (ga, gb) {
            // FORMAT / both slots empty: establish BOTH slots — there is no previous
            // valid state to lose, so there is immediately a backup.
            (None, None) => {
                dev.write_blocks(SUPERBLOCK_BLOCK, 1, &block)?;
                dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &block)?;
            }
            // Steady-state: overwrite ONLY the oldest (or corrupt) slot; the
            // other — newer, valid — slot remains as a fallback, so a torn write
            // never destroys the last good state with this commit.
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

        // 3) Make the new superblock durable.
        dev.flush()
    }

    /// How many of the two superblock slots are currently INVALID (magic/checksum
    /// broken)? 0 = both intact, 1 = degraded but mountable (one valid copy
    /// left), 2 = both corrupt (unrecoverable).
    pub fn degraded_slots<D: BlockDevice>(dev: &D) -> u8 {
        let a = Self::read_slot(dev, SUPERBLOCK_BLOCK).is_some();
        let b = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK).is_some();
        (!a as u8) + (!b as u8)
    }

    /// SELF-HEALING of the A/B redundancy: if one slot is corrupt and the other
    /// valid, rewrite the corrupt slot from the valid copy (and flush).
    /// Returns the number of repaired slots (0 if there is nothing to heal: both
    /// valid, or both corrupt — then there is no good source). This is the repair
    /// counterpart to the torn-write protection: after a cut-short commit this
    /// restores the backup so the filesystem again has two valid copies.
    pub fn heal_slots<D: BlockDevice>(dev: &mut D) -> BlockResult<usize> {
        let a = Self::read_slot(dev, SUPERBLOCK_BLOCK);
        let b = Self::read_slot(dev, SUPERBLOCK_BACKUP_BLOCK);
        let (loc, bytes) = match (a, b) {
            (Some(valid), None) => (SUPERBLOCK_BACKUP_BLOCK, valid.to_bytes()),
            (None, Some(valid)) => (SUPERBLOCK_BLOCK, valid.to_bytes()),
            _ => return Ok(0), // both valid, or both corrupt → nothing (safe) to do
        };
        let bs = dev.block_size() as usize;
        let mut block = alloc::vec![0u8; bs];
        block[..512].copy_from_slice(&bytes);
        dev.write_blocks(loc, 1, &block)?;
        dev.flush()?;
        Ok(1)
    }

    /// Read + validate the superblock: pick the slot with the HIGHEST valid generation.
    /// If the newest is corrupt due to a torn write, this automatically falls back to
    /// the older, still-consistent slot.
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
        bytes[40] ^= 0xFF; // flip a byte in free_blocks
        let corrupt = EuroFsSuperblock::from_bytes(&bytes);
        assert!(!corrupt.is_valid(), "bit-flip must break checksum");
    }

    #[test]
    fn format_en_mount_via_device() {
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        let sb = EuroFsSuperblock::new_empty(1024, [9; 16], 100);
        sb.write_to(&mut dev).unwrap();
        // A/B commit flushes twice: a barrier (data durable first) + the superblock.
        assert_eq!(dev.flush_count, 2, "A/B commit: barrier flush + superblock flush");

        let mounted = EuroFsSuperblock::read_from(&dev).unwrap();
        assert!(mounted.is_valid());
        let blocks = mounted.total_blocks;
        assert_eq!(blocks, 1024);
    }

    #[test]
    fn ab_torn_write_valt_terug_op_vorige_generatie() {
        // Prove the A/B guarantee: a torn write to the NEWEST superblock slot must not
        // destroy the previous valid generation — mount recovers to that state.
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        // Generation 1 (format) -> both slots.
        let mut sb = EuroFsSuperblock::new_empty(1024, [5; 16], 100);
        sb.checkpoint_id = 1;
        sb.checksum = sb.compute_checksum();
        sb.write_to(&mut dev).unwrap();
        // Commit generation 2 -> goes to the oldest slot; the other keeps gen 1.
        sb.checkpoint_id = 2;
        sb.checksum = sb.compute_checksum();
        sb.write_to(&mut dev).unwrap();
        let g2 = EuroFsSuperblock::read_from(&dev).unwrap().checkpoint_id;
        assert_eq!(g2, 2);

        // Simulate a TORN write of the next commit: the slot that carried gen 2
        // becomes corrupt. (gen 2 was in the slot that was the oldest at commit 2.)
        let ga = EuroFsSuperblock::read_slot(&dev, SUPERBLOCK_BLOCK).map(|s| s.checkpoint_id);
        let newest = if ga == Some(2) { SUPERBLOCK_BLOCK } else { SUPERBLOCK_BACKUP_BLOCK };
        dev.write_blocks(newest, 1, &alloc::vec![0xCDu8; 4096]).unwrap();

        // Mount falls back to the previous valid generation (1) — no loss of a
        // CONSISTENT state, only of the half-written commit.
        let mounted = EuroFsSuperblock::read_from(&dev).unwrap();
        let gen = mounted.checkpoint_id;
        let blocks = mounted.total_blocks;
        assert_eq!(gen, 1, "must fall back to the previous generation");
        assert_eq!(blocks, 1024);
    }

    #[test]
    fn valt_terug_op_backup_bij_corrupt_primair() {
        let mut dev = MemoryBlockDevice::new(1024, 4096);
        let sb = EuroFsSuperblock::new_empty(1024, [3; 16], 100);
        sb.write_to(&mut dev).unwrap();

        // Destroy the primary superblock (block 1) completely.
        let zero = alloc::vec![0xABu8; 4096];
        dev.write_blocks(SUPERBLOCK_BLOCK, 1, &zero).unwrap();

        // Mount must succeed via the backup on block 2.
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
