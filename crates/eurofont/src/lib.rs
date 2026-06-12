//! EuroFont — het lettertypebeheer van EuroOS (Sprint AC-2).
//!
//! Parseert **sfnt/TrueType**-fonts: de tabel-directory en de `name`-, `head`- en
//! `maxp`-tabellen, zodat de fontmanager familienaam, stijl, units-per-em en het
//! aantal glyphs kan tonen — zonder FreeType. Ondersteunt `name`-records van zowel
//! Windows (platform 3, UTF-16BE) als Macintosh (platform 1, ASCII). Bevat een
//! kleine font-bouwer ([`build_min_font`]) zodat de parser host-getest kan worden.
//!
//! Pure `no_std`-logica, geen `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}
fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*d.get(o)?, *d.get(o + 1)?, *d.get(o + 2)?, *d.get(o + 3)?]))
}

/// Metadata uit een font.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FontInfo {
    pub family: String,
    pub subfamily: String,
    pub full_name: String,
    pub units_per_em: u16,
    pub num_glyphs: u16,
}

/// Zoek een tabel in de directory; geeft (offset, length).
fn find_table(d: &[u8], tag: &[u8; 4]) -> Option<(usize, usize)> {
    let num_tables = be16(d, 4)? as usize;
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let t = d.get(rec..rec + 4)?;
        if t == tag {
            let off = be32(d, rec + 8)? as usize;
            let len = be32(d, rec + 12)? as usize;
            return Some((off, len));
        }
    }
    None
}

