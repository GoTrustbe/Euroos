//! Sectorgebaseerde FAT32-toegang voor de KERNEL (post-ExitBootServices, geen
//! UEFI-bestandssysteem meer). Leest/overschrijft een KLEIN bestand in de root van
//! een ESP via 512-byte-sector-callbacks, zónder de hele ESP (~40 MiB) in te laden.
//!
//! Bewust beperkt tot root-bestanden van ≤ 1 cluster (genoeg voor `\slot_config`,
//! 32 bytes). Zo kan de kernel ná een succesvolle boot het door de loader beheerde
//! `\slot_config` op de ESP "goed" markeren (mark-good), zodat de A/B-rollback
//! stopt zodra een update bevestigd is — de loader zelf regelt het terugrollen.
//!
//! Pure `no_std`-logica; host-getest door de callbacks met een in-RAM-ESP (door
//! `FatFs` gebouwd) te backen.

use alloc::vec::Vec;

const SECTOR: usize = 512;
const EOC: u32 = 0x0FFF_FFF8; // ≥ deze waarde = einde-keten (FAT32)

/// Uit de BPB (sector 0 van de ESP) geparste geometrie.
struct Bpb {
    reserved: u32,
    num_fats: u32,
    spf: u32,
    spc: u32,
    root_cluster: u32,
}

impl Bpb {
    fn parse(sec0: &[u8]) -> Option<Bpb> {
        if sec0.len() < SECTOR || sec0[510] != 0x55 || sec0[511] != 0xAA {
            return None;
        }
        let bps = u16::from_le_bytes([sec0[11], sec0[12]]) as u32;
        if bps != SECTOR as u32 {
            return None; // we ondersteunen alleen 512 B/sector
        }
        let spc = sec0[13] as u32;
        let reserved = u16::from_le_bytes([sec0[14], sec0[15]]) as u32;
        let num_fats = sec0[16] as u32;
        let spf = u32::from_le_bytes([sec0[36], sec0[37], sec0[38], sec0[39]]);
        let root_cluster = u32::from_le_bytes([sec0[44], sec0[45], sec0[46], sec0[47]]);
        if spc == 0 || spf == 0 || root_cluster < 2 {
            return None;
        }
        Some(Bpb { reserved, num_fats, spf, spc, root_cluster })
    }
    fn data_start(&self) -> u32 {
        self.reserved + self.num_fats * self.spf
    }
    /// Eerste sector (binnen het volume) van datacluster `cl`.
    fn cluster_sector(&self, cl: u32) -> u32 {
        self.data_start() + (cl - 2) * self.spc
    }
}

/// Lees de FAT-entry voor cluster `cl` (om de root-mapketen te volgen).
fn fat_next<R: FnMut(u64, &mut [u8]) -> bool>(bpb: &Bpb, esp_first: u64, cl: u32, read: &mut R) -> u32 {
    let byte = bpb.reserved as u64 * SECTOR as u64 + cl as u64 * 4;
    let sec = esp_first + byte / SECTOR as u64;
    let within = (byte % SECTOR as u64) as usize;
    let mut buf = [0u8; SECTOR];
    if !read(sec, &mut buf) {
        return EOC;
    }
    u32::from_le_bytes([buf[within], buf[within + 1], buf[within + 2], buf[within + 3]]) & 0x0FFF_FFFF
}

