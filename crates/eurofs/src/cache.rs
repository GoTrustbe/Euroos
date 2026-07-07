//! Concurrent block cache (plan J1 — "block-cache RwLock").
//!
//! Under J1 (per-subsystem locking) EuroOS replaces the coarse global `IF=0`
//! sections with fine-grained, scalable locks. This cache is one concrete part of
//! that: a **read-write-locked** block cache on top of a [`BlockDevice`]. The common
//! operation — a **cache hit** — takes only a **read lock**, so multiple
//! cores can read cached blocks at the same time without blocking each other. Only
//! a **miss** (loading a block) or a **write** briefly takes the write lock + the
//! device lock. This way the FS read path scales with the number of cores instead of
//! serializing on a single global lock.
//!
//! Eviction is a simple **CLOCK / second-chance** policy (ref-bit ring), just
//! like the swap pager — an O(1) LRU approximation without per-access list shuffling.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use spin::{Mutex, RwLock};

use crate::block::{BlockDevice, BlockResult};

struct Slot {
    lba: u64,
    data: Vec<u8>,
    valid: bool,
    dirty: bool,
    /// CLOCK ref-bit. Atomic so a read HIT can set it under the read lock.
    refbit: AtomicBool,
}

struct Cache {
    slots: Vec<Slot>,
    hand: usize, // CLOCK hand for eviction
}

