//! De `FileSystem`-abstractie die ramdisk en (later) EuroFS delen.

use alloc::string::String;
use alloc::vec::Vec;

/// Foutcategorieën, POSIX-achtig maar bewust eigen set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    NotADirectory,
    NotAFile,
    AlreadyExists,
    PermissionDenied,
    NoSpace,
    InvalidPath,
    IoError,
    Corruption,
    /// Bewerking niet ondersteund door dit filesysteem.
    Unsupported,
    /// Map is niet leeg (bv. bij `remove_dir`).
    NotEmpty,
}

pub type FsResult<T> = Result<T, FsError>;

// ── L1: immutability-vlaggen ────────────────────────────────────────────────
/// IMMUTABLE: het bestand kan NIET gewijzigd, verwijderd of hernoemd worden — zelfs
/// niet door root — tot de vlag gewist wordt (en dat vereist `CAP_IMMUTABLE_ADMIN`).
pub const FLAG_IMMUTABLE: u32 = 1 << 0;
/// APPEND_ONLY: schrijven mag enkel de bestaande inhoud UITBREIDEN (de nieuwe data
/// moet met de oude beginnen); geen overschrijven, inkorten of verwijderen. Basis
/// voor de tamper-evident audit-log (P3).
pub const FLAG_APPEND_ONLY: u32 = 1 << 1;

// ── EuroSnap (Sprint S): CoW-snapshots ──────────────────────────────────────
/// Snapshot-vlaggen.
pub const SNAP_READONLY: u32 = 0; // bevroren toestand (default)
pub const SNAP_AUTO_ROLLBACK: u32 = 1 << 0; // auto-verwijderen na een geslaagde boot (G4-update)

/// Publieke beschrijving van een snapshot (voor `snapshot_list`/de shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub id: u64,
    pub parent: u64,
    pub timestamp: u64,
    pub checkpoint_id: u64,
    pub flags: u32,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// POSIX-rechten (bv. 0o644 voor bestanden, 0o755 voor mappen).
    pub mode: u16,
    /// Laatste-wijziging-tijd (kernelklok, seconden sinds boot/epoch; 0 = onbekend).
    pub mtime: u64,
}

/// Uitkomst van een integriteitscontrole (scrub/fsck) — S7 storage-betrouwbaarheid.
#[derive(Debug, Clone, Default)]
pub struct ScrubReport {
    /// Aantal gecontroleerde objecten (inodes).
    pub objects: usize,
    /// Aantal gevonden fouten (checksum/magic/struct/cross-link).
    pub errors: usize,
    /// Aantal datablokken waar inodes naar verwijzen.
    pub blocks_referenced: u64,
    /// Superblok (magic + checksum) intact?
    pub superblock_ok: bool,
    /// Stroken de gerefereerde blokken met de vrije-ruimte-bitmap?
    pub bitmap_ok: bool,
    /// Aantal door `repair()` daadwerkelijk herstelde zaken (bv. superblok-slots).
    pub repaired: usize,
    /// Aantal objecten waarvan de data-path-checksum (XXH3 over de inhoud) is geverifieerd.
    pub data_verified: usize,
    /// Aantal objecten met data-corruptie die NIET hersteld kon worden (één schijf,
    /// geen redundantie — pas met een mirror/RAID (B3) is reconstructie mogelijk).
    pub data_unrecoverable: usize,
    /// Detailmeldingen (begrensd).
    pub messages: Vec<String>,
}

/// Kerninterface voor elk EuroKernel-filesysteem.
///
/// Bewust minimaal en synchroon: in een microkernel draait FS-I/O via IPC en
/// polling, niet via `async` (geen async runtime in kernelruimte).
pub trait FileSystem {
    fn read_file(&self, path: &str) -> FsResult<Vec<u8>>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> FsResult<()>;
    fn remove_file(&mut self, path: &str) -> FsResult<()>;
    fn create_dir(&mut self, path: &str) -> FsResult<()>;

    /// Verwijder een LEGE map (zoals `rmdir`). Een niet-lege map → `NotEmpty`; een
    /// bestand → `NotADirectory`. Standaard: niet ondersteund.
    fn remove_dir(&mut self, path: &str) -> FsResult<()> {
        let _ = path;
        Err(FsError::Unsupported)
    }