/// Doorloop de root-map (volg de clusterketen, verzamel de 32-byte-entries met hun
/// absolute sector + offset) en geef voor `name` terug: (absolute datacluster-sector,
/// absolute SFN-entry-sector, byte-offset binnen die sector, opgeslagen grootte).
/// Reconstrueert LFN-namen, dus matcht ook lange namen zoals `slot_config`.
fn locate_in_root<R: FnMut(u64, &mut [u8]) -> bool>(
    bpb: &Bpb,
    esp_first: u64,
    name: &str,
    read: &mut R,
) -> Option<(u64, u64, usize, u32)> {
    // Verzamel root-dir-entries: (sector-lba, offset-in-sector, 32 bytes).
    let mut entries: Vec<(u64, usize, [u8; 32])> = Vec::new();
    let mut cl = bpb.root_cluster;
    let mut guard = 0;
    'outer: while cl >= 2 && cl < EOC && guard < 4096 {
        guard += 1;
        let base_sec = esp_first + bpb.cluster_sector(cl) as u64;
        for s in 0..bpb.spc as u64 {
            let mut buf = [0u8; SECTOR];
            if !read(base_sec + s, &mut buf) {
                return None;
            }
            let mut e = 0;
            while e + 32 <= SECTOR {
                if buf[e] == 0x00 {
                    break 'outer; // einde van de map
                }
                let mut ent = [0u8; 32];
                ent.copy_from_slice(&buf[e..e + 32]);
                entries.push((base_sec + s, e, ent));
                e += 32;
            }
        }
        cl = fat_next(bpb, esp_first, cl, read);
    }

    // Parse met LFN-reconstructie.
    let name83 = pack83(name);
    let mut lfn = alloc::string::String::new();
    for (lba, off, ent) in &entries {
        if ent[0] == 0xE5 {
            lfn.clear();
            continue;
        }
        if ent[11] == 0x0F {
            // LFN-fragment: zet vooraan (entries staan in omgekeerde volgorde).
            let frag = lfn_chars(ent);
            let mut s = frag;
            s.push_str(&lfn);
            lfn = s;
            continue;
        }
        let long = lfn.trim_end_matches('\u{0}');
        let matches = (!long.is_empty() && long.eq_ignore_ascii_case(name))
            || name83.map(|n| n[..] == ent[0..11]).unwrap_or(false);
        lfn.clear();
        if matches {
            let first_cluster = ((u16::from_le_bytes([ent[20], ent[21]]) as u32) << 16)
                | u16::from_le_bytes([ent[26], ent[27]]) as u32;
            let size = u32::from_le_bytes([ent[28], ent[29], ent[30], ent[31]]);
            let data_sec = esp_first + bpb.cluster_sector(first_cluster) as u64;
            return Some((data_sec, *lba, *off, size));
        }
    }
    None
}

/// Reconstrueer de 13 UTF-16-tekens uit een LFN-entry (posities 1..11, 14..26, 28..32).
fn lfn_chars(ent: &[u8; 32]) -> alloc::string::String {
    let mut u: Vec<u16> = Vec::new();
    let mut push = |off: usize| {
        let c = u16::from_le_bytes([ent[off], ent[off + 1]]);
        if c != 0x0000 && c != 0xFFFF {
            u.push(c);
        }
    };
    for i in 0..5 {
        push(1 + i * 2);
    }
    for i in 0..6 {
        push(14 + i * 2);
    }
    for i in 0..2 {
        push(28 + i * 2);
    }
    alloc::string::String::from_utf16_lossy(&u)
}

/// Pak een root-bestandsnaam als 8.3 (11 bytes, met spaties opgevuld), of None bij
/// een lange naam (die wordt dan via LFN-reconstructie gematcht).
fn pack83(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let mut out = [b' '; 11];
    for (i, c) in base.bytes().enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, c) in ext.bytes().enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    Some(out)
}

/// Lees een klein root-bestand (≤ 1 cluster) van een ESP via sector-callbacks.
/// Geeft de bestandsbytes (afgekapt op de opgeslagen grootte) of None.
pub fn read_small_file<R: FnMut(u64, &mut [u8]) -> bool>(
    esp_first: u64,
    name: &str,
    mut read: R,
) -> Option<Vec<u8>> {
    let mut sec0 = [0u8; SECTOR];
    if !read(esp_first, &mut sec0) {
        return None;
    }
    let bpb = Bpb::parse(&sec0)?;
    let (data_sec, _esec, _eoff, size) = locate_in_root(&bpb, esp_first, name, &mut read)?;
    let want = size as usize;
    let mut out = Vec::with_capacity(want);
    let mut got = 0;
    let mut s = 0u64;
    while got < want && s < bpb.spc as u64 {
        let mut buf = [0u8; SECTOR];
        if !read(data_sec + s, &mut buf) {
            return None;
        }
        let take = (want - got).min(SECTOR);
        out.extend_from_slice(&buf[..take]);
        got += take;
        s += 1;
    }
    Some(out)
}

