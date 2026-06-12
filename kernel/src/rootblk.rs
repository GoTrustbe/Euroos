//! Root-block-device (Run 7): één type dat EuroFS draagt — óf in RAM (live-modus,
//! geen schijf) óf RECHTSTREEKS op de virtio-blk-schijf (GEÏNSTALLEERDE modus).
//! In de schijf-modus staat EuroFS als ECHTE root op een GPT-partitie: bestanden
//! worden van schijf gelezen/geschreven i.p.v. elke boot opnieuw in RAM gebouwd —
//! het verschil tussen een live-USB en een geïnstalleerd OS.

use alloc::vec;
use alloc::vec::Vec;

use spin::RwLock;

use eurofs::{BlockDevice, BlockError, BlockResult};

const BS: usize = 4096;
const SPB: u64 = (BS / 512) as u64; // 512-byte sectoren per 4 KiB EuroFS-blok = 8

// ── Write-back block-cache (maakt de schijf-modus snel) ─────────────────────
// Direct-mapped cache van 4 KiB-blokken; reads vermijden herhaalde schijf-reads,
// writes worden gebatcht en pas bij `flush()` (= EuroFS-checkpoint) weggeschreven.
//
// J1: het slot wordt door een **RwLock** beschermd i.p.v. één globale Mutex, zodat
// een cache-HIT (de gangbare FS-leesweg) enkel een **read-lock** neemt → meerdere
// cores lezen tegelijk gecachede FS-blokken zonder elkaar te serialiseren. Alleen
// een miss/write/flush neemt de write-lock. Write-back-semantiek (dirty tot flush)
// blijft exact behouden, dus de EuroFS-checkpoint-/crash-consistentie verandert niet.
const CACHE_SLOTS: usize = 1024; // 1024 × 4 KiB = 4 MiB cache

struct Slot {
    sector: u64, // begin-sector van dit 4 KiB-blok op de schijf
    valid: bool,
    dirty: bool,
    data: [u8; BS],
}

static CACHE: RwLock<[Slot; CACHE_SLOTS]> =
    RwLock::new([const { Slot { sector: 0, valid: false, dirty: false, data: [0u8; BS] } }; CACHE_SLOTS]);

fn slot_index(sector: u64) -> usize {
    ((sector / SPB) as usize) % CACHE_SLOTS
}

/// Lees een 4 KiB-blok dat op `sector` begint (via de cache).
fn cache_read(sector: u64, out: &mut [u8]) -> bool {
    let i = slot_index(sector);
    // Snelle weg: read-lock, hit? (concurrent met andere lezers)
    {
        let c = CACHE.read();
        if c[i].valid && c[i].sector == sector {
            out[..BS].copy_from_slice(&c[i].data);
            return true;
        }
    }
    // Miss: write-lock, dubbel-check (een andere core kan 'm intussen geladen hebben),
    // vuile bewoner terugschrijven, dan van schijf laden.
    let mut c = CACHE.write();
    if !(c[i].valid && c[i].sector == sector) {
        if c[i].valid && c[i].dirty {
            let s = c[i].sector;
            let data = c[i].data;
            if !crate::virtio_blk::write_io(s, &data) {
                return false;
            }
        }
        let mut tmp = [0u8; BS];
        if !crate::virtio_blk::read_io(sector, &mut tmp) {
            return false;
        }
        c[i].data = tmp;
        c[i].sector = sector;
        c[i].valid = true;
        c[i].dirty = false;
    }
    out[..BS].copy_from_slice(&c[i].data);
    true
}

/// Schrijf een 4 KiB-blok naar `sector` (write-back: blijft in de cache, dirty).
fn cache_write(sector: u64, data: &[u8]) -> bool {
    let mut c = CACHE.write();
    let i = slot_index(sector);
    if c[i].valid && c[i].sector != sector && c[i].dirty {
        let s = c[i].sector;
        let old = c[i].data;
        if !crate::virtio_blk::write_io(s, &old) {
            return false;
        }
    }
    c[i].data[..BS].copy_from_slice(&data[..BS]);
    c[i].sector = sector;
    c[i].valid = true;
    c[i].dirty = true;
    true
}

