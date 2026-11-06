//! RDP **interleaved RLE** bitmap decompression (MS-RDPBCGR 2.2.9.1.1.3.1.2.4),
//! client side, `no_std`, no `unsafe`. xrdp and Windows send TS_BITMAP_DATA with
//! the COMPRESSION flag unless the client advertises no bitmap-compression
//! support, which xrdp ignores, so a real remote desktop needs this decoder.
//!
//! The scheme is a per-scanline byte code stream shared by 15/16/24/32 bpp
//! (a pixel is 2, 3 or 4 little-endian bytes). The output is the raw bitmap in
//! the same bottom-up storage order as an uncompressed TS_BITMAP_DATA, so the
//! existing XRGB conversion handles both with a different stride.
//!
//! Ported faithfully from the FreeRDP `interleaved_decompress` code-id scheme.

use alloc::vec;
use alloc::vec::Vec;

// Code ids after ExtractCodeId.
const BG_RUN: u8 = 0x00;
const FG_RUN: u8 = 0x01;
const FGBG_IMAGE: u8 = 0x02;
const COLOR_RUN: u8 = 0x03;
const COLOR_IMAGE: u8 = 0x04;
const LITE_SET_FG_FG_RUN: u8 = 0x0C;
const LITE_SET_FG_FGBG_IMAGE: u8 = 0x0D;
const LITE_DITHERED_RUN: u8 = 0x0E;
const MEGA_MEGA_BG_RUN: u8 = 0xF0;
const MEGA_MEGA_FG_RUN: u8 = 0xF1;
const MEGA_MEGA_FGBG_IMAGE: u8 = 0xF2;
const MEGA_MEGA_COLOR_RUN: u8 = 0xF3;
const MEGA_MEGA_COLOR_IMAGE: u8 = 0xF4;
const MEGA_MEGA_SET_FG_RUN: u8 = 0xF6;
const MEGA_MEGA_SET_FGBG_IMAGE: u8 = 0xF7;
const MEGA_MEGA_DITHERED_RUN: u8 = 0xF8;
const SPECIAL_FGBG_1: u8 = 0xF9;
const SPECIAL_FGBG_2: u8 = 0xFA;
const SPECIAL_WHITE: u8 = 0xFD;
const SPECIAL_BLACK: u8 = 0xFE;

const SPECIAL_FGBG_MASK_1: u8 = 0x03; // 0b00000011
const SPECIAL_FGBG_MASK_2: u8 = 0x05; // 0b00000101

fn extract_code_id(b: u8) -> u8 {
    if (b & 0xC0) != 0xC0 {
        b >> 5 // regular: top 3 bits (0..4)
    } else if (b & 0xF0) == 0xF0 {
        b // mega-mega / special: the whole byte
    } else {
        b >> 4 // lite: top 4 bits (0xC..0xE)
    }
}

fn is_regular(code: u8) -> bool {
    matches!(code, BG_RUN | FG_RUN | FGBG_IMAGE | COLOR_RUN | COLOR_IMAGE)
}
fn is_lite(code: u8) -> bool {
    matches!(code, LITE_SET_FG_FG_RUN | LITE_SET_FG_FGBG_IMAGE | LITE_DITHERED_RUN)
}
fn is_megamega(code: u8) -> bool {
    (0xF0..=0xF8).contains(&code)
}