    /// Hernoem/verplaats `old` naar `new` (zoals `mv`/`rename(2)`). Een bestaand
    /// BESTAND op `new` wordt vervangen; een bestaande MAP op `new` is een fout.
    /// Standaard: niet ondersteund.
    fn rename(&mut self, old: &str, new: &str) -> FsResult<()> {
        let _ = (old, new);
        Err(FsError::Unsupported)
    }
    /// L1: lees de immutability-vlaggen van een bestand (`FLAG_IMMUTABLE` | `FLAG_APPEND_ONLY`).
    /// Standaard 0 (mutabel / niet ondersteund).
    fn get_flags(&self, path: &str) -> FsResult<u32> {
        let _ = path;
        Ok(0)
    }

    /// L1: zet de immutability-vlaggen van een bestand. De CAPABILITY-controle
    /// (`CAP_IMMUTABLE_ADMIN`, L2) gebeurt in de kernel-laag BOVEN deze call.
    /// Standaard: niet ondersteund.
    fn set_flags(&mut self, path: &str, flags: u32) -> FsResult<()> {
        let _ = (path, flags);
        Err(FsError::Unsupported)
    }

    // ── EuroSnap (Sprint S): CoW-snapshots — standaard niet ondersteund ──
    /// Maak een snapshot van de huidige FS-toestand (goedkoop dankzij CoW: enkel een
    /// bevroren root-pointer). Geeft de snapshot-id terug.
    fn snapshot_create(&mut self, label: &str, flags: u32) -> FsResult<u64> {
        let _ = (label, flags);
        Err(FsError::Unsupported)
    }
    /// Lijst alle snapshots (oudste → nieuwste).
    fn snapshot_list(&self) -> Vec<SnapshotInfo> {
        Vec::new()
    }
    /// Herstel de FS naar de toestand van snapshot `id` (vereist daarna doorgaans een
    /// remount/reboot voor in-flight state).
    fn snapshot_rollback(&mut self, id: u64) -> FsResult<()> {
        let _ = id;
        Err(FsError::Unsupported)
    }
    /// Verwijder een snapshot + reclaim z'n exclusieve blokken (GC).
    fn snapshot_delete(&mut self, id: u64) -> FsResult<()> {
        let _ = id;
        Err(FsError::Unsupported)
    }

    fn list_dir(&self, path: &str) -> FsResult<Vec<DirEntry>>;
    fn exists(&self, path: &str) -> bool;
    fn metadata(&self, path: &str) -> FsResult<DirEntry>;
    /// `(totaal, vrij)` in bytes.
    fn space_info(&self) -> (u64, u64);

    /// Ruimte per mountpoint voor `df`: `(mountpoint, totaal, vrij)`. Standaard:
    /// alleen de root (`/`). De VFS overschrijft dit met één regel per mount.
    fn df(&self) -> Vec<(String, u64, u64)> {
        let (t, f) = self.space_info();
        alloc::vec![(String::from("/"), t, f)]
    }

    /// Integriteitscontrole (scrub/fsck): verifieer superblok + alle inode-checksums
    /// + structurele consistentie. Standaard: niet ondersteund (alles "ok", 0 objecten).
    fn scrub(&self) -> ScrubReport {
        ScrubReport { superblock_ok: true, bitmap_ok: true, ..Default::default() }
    }

    /// Herstel wat veilig herstelbaar is (bv. een gedegradeerd superblok-slot uit de
    /// geldige A/B-kopie) en geef daarna een verse scrub-rapportage terug. Standaard:
    /// niets te repareren, gelijk aan [`scrub`].
    fn repair(&mut self) -> ScrubReport {
        self.scrub()
    }

    /// Herstel-interface voor toekomstige redundantie (mirror/RAID — B3): herschrijf
    /// het logische blok `lba` met een geverifieerde goede kopie. Standaard niet
    /// ondersteund (één schijf zonder redundantie kan niet reconstrueren).
    fn repair_block(&mut self, _lba: u64, _good: &[u8]) -> FsResult<()> {
        Err(FsError::Unsupported)
    }

    /// Stel de kernelklok in (Unix-seconden) die het filesysteem voor `mtime`
    /// gebruikt bij create/write. Standaard: genegeerd (FS zonder tijdsbesef).
    fn set_clock(&mut self, now: u64) {
        let _ = now;
    }
}
