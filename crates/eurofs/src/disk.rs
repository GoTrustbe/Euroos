//! EuroFS on-disk filesysteem (Track 2, Fase 2): copy-on-write, per-blok
//! integriteit, crash-consistent.
//!
//! Ontwerp (bewuste keuzes, expliciet gedocumenteerd):
//! - **Copy-on-write**: een mutatie overschrijft nooit live data. Nieuwe data
//!   en een nieuwe inode worden naar VRIJE blokken geschreven; pas de atomische
//!   superblok-update (checkpoint-bump + flush) maakt ze "live". Crash vóór die
//!   update → oude staat blijft volledig intact.
//! - **Allocator zonder on-disk bitmap**: de vrije-ruimte wordt bij `mount`
//!   gereconstrueerd door vanaf het gecommitte superblok alle bereikbare blokken
//!   te scannen. Zo zijn niet-gecommitte (gelekte) blokken automatisch weer vrij
//!   — exact de "space scan" uit de spec. Geen bitmap die kan de-synchroniseren.
//! - **Object map** (OID → inode-blok): nu een platte, CoW-herschreven tabel.
//!   Correct en eenvoudig; een B+tree (O(log n) op schaal) is de Fase-3
//!   vervanging met dezelfde semantiek. BEWUST geen B+tree nu — zie roadmap.
//! - **Inode** vult één 4 KiB-blok: header + tot 8 extents + inline data
//!   (≤ 3896 B) + XXH3-checksum. Directories zijn "files" waarvan de data een
//!   reeks 64-byte dir-entries is — één datapad voor files én mappen.
//!
//! Blokgrootte is vastgezet op 4096 voor Fase 2 (DEFAULT_BLOCK_SIZE).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::block::BlockDevice;
use crate::checksum::xxh3_64;
use crate::fs::{DirEntry, EntryKind, FileSystem, FsError, FsResult, SnapshotInfo, FLAG_APPEND_ONLY, FLAG_IMMUTABLE};
use crate::path::{filename, parent, split_path};
use crate::superblock::{EuroFsSuperblock, RESERVED_BLOCKS};

const BS: usize = 4096;
const INODE_MAGIC: u32 = 0x4546_494E; // "EFIN"
const ROOT_OID: u64 = 1;

// Inode-blok layout (offsets in bytes).
const OFF_MAGIC: usize = 0;
const OFF_OID: usize = 8;
const OFF_PARENT: usize = 16;
const OFF_TYPE: usize = 24;
const OFF_FLAGS: usize = 28; // u32 immutability-vlaggen (L1) — vrije slot, 0 = legacy/mutabel
const OFF_MODE: usize = 26;
const OFF_SIZE: usize = 32;
const OFF_INLINE_LEN: usize = 40;
const OFF_EXTENT_COUNT: usize = 44;
const OFF_MTIME: usize = 48; // u64 laatste-wijziging
const OFF_DATA_CHECKSUM: usize = 56; // u64 XXH3 over de volledige bestandsdata (0 = legacy/onbekend)
const OFF_EXTENTS: usize = 64; // 8 × 16 bytes
const MAX_EXTENTS: usize = 8;
const OFF_INLINE: usize = OFF_EXTENTS + MAX_EXTENTS * 16; // 192
const OFF_CHECKSUM: usize = BS - 8; // 4088
const INLINE_CAP: usize = OFF_CHECKSUM - OFF_INLINE; // 3896

const TYPE_FILE: u8 = 1;
const TYPE_DIR: u8 = 2;

const DIRENT_SIZE: usize = 64;
const DIRENT_NAME_CAP: usize = 48;

// ── kleine little-endian helpers ──────────────────────────────────────────
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}
fn wr_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

/// In-memory voorstelling van een inode (gedecodeerd uit één blok).
#[derive(Clone)]
struct Inode {
    oid: u64,
    parent: u64,
    otype: u8,
    /// L1: immutability-vlaggen (FLAG_IMMUTABLE | FLAG_APPEND_ONLY). 0 = mutabel.
    flags: u32,
    mode: u16,
    size: u64,
    mtime: u64,
    /// XXH3 over de volledige bestandsdata (data-path-integriteit). 0 = niet gezet
    /// (oud/legacy formaat) → verificatie wordt dan overgeslagen.
    data_checksum: u64,
    extents: Vec<(u64, u32)>, // (physical_block, block_count)
    inline: Vec<u8>,
}

impl Inode {
    fn new(oid: u64, parent: u64, otype: u8, now: u64) -> Self {
        Self {
            oid,
            parent,
            otype,
            flags: 0,
            mode: if otype == TYPE_DIR { 0o755 } else { 0o644 },
            size: 0,
            mtime: now,
            data_checksum: 0,
            extents: Vec::new(),
            inline: Vec::new(),
        }
    }

    fn encode(&self) -> [u8; BS] {
        let mut b = [0u8; BS];
        wr_u32(&mut b, OFF_MAGIC, INODE_MAGIC);
        wr_u64(&mut b, OFF_OID, self.oid);
        wr_u64(&mut b, OFF_PARENT, self.parent);
        b[OFF_TYPE] = self.otype;
        wr_u32(&mut b, OFF_FLAGS, self.flags);
        wr_u16(&mut b, OFF_MODE, self.mode);
        wr_u64(&mut b, OFF_SIZE, self.size);
        wr_u64(&mut b, OFF_MTIME, self.mtime);
        wr_u64(&mut b, OFF_DATA_CHECKSUM, self.data_checksum);
        wr_u32(&mut b, OFF_INLINE_LEN, self.inline.len() as u32);
        wr_u32(&mut b, OFF_EXTENT_COUNT, self.extents.len() as u32);
        for (i, &(phys, cnt)) in self.extents.iter().enumerate() {
            let o = OFF_EXTENTS + i * 16;
            wr_u64(&mut b, o, phys);
            wr_u32(&mut b, o + 8, cnt);
        }
        if !self.inline.is_empty() {
            b[OFF_INLINE..OFF_INLINE + self.inline.len()].copy_from_slice(&self.inline);
        }
        let csum = xxh3_64(&b[..OFF_CHECKSUM]);
        wr_u64(&mut b, OFF_CHECKSUM, csum);
        b
    }

    fn decode(b: &[u8]) -> FsResult<Self> {
        if rd_u32(b, OFF_MAGIC) != INODE_MAGIC {
            return Err(FsError::Corruption);
        }
        if rd_u64(b, OFF_CHECKSUM) != xxh3_64(&b[..OFF_CHECKSUM]) {
            return Err(FsError::Corruption);
        }
        let inline_len = rd_u32(b, OFF_INLINE_LEN) as usize;
        let ext_count = rd_u32(b, OFF_EXTENT_COUNT) as usize;
        // Een extent-telling > MAX_EXTENTS is geen geldige inode — eerder stil
        // afkappen (verloren extents → datalek/leak); behandel als corruptie.
        if ext_count > MAX_EXTENTS {
            return Err(FsError::Corruption);
        }
        let mut extents = Vec::new();
        for i in 0..ext_count {
            let o = OFF_EXTENTS + i * 16;
            extents.push((rd_u64(b, o), rd_u32(b, o + 8)));
        }
        Ok(Inode {
            oid: rd_u64(b, OFF_OID),
            parent: rd_u64(b, OFF_PARENT),
            otype: b[OFF_TYPE],
            flags: rd_u32(b, OFF_FLAGS),
            mode: rd_u16(b, OFF_MODE),
            size: rd_u64(b, OFF_SIZE),
            mtime: rd_u64(b, OFF_MTIME),
            data_checksum: rd_u64(b, OFF_DATA_CHECKSUM),
            extents,
            inline: b[OFF_INLINE..OFF_INLINE + inline_len].to_vec(),
        })
    }
}

// ── EuroSnap (Sprint S): CoW-snapshots ──────────────────────────────────────
/// Het gereserveerde blok waarin de snapshot-tabel staat (binnen RESERVED_BLOCKS;
/// blok 1/2 = superblok-A/B, 0 = boot, 3..15 = slack → 8 is vrij).
const SNAPSHOT_TABLE_BLOCK: u64 = 8;
const SNAPSHOT_MAGIC: u32 = 0x5346_4E53; // "SNFS"
const MAX_SNAPSHOTS: usize = 32;
const SNAP_ENTRY_LEN: usize = 80; // id+parent+ts+objmap_root+map_blocks+ckpt+flags+label(32)