/// Decompress `src` into `width*height*bpp` raw bytes (bottom-up storage order,
/// no row padding). None on any malformed input (never panics, never loops
/// unbounded). `bpp` is bytes per pixel (2 for 15/16, 3 for 24, 4 for 32).
pub fn decompress(src: &[u8], width: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || !(1..=4).contains(&bpp) || width > 8192 || height > 8192 {
        return None;
    }
    let npix = width.checked_mul(height)?;
    let mut out = vec![0u8; npix.checked_mul(bpp)?];

    let read_pel = |b: &[u8], p: usize| -> u64 {
        let mut v = 0u64;
        for k in 0..bpp { v |= (b[p * bpp + k] as u64) << (8 * k); }
        v
    };
    let write_pel = |b: &mut [u8], p: usize, v: u64| {
        for k in 0..bpp { b[p * bpp + k] = ((v >> (8 * k)) & 0xFF) as u8; }
    };

    let white: u64 = (1u64 << (8 * bpp as u64)).wrapping_sub(1);
    let mut fgpel: u64 = white;
    let mut insert_fgpel = false; // "first pixel of the next run mixes in fgPel"
    let mut cur = 0usize; // output pixel cursor
    let mut i = 0usize; // input cursor

    // bounds helpers
    macro_rules! need { ($n:expr) => { if i + $n > src.len() { return None; } } }

    while i < src.len() {
        let header = src[i];
        let code = extract_code_id(header);

        // ---- run length ----
        let mut run: usize;
        if is_regular(code) {
            if code == FGBG_IMAGE {
                run = (header & 0x0F) as usize;
                if run == 0 { need!(2); run = src[i + 1] as usize + 1; i += 2; }
                else { run *= 8; i += 1; }
            } else {
                run = (header & 0x1F) as usize;
                if run == 0 { need!(2); run = src[i + 1] as usize + 32; i += 2; }
                else { i += 1; }
            }
        } else if is_lite(code) {
            if code == LITE_SET_FG_FGBG_IMAGE {
                run = (header & 0x0F) as usize;
                if run == 0 { need!(2); run = src[i + 1] as usize + 1; i += 2; }
                else { run *= 8; i += 1; }
            } else {
                run = (header & 0x0F) as usize;
                if run == 0 { need!(2); run = src[i + 1] as usize + 16; i += 2; }
                else { i += 1; }
            }
        } else if is_megamega(code) {
            need!(3);
            run = src[i + 1] as usize | ((src[i + 2] as usize) << 8);
            i += 3;
        } else {
            // special (single pixel) or unknown: handled below with run meaning 0
            run = 0;
            i += 1;
        }

        let rowdelta = width; // pixels to the line above
        let above = |o: &[u8], p: usize| -> u64 { if p >= rowdelta { read_pel(o, p - rowdelta) } else { 0 } };

        match code {
            BG_RUN | MEGA_MEGA_BG_RUN => {
                // copy from the line above; the first pixel may mix fgPel in.
                if insert_fgpel {
                    if cur >= npix { return None; }
                    let v = above(&out, cur) ^ fgpel;
                    write_pel(&mut out, cur, v); cur += 1;
                    if run > 0 { run -= 1; }
                }
                for _ in 0..run {
                    if cur >= npix { return None; }
                    let v = above(&out, cur);
                    write_pel(&mut out, cur, v); cur += 1;
                }
                // After a background run the decoder arms fgPel insertion.
                insert_fgpel = true;
            }
            FG_RUN | MEGA_MEGA_FG_RUN | LITE_SET_FG_FG_RUN | MEGA_MEGA_SET_FG_RUN => {
                if code == LITE_SET_FG_FG_RUN || code == MEGA_MEGA_SET_FG_RUN {
                    need!(bpp);
                    let mut v = 0u64; for k in 0..bpp { v |= (src[i + k] as u64) << (8 * k); }
                    fgpel = v; i += bpp;
                }
                // Foreground run: each pixel is the one above XORed with fgPel.
                for _ in 0..run {
                    if cur >= npix { return None; }
                    let v = above(&out, cur) ^ fgpel;
                    write_pel(&mut out, cur, v); cur += 1;
                }
                insert_fgpel = false;
            }
            COLOR_RUN | MEGA_MEGA_COLOR_RUN => {
                need!(bpp);
                let mut v = 0u64; for k in 0..bpp { v |= (src[i + k] as u64) << (8 * k); }
                i += bpp;
                for _ in 0..run {
                    if cur >= npix { return None; }
                    write_pel(&mut out, cur, v); cur += 1;
                }
                insert_fgpel = false;
            }
            COLOR_IMAGE | MEGA_MEGA_COLOR_IMAGE => {
                need!(run * bpp);
                for n in 0..run {
                    if cur >= npix { return None; }
                    let mut v = 0u64; for k in 0..bpp { v |= (src[i + n * bpp + k] as u64) << (8 * k); }
                    write_pel(&mut out, cur, v); cur += 1;
                }
                i += run * bpp;
                insert_fgpel = false;
            }
            FGBG_IMAGE | MEGA_MEGA_FGBG_IMAGE | LITE_SET_FG_FGBG_IMAGE | MEGA_MEGA_SET_FGBG_IMAGE => {
                if code == LITE_SET_FG_FGBG_IMAGE || code == MEGA_MEGA_SET_FGBG_IMAGE {
                    need!(bpp);
                    let mut v = 0u64; for k in 0..bpp { v |= (src[i + k] as u64) << (8 * k); }
                    fgpel = v; i += bpp;
                }
                // run pixels described by ceil(run/8) mask bytes: bit set = fgPel mixed.
                let nbytes = run.div_ceil(8);
                need!(nbytes);
                let mut done = 0usize;
                for b in 0..nbytes {
                    let mask = src[i + b];
                    let bits = core::cmp::min(8, run - done);
                    for bit in 0..bits {
                        if cur >= npix { return None; }
                        let set = (mask >> bit) & 1 != 0;
                        let base = above(&out, cur);
                        let v = if set { base ^ fgpel } else { base };
                        write_pel(&mut out, cur, v); cur += 1;
                    }
                    done += bits;
                }
                i += nbytes;
                insert_fgpel = false;
            }
            SPECIAL_FGBG_1 => {
                // 8 pixels, mask 0x03
                for bit in 0..8 {
                    if cur >= npix { return None; }
                    let set = (SPECIAL_FGBG_MASK_1 >> bit) & 1 != 0;
                    let base = above(&out, cur);
                    write_pel(&mut out, cur, if set { base ^ fgpel } else { base }); cur += 1;
                }
                insert_fgpel = false;
            }
            SPECIAL_FGBG_2 => {
                for bit in 0..8 {
                    if cur >= npix { return None; }
                    let set = (SPECIAL_FGBG_MASK_2 >> bit) & 1 != 0;
                    let base = above(&out, cur);
                    write_pel(&mut out, cur, if set { base ^ fgpel } else { base }); cur += 1;
                }
                insert_fgpel = false;
            }
            SPECIAL_WHITE => { if cur >= npix { return None; } write_pel(&mut out, cur, white); cur += 1; insert_fgpel = false; }
            SPECIAL_BLACK => { if cur >= npix { return None; } write_pel(&mut out, cur, 0); cur += 1; insert_fgpel = false; }
            LITE_DITHERED_RUN | MEGA_MEGA_DITHERED_RUN => {
                // two alternating pixels, run times
                need!(bpp * 2);
                let mut a = 0u64; let mut b = 0u64;
                for k in 0..bpp { a |= (src[i + k] as u64) << (8 * k); b |= (src[i + bpp + k] as u64) << (8 * k); }
                i += bpp * 2;
                for n in 0..run * 2 {
                    if cur >= npix { return None; }
                    write_pel(&mut out, cur, if n % 2 == 0 { a } else { b }); cur += 1;
                }
                insert_fgpel = false;
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_image_is_raw_pixels() {
        // COLOR_IMAGE (code 4) regular, run 2, 16bpp: two raw pixels.
        let src = [0x80 | 2u8, 0x34, 0x12, 0x78, 0x56];
        let out = decompress(&src, 2, 1, 2).unwrap();
        assert_eq!(out, [0x34, 0x12, 0x78, 0x56]);
    }
    #[test]
    fn color_run_repeats_one_pixel() {
        // COLOR_RUN (code 3) regular, run 3, pixel 0xBEEF.
        let src = [0x60 | 3u8, 0xEF, 0xBE];
        let out = decompress(&src, 3, 1, 2).unwrap();
        assert_eq!(out, [0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE]);
    }
    #[test]
    fn special_black_and_white() {
        let src = [SPECIAL_WHITE, SPECIAL_BLACK];
        let out = decompress(&src, 2, 1, 2).unwrap();
        assert_eq!(out, [0xFF, 0xFF, 0x00, 0x00]);
    }
    #[test]
    fn bg_run_copies_from_line_above() {
        // line 0: COLOR_IMAGE of 2 px (AA, BB); line 1: BG_RUN of 2 copies the line above.
        let src = [0x80 | 2u8, 0xAA, 0x00, 0xBB, 0x00, /*bg run 2*/ 0x00 | 2u8];
        let out = decompress(&src, 2, 2, 2).unwrap();
        // storage order: line0 then line1, each 2 px * 2 bytes
        assert_eq!(&out[0..4], &[0xAA, 0x00, 0xBB, 0x00]);
        assert_eq!(&out[4..8], &[0xAA, 0x00, 0xBB, 0x00]);
    }
    #[test]
    fn malformed_never_panics() {
        for b in 0u8..=255 {
            let _ = decompress(&[b], 4, 4, 2);
            let _ = decompress(&[b, 1, 2, 3], 4, 4, 2);
        }
        assert!(decompress(&[0x80 | 5u8, 1, 2], 4, 4, 2).is_none()); // color image past end
    }
}