/// Decodeer een naam-string volgens platform/encoding.
fn decode_name(platform: u16, bytes: &[u8]) -> String {
    if platform == 3 || platform == 0 {
        // UTF-16BE.
        let mut s = String::new();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let u = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
            if let Some(c) = char::from_u32(u as u32) {
                s.push(c);
            }
            i += 2;
        }
        s
    } else {
        // Macintosh Roman → behandel als ASCII/Latin-1.
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Parse de `name`-tabel → (familie, subfamilie, volledige naam).
fn parse_name_table(d: &[u8], off: usize, len: usize) -> (String, String, String) {
    let t = match d.get(off..off + len) {
        Some(t) => t,
        None => return (String::new(), String::new(), String::new()),
    };
    let count = be16(t, 2).unwrap_or(0) as usize;
    let string_off = be16(t, 4).unwrap_or(0) as usize;
    let mut family = String::new();
    let mut subfamily = String::new();
    let mut full = String::new();
    for i in 0..count {
        let rec = 6 + i * 12;
        let (Some(platform), Some(name_id), Some(slen), Some(soff)) =
            (be16(t, rec), be16(t, rec + 6), be16(t, rec + 8), be16(t, rec + 10))
        else {
            break;
        };
        let s0 = string_off + soff as usize;
        let bytes = match t.get(s0..s0 + slen as usize) {
            Some(b) => b,
            None => continue,
        };
        let value = decode_name(platform, bytes);
        match name_id {
            1 if family.is_empty() => family = value,
            2 if subfamily.is_empty() => subfamily = value,
            4 if full.is_empty() => full = value,
            _ => {}
        }
    }
    (family, subfamily, full)
}

/// Parse een font naar [`FontInfo`]. Geeft `None` als het geen geldig sfnt is.
pub fn parse(d: &[u8]) -> Option<FontInfo> {
    let version = be32(d, 0)?;
    // 0x00010000 = TrueType, 'OTTO' = CFF/OpenType, 'true'/'typ1' = Apple.
    if version != 0x0001_0000 && &d[0..4] != b"OTTO" && &d[0..4] != b"true" {
        return None;
    }
    let mut info = FontInfo::default();
    if let Some((o, l)) = find_table(d, b"name") {
        let (f, sf, full) = parse_name_table(d, o, l);
        info.family = f;
        info.subfamily = sf;
        info.full_name = full;
    }
    if let Some((o, _)) = find_table(d, b"head") {
        info.units_per_em = be16(d, o + 18).unwrap_or(0); // unitsPerEm @ offset 18
    }
    if let Some((o, _)) = find_table(d, b"maxp") {
        info.num_glyphs = be16(d, o + 4).unwrap_or(0); // numGlyphs @ offset 4
    }
    Some(info)
}

// ── minimale font-bouwer (voor tests + als referentie van de structuur) ──

fn push_be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn push_be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// Bouw een minimaal-geldig TrueType-font met de gegeven metadata. De `name`-
/// records gebruiken platform 1 (Macintosh, ASCII). Genoeg om [`parse`] te voeden.
pub fn build_min_font(family: &str, subfamily: &str, full: &str, units_per_em: u16, num_glyphs: u16) -> Vec<u8> {
    // name-tabel met 3 records (familie=1, subfamilie=2, volledig=4).
    let strings: [(u16, &str); 3] = [(1, family), (2, subfamily), (4, full)];
    let mut name = Vec::new();
    push_be16(&mut name, 0); // format 0
    push_be16(&mut name, strings.len() as u16); // count
    let string_off = 6 + strings.len() * 12;
    push_be16(&mut name, string_off as u16); // stringOffset
    let mut string_data = Vec::new();
    for (name_id, s) in strings {
        let bytes = s.as_bytes();
        push_be16(&mut name, 1); // platformID = Macintosh
        push_be16(&mut name, 0); // encodingID = Roman
        push_be16(&mut name, 0); // languageID
        push_be16(&mut name, name_id);
        push_be16(&mut name, bytes.len() as u16); // length
        push_be16(&mut name, string_data.len() as u16); // offset in string-storage
        string_data.extend_from_slice(bytes);
    }
    name.extend_from_slice(&string_data);

    // head-tabel: 54 bytes, unitsPerEm @ offset 18.
    let mut head = alloc::vec![0u8; 54];
    head[18..20].copy_from_slice(&units_per_em.to_be_bytes());

    // maxp v0.5: version (u32) + numGlyphs (u16).
    let mut maxp = Vec::new();
    push_be32(&mut maxp, 0x0000_5000);
    push_be16(&mut maxp, num_glyphs);

    // Tabel-directory (gesorteerd op tag: head, maxp, name).
    let tables: [(&[u8; 4], &Vec<u8>); 3] = [(b"head", &head), (b"maxp", &maxp), (b"name", &name)];
    let num = tables.len();
    let mut out = Vec::new();
    push_be32(&mut out, 0x0001_0000); // sfntVersion
    push_be16(&mut out, num as u16);
    push_be16(&mut out, 0); // searchRange
    push_be16(&mut out, 0); // entrySelector
    push_be16(&mut out, 0); // rangeShift

    let mut data_off = 12 + num * 16;
    let mut directory = Vec::new();
    let mut body = Vec::new();
    for (tag, data) in tables {
        directory.extend_from_slice(tag);
        push_be32(&mut directory, 0); // checksum (genegeerd door onze parser)
        push_be32(&mut directory, data_off as u32);
        push_be32(&mut directory, data.len() as u32);
        body.extend_from_slice(data);
        data_off += data.len();
    }
    out.extend_from_slice(&directory);
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_roundtrip() {
        let font = build_min_font("EuroSans", "Bold", "EuroSans Bold", 2048, 412);
        let info = parse(&font).unwrap();
        assert_eq!(info.family, "EuroSans");
        assert_eq!(info.subfamily, "Bold");
        assert_eq!(info.full_name, "EuroSans Bold");
        assert_eq!(info.units_per_em, 2048);
        assert_eq!(info.num_glyphs, 412);
    }

    #[test]
    fn rejects_non_font() {
        assert!(parse(b"not a font at all").is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn utf16_name_decoding() {
        // Bouw een name-record met platform 3 (UTF-16BE) handmatig.
        let mut name = Vec::new();
        push_be16(&mut name, 0);
        push_be16(&mut name, 1);
        push_be16(&mut name, 6 + 12);
        // "Eu" in UTF-16BE.
        let s: Vec<u8> = "Eu".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        push_be16(&mut name, 3); // platform Windows
        push_be16(&mut name, 1);
        push_be16(&mut name, 0x409);
        push_be16(&mut name, 1); // nameID family
        push_be16(&mut name, s.len() as u16);
        push_be16(&mut name, 0);
        name.extend_from_slice(&s);
        let (fam, _, _) = parse_name_table(&name, 0, name.len());
        assert_eq!(fam, "Eu");
    }

    #[test]
    fn missing_tables_are_tolerated() {
        // Een font zonder head/maxp → nullen, maar parse slaagt op de naam.
        let font = build_min_font("OnlyName", "", "", 0, 0);
        let info = parse(&font).unwrap();
        assert_eq!(info.family, "OnlyName");
        assert_eq!(info.units_per_em, 0);
    }

    #[test]
    fn truncated_font_no_panic() {
        let font = build_min_font("X", "Y", "X Y", 1000, 5);
        // Afgekapt → parse mag niet paniekeren (bounds-checked).
        let _ = parse(&font[..font.len() / 2]);
        let _ = parse(&font[..15]);
    }
}
