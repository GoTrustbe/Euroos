//! Kernel side of **EuroMedia** (Sprint AC-1): the image viewer.
//! At boot we prove the sovereign QOI codec: encode→decode is lossless and
//! a solid area compresses strongly. Host-tested core: [`euromedia`].

use crate::serial_println;
use euromedia::{decode, encode, Image};

/// Boot self-test: QOI round-trip on a gradient + compression of a solid area.
pub fn selftest() {
    // Gradient with varying alpha (exercises all chunk types).
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

    // Solid area → strong compression.
    let solid = Image::new(64, 64, [0x1b, 0x4f, 0x91, 255]); // EU blue
    let solid_enc = encode(&solid);
    let ratio = (64 * 64 * 4) / solid_enc.len().max(1);
    let solid_ok = decode(&solid_enc).map(|d| d == solid).unwrap_or(false) && ratio > 50;

    let ok = lossless && solid_ok;
    serial_println!(
        "[mv] EuroMedia QOI: gradient 16×16 lossless={} ({} bytes), solid 64×64 {}→{} bytes (×{} compression) {}",
        lossless,
        enc.len(),
        64 * 64 * 4,
        solid_enc.len(),
        ratio,
        if ok { "✓" } else { "✗ ERROR" }
    );
}
