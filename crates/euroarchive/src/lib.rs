//! EuroArchive — de archiefbeheerder van EuroOS (Sprint AC-2).
//!
//! Een soevereine **USTAR tar**-implementatie: lezen en schrijven van het
//! `tar`-formaat met octale headervelden en **checksum-verificatie**. Tar is de
//! container; compressie (gzip/zstd) komt als aparte laag erbovenop. Bij het
//! uitpakken kan een meegeleverd manifest met **Ed25519-handtekeningen**
//! geverifieerd worden (haak: [`verify_manifest`]).
//!
//! Pure `no_std`-logica, host-getest. Geen `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

const BLOCK: usize = 512;

/// Het soort archief-item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// Eén item in een archief.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Octale Unix-rechten (bv. 0o644).
    pub mode: u32,
    pub data: Vec<u8>,
}

impl Entry {
    /// Maak een bestand-item.
    pub fn file(name: &str, data: &[u8]) -> Entry {
        Entry { name: name.to_string(), kind: Kind::File, mode: 0o644, data: data.to_vec() }
    }
    /// Maak een map-item.
    pub fn dir(name: &str) -> Entry {
        let name = if name.ends_with('/') { name.to_string() } else { alloc::format!("{name}/") };
        Entry { name, kind: Kind::Dir, mode: 0o755, data: Vec::new() }
    }
}

/// Foutsoorten bij het lezen van een archief.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    Truncated,
    BadChecksum { at: usize },
    BadNumber,
}

// ── octale velden ─────────────────────────────────────────────────────────────

fn write_octal(buf: &mut [u8], value: u64) {
    // Veld van n bytes: octaal, rechts uitgelijnd met voorloopnullen, NUL-afgesloten.
    let n = buf.len();
    let mut v = value;
    let mut i = n - 1;
    buf[i] = 0; // afsluitende NUL
    if i == 0 {
        return;
    }
    i -= 1;
    loop {
        buf[i] = b'0' + (v & 0o7) as u8;
        v >>= 3;
        if i == 0 || v == 0 {
            break;
        }
        i -= 1;
    }
    // Vul de rest met '0'.
    while i > 0 {
        i -= 1;
        buf[i] = b'0';
    }
}

fn read_octal(field: &[u8]) -> Result<u64, ArchiveError> {
    let mut v: u64 = 0;
    let mut any = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8).and_then(|x| x.checked_add((b - b'0') as u64)).ok_or(ArchiveError::BadNumber)?;
                any = true;
            }
            b' ' | 0 => {
                if any {
                    break;
                }
            }
            _ => return Err(ArchiveError::BadNumber),
        }
    }
    Ok(v)
}

fn checksum(header: &[u8; BLOCK]) -> u32 {
    let mut sum: u32 = 0;
    for (i, &b) in header.iter().enumerate() {
        // De 8 chksum-bytes (148..156) tellen als spaties.
        if (148..156).contains(&i) {
            sum += b' ' as u32;
        } else {
            sum += b as u32;
        }
    }
    sum
}

fn put_str(buf: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&bytes[..n]);
}

// ── schrijven ─────────────────────────────────────────────────────────────────

/// Schrijf een lijst entries naar een tar-bytestroom (USTAR).
pub fn write_tar(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        let mut header = [0u8; BLOCK];
        put_str(&mut header[0..100], &e.name);
        write_octal(&mut header[100..108], e.mode as u64);
        write_octal(&mut header[108..116], 0); // uid
        write_octal(&mut header[116..124], 0); // gid
        let size = if e.kind == Kind::Dir { 0 } else { e.data.len() as u64 };
        write_octal(&mut header[124..136], size);
        write_octal(&mut header[136..148], 0); // mtime (deterministisch = 0)
        header[156] = match e.kind {
            Kind::File => b'0',
            Kind::Dir => b'5',
        };
        put_str(&mut header[257..263], "ustar\0");
        header[263] = b'0';
        header[264] = b'0';
        // Checksum als laatste, octaal in 6 cijfers + NUL + spatie.
        let sum = checksum(&header);
        let mut cs = [0u8; 8];
        write_octal(&mut cs[..7], sum as u64); // 6 cijfers + NUL op index 6
        cs[7] = b' ';
        header[148..156].copy_from_slice(&cs);

        out.extend_from_slice(&header);
        if e.kind == Kind::File {
            out.extend_from_slice(&e.data);
            let pad = (BLOCK - e.data.len() % BLOCK) % BLOCK;
            out.extend(core::iter::repeat(0u8).take(pad));
        }
    }
    // Twee lege blokken als einde-markering.
    out.extend(vec![0u8; BLOCK * 2]);
    out
}