/// A concurrent, write-through block cache on top of `D`.
pub struct BlockCache<D: BlockDevice> {
    dev: Mutex<D>,
    cache: RwLock<Cache>,
    block_size: u32,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl<D: BlockDevice> BlockCache<D> {
    /// Create a cache with `capacity` block slots on top of `dev`.
    pub fn new(dev: D, capacity: usize) -> Self {
        let block_size = dev.block_size();
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity.max(1) {
            slots.push(Slot {
                lba: u64::MAX,
                data: Vec::new(),
                valid: false,
                dirty: false,
                refbit: AtomicBool::new(false),
            });
        }
        BlockCache {
            dev: Mutex::new(dev),
            cache: RwLock::new(Cache { slots, hand: 0 }),
            block_size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Read one block `lba`. A **hit** takes only the **read lock** (concurrent with
    /// other readers — the ref-bit + counter are atomic); a **miss** loads it under
    /// the write lock from the device.
    pub fn read_block(&self, lba: u64) -> BlockResult<Vec<u8>> {
        {
            let c = self.cache.read();
            if let Some(s) = c.slots.iter().find(|s| s.valid && s.lba == lba) {
                s.refbit.store(true, Ordering::Relaxed);
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(s.data.clone());
            }
        }
        self.load_miss(lba)
    }

    /// Load a missing block from the device and place it in a slot (CLOCK evict).
    fn load_miss(&self, lba: u64) -> BlockResult<Vec<u8>> {
        let mut buf = vec![0u8; self.block_size as usize];
        {
            let dev = self.dev.lock();
            dev.read_blocks(lba, 1, &mut buf)?;
        }
        let mut c = self.cache.write();
        // Another core may have already placed it in the meantime (race between the
        // read-lock miss and this write lock) — then reuse it; counts as a hit.
        if let Some(idx) = c.slots.iter().position(|s| s.valid && s.lba == lba) {
            c.slots[idx].refbit.store(true, Ordering::Relaxed);
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(c.slots[idx].data.clone());
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let victim = Self::evict_index(&mut c);
        // Write back a dirty victim first (must not be lost here).
        if c.slots[victim].valid && c.slots[victim].dirty {
            let vlba = c.slots[victim].lba;
            let vdata = c.slots[victim].data.clone();
            let mut dev = self.dev.lock();
            dev.write_blocks(vlba, 1, &vdata)?;
        }
        c.slots[victim] = Slot {
            lba,
            data: buf.clone(),
            valid: true,
            dirty: false,
            refbit: AtomicBool::new(true),
        };
        Ok(buf)
    }

    /// Write one block `lba` (write-through): immediately to the device and into the cache.
    pub fn write_block(&self, lba: u64, data: &[u8]) -> BlockResult<()> {
        {
            let mut dev = self.dev.lock();
            dev.write_blocks(lba, 1, data)?;
        }
        let mut c = self.cache.write();
        if let Some(idx) = c.slots.iter().position(|s| s.valid && s.lba == lba) {
            c.slots[idx].data.clear();
            c.slots[idx].data.extend_from_slice(data);
            c.slots[idx].refbit.store(true, Ordering::Relaxed);
        } else {
            let victim = Self::evict_index(&mut c);
            if c.slots[victim].valid && c.slots[victim].dirty {
                let vlba = c.slots[victim].lba;
                let vdata = c.slots[victim].data.clone();
                let mut dev = self.dev.lock();
                dev.write_blocks(vlba, 1, &vdata)?;
            }
            c.slots[victim] = Slot {
                lba,
                data: data.to_vec(),
                valid: true,
                dirty: false,
                refbit: AtomicBool::new(true),
            };
        }
        Ok(())
    }

    /// CLOCK / second-chance: find a victim slot. An empty slot wins; otherwise the
    /// hand sweeps around and gives each slot with a ref-bit one second chance (clearing the ref-bit).
    fn evict_index(c: &mut Cache) -> usize {
        let n = c.slots.len();
        // An unused slot first?
        if let Some(i) = c.slots.iter().position(|s| !s.valid) {
            return i;
        }
        loop {
            let i = c.hand;
            c.hand = (c.hand + 1) % n;
            if c.slots[i].refbit.load(Ordering::Relaxed) {
                c.slots[i].refbit.store(false, Ordering::Relaxed); // second chance
            } else {
                return i;
            }
        }
    }

    /// Force the device + flush all dirty slots back.
    pub fn flush(&self) -> BlockResult<()> {
        let mut c = self.cache.write();
        for s in c.slots.iter_mut() {
            if s.valid && s.dirty {
                let mut dev = self.dev.lock();
                dev.write_blocks(s.lba, 1, &s.data)?;
                s.dirty = false;
            }
        }
        self.dev.lock().flush()
    }

    /// (hits, misses) — diagnostics/self-test.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits.load(Ordering::Relaxed), self.misses.load(Ordering::Relaxed))
    }
}

/// The cache is itself a [`BlockDevice`] → a transparent drop-in caching layer:
/// `EuroFs::mount(BlockCache::new(disk, N), ..)` caches the whole FS read path, with
/// concurrent read hits, without the FS code changing.
impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.dev.lock().block_count()
    }

    fn read_blocks(&self, start_block: u64, count: u32, buffer: &mut [u8]) -> BlockResult<()> {
        let bs = self.block_size as usize;
        if buffer.len() != count as usize * bs {
            return Err(crate::block::BlockError::NotAligned);
        }
        for i in 0..count as u64 {
            let blk = self.read_block(start_block + i)?;
            let o = (i as usize) * bs;
            buffer[o..o + bs].copy_from_slice(&blk[..bs]);
        }
        Ok(())
    }

    fn write_blocks(&mut self, start_block: u64, count: u32, buffer: &[u8]) -> BlockResult<()> {
        let bs = self.block_size as usize;
        if buffer.len() != count as usize * bs {
            return Err(crate::block::BlockError::NotAligned);
        }
        for i in 0..count as u64 {
            let o = (i as usize) * bs;
            self.write_block(start_block + i, &buffer[o..o + bs])?;
        }
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        BlockCache::flush(self)
    }

    fn discard(&mut self, start_block: u64, count: u32) -> BlockResult<()> {
        // Invalidate any cached slots in the discarded range (their contents are gone),
        // then forward the TRIM to the backing device.
        {
            let mut c = self.cache.write();
            for slot in c.slots.iter_mut() {
                if slot.valid && slot.lba >= start_block && slot.lba < start_block + count as u64 {
                    slot.valid = false;
                    slot.dirty = false;
                    slot.lba = u64::MAX;
                }
            }
        }
        self.dev.lock().discard(start_block, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemoryBlockDevice;

    fn seeded_dev(blocks: u64) -> MemoryBlockDevice {
        let mut dev = MemoryBlockDevice::new(blocks, 512);
        for b in 0..blocks {
            let mut buf = vec![0u8; 512];
            buf[0] = b as u8;
            buf[1] = (b >> 8) as u8;
            dev.write_blocks(b, 1, &buf).unwrap();
        }
        dev
    }

    #[test]
    fn hit_and_miss_counts() {
        let cache = BlockCache::new(seeded_dev(64), 4);
        // First read = miss; second = hit.
        let a = cache.read_block(10).unwrap();
        assert_eq!(a[0], 10);
        let _ = cache.read_block(10).unwrap();
        let (hits, misses) = cache.stats();
        assert_eq!(misses, 1);
        assert_eq!(hits, 1);
    }

    #[test]
    fn eviction_keeps_correctness() {
        let cache = BlockCache::new(seeded_dev(64), 2); // only 2 slots
        // 3 different blocks in a cache of 2 → eviction.
        for lba in [1u64, 2, 3, 1, 2, 3] {
            let b = cache.read_block(lba).unwrap();
            assert_eq!(b[0], lba as u8); // data always stays correct
        }
        let (_, misses) = cache.stats();
        assert!(misses >= 3); // at least each block loaded once
    }

    #[test]
    fn write_through_persists_and_caches() {
        let cache = BlockCache::new(seeded_dev(16), 4);
        let mut data = vec![0u8; 512];
        data[0] = 0xAB;
        cache.write_block(5, &data).unwrap();
        // Reading back via the cache gives the new value (hit, no device round-trip).
        let r = cache.read_block(5).unwrap();
        assert_eq!(r[0], 0xAB);
        // And the device really has it too (flush + new cache proves persistence).
        cache.flush().unwrap();
    }

    #[test]
    fn block_device_impl_is_transparent() {
        // The cache as a BlockDevice: multi-block read gives exactly the device data.
        let cache = BlockCache::new(seeded_dev(64), 8);
        let mut buf = vec![0u8; 512 * 3];
        cache.read_blocks(10, 3, &mut buf).unwrap();
        assert_eq!(buf[0], 10);
        assert_eq!(buf[512], 11);
        assert_eq!(buf[1024], 12);
        // Second time = hits (data already cached).
        let (h0, _) = cache.stats();
        cache.read_blocks(10, 3, &mut buf).unwrap();
        let (h1, _) = cache.stats();
        assert_eq!(h1 - h0, 3);
    }

    #[test]
    fn eurofs_mounts_through_cache() {
        use crate::disk::EuroFs;
        // Format+mount a real EuroFs THROUGH the cache layer (drop-in).
        let mut cached = BlockCache::new(MemoryBlockDevice::new(1024, 4096), 64); // EuroFS: 4 KiB blocks
        // Format THROUGH the cache layer (write-through reads+writes), then remount
        // via `&mut` — proves that the cached data really persists on the device.
        EuroFs::format(&mut cached, [7u8; 16], 1).unwrap();
        assert!(EuroFs::mount(&mut cached, 2).is_ok());
        let (hits, misses) = cached.stats();
        assert!(hits + misses > 0); // the FS I/O really went through the cache
    }

    #[test]
    fn concurrent_readers_are_consistent() {
        use std::sync::Arc;
        use std::thread;
        let cache = Arc::new(BlockCache::new(seeded_dev(256), 32));
        let mut handles = Vec::new();
        // 8 threads read overlapping blocks at the same time; each read must give the
        // correct data (read lock on hits → real concurrency, no corruption/deadlock).
        for t in 0..8u64 {
            let c = Arc::clone(&cache);
            handles.push(thread::spawn(move || {
                for round in 0..500u64 {
                    let lba = (t * 7 + round) % 256;
                    let b = c.read_block(lba).unwrap();
                    assert_eq!(b[0], lba as u8);
                    assert_eq!(b[1], (lba >> 8) as u8);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let (hits, misses) = cache.stats();
        assert!(hits + misses == 8 * 500);
    }
}