/// Overschrijf een klein root-bestand (≤ 1 cluster) op een ESP IN-PLACE via sector-
/// callbacks: werkt de data-cluster + de grootte in de SFN-entry bij. Verandert geen
/// FAT-ketens (de clusterindeling blijft gelijk), dus veilig voor `\slot_config`.
/// Geeft `true` bij succes.
pub fn write_small_file<R, W>(esp_first: u64, name: &str, data: &[u8], mut read: R, mut write: W) -> bool
where
    R: FnMut(u64, &mut [u8]) -> bool,
    W: FnMut(u64, &[u8]) -> bool,
{
    let mut sec0 = [0u8; SECTOR];
    if !read(esp_first, &mut sec0) {
        return false;
    }
    let bpb = match Bpb::parse(&sec0) {
        Some(b) => b,
        None => return false,
    };
    if data.len() > bpb.spc as usize * SECTOR {
        return false; // alleen ≤ 1 cluster
    }
    let (data_sec, esec, eoff, _size) = match locate_in_root(&bpb, esp_first, name, &mut read) {
        Some(x) => x,
        None => return false,
    };
    // 1) Schrijf de nieuwe data (eerste sector(en) van de cluster), met nul-padding.
    let mut rem = data;
    let mut s = 0u64;
    while !rem.is_empty() || s == 0 {
        let mut buf = [0u8; SECTOR];
        let take = rem.len().min(SECTOR);
        buf[..take].copy_from_slice(&rem[..take]);
        if !write(data_sec + s, &buf) {
            return false;
        }
        rem = &rem[take..];
        s += 1;
        if s >= bpb.spc as u64 {
            break;
        }
    }
    // 2) Werk de grootte in de SFN-directory-entry bij.
    let mut esecbuf = [0u8; SECTOR];
    if !read(esec, &mut esecbuf) {
        return false;
    }
    esecbuf[eoff + 28..eoff + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
    write(esec, &esecbuf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FatFs;

    /// Bouw een ESP-image met een paar bestanden + een 32-byte `slot_config`.
    fn make_esp() -> Vec<u8> {
        let sectors = 48 * 1024 * 1024 / SECTOR as u32;
        let mut fs = FatFs::new(sectors, 0xCAFE_F00D, "EUROKERNEL");
        fs.add_file("/EFI/BOOT/BOOTX64.EFI", &alloc::vec![0xAB; 20_000]);
        fs.add_file("/EFI/BOOT/eurokernel-A.efi", &alloc::vec![0x11; 100_000]);
        let cfg = [0x42u8; 32];
        fs.add_file("/slot_config", &cfg);
        fs.build()
    }

    #[test]
    fn sectored_read_matches_written_config() {
        let mut esp = make_esp();
        let read = |lba: u64, buf: &mut [u8]| {
            let off = lba as usize * SECTOR;
            if off + SECTOR > esp.len() {
                return false;
            }
            buf[..SECTOR].copy_from_slice(&esp[off..off + SECTOR]);
            true
        };
        let got = read_small_file(0, "slot_config", read).expect("read");
        assert_eq!(got, alloc::vec![0x42u8; 32]);
        let _ = &mut esp; // touch
    }

    #[test]
    fn inplace_update_roundtrips_via_sectors() {
        let mut esp = make_esp();
        // Nieuwe 32-byte config (ander patroon).
        let mut newcfg = [0u8; 32];
        for (i, b) in newcfg.iter_mut().enumerate() {
            *b = i as u8;
        }
        {
            let snapshot = esp.clone();
            let read = |lba: u64, buf: &mut [u8]| {
                let off = lba as usize * SECTOR;
                buf[..SECTOR].copy_from_slice(&snapshot[off..off + SECTOR]);
                true
            };
            let write = |lba: u64, buf: &[u8]| {
                let off = lba as usize * SECTOR;
                esp[off..off + SECTOR].copy_from_slice(buf);
                true
            };
            assert!(write_small_file(0, "slot_config", &newcfg, read, write));
        }
        // Lees terug via de sector-lezer + via de volledige-image-lezer.
        let read = |lba: u64, buf: &mut [u8]| {
            let off = lba as usize * SECTOR;
            buf[..SECTOR].copy_from_slice(&esp[off..off + SECTOR]);
            true
        };
        assert_eq!(read_small_file(0, "slot_config", read), Some(newcfg.to_vec()));
        assert_eq!(crate::read_file(&esp, "/slot_config"), Some(newcfg.to_vec()));
        // De andere bestanden zijn ongemoeid.
        assert_eq!(crate::read_file(&esp, "/EFI/BOOT/eurokernel-A.efi"), Some(alloc::vec![0x11u8; 100_000]));
    }

    #[test]
    fn esp_first_lba_offset_is_honoured() {
        // Plaats de ESP op een niet-nul LBA (zoals in een GPT-disk) en bewijs dat
        // read/write met een esp_first-offset correct werken.
        let esp = make_esp();
        let offset_lba = 2048u64;
        let mut disk = alloc::vec![0u8; offset_lba as usize * SECTOR + esp.len()];
        disk[offset_lba as usize * SECTOR..].copy_from_slice(&esp);
        let read = |lba: u64, buf: &mut [u8]| {
            let off = lba as usize * SECTOR;
            buf[..SECTOR].copy_from_slice(&disk[off..off + SECTOR]);
            true
        };
        assert_eq!(read_small_file(offset_lba, "slot_config", read), Some(alloc::vec![0x42u8; 32]));
    }
}