/// Een snapshot-entry: een bevroren root-pointer naar een complete FS-toestand.
#[derive(Clone)]
struct SnapshotEntry {
    id: u64,
    parent: u64,
    timestamp: u64,
    objmap_root: u64,
    map_blocks: u64,
    checkpoint_id: u64,
    flags: u32,
    label: String,
}

/// Het on-disk EuroFS-volume over een willekeurig `BlockDevice`.
pub struct EuroFs<D: BlockDevice> {
    dev: D,
    sb: EuroFsSuperblock,
    /// OID → blok waar de inode staat.
    objmap: BTreeMap<u64, u64>,
    /// In-memory vrije-ruimte bitmap (1 bit per blok; true = in gebruik).
    used: Vec<bool>,
    next_oid: u64,
    now: u64,
    /// EuroSnap: actieve snapshots (bevroren root-pointers). Hun blokken worden in
    /// `rebuild_allocator` GEPIND zodat CoW ze niet reclaimt.
    snapshots: Vec<SnapshotEntry>,
    next_snap_id: u64,
}

impl<D: BlockDevice> EuroFs<D> {
    /// Formatteer een vers volume en geef een gemount filesysteem terug.
    pub fn format(dev: D, uuid: [u8; 16], now: u64) -> FsResult<Self> {
        assert_eq!(dev.block_size() as usize, BS, "EuroFS Fase 2 vereist 4096-byte blokken");
        let total = dev.block_count();
        let sb = EuroFsSuperblock::new_empty(total, uuid, now);
        let mut fs = EuroFs {
            dev,
            sb,
            objmap: BTreeMap::new(),
            used: vec![false; total as usize],
            next_oid: ROOT_OID + 1,
            now,
            snapshots: Vec::new(),
            next_snap_id: 1,
        };
        // Reserveer de eerste blokken (boot/superblok/backup/slack).
        for i in 0..(RESERVED_BLOCKS as usize).min(fs.used.len()) {
            fs.used[i] = true;
        }
        // Root-directory aanmaken (lege map).
        let root = Inode::new(ROOT_OID, ROOT_OID, TYPE_DIR, now);
        let blk = fs.alloc_block()?;
        fs.write_block(blk, &root.encode())?;
        fs.objmap.insert(ROOT_OID, blk);
        fs.commit()?;
        Ok(fs)
    }

    /// Mount een bestaand volume: lees superblok + object map, herbouw allocator.
    pub fn mount(dev: D, now: u64) -> FsResult<Self> {
        assert_eq!(dev.block_size() as usize, BS);
        let sb = EuroFsSuperblock::read_from(&dev)?;
        let total = dev.block_count();
        let mut fs = EuroFs {
            dev,
            sb,
            objmap: BTreeMap::new(),
            used: vec![false; total as usize],
            next_oid: ROOT_OID + 1,
            now,
            snapshots: Vec::new(),
            next_snap_id: 1,
        };
        fs.load_objmap()?;
        fs.load_snapshots()?;
        fs.rebuild_allocator()?;
        fs.next_oid = fs.objmap.keys().copied().max().unwrap_or(ROOT_OID) + 1;
        // Zelf-heling: read_from() slaagde dus minstens één A/B-slot is geldig. Staat
        // het ANDERE slot corrupt (door een eerdere torn write / bitrot), herstel het
        // dan stil uit de geldige kopie — zo is de superblok-redundantie meteen na de
        // mount weer compleet, zonder handmatige `fsck repair`.
        if EuroFsSuperblock::degraded_slots(&fs.dev) == 1 {
            let _ = EuroFsSuperblock::heal_slots(&mut fs.dev);
        }
        Ok(fs)
    }

    pub fn superblock(&self) -> &EuroFsSuperblock {
        &self.sb
    }

    // ── blok-I/O ──────────────────────────────────────────────────────────
    fn read_block(&self, blk: u64, buf: &mut [u8; BS]) -> FsResult<()> {
        self.dev.read_blocks(blk, 1, buf).map_err(|_| FsError::IoError)
    }
    fn write_block(&mut self, blk: u64, data: &[u8; BS]) -> FsResult<()> {
        self.dev.write_blocks(blk, 1, data).map_err(|_| FsError::IoError)
    }

    // ── allocator ─────────────────────────────────────────────────────────
    fn alloc_block(&mut self) -> FsResult<u64> {
        for (i, slot) in self.used.iter_mut().enumerate() {
            if !*slot {
                *slot = true;
                return Ok(i as u64);
            }
        }
        Err(FsError::NoSpace)
    }