/// Schrijf alle vuile blokken naar schijf (bij EuroFS-checkpoint / shutdown).
pub fn cache_flush() {
    let mut c = CACHE.write();
    for slot in c.iter_mut() {
        if slot.valid && slot.dirty {
            if crate::virtio_blk::write_io(slot.sector, &slot.data) {
                slot.dirty = false;
            }
        }
    }
}

#[derive(Clone)]
pub struct RootBlk {
    data: Vec<u8>,   // RAM-backing (leeg in schijf-modus)
    part_start: u64, // schijf-modus: eerste 512-byte sector van de EuroFS-partitie
    on_disk: bool,
    blocks: u64,
    dev: usize, // virtio-blk-apparaatindex (0 = root via cache; >0 = extra disk, direct)
}

impl RootBlk {
    pub fn ram(blocks: u64) -> Self {
        Self { data: vec![0u8; (blocks * BS as u64) as usize], part_start: 0, on_disk: false, blocks, dev: 0 }
    }
    pub fn disk(part_start: u64, blocks: u64) -> Self {
        Self::disk_on(0, part_start, blocks)
    }
    /// Schijf-modus op een specifiek virtio-blk-apparaat (B3 multi-disk). Apparaat 0
    /// gaat via de blok-cache; verdere apparaten doen ongecachte directe I/O.
    pub fn disk_on(dev: usize, part_start: u64, blocks: u64) -> Self {
        Self { data: Vec::new(), part_start, on_disk: true, blocks, dev }
    }
    pub fn is_disk(&self) -> bool {
        self.on_disk
    }
}

impl BlockDevice for RootBlk {
    fn block_size(&self) -> u32 {
        BS as u32
    }
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_blocks(&self, start: u64, count: u32, buf: &mut [u8]) -> BlockResult<()> {
        if !self.on_disk {
            let off = (start * BS as u64) as usize;
            let len = count as usize * BS;
            if off + len > self.data.len() {
                return Err(BlockError::OutOfBounds);
            }
            buf[..len].copy_from_slice(&self.data[off..off + len]);
            return Ok(());
        }
        for i in 0..count as u64 {
            let base = self.part_start + (start + i) * SPB;
            let o = (i * BS as u64) as usize;
            let ok = if self.dev == 0 {
                cache_read(base, &mut buf[o..o + BS])
            } else {
                crate::virtio_blk::read_io_dev(self.dev, base, &mut buf[o..o + BS])
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn write_blocks(&mut self, start: u64, count: u32, buf: &[u8]) -> BlockResult<()> {
        if !self.on_disk {
            let off = (start * BS as u64) as usize;
            let len = count as usize * BS;
            if off + len > self.data.len() {
                return Err(BlockError::OutOfBounds);
            }
            self.data[off..off + len].copy_from_slice(&buf[..len]);
            return Ok(());
        }
        for i in 0..count as u64 {
            let base = self.part_start + (start + i) * SPB;
            let o = (i * BS as u64) as usize;
            let ok = if self.dev == 0 {
                cache_write(base, &buf[o..o + BS])
            } else {
                crate::virtio_blk::write_io_dev(self.dev, base, &buf[o..o + BS])
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> BlockResult<()> {
        if self.on_disk {
            // Apparaat 0 gaat via de cache; verdere schijven schrijven direct.
            let ok = if self.dev == 0 {
                cache_flush(); // vuile cache-blokken naar schijf (EuroFS-checkpoint-commit)
                // Forceer daarna de schijf z'n EIGEN write-back-cache naar het persistente
                // medium (VIRTIO_BLK_T_FLUSH) — maakt de A/B-superblok-barrière een harde
                // I/O-barrière. No-op als het device geen vluchtige cache heeft.
                crate::virtio_blk::flush()
            } else {
                crate::virtio_blk::flush_dev(self.dev)
            };
            if !ok {
                return Err(BlockError::IoError);
            }
        }
        Ok(())
    }
}