// ── lezen ─────────────────────────────────────────────────────────────────────

/// Lees een tar-bytestroom naar entries; verifieert per header de checksum.
pub fn read_tar(bytes: &[u8]) -> Result<Vec<Entry>, ArchiveError> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + BLOCK <= bytes.len() {
        let mut header = [0u8; BLOCK];
        header.copy_from_slice(&bytes[pos..pos + BLOCK]);
        // Einde: een leeg blok.
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let stored = read_octal(&header[148..156])?;
        let actual = checksum(&header);
        if stored as u32 != actual {
            return Err(ArchiveError::BadChecksum { at: pos });
        }
        let name = cstr(&header[0..100]);
        let mode = read_octal(&header[100..108])? as u32;
        let size = read_octal(&header[124..136])? as usize;
        let kind = match header[156] {
            b'5' => Kind::Dir,
            _ => Kind::File,
        };
        pos += BLOCK;
        let data = if kind == Kind::File {
            if pos + size > bytes.len() {
                return Err(ArchiveError::Truncated);
            }
            let d = bytes[pos..pos + size].to_vec();
            let padded = size.div_ceil(BLOCK) * BLOCK;
            pos += padded;
            d
        } else {
            Vec::new()
        };
        entries.push(Entry { name, kind, mode, data });
    }
    Ok(entries)
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Haak voor Ed25519-manifest-verificatie bij het uitpakken.
///
/// `verify` is een door de aanroeper geleverde verificatiefunctie
/// (bv. via `eurotls`), zodat deze crate `no_std`-zuiver en crypto-vrij blijft.
/// Retourneert de lijst bestandsnamen waarvan de hash-handtekening klopt.
pub fn verify_manifest<F>(entries: &[Entry], manifest: &[(String, Vec<u8>)], mut verify: F) -> Vec<String>
where
    F: FnMut(&[u8], &[u8]) -> bool,
{
    let mut ok = Vec::new();
    for (name, sig) in manifest {
        if let Some(e) = entries.iter().find(|e| &e.name == name) {
            if verify(&e.data, sig) {
                ok.push(name.clone());
            }
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_files_and_dirs() {
        let entries = vec![
            Entry::dir("src"),
            Entry::file("src/main.rs", b"fn main() {}\n"),
            Entry::file("README.md", b"# EuroArchive\n"),
        ];
        let tar = write_tar(&entries);
        assert_eq!(tar.len() % BLOCK, 0);
        let back = read_tar(&tar).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].kind, Kind::Dir);
        assert_eq!(back[0].name, "src/");
        assert_eq!(back[1].name, "src/main.rs");
        assert_eq!(back[1].data, b"fn main() {}\n");
        assert_eq!(back[2].data, b"# EuroArchive\n");
    }

    #[test]
    fn checksum_detects_corruption() {
        let tar = write_tar(&[Entry::file("a.txt", b"hallo")]);
        let mut corrupt = tar.clone();
        corrupt[0] = b'Z'; // verander de naam → checksum klopt niet meer
        assert!(matches!(read_tar(&corrupt), Err(ArchiveError::BadChecksum { .. })));
    }

    #[test]
    fn modes_preserved() {
        let mut e = Entry::file("run.sh", b"#!/bin/sh\n");
        e.mode = 0o755;
        let back = read_tar(&write_tar(&[e])).unwrap();
        assert_eq!(back[0].mode, 0o755);
    }

    #[test]
    fn empty_file_and_block_alignment() {
        let back = read_tar(&write_tar(&[Entry::file("empty", b"")])).unwrap();
        assert_eq!(back[0].data.len(), 0);
        // Exact 512-byte bestand → geen extra padding-fouten.
        let big = vec![7u8; 512];
        let back2 = read_tar(&write_tar(&[Entry::file("big", &big)])).unwrap();
        assert_eq!(back2[0].data, big);
    }

    #[test]
    fn octal_field_values() {
        let mut buf = [0u8; 12];
        write_octal(&mut buf, 1234);
        assert_eq!(read_octal(&buf).unwrap(), 1234);
        write_octal(&mut buf, 0);
        assert_eq!(read_octal(&buf).unwrap(), 0);
    }

    #[test]
    fn manifest_verification_hook() {
        let entries = vec![Entry::file("a", b"alpha"), Entry::file("b", b"beta")];
        // Nep-verificatie: handtekening == data (alleen voor de test).
        let manifest = vec![
            ("a".to_string(), b"alpha".to_vec()),
            ("b".to_string(), b"WRONG".to_vec()),
        ];
        let ok = verify_manifest(&entries, &manifest, |data, sig| data == sig);
        assert_eq!(ok, vec!["a".to_string()]);
    }
}
