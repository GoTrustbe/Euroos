//! Kernel-zijde van **EuroMedia** (Sprint AC-1): de afbeeldingsviewer.
//! Bij boot bewijzen we de soevereine QOI-codec: encode→decode is lossless en
//! een effen vlak comprimeert sterk. Host-geteste kern: [`euromedia`].

use crate::serial_println;
use euromedia::{decode, encode, Image};

/// Boot-zelftest: QOI-round-trip op een gradiënt + compressie van een vlak.
pub fn selftest() {
    // Gradiënt met wisselende alpha (lokt alle chunk-types uit).
    let mut img = Image::new(16, 16, [10, 20, 30, 255]);
    for y in 0..16u32 {
        for x in 0..16u32 {
            let a = if (x + y) % 5 == 0 { 128 } else { 255 };
            img.set(x, y, [(x * 16) as u8, (y * 16) as u8, ((x + y) * 8) as u8, a]);
        }
    }
    let enc = encode(&img);
    let dec = decode(&enc);
    let lossless = matches!(&dec, Ok(d) if *d == img);

    // Effen vlak → sterke compressie.
    let solid = Image::new(64, 64, [0x1b, 0x4f, 0x91, 255]); // EU-blauw
    let solid_enc = encode(&solid);
    let ratio = (64 * 64 * 4) / solid_enc.len().max(1);
    let solid_ok = decode(&solid_enc).map(|d| d == solid).unwrap_or(false) && ratio > 50;

    let ok = lossless && solid_ok;
    serial_println!(
        "[mv] EuroMedia QOI: gradiënt 16×16 lossless={} ({} bytes), effen 64×64 {}→{} bytes (×{} compressie) {}",
        lossless,
        enc.len(),
        64 * 64 * 4,
        solid_enc.len(),
        ratio,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