    fn alloc_contiguous(&mut self, count: usize) -> FsResult<u64> {
        if count == 0 {
            return Err(FsError::IoError);
        }
        let n = self.used.len();
        let mut i = 0;
        'outer: while i + count <= n {
            for j in 0..count {
                if self.used[i + j] {
                    i += j + 1;
                    continue 'outer;
                }
            }
            for j in 0..count {
                self.used[i + j] = true;
            }
            return Ok(i as u64);
        }
        Err(FsError::NoSpace)
    }

    fn free_count(&self) -> u64 {
        self.used.iter().filter(|u| !**u).count() as u64
    }

    /// Herbouw de allocator door vanaf het gecommitte superblok alle bereikbare
    /// blokken te markeren. Reclaimt automatisch alle gelekte/oude blokken.
    fn rebuild_allocator(&mut self) -> FsResult<()> {
        let n = self.used.len();
        for u in self.used.iter_mut() {
            *u = false;
        }
        for i in 0..(RESERVED_BLOCKS as usize).min(n) {
            self.used[i] = true;
        }
        // Object map-blokken.
        let map_root = self.sb.object_map_root;
        let map_blocks = self.sb.extent_tree_root; // hergebruikt veld: #map-blokken
        for b in map_root..map_root + map_blocks {
            if (b as usize) < n {
                self.used[b as usize] = true;
            }
        }
        // Inodes + hun extents.
        let inode_blocks: Vec<u64> = self.objmap.values().copied().collect();
        for blk in inode_blocks {
            if (blk as usize) < n {
                self.used[blk as usize] = true;
            }
            let mut buf = [0u8; BS];
            self.read_block(blk, &mut buf)?;
            let inode = Inode::decode(&buf)?;
            for (phys, cnt) in inode.extents {
                for k in 0..cnt as u64 {
                    let bb = phys.saturating_add(k) as usize; // overloop-veilig (corrupte extent)
                    if bb < n {
                        self.used[bb] = true;
                    }
                }
            }
        }
        // EuroSnap: PIN de blokken van elke actieve snapshot zodat de CoW-reclaim ze
        // niet hergebruikt — zo blijft elke bevroren toestand intact.
        let snaps: Vec<(u64, u64)> = self.snapshots.iter().map(|s| (s.objmap_root, s.map_blocks)).collect();
        for (root, blocks) in snaps {
            self.mark_state_blocks(root, blocks)?;
        }
        self.sb.free_blocks = self.free_count();
        Ok(())
    }

    /// Markeer alle blokken die bereikbaar zijn vanuit de objmap op `objmap_root`
    /// (de tabel-blokken + de inode-blokken + hun data-extents) als IN GEBRUIK. Voor
    /// het pinnen van snapshot-toestanden in [`Self::rebuild_allocator`].
    fn mark_state_blocks(&mut self, objmap_root: u64, map_blocks: u64) -> FsResult<()> {
        let n = self.used.len();
        for b in objmap_root..objmap_root.saturating_add(map_blocks) {
            if (b as usize) < n {
                self.used[b as usize] = true;
            }
        }
        let mut data = Vec::new();
        for b in 0..map_blocks {
            let mut buf = [0u8; BS];
            if self.read_block(objmap_root + b, &mut buf).is_err() {
                return Ok(()); // corrupte snapshot → veilig overslaan
            }
            data.extend_from_slice(&buf);
        }
        if data.len() < 8 {
            return Ok(());
        }
        let count = rd_u64(&data, 0) as usize;
        for i in 0..count {
            let o = 8 + i * 16;
            if o + 16 > data.len() {
                break;
            }
            let blk = rd_u64(&data, o + 8);
            if (blk as usize) < n {
                self.used[blk as usize] = true;
            }
            let mut ib = [0u8; BS];
            if self.read_block(blk, &mut ib).is_err() {
                continue;
            }
            if let Ok(inode) = Inode::decode(&ib) {
                for (phys, cnt) in inode.extents {
                    for k in 0..cnt as u64 {
                        let bb = phys.saturating_add(k) as usize; // overloop-veilig (corrupte extent)
                        if bb < n {
                            self.used[bb] = true;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Lees de snapshot-tabel uit het gereserveerde blok (leeg/legacy = geen snapshots).
    fn load_snapshots(&mut self) -> FsResult<()> {
        let mut buf = [0u8; BS];
        if self.read_block(SNAPSHOT_TABLE_BLOCK, &mut buf).is_err() || rd_u32(&buf, 0) != SNAPSHOT_MAGIC {
            return Ok(());
        }
        let count = (rd_u32(&buf, 4) as usize).min(MAX_SNAPSHOTS);
        self.snapshots.clear();
        let mut max_id = 0u64;
        for i in 0..count {
            let o = 8 + i * SNAP_ENTRY_LEN;
            let id = rd_u64(&buf, o);
            let lab = &buf[o + 52..o + 52 + 28];
            let end = lab.iter().position(|&b| b == 0).unwrap_or(28);
            self.snapshots.push(SnapshotEntry {
                id,
                parent: rd_u64(&buf, o + 8),
                timestamp: rd_u64(&buf, o + 16),
                objmap_root: rd_u64(&buf, o + 24),
                map_blocks: rd_u64(&buf, o + 32),
                checkpoint_id: rd_u64(&buf, o + 40),
                flags: rd_u32(&buf, o + 48),
                label: String::from_utf8_lossy(&lab[..end]).into_owned(),
            });
            max_id = max_id.max(id);
        }
        self.next_snap_id = max_id + 1;
        Ok(())
    }

    /// Schrijf de snapshot-tabel naar het gereserveerde blok (+ flush voor duurzaamheid).
    fn save_snapshots(&mut self) -> FsResult<()> {
        let mut buf = [0u8; BS];
        wr_u32(&mut buf, 0, SNAPSHOT_MAGIC);
        wr_u32(&mut buf, 4, self.snapshots.len().min(MAX_SNAPSHOTS) as u32);
        for (i, s) in self.snapshots.iter().take(MAX_SNAPSHOTS).enumerate() {
            let o = 8 + i * SNAP_ENTRY_LEN;
            wr_u64(&mut buf, o, s.id);
            wr_u64(&mut buf, o + 8, s.parent);
            wr_u64(&mut buf, o + 16, s.timestamp);
            wr_u64(&mut buf, o + 24, s.objmap_root);
            wr_u64(&mut buf, o + 32, s.map_blocks);
            wr_u64(&mut buf, o + 40, s.checkpoint_id);
            wr_u32(&mut buf, o + 48, s.flags);
            let lb = s.label.as_bytes();
            let ln = lb.len().min(28);
            buf[o + 52..o + 52 + ln].copy_from_slice(&lb[..ln]);
        }
        self.write_block(SNAPSHOT_TABLE_BLOCK, &buf)?;
        let _ = self.dev.flush();
        Ok(())
    }

    // ── object map (platte CoW-tabel) ─────────────────────────────────────
    fn load_objmap(&mut self) -> FsResult<()> {
        let root = self.sb.object_map_root;
        let map_blocks = self.sb.extent_tree_root.max(1);
        let mut data = Vec::new();
        for b in 0..map_blocks {
            let mut buf = [0u8; BS];
            self.read_block(root + b, &mut buf)?;
            data.extend_from_slice(&buf);
        }
        // De objmap-tabel is niet apart gechecksumd; begrens `count` op wat er
        // werkelijk aan data is (audit H9), anders indexeert een corrupt/te groot
        // aantal buiten `data` en panikeert de FS bij het mounten.
        let max_entries = data.len().saturating_sub(8) / 16;
        let count = (rd_u64(&data, 0) as usize).min(max_entries);
        self.objmap.clear();
        for i in 0..count {
            let o = 8 + i * 16;
            self.objmap.insert(rd_u64(&data, o), rd_u64(&data, o + 8));
        }
        Ok(())
    }

    /// Serialiseer de object map naar verse blokken (CoW) en geef
    /// (root_block, block_count) terug.
    fn write_objmap(&mut self) -> FsResult<(u64, u64)> {
        let entries: Vec<(u64, u64)> = self.objmap.iter().map(|(&k, &v)| (k, v)).collect();
        let bytes_needed = 8 + entries.len() * 16;
        let blocks_needed = bytes_needed.div_ceil(BS).max(1);
        let mut data = vec![0u8; blocks_needed * BS];
        wr_u64(&mut data, 0, entries.len() as u64);
        for (i, &(oid, blk)) in entries.iter().enumerate() {
            let o = 8 + i * 16;
            wr_u64(&mut data, o, oid);
            wr_u64(&mut data, o + 8, blk);
        }
        let root = self.alloc_contiguous(blocks_needed)?;
        for b in 0..blocks_needed {
            let mut buf = [0u8; BS];
            buf.copy_from_slice(&data[b * BS..(b + 1) * BS]);
            self.write_block(root + b as u64, &buf)?;
        }
        Ok((root, blocks_needed as u64))
    }

    /// De atomische commit: schrijf object map + superblok, flush, en herbouw
    /// daarna de allocator (reclaimt oude blokken).
    fn commit(&mut self) -> FsResult<()> {
        let (root, blocks) = self.write_objmap()?;
        self.sb.object_map_root = root;
        self.sb.extent_tree_root = blocks; // #map-blokken
        self.sb.checkpoint_id += 1;
        self.sb.last_written = self.now;
        self.sb.free_blocks = self.free_count();
        self.sb.checksum = self.sb.compute_checksum();
        self.sb.write_to(&mut self.dev).map_err(|_| FsError::IoError)?;
        self.rebuild_allocator()?;
        Ok(())
    }

    // ── inode-data lezen/schrijven ────────────────────────────────────────
    fn read_inode(&self, oid: u64) -> FsResult<Inode> {
        let blk = *self.objmap.get(&oid).ok_or(FsError::NotFound)?;
        let mut buf = [0u8; BS];
        self.read_block(blk, &mut buf)?;
        Inode::decode(&buf)
    }

    fn read_data(&self, inode: &Inode) -> FsResult<Vec<u8>> {
        let out = if inode.extents.is_empty() {
            inode.inline.clone()
        } else {
            let mut out = Vec::with_capacity(inode.size as usize);
            for &(phys, cnt) in &inode.extents {
                for k in 0..cnt as u64 {
                    let mut buf = [0u8; BS];
                    self.read_block(phys.saturating_add(k), &mut buf)?; // overloop-veilig
                    out.extend_from_slice(&buf);
                }
            }
            out.truncate(inode.size as usize);
            out
        };
        // Verifieer de data-checksum (overgeslagen voor legacy-inodes met 0). Zo
        // levert een gecorrumpeerd datablok een fout i.p.v. stil foute bytes.
        if inode.data_checksum != 0 && xxh3_64(&out) != inode.data_checksum {
            return Err(FsError::Corruption);
        }
        Ok(out)
    }

    /// Schrijf data voor een (nieuwe) inode via CoW: alloceer verse data- en
    /// inode-blokken, schrijf ze, en update de object map. Commit gebeurt apart.
    fn write_object(&mut self, mut inode: Inode, data: &[u8]) -> FsResult<()> {
        inode.size = data.len() as u64;
        // Data-path-integriteit: XXH3 over de volledige inhoud, zodat bit-rot in
        // een datablok (buiten de inode) gedetecteerd kan worden bij lezen/scrub.
        inode.data_checksum = xxh3_64(data);
        inode.extents.clear();
        inode.inline.clear();
        if data.len() <= INLINE_CAP {
            inode.inline = data.to_vec();
        } else {
            let nblocks = data.len().div_ceil(BS);
            // De extent-blokteller is een u32 op schijf; weiger een bestand dat niet
            // in één extent past i.p.v. de teller stil af te kappen (→ datalek).
            if nblocks > u32::MAX as usize {
                return Err(FsError::NoSpace);
            }
            let start = self.alloc_contiguous(nblocks)?;
            for b in 0..nblocks {
                let mut buf = [0u8; BS];
                let from = b * BS;
                let to = (from + BS).min(data.len());
                buf[..to - from].copy_from_slice(&data[from..to]);
                self.write_block(start + b as u64, &buf)?;
            }
            inode.extents.push((start, nblocks as u32));
        }
        let blk = self.alloc_block()?;
        let enc = inode.encode();
        self.write_block(blk, &enc)?;
        self.objmap.insert(inode.oid, blk);
        Ok(())
    }

    // ── directory-helpers ─────────────────────────────────────────────────
    fn read_dir_entries(&self, dir_oid: u64) -> FsResult<Vec<(String, u64, u8)>> {
        let inode = self.read_inode(dir_oid)?;
        if inode.otype != TYPE_DIR {
            return Err(FsError::NotADirectory);
        }
        let data = self.read_data(&inode)?;
        let mut out = Vec::new();
        let mut o = 0;
        while o + DIRENT_SIZE <= data.len() {
            let oid = rd_u64(&data, o);
            let otype = data[o + 8];
            let name_len = data[o + 9] as usize;
            if oid != 0 && name_len <= DIRENT_NAME_CAP {
                let name = String::from_utf8_lossy(&data[o + 16..o + 16 + name_len]).into_owned();
                out.push((name, oid, otype));
            }
            o += DIRENT_SIZE;
        }
        Ok(out)
    }

    fn encode_dir_entries(entries: &[(String, u64, u8)]) -> Vec<u8> {
        let mut data = vec![0u8; entries.len() * DIRENT_SIZE];
        for (i, (name, oid, otype)) in entries.iter().enumerate() {
            let o = i * DIRENT_SIZE;
            wr_u64(&mut data, o, *oid);
            data[o + 8] = *otype;
            let nb = name.as_bytes();
            let n = nb.len().min(DIRENT_NAME_CAP);
            data[o + 9] = n as u8;
            data[o + 16..o + 16 + n].copy_from_slice(&nb[..n]);
        }
        data
    }

    fn resolve(&self, path: &str) -> FsResult<u64> {
        let mut oid = ROOT_OID;
        for comp in split_path(path) {
            let entries = self.read_dir_entries(oid)?;
            oid = entries
                .iter()
                .find(|(name, _, _)| name == comp)
                .map(|(_, o, _)| *o)
                .ok_or(FsError::NotFound)?;
        }
        Ok(oid)
    }

    /// Werk een directory bij (CoW) met een nieuwe entry-lijst.
    fn rewrite_dir(&mut self, dir_oid: u64, entries: &[(String, u64, u8)]) -> FsResult<()> {
        let inode = self.read_inode(dir_oid)?;
        let data = Self::encode_dir_entries(entries);
        let fresh = Inode {
            extents: Vec::new(),
            inline: Vec::new(),
            size: 0,
            ..inode
        };
        self.write_object(fresh, &data)
    }
}

impl<D: BlockDevice> FileSystem for EuroFs<D> {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_FILE {
            return Err(FsError::NotAFile);
        }
        self.read_data(&inode)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let parent_path = parent(path);
        let name = filename(path);
        if name.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let parent_oid = self.resolve(parent_path)?;
        let mut entries = self.read_dir_entries(parent_oid)?;

        let existing = entries.iter().find(|(n, _, _)| n == name).map(|(_, o, t)| (*o, *t));
        let oid = match existing {
            Some((_, t)) if t == TYPE_DIR => return Err(FsError::NotAFile),
            Some((o, _)) => o,
            None => {
                let o = self.next_oid;
                self.next_oid += 1;
                entries.push((name.to_string(), o, TYPE_FILE));
                o
            }
        };

        // L1: handhaaf de immutability-vlaggen van een bestaand bestand + behoud ze
        // over een (toegestane) overschrijving.
        let mut keep_flags = 0u32;
        if existing.is_some() {
            let old = self.read_inode(oid)?;
            keep_flags = old.flags;
            if old.flags & FLAG_IMMUTABLE != 0 {
                return Err(FsError::PermissionDenied); // immutabel → geen wijziging
            }
            if old.flags & FLAG_APPEND_ONLY != 0 {
                // Append-only: de nieuwe data moet de oude UITBREIDEN (zelfde prefix).
                let old_data = self.read_data(&old)?;
                if data.len() < old_data.len() || data[..old_data.len()] != old_data[..] {
                    return Err(FsError::PermissionDenied);
                }
            }
        }

        let mut inode = Inode::new(oid, parent_oid, TYPE_FILE, self.now);
        inode.flags = keep_flags;
        self.write_object(inode, data)?;
        if existing.is_none() {
            self.rewrite_dir(parent_oid, &entries)?;
        }
        self.commit()
    }

    fn remove_file(&mut self, path: &str) -> FsResult<()> {
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        let mut entries = self.read_dir_entries(parent_oid)?;
        let pos = entries
            .iter()
            .position(|(n, _, t)| n == name && *t == TYPE_FILE)
            .ok_or(FsError::NotFound)?;
        let oid = entries[pos].1;
        // L1: een immutabel of append-only bestand mag NIET verwijderd worden.
        let inode = self.read_inode(oid)?;
        if inode.flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0 {
            return Err(FsError::PermissionDenied);
        }
        entries.remove(pos);
        self.objmap.remove(&oid);
        self.rewrite_dir(parent_oid, &entries)?;
        self.commit()
    }

    fn get_flags(&self, path: &str) -> FsResult<u32> {
        let oid = self.resolve(path)?;
        Ok(self.read_inode(oid)?.flags)
    }

    // ── EuroSnap: CoW-snapshots ──────────────────────────────────────────
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        if self.snapshots.len() >= MAX_SNAPSHOTS {
            return Err(FsError::NoSpace);
        }
        // Commit eerst → de huidige toestand is een schone, atomair-vastgelegde root.
        self.commit()?;
        let id = self.next_snap_id;
        self.next_snap_id += 1;
        self.snapshots.push(SnapshotEntry {
            id,
            parent: self.sb.checkpoint_id, // herkomst = het checkpoint waarop 't berust
            timestamp: self.now,
            objmap_root: self.sb.object_map_root,
            map_blocks: self.sb.extent_tree_root,
            checkpoint_id: self.sb.checkpoint_id,
            flags,
            // Knip op een teken-grens, niet op byte 28 (audit H8: anders paniek op
            // een multibyte UTF-8-teken rond de grens).
            label: label.chars().take(28).collect::<String>(),
        });
        self.save_snapshots()?;
        Ok(id)
    }

    fn snapshot_list(&self) -> Vec<SnapshotInfo> {
        self.snapshots
            .iter()
            .map(|s| SnapshotInfo {
                id: s.id,
                parent: s.parent,
                timestamp: s.timestamp,
                checkpoint_id: s.checkpoint_id,
                flags: s.flags,
                label: s.label.clone(),
            })
            .collect()
    }

    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        let snap = self.snapshots.iter().find(|s| s.id == id).cloned().ok_or(FsError::NotFound)?;
        // Zet de root-pointer naar de bevroren toestand + herlaad die objmap.
        self.sb.object_map_root = snap.objmap_root;
        self.sb.extent_tree_root = snap.map_blocks;
        self.load_objmap()?;
        self.next_oid = self.objmap.keys().copied().max().unwrap_or(ROOT_OID) + 1;
        // Commit schrijft een verse objmap + superblok; rebuild_allocator reclaimt de
        // blokken van de verlaten toestand (tenzij door een andere snapshot gepind).
        self.commit()
    }

    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        let before = self.snapshots.len();
        self.snapshots.retain(|s| s.id != id);
        if self.snapshots.len() == before {
            return Err(FsError::NotFound);
        }
        self.save_snapshots()?;
        // GC: rebuild_allocator (via commit) reclaimt nu de exclusief-snapshot-blokken.
        self.commit()
    }

    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_FILE {
            return Err(FsError::NotAFile);
        }
        // De inode + dezelfde data herschrijven met de nieuwe vlaggen (CoW). De
        // CAP_IMMUTABLE_ADMIN-controle (L2) zit in de kernel-laag boven deze call.
        let data = self.read_data(&inode)?;
        let mut ni = inode;
        ni.flags = flags;
        self.write_object(ni, &data)?;
        self.commit()
    }

    fn create_dir(&mut self, path: &str) -> FsResult<()> {
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        if name.is_empty() {
            return Ok(()); // root
        }
        let mut entries = self.read_dir_entries(parent_oid)?;
        if entries.iter().any(|(n, _, _)| n == name) {
            return Err(FsError::AlreadyExists);
        }
        let oid = self.next_oid;
        self.next_oid += 1;
        let dir = Inode::new(oid, parent_oid, TYPE_DIR, self.now);
        self.write_object(dir, &[])?;
        entries.push((name.to_string(), oid, TYPE_DIR));
        self.rewrite_dir(parent_oid, &entries)?;
        self.commit()
    }

    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let old_parent = self.resolve(parent(old))?;
        let old_name = filename(old);
        let new_parent = self.resolve(parent(new))?;
        let new_name = filename(new);
        if old_name.is_empty() || new_name.is_empty() {
            return Err(FsError::InvalidPath);
        }

        // Bron-entry opzoeken.
        let src = self.read_dir_entries(old_parent)?;
        let (_, oid, otype) = src
            .iter()
            .find(|(n, _, _)| n == old_name)
            .cloned()
            .ok_or(FsError::NotFound)?;

        // L1: een immutabel/append-only bestand mag niet hernoemd/verplaatst worden.
        if otype == TYPE_FILE {
            let inode = self.read_inode(oid)?;
            if inode.flags & (FLAG_IMMUTABLE | FLAG_APPEND_ONLY) != 0 {
                return Err(FsError::PermissionDenied);
            }
        }

        // Anti-lus: een map mag niet IN haar eigen substructuur verplaatst worden.
        if otype == TYPE_DIR {
            let mut walk = new_parent;
            loop {
                if walk == oid {
                    return Err(FsError::InvalidPath);
                }
                if walk == ROOT_OID {
                    break;
                }
                walk = self.read_inode(walk)?.parent;
            }
        }

        if old_parent == new_parent {
            // Zelfde map: enkel de naam wijzigen (en evt. een bestaand doelbestand vervangen).
            let mut entries = src;
            if let Some(tp) = entries.iter().position(|(n, _, _)| n == new_name) {
                let (_, t_oid, t_type) = entries[tp].clone();
                if t_oid == oid {
                    return Ok(()); // old == new, niets te doen
                }
                if t_type == TYPE_DIR {
                    return Err(FsError::AlreadyExists); // mappen niet overschrijven
                }
                entries.remove(tp);
                self.objmap.remove(&t_oid);
            }
            let pos = entries
                .iter()
                .position(|(n, _, _)| n == old_name)
                .ok_or(FsError::NotFound)?;
            entries[pos].0 = new_name.to_string();
            self.rewrite_dir(old_parent, &entries)?;
        } else {
            // Verplaatsen naar een ANDERE map.
            let mut from = src;
            let mut to = self.read_dir_entries(new_parent)?;
            if let Some(tp) = to.iter().position(|(n, _, _)| n == new_name) {
                let (_, t_oid, t_type) = to[tp].clone();
                if t_type == TYPE_DIR {
                    return Err(FsError::AlreadyExists);
                }
                to.remove(tp);
                self.objmap.remove(&t_oid);
            }
            from.retain(|(n, _, _)| n != old_name);
            to.push((new_name.to_string(), oid, otype));
            // Een verplaatste MAP draagt z'n ouder-verwijzing mee.
            if otype == TYPE_DIR {
                let mut inode = self.read_inode(oid)?;
                inode.parent = new_parent;
                let data = self.read_data(&inode)?;
                self.write_object(inode, &data)?;
            }
            self.rewrite_dir(old_parent, &from)?;
            self.rewrite_dir(new_parent, &to)?;
        }
        self.commit()
    }

    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let oid = self.resolve(path)?;
        if oid == ROOT_OID {
            return Err(FsError::InvalidPath); // de root verwijder je niet
        }
        let inode = self.read_inode(oid)?;
        if inode.otype != TYPE_DIR {
            return Err(FsError::NotADirectory);
        }
        if !self.read_dir_entries(oid)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        let parent_oid = self.resolve(parent(path))?;
        let name = filename(path);
        let mut entries = self.read_dir_entries(parent_oid)?;
        let pos = entries
            .iter()
            .position(|(n, _, t)| n == name && *t == TYPE_DIR)
            .ok_or(FsError::NotFound)?;
        entries.remove(pos);
        self.objmap.remove(&oid);
        self.rewrite_dir(parent_oid, &entries)?;
        self.commit()
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>> {
        let oid = self.resolve(path)?;
        let entries = self.read_dir_entries(oid)?;
        let mut out = Vec::with_capacity(entries.len());
        for (name, child_oid, otype) in entries {
            let inode = self.read_inode(child_oid).ok();
            let (size, mode, mtime) = match &inode {
                Some(i) => (if otype == TYPE_FILE { i.size } else { 0 }, i.mode, i.mtime),
                None => (0, if otype == TYPE_DIR { 0o755 } else { 0o644 }, 0),
            };
            out.push(DirEntry {
                name,
                kind: if otype == TYPE_DIR { EntryKind::Directory } else { EntryKind::File },
                size,
                mode,
                mtime,
            });
        }
        Ok(out)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    fn metadata(&self, path: &str) -> FsResult<DirEntry> {
        let oid = self.resolve(path)?;
        let inode = self.read_inode(oid)?;
        Ok(DirEntry {
            name: filename(path).to_string(),
            kind: if inode.otype == TYPE_DIR { EntryKind::Directory } else { EntryKind::File },
            size: inode.size,
            mode: inode.mode,
            mtime: inode.mtime,
        })
    }

    fn space_info(&self) -> (u64, u64) {
        let total = self.used.len() as u64 * BS as u64;
        let free = self.free_count() * BS as u64;
        (total, free)
    }

    /// Scrub/fsck (S7): verifieer het superblok, élke inode-checksum, en de
    /// structurele consistentie (extents binnen de schijf, geen cross-links, en de
    /// gerefereerde blokken stroken met de vrije-ruimte-bitmap).
    fn scrub(&self) -> crate::fs::ScrubReport {
        let mut r = crate::fs::ScrubReport { superblock_ok: true, bitmap_ok: true, ..Default::default() };
        // 1) Superblok: controleer BEIDE A/B-slots afzonderlijk (her-lezen van schijf).
        //    1 gedegradeerd slot is nog mountbaar én repareerbaar (zie `repair`); pas
        //    bij twee corrupte slots is het superblok echt verloren.
        match EuroFsSuperblock::degraded_slots(&self.dev) {
            0 => {}
            1 => {
                r.errors += 1;
                r.messages.push(String::from(
                    "superblok: 1 A/B-slot gedegradeerd (geldige kopie intact, mountbaar — herstelbaar via repair)",
                ));
            }
            _ => {
                r.superblock_ok = false;
                r.errors += 1;
                r.messages.push(String::from("superblok: BEIDE slots corrupt (magic/checksum)"));
            }
        }
        // 2) Alle inodes + extents kruisverwijzen met een verse referentie-bitmap.
        let total = self.dev.block_count();
        let mut referenced = alloc::vec![false; self.used.len()];
        for (&oid, &blk) in &self.objmap {
            r.objects += 1;
            if (blk as usize) < referenced.len() {
                referenced[blk as usize] = true; // de inode-blok zelf
            }
            match self.read_inode(oid) {
                Ok(inode) => {
                    // Data-path-scrub: verifieer de XXH3 over de inhoud, zodat bit-rot
                    // in een datablok (buiten de inode zelf) gedetecteerd wordt — niet
                    // alleen corruptie van de inode of de structuur.
                    if inode.data_checksum != 0 {
                        match self.read_data(&inode) {
                            Ok(_) => r.data_verified += 1,
                            Err(_) => {
                                r.errors += 1;
                                // Eén schijf, geen redundantie → niet herstelbaar (B3 mirror nodig).
                                r.data_unrecoverable += 1;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!(
                                        "oid {oid}: DATA-checksum mismatch (bit-rot) — ONHERSTELBAAR (geen redundantie)"
                                    ));
                                }
                            }
                        }
                    }
                    for &(phys, cnt) in &inode.extents {
                        r.blocks_referenced += cnt as u64;
                        if phys + cnt as u64 > total {
                            r.errors += 1;
                            if r.messages.len() < 8 {
                                r.messages.push(alloc::format!("oid {oid}: extent {phys}+{cnt} buiten schijf"));
                            }
                            continue;
                        }
                        for b in phys..phys + cnt as u64 {
                            let bi = b as usize;
                            if bi >= referenced.len() {
                                continue;
                            }
                            if referenced[bi] {
                                r.errors += 1;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!("blok {b}: DUBBEL gerefereerd (cross-link)"));
                                }
                            }
                            referenced[bi] = true;
                            if !self.used[bi] {
                                r.bitmap_ok = false;
                                if r.messages.len() < 8 {
                                    r.messages.push(alloc::format!("blok {b}: gerefereerd maar niet als gebruikt gemarkeerd"));
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    r.errors += 1;
                    if r.messages.len() < 8 {
                        r.messages.push(alloc::format!("oid {oid} @ blok {blk}: inode magic/checksum CORRUPT"));
                    }
                }
            }
        }
        r
    }

    fn set_clock(&mut self, now: u64) {
        self.now = now;
    }

    fn repair(&mut self) -> crate::fs::ScrubReport {
        // Heel eerst de A/B-superblok-redundantie: staat één slot corrupt en het
        // andere geldig, dan herschrijven we het corrupte uit de geldige kopie. Het
        // filesysteem heeft daarna weer twee geldige superblok-kopieën.
        let healed = EuroFsSuperblock::heal_slots(&mut self.dev).unwrap_or(0);
        // Rapporteer de staat NA de heling (zo toont het rapport het herstel).
        let mut r = self.scrub();
        r.repaired = healed;
        if healed > 0 {
            r.messages.push(alloc::format!(
                "REPARATIE: {healed} superblok-slot hersteld uit de geldige A/B-kopie"
            ));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::MemoryBlockDevice;

    fn dev(blocks: u64) -> MemoryBlockDevice {
        MemoryBlockDevice::new(blocks, BS as u32)
    }

    #[test]
    fn format_en_root_leeg() {
        let fs = EuroFs::format(dev(256), [1; 16], 100).unwrap();
        assert!(fs.exists("/"));
        assert_eq!(fs.list_dir("/").unwrap().len(), 0);
        let ckpt = fs.superblock().checkpoint_id;
        assert_eq!(ckpt, 2); // format commit
    }

    #[test]
    fn schrijf_lees_klein_bestand() {
        let mut fs = EuroFs::format(dev(256), [2; 16], 1).unwrap();
        fs.write_file("/hallo.txt", b"Hallo EuroKernel").unwrap();
        assert_eq!(fs.read_file("/hallo.txt").unwrap(), b"Hallo EuroKernel");
        assert_eq!(fs.list_dir("/").unwrap()[0].name, "hallo.txt");
    }

    #[test]
    fn groot_bestand_via_extents() {
        let mut fs = EuroFs::format(dev(512), [3; 16], 1).unwrap();
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/big.bin", &big).unwrap();
        assert_eq!(fs.read_file("/big.bin").unwrap(), big);
        assert_eq!(fs.metadata("/big.bin").unwrap().size, 20_000);
    }

    #[test]
    fn inode_met_te_veel_extents_is_corruptie() {
        // Audit #5: een inode-blok dat ext_count > MAX_EXTENTS claimt mag NIET stil
        // afgekapt worden (verloren extents → datalek), maar als corruptie falen.
        let good = Inode {
            oid: 1,
            parent: 0,
            otype: 1,
            flags: 0,
            mode: 0,
            size: 0,
            mtime: 0,
            data_checksum: 0,
            extents: Vec::new(),
            inline: Vec::new(),
        };
        let mut buf = good.encode();
        wr_u32(&mut buf, OFF_EXTENT_COUNT, (MAX_EXTENTS + 1) as u32);
        // Checksum opnieuw zetten zodat het NIET op de checksum struikelt maar op de telling.
        let cs = xxh3_64(&buf[..OFF_CHECKSUM]);
        wr_u64(&mut buf, OFF_CHECKSUM, cs);
        assert!(matches!(Inode::decode(&buf), Err(FsError::Corruption)));
    }

    #[test]
    fn overloop_veilige_extent_blijft_geldig() {
        // Het saturating_add-pad mag een normaal groot bestand niet breken.
        let mut fs = EuroFs::format(dev(512), [7; 16], 1).unwrap();
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 97) as u8).collect();
        fs.write_file("/x.bin", &big).unwrap();
        assert_eq!(fs.read_file("/x.bin").unwrap(), big);
    }

    #[test]
    fn data_path_scrub_detecteert_bitrot_in_datablok() {
        let mut fs = EuroFs::format(dev(512), [9; 16], 1).unwrap();
        let big: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        fs.write_file("/big.bin", &big).unwrap();

        // Gezonde scrub: de data-checksum van het bestand is geverifieerd, 0 fouten.
        let r = fs.scrub();
        assert!(r.data_verified >= 1, "scrub hoort de bestandsdata te verifiëren");
        assert_eq!(r.errors, 0);

        // Corrumpeer één byte in het eerste datablok van het bestand, rechtstreeks
        // op het device — dit simuleert bit-rot buiten de inode.
        let oid = fs.resolve("/big.bin").unwrap();
        let (phys, _) = fs.read_inode(oid).unwrap().extents[0];
        let mut buf = [0u8; BS];
        fs.read_block(phys, &mut buf).unwrap();
        buf[10] ^= 0xFF;
        fs.write_block(phys, &buf).unwrap();

        // Lezen levert nu een Corruption-fout i.p.v. stil-foute bytes.
        assert_eq!(fs.read_file("/big.bin"), Err(FsError::Corruption));
        // En de scrub detecteert + rapporteert de data-corruptie als ONHERSTELBAAR
        // (één schijf, geen redundantie).
        let r2 = fs.scrub();
        assert!(r2.errors >= 1);
        assert_eq!(r2.data_unrecoverable, 1);
        assert!(r2.messages.iter().any(|m| m.contains("ONHERSTELBAAR")));
        // De mirror-herstel-interface bestaat maar is (één schijf) niet ondersteund.
        assert_eq!(fs.repair_block(phys, &[0u8; BS]), Err(FsError::Unsupported));
    }

    #[test]
    fn data_checksum_overleeft_remount() {
        let mut dev = dev(512);
        {
            let mut fs = EuroFs::format(&mut dev, [10; 16], 1).unwrap();
            fs.write_file("/big.bin", &[42u8; 9000]).unwrap();
        }
        // Na hermount blijft de data-checksum geldig en wordt geverifieerd.
        let fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.read_file("/big.bin").unwrap(), vec![42u8; 9000]);
        assert!(fs.scrub().data_verified >= 1);
    }

    #[test]
    fn mappen_en_nesting() {
        let mut fs = EuroFs::format(dev(256), [4; 16], 1).unwrap();
        fs.create_dir("/etc").unwrap();
        fs.create_dir("/etc/net").unwrap();
        fs.write_file("/etc/net/hosts", b"127.0.0.1 localhost\n").unwrap();
        assert_eq!(fs.read_file("/etc/net/hosts").unwrap(), b"127.0.0.1 localhost\n");
        let etc = fs.list_dir("/etc").unwrap();
        assert!(etc.iter().any(|e| e.name == "net" && e.kind == EntryKind::Directory));
    }

    #[test]
    fn persistentie_na_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [5; 16], 1).unwrap();
            fs.create_dir("/boot").unwrap();
            fs.write_file("/boot/version", b"EuroKernel v0.1\n").unwrap();
            fs.write_file("/boot/grote", &[7u8; 9000]).unwrap();
        }
        // Opnieuw mounten van hetzelfde device.
        let fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.read_file("/boot/version").unwrap(), b"EuroKernel v0.1\n");
        assert_eq!(fs.read_file("/boot/grote").unwrap(), vec![7u8; 9000]);
    }

    #[test]
    fn overschrijven_cow() {
        let mut fs = EuroFs::format(dev(256), [6; 16], 1).unwrap();
        fs.write_file("/f", b"oud").unwrap();
        fs.write_file("/f", b"nieuwe inhoud").unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"nieuwe inhoud");
        assert_eq!(fs.list_dir("/").unwrap().len(), 1); // geen duplicaat
    }

    #[test]
    fn verwijderen() {
        let mut fs = EuroFs::format(dev(256), [7; 16], 1).unwrap();
        fs.write_file("/tmp", b"x").unwrap();
        assert!(fs.exists("/tmp"));
        fs.remove_file("/tmp").unwrap();
        assert!(!fs.exists("/tmp"));
        assert_eq!(fs.read_file("/tmp"), Err(FsError::NotFound));
    }

    #[test]
    fn crash_voor_checkpoint_behoudt_oude_staat() {
        // Bewijs crash-consistentie: een commit die net vóór de atomische
        // superblok-update "verloren gaat" mag de oude staat niet beschadigen.
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [8; 16], 1).unwrap();
        fs.write_file("/f", b"OUD").unwrap();

        // Bewaar het huidige (gecommitte) superblok.
        let mut sb_old = [0u8; BS];
        fs.read_block(1, &mut sb_old).unwrap();

        // Nieuwe write — committeert volledig.
        fs.write_file("/f", b"NIEUW").unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"NIEUW");

        // Simuleer: de superblok-update van de NIEUW-commit landde nooit
        // (stroomuitval net vóór stap 5). Herstel oud superblok op blok 1 én 2.
        fs.write_block(1, &sb_old).unwrap();
        fs.write_block(2, &sb_old).unwrap();
        drop(fs);

        // Remount → moet de OUDE inhoud zien, niet corrupt.
        let fs = EuroFs::mount(&mut dev, 9).unwrap();
        assert_eq!(fs.read_file("/f").unwrap(), b"OUD");
    }

    #[test]
    fn inode_checksum_detecteert_corruptie() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [9; 16], 1).unwrap();
        fs.write_file("/a", b"data").unwrap();
        let blk = *fs.objmap.get(&fs.resolve("/a").unwrap()).unwrap();
        let mut buf = [0u8; BS];
        fs.read_block(blk, &mut buf).unwrap();
        buf[OFF_INLINE] ^= 0xFF; // corrumpeer inline data
        fs.write_block(blk, &buf).unwrap();
        assert_eq!(fs.read_file("/a"), Err(FsError::Corruption));
    }

    #[test]
    fn repair_heelt_gedegradeerd_backup_slot() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [12; 16], 1).unwrap();
            fs.write_file("/data.txt", b"belangrijk").unwrap();
            // Corrumpeer het BACK-UP-slot TIJDENS bedrijf (na mount; geen remount, dus
            // de auto-heling van mount pre-empt dit niet — we testen `repair` zelf).
            fs.write_block(SUPERBLOCK_BACKUP_BLOCK, &[0xFFu8; BS]).unwrap();
            let before = fs.scrub();
            assert!(before.errors >= 1 && before.superblock_ok);
            let rep = fs.repair();
            assert_eq!(rep.repaired, 1);
            assert_eq!(rep.errors, 0); // geen superblok-fout meer na heling
            assert_eq!(fs.read_file("/data.txt").unwrap(), b"belangrijk");
        }
        // Beide slots weer geldig.
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0);
    }

    #[test]
    fn repair_heelt_primair_slot_uit_backup() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [13; 16], 1).unwrap();
            fs.write_block(SUPERBLOCK_BLOCK, &[0u8; BS]).unwrap(); // primair corrupt
            assert_eq!(fs.repair().repaired, 1);
        }
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0);
    }

    #[test]
    fn mount_heelt_gedegradeerd_slot_automatisch() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK};
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [15; 16], 1).unwrap();
            fs.write_file("/x", b"y").unwrap();
        }
        // Corrumpeer een slot op de RUWE schijf (alsof een crash het sloopte).
        dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 1);
        {
            // Mount herstelt het slot automatisch (zelf-heling), zonder handmatige fsck.
            let fs = EuroFs::mount(&mut dev, 2).unwrap();
            assert_eq!(fs.read_file("/x").unwrap(), b"y");
        }
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 0); // automatisch geheeld
    }

    #[test]
    fn repair_kan_niet_helen_bij_twee_corrupte_slots() {
        use crate::superblock::{EuroFsSuperblock, SUPERBLOCK_BACKUP_BLOCK, SUPERBLOCK_BLOCK};
        let mut dev = dev(256);
        {
            let _ = EuroFs::format(&mut dev, [14; 16], 1).unwrap();
        }
        dev.write_blocks(SUPERBLOCK_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        dev.write_blocks(SUPERBLOCK_BACKUP_BLOCK, 1, &[0xFFu8; BS]).unwrap();
        assert_eq!(EuroFsSuperblock::degraded_slots(&dev), 2);
        // Geen geldige bron → heal doet (veilig) niets.
        assert_eq!(EuroFsSuperblock::heal_slots(&mut dev).unwrap(), 0);
    }

    #[test]
    fn mtime_en_mode_bij_aanmaak() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [20; 16], 1000).unwrap();
        fs.write_file("/a.txt", b"hoi").unwrap();
        fs.create_dir("/d").unwrap();
        let f = fs.metadata("/a.txt").unwrap();
        assert_eq!(f.mtime, 1000);
        assert_eq!(f.mode, 0o644);
        let d = fs.metadata("/d").unwrap();
        assert_eq!(d.mtime, 1000);
        assert_eq!(d.mode, 0o755);
    }

    #[test]
    fn mtime_loopt_mee_met_de_klok() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [21; 16], 1000).unwrap();
        fs.write_file("/x", b"v1").unwrap();
        assert_eq!(fs.metadata("/x").unwrap().mtime, 1000);
        // Klok vooruit + herschrijf → mtime volgt.
        fs.set_clock(2500);
        fs.write_file("/x", b"v2-met-langere-inhoud").unwrap();
        assert_eq!(fs.metadata("/x").unwrap().mtime, 2500);
    }

    #[test]
    fn rename_bestand_zelfde_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [30; 16], 1).unwrap();
        fs.write_file("/a.txt", b"inhoud").unwrap();
        fs.rename("/a.txt", "/b.txt").unwrap();
        assert!(!fs.exists("/a.txt"));
        assert_eq!(fs.read_file("/b.txt").unwrap(), b"inhoud");
    }

    #[test]
    fn rename_verplaatst_naar_andere_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [31; 16], 1).unwrap();
        fs.create_dir("/sub").unwrap();
        fs.write_file("/x", b"data").unwrap();
        fs.rename("/x", "/sub/y").unwrap();
        assert!(!fs.exists("/x"));
        assert_eq!(fs.read_file("/sub/y").unwrap(), b"data");
    }

    #[test]
    fn rename_vervangt_bestaand_bestand() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [32; 16], 1).unwrap();
        fs.write_file("/a", b"nieuw").unwrap();
        fs.write_file("/b", b"oud").unwrap();
        fs.rename("/a", "/b").unwrap();
        assert!(!fs.exists("/a"));
        assert_eq!(fs.read_file("/b").unwrap(), b"nieuw");
    }

    #[test]
    fn l1_immutable_blokkeert_wijzigingen() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [40; 16], 1).unwrap();
        fs.write_file("/sys", b"kernel-config").unwrap();
        fs.set_flags("/sys", FLAG_IMMUTABLE).unwrap();
        assert_eq!(fs.get_flags("/sys").unwrap(), FLAG_IMMUTABLE);
        // Schrijven, verwijderen, hernoemen → allemaal geweigerd.
        assert_eq!(fs.write_file("/sys", b"gehackt"), Err(FsError::PermissionDenied));
        assert_eq!(fs.remove_file("/sys"), Err(FsError::PermissionDenied));
        assert_eq!(fs.rename("/sys", "/elders"), Err(FsError::PermissionDenied));
        // Lezen werkt nog steeds, inhoud ongewijzigd.
        assert_eq!(fs.read_file("/sys").unwrap(), b"kernel-config");
        // Vlag wissen → weer wijzigbaar (de L2-cap-check zit in de kernel-laag).
        fs.set_flags("/sys", 0).unwrap();
        fs.write_file("/sys", b"nu-wel").unwrap();
        assert_eq!(fs.read_file("/sys").unwrap(), b"nu-wel");
    }

    #[test]
    fn l1_append_only_alleen_uitbreiden() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [41; 16], 1).unwrap();
        fs.write_file("/audit.log", b"regel1\n").unwrap();
        fs.set_flags("/audit.log", FLAG_APPEND_ONLY).unwrap();
        // Uitbreiden (zelfde prefix + langer) → OK.
        fs.write_file("/audit.log", b"regel1\nregel2\n").unwrap();
        assert_eq!(fs.read_file("/audit.log").unwrap(), b"regel1\nregel2\n");
        // Inkorten of een andere prefix → geweigerd (tamper-evident).
        assert_eq!(fs.write_file("/audit.log", b"regel1\n"), Err(FsError::PermissionDenied));
        assert_eq!(fs.write_file("/audit.log", b"VERVALST\n..........."), Err(FsError::PermissionDenied));
        // Verwijderen → geweigerd.
        assert_eq!(fs.remove_file("/audit.log"), Err(FsError::PermissionDenied));
    }

    #[test]
    fn l1_vlaggen_overleven_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [42; 16], 1).unwrap();
            fs.write_file("/boot/kernel", b"ELF...").unwrap_or_else(|_| {
                fs.create_dir("/boot").unwrap();
                fs.write_file("/boot/kernel", b"ELF...").unwrap();
            });
            fs.set_flags("/boot/kernel", FLAG_IMMUTABLE).unwrap();
        }
        // Na remount is de vlag persistent → schrijven blijft geweigerd.
        let mut fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.get_flags("/boot/kernel").unwrap(), FLAG_IMMUTABLE);
        assert_eq!(fs.write_file("/boot/kernel", b"x"), Err(FsError::PermissionDenied));
    }

    #[test]
    fn snap_create_modify_rollback() {
        let mut dev = dev(512);
        let mut fs = EuroFs::format(&mut dev, [50; 16], 1).unwrap();
        fs.write_file("/data", b"originele-inhoud").unwrap();
        let snap = fs.snapshot_create("voor-wijziging", crate::fs::SNAP_READONLY).unwrap();
        // Wijzig ná de snapshot.
        fs.write_file("/data", b"gewijzigd").unwrap();
        fs.write_file("/nieuw", b"toegevoegd").unwrap();
        assert_eq!(fs.read_file("/data").unwrap(), b"gewijzigd");
        assert!(fs.exists("/nieuw"));
        // Rollback → terug naar de bevroren toestand.
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/data").unwrap(), b"originele-inhoud");
        assert!(!fs.exists("/nieuw")); // het na-snapshot-bestand is verdwenen
    }

    #[test]
    fn snap_pint_grote_bestand_blokken() {
        // De échte test: na een snapshot moeten de (extent-)blokken van de bevroren
        // grote-bestand-versie GEPIND blijven, óók als er daarna veel wordt geschreven.
        let mut dev = dev(1024);
        let mut fs = EuroFs::format(&mut dev, [53; 16], 1).unwrap();
        let big_a = alloc::vec![0xAAu8; 40000]; // > INLINE_CAP → echte data-extents
        fs.write_file("/big", &big_a).unwrap();
        let snap = fs.snapshot_create("big-v1", 0).unwrap();
        // Overschrijf + veel extra allocaties: zonder pinning zouden de oude blokken
        // hergebruikt en de snapshot-data overschreven worden.
        fs.write_file("/big", &alloc::vec![0xBBu8; 40000]).unwrap();
        for i in 0..8 {
            fs.write_file(&alloc::format!("/f{i}"), &alloc::vec![0xCCu8; 9000]).unwrap();
        }
        // Rollback → de oude grote data moet BYTE-voor-BYTE intact zijn.
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/big").unwrap(), big_a);
    }

    #[test]
    fn snap_overleeft_remount() {
        let mut dev = dev(512);
        let snap;
        {
            let mut fs = EuroFs::format(&mut dev, [51; 16], 1).unwrap();
            fs.write_file("/x", b"snap-state").unwrap();
            snap = fs.snapshot_create("s1", 0).unwrap();
            fs.write_file("/x", b"na-snap").unwrap();
        }
        // Na remount is de snapshot-tabel persistent.
        let mut fs = EuroFs::mount(&mut dev, 2).unwrap();
        assert_eq!(fs.snapshot_list().len(), 1);
        fs.snapshot_rollback(snap).unwrap();
        assert_eq!(fs.read_file("/x").unwrap(), b"snap-state");
    }

    #[test]
    fn snap_list_en_delete_gc() {
        let mut dev = dev(512);
        let mut fs = EuroFs::format(&mut dev, [52; 16], 1).unwrap();
        fs.write_file("/a", b"1").unwrap();
        let s1 = fs.snapshot_create("eerste", 0).unwrap();
        fs.write_file("/a", b"2").unwrap();
        let s2 = fs.snapshot_create("tweede", 0).unwrap();
        assert_eq!(fs.snapshot_list().len(), 2);
        let free_before = fs.space_info().1;
        fs.snapshot_delete(s1).unwrap();
        assert_eq!(fs.snapshot_list().len(), 1);
        assert_eq!(fs.snapshot_delete(s1), Err(FsError::NotFound));
        // GC gaf ruimte vrij (of hield ze gelijk), nooit minder.
        assert!(fs.space_info().1 >= free_before);
        // s2 blijft bruikbaar voor rollback.
        fs.write_file("/a", b"3").unwrap();
        fs.snapshot_rollback(s2).unwrap();
        assert_eq!(fs.read_file("/a").unwrap(), b"2");
    }

    #[test]
    fn rename_map_met_inhoud() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [33; 16], 1).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/f", b"in-map").unwrap();
        fs.create_dir("/dest").unwrap();
        fs.rename("/d", "/dest/d2").unwrap();
        assert!(!fs.exists("/d"));
        assert_eq!(fs.read_file("/dest/d2/f").unwrap(), b"in-map");
        assert_eq!(fs.metadata("/dest/d2").unwrap().kind, EntryKind::Directory);
    }

    #[test]
    fn rename_weigert_map_overschrijven_en_lus() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [34; 16], 1).unwrap();
        fs.create_dir("/a").unwrap();
        fs.create_dir("/b").unwrap();
        assert_eq!(fs.rename("/a", "/b"), Err(FsError::AlreadyExists)); // map → map
        assert_eq!(fs.rename("/a", "/a/sub"), Err(FsError::InvalidPath)); // lus
        assert_eq!(fs.rename("/weg", "/x"), Err(FsError::NotFound)); // bron weg
    }

    #[test]
    fn rename_overleeft_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [35; 16], 1).unwrap();
            fs.create_dir("/etc").unwrap();
            fs.write_file("/old.conf", b"cfg").unwrap();
            fs.rename("/old.conf", "/etc/new.conf").unwrap();
        }
        let fs = EuroFs::mount(&mut dev, 0).unwrap();
        assert!(!fs.exists("/old.conf"));
        assert_eq!(fs.read_file("/etc/new.conf").unwrap(), b"cfg");
    }

    #[test]
    fn mtime_en_mode_overleven_remount() {
        let mut dev = dev(256);
        {
            let mut fs = EuroFs::format(&mut dev, [22; 16], 7777).unwrap();
            fs.write_file("/keep", b"data").unwrap();
        }
        let fs = EuroFs::mount(&mut dev, 0).unwrap();
        assert_eq!(fs.metadata("/keep").unwrap().mtime, 7777);
        // list_dir levert óók mtime/mode per entry.
        let e = &fs.list_dir("/").unwrap()[0];
        assert_eq!(e.name, "keep");
        assert_eq!(e.mtime, 7777);
        assert_eq!(e.mode, 0o644);
    }

    #[test]
    fn remove_dir_lege_map() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [40; 16], 1).unwrap();
        fs.create_dir("/leeg").unwrap();
        assert!(fs.exists("/leeg"));
        fs.remove_dir("/leeg").unwrap();
        assert!(!fs.exists("/leeg"));
    }

    #[test]
    fn remove_dir_weigert_niet_leeg_bestand_en_root() {
        let mut dev = dev(256);
        let mut fs = EuroFs::format(&mut dev, [41; 16], 1).unwrap();
        fs.create_dir("/d").unwrap();
        fs.write_file("/d/f", b"x").unwrap();
        assert_eq!(fs.remove_dir("/d"), Err(FsError::NotEmpty)); // niet leeg
        fs.write_file("/file", b"y").unwrap();
        assert_eq!(fs.remove_dir("/file"), Err(FsError::NotADirectory)); // bestand
        assert_eq!(fs.remove_dir("/weg"), Err(FsError::NotFound)); // bestaat niet
        assert_eq!(fs.remove_dir("/"), Err(FsError::InvalidPath)); // root
        // Leeggemaakt → wel verwijderbaar.
        fs.remove_file("/d/f").unwrap();
        fs.remove_dir("/d").unwrap();
        assert!(!fs.exists("/d"));
    }
}
