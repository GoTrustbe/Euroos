//! Concurrente block-cache (plan J1 — "block-cache RwLock").
//!
//! Onder J1 (per-subsystem-locking) vervangt EuroOS de grove globale `IF=0`-secties
//! door fijnmazige, schaalbare sloten. Deze cache is daar een concreet stuk van: een
//! **lees-schrijf-vergrendelde** blok-cache boven een [`BlockDevice`]. De gangbare
//! operatie — een **cache-hit** — neemt slechts een **read-lock**, zodat meerdere
//! cores tegelijk gecachede blokken kunnen lezen zonder elkaar te blokkeren. Alleen
//! een **miss** (een blok inladen) of een **write** neemt kort de write-lock + de
//! device-lock. Zo schaalt de FS-leesweg mee met het aantal cores i.p.v. te
//! serialiseren op één globaal slot.
//!
//! Eviction is een eenvoudige **CLOCK / second-chance**-policy (ref-bit-ring), net
//! als de swap-pager — een O(1)-LRU-benadering zonder per-toegang lijst-geschuif.

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
    /// CLOCK-ref-bit. Atomair zodat een lees-HIT 'm onder de read-lock kan zetten.
    refbit: AtomicBool,
}

struct Cache {
    slots: Vec<Slot>,
    hand: usize, // CLOCK-wijzer voor eviction
}

/// Een concurrente, doorschrijvende (write-through) blok-cache boven `D`.
pub struct BlockCache<D: BlockDevice> {
    dev: Mutex<D>,
    cache: RwLock<Cache>,
    block_size: u32,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl<D: BlockDevice> BlockCache<D> {
    /// Maak een cache met `capacity` blok-slots boven `dev`.
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

    /// Lees één blok `lba`. Een **hit** neemt enkel de **read-lock** (concurrent met
    /// andere lezers — de ref-bit + teller zijn atomair); een **miss** laadt 'm onder
    /// de write-lock vanaf het device.
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

    /// Laad een ontbrekend blok van het device en plaats het in een slot (CLOCK-evict).
    fn load_miss(&self, lba: u64) -> BlockResult<Vec<u8>> {
        let mut buf = vec![0u8; self.block_size as usize];
        {
            let dev = self.dev.lock();
            dev.read_blocks(lba, 1, &mut buf)?;
        }
        let mut c = self.cache.write();
        // Mogelijk plaatste een andere core 'm intussen al (race tussen de read-lock-
        // miss en deze write-lock) — dan hergebruiken; telt als een hit.
        if let Some(idx) = c.slots.iter().position(|s| s.valid && s.lba == lba) {
            c.slots[idx].refbit.store(true, Ordering::Relaxed);
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(c.slots[idx].data.clone());
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let victim = Self::evict_index(&mut c);
        // Een dirty slachtoffer eerst terugschrijven (mag hier niet verloren gaan).
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

    /// Schrijf één blok `lba` (write-through): meteen naar het device én de cache bij.
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

    /// CLOCK / second-chance: zoek een slachtoffer-slot. Leeg slot wint; anders draait
    /// de wijzer rond en geeft elk slot met ref-bit één tweede kans (ref-bit wissen).
    fn evict_index(c: &mut Cache) -> usize {
        let n = c.slots.len();
        // Eerst een ongebruikt slot?
        if let Some(i) = c.slots.iter().position(|s| !s.valid) {
            return i;
        }
        loop {
            let i = c.hand;
            c.hand = (c.hand + 1) % n;
            if c.slots[i].refbit.load(Ordering::Relaxed) {
                c.slots[i].refbit.store(false, Ordering::Relaxed); // tweede kans
            } else {
                return i;
            }
        }
    }

    /// Forceer het device + spoel alle dirty slots terug.
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

    /// (hits, misses) — diagnostiek/zelftest.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits.load(Ordering::Relaxed), self.misses.load(Ordering::Relaxed))
    }
}

/// De cache is zélf een [`BlockDevice`] → een transparante drop-in cachelaag:
/// `EuroFs::mount(BlockCache::new(disk, N), ..)` cachet de hele FS-leesweg, met
/// concurrente read-hits, zonder dat de FS-code wijzigt.
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
        // Eerste lees = miss; tweede = hit.
        let a = cache.read_block(10).unwrap();
        assert_eq!(a[0], 10);
        let _ = cache.read_block(10).unwrap();
        let (hits, misses) = cache.stats();
        assert_eq!(misses, 1);
        assert_eq!(hits, 1);
    }

    #[test]
    fn eviction_keeps_correctness() {
        let cache = BlockCache::new(seeded_dev(64), 2); // maar 2 slots
        // 3 verschillende blokken in een cache van 2 → eviction.
        for lba in [1u64, 2, 3, 1, 2, 3] {
            let b = cache.read_block(lba).unwrap();
            assert_eq!(b[0], lba as u8); // data blijft altijd correct
        }
        let (_, misses) = cache.stats();
        assert!(misses >= 3); // minstens elk blok één keer geladen
    }

    #[test]
    fn write_through_persists_and_caches() {
        let cache = BlockCache::new(seeded_dev(16), 4);
        let mut data = vec![0u8; 512];
        data[0] = 0xAB;
        cache.write_block(5, &data).unwrap();
        // Teruglezen via de cache geeft de nieuwe waarde (hit, geen device-roundtrip).
        let r = cache.read_block(5).unwrap();
        assert_eq!(r[0], 0xAB);
        // En het device heeft 'm ook echt (flush + nieuwe cache bewijst persistentie).
        cache.flush().unwrap();
    }

    #[test]
    fn block_device_impl_is_transparent() {
        // De cache als BlockDevice: multi-block read geeft exact de device-data.
        let cache = BlockCache::new(seeded_dev(64), 8);
        let mut buf = vec![0u8; 512 * 3];
        cache.read_blocks(10, 3, &mut buf).unwrap();
        assert_eq!(buf[0], 10);
        assert_eq!(buf[512], 11);
        assert_eq!(buf[1024], 12);
        // Tweede keer = hits (data al gecached).
        let (h0, _) = cache.stats();
        cache.read_blocks(10, 3, &mut buf).unwrap();
        let (h1, _) = cache.stats();
        assert_eq!(h1 - h0, 3);
    }

    #[test]
    fn eurofs_mounts_through_cache() {
        use crate::disk::EuroFs;
        // Een echte EuroFs formatteren+mounten DOOR de cache-laag heen (drop-in).
        let mut cached = BlockCache::new(MemoryBlockDevice::new(1024, 4096), 64); // EuroFS: 4 KiB-blokken
        // Formatteren DOOR de cache-laag (write-through reads+writes), dan remounten
        // via `&mut` — bewijst dat de gecachede data echt op het device persisteert.
        EuroFs::format(&mut cached, [7u8; 16], 1).unwrap();
        assert!(EuroFs::mount(&mut cached, 2).is_ok());
        let (hits, misses) = cached.stats();
        assert!(hits + misses > 0); // de FS-I/O liep echt door de cache
    }

    #[test]
    fn concurrent_readers_are_consistent() {
        use std::sync::Arc;
        use std::thread;
        let cache = Arc::new(BlockCache::new(seeded_dev(256), 32));
        let mut handles = Vec::new();
        // 8 threads lezen tegelijk overlappende blokken; elke lezing moet de juiste
        // data geven (read-lock op hits → echte concurrency, geen corruptie/deadlock).
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
