//! EuroMedia — the image-viewer core of EuroOS (Sprint AC-1).
//!
//! A sovereign **QOI** codec (Quite OK Image): a simple, modern,
//! patent-free image format that can be fully implemented from scratch — no
//! `libpng`/`libjpeg` dependency. Decodes and encodes lossless RGBA, plus
//! an [`Image`] model with basic operations (pixel access, cropping, flip).
//! PNG/JPEG/WebP will be added as separate decoders; QOI proves the pipeline.
//!
//! Pure `no_std` logic, host-tested. No `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod jpeg;
pub use jpeg::{decode_jpeg, JpegError};

use alloc::vec;
use alloc::vec::Vec;

/// An RGBA pixel.
pub type Rgba = [u8; 4];

/// An image: width × height RGBA pixels (row by row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
}

impl Image {
    /// Create an image filled with a single color.
    pub fn new(width: u32, height: u32, fill: Rgba) -> Self {
        Image { width, height, pixels: vec![fill; (width * height) as usize] }
    }
    /// Read a pixel (None out of bounds).
    pub fn get(&self, x: u32, y: u32) -> Option<Rgba> {
        if x < self.width && y < self.height {
            Some(self.pixels[(y * self.width + x) as usize])
        } else {
            None
        }
    }
    /// Set a pixel.
    pub fn set(&mut self, x: u32, y: u32, px: Rgba) {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize] = px;
        }
    }
    /// Flip vertically (upside down).
    pub fn flip_vertical(&self) -> Image {
        let mut out = self.pixels.clone();
        for y in 0..self.height {
            let src = self.height - 1 - y;
            let d = (y * self.width) as usize;
            let s = (src * self.width) as usize;
            out[d..d + self.width as usize]
                .copy_from_slice(&self.pixels[s..s + self.width as usize]);
        }
        Image { width: self.width, height: self.height, pixels: out }
    }
    /// Crop out a rectangle (clamped to the edges).
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Image {
        let w = w.min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                pixels.push(self.get(x + col, y + row).unwrap());
            }
        }
        Image { width: w, height: h, pixels }
    }
}

/// Error kinds during decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QoiError {
    BadMagic,
    Truncated,
    DimensionMismatch,
}

const QOI_OP_INDEX: u8 = 0x00; // 00xxxxxx
const QOI_OP_DIFF: u8 = 0x40; // 01xxxxxx
const QOI_OP_LUMA: u8 = 0x80; // 10xxxxxx
const QOI_OP_RUN: u8 = 0xC0; // 11xxxxxx
const QOI_OP_RGB: u8 = 0xFE;
const QOI_OP_RGBA: u8 = 0xFF;
const MASK2: u8 = 0xC0;

fn hash(px: Rgba) -> usize {
    (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11) % 64
}

/// Encode an image into a QOI byte stream (RGBA, linear colorspace).
pub fn encode(img: &Image) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&img.width.to_be_bytes());
    out.extend_from_slice(&img.height.to_be_bytes());
    out.push(4); // channels
    out.push(0); // colorspace: 0 = sRGB with linear alpha

    let mut index = [[0u8; 4]; 64];
    let mut prev: Rgba = [0, 0, 0, 255];
    let mut run: u8 = 0;

    let n = img.pixels.len();
    for (i, &px) in img.pixels.iter().enumerate() {
        if px == prev {
            run += 1;
            if run == 62 || i == n - 1 {
                out.push(QOI_OP_RUN | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(QOI_OP_RUN | (run - 1));
            run = 0;
        }
        let h = hash(px);
        if index[h] == px {
            out.push(QOI_OP_INDEX | h as u8);
        } else {
            index[h] = px;
            if px[3] == prev[3] {
                let vr = px[0].wrapping_sub(prev[0]) as i8;
                let vg = px[1].wrapping_sub(prev[1]) as i8;
                let vb = px[2].wrapping_sub(prev[2]) as i8;
                let vg_r = vr.wrapping_sub(vg);
                let vg_b = vb.wrapping_sub(vg);
                if (-2..=1).contains(&vr) && (-2..=1).contains(&vg) && (-2..=1).contains(&vb) {
                    out.push(
                        QOI_OP_DIFF
                            | (((vr + 2) as u8) << 4)
                            | (((vg + 2) as u8) << 2)
                            | ((vb + 2) as u8),
                    );
                } else if (-32..=31).contains(&vg)
                    && (-8..=7).contains(&vg_r)
                    && (-8..=7).contains(&vg_b)
                {
                    out.push(QOI_OP_LUMA | ((vg + 32) as u8));
                    out.push((((vg_r + 8) as u8) << 4) | ((vg_b + 8) as u8));
                } else {
                    out.push(QOI_OP_RGB);
                    out.extend_from_slice(&px[0..3]);
                }
            } else {
                out.push(QOI_OP_RGBA);
                out.extend_from_slice(&px);
            }
        }
        prev = px;
    }
    // End marker: 7× 0x00 + 0x01.
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

/// Decode a QOI byte stream into an [`Image`].
pub fn decode(data: &[u8]) -> Result<Image, QoiError> {
    if data.len() < 14 || &data[0..4] != b"qoif" {
        return Err(QoiError::BadMagic);
    }
    let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let channels = data[12];
    let n_pixels = (width as usize) * (height as usize);

    let mut index = [[0u8; 4]; 64];
    let mut px: Rgba = [0, 0, 0, 255];
    let mut pixels: Vec<Rgba> = Vec::with_capacity(n_pixels);
    let mut p = 14;

    while pixels.len() < n_pixels {
        if p >= data.len() {
            return Err(QoiError::Truncated);
        }
        let b0 = data[p];
        p += 1;
        if b0 == QOI_OP_RGB {
            if p + 3 > data.len() {
                return Err(QoiError::Truncated);
            }
            px = [data[p], data[p + 1], data[p + 2], px[3]];
            p += 3;
        } else if b0 == QOI_OP_RGBA {
            if p + 4 > data.len() {
                return Err(QoiError::Truncated);
            }
            px = [data[p], data[p + 1], data[p + 2], data[p + 3]];
            p += 4;
        } else {
            match b0 & MASK2 {
                QOI_OP_INDEX => {
                    px = index[(b0 & 0x3F) as usize];
                }
                QOI_OP_DIFF => {
                    let vr = ((b0 >> 4) & 0x03) as i8 - 2;
                    let vg = ((b0 >> 2) & 0x03) as i8 - 2;
                    let vb = (b0 & 0x03) as i8 - 2;
                    px = [
                        px[0].wrapping_add(vr as u8),
                        px[1].wrapping_add(vg as u8),
                        px[2].wrapping_add(vb as u8),
                        px[3],
                    ];
                }
                QOI_OP_LUMA => {
                    if p >= data.len() {
                        return Err(QoiError::Truncated);
                    }
                    let b1 = data[p];
                    p += 1;
                    let vg = (b0 & 0x3F) as i8 - 32;
                    let vg_r = ((b1 >> 4) & 0x0F) as i8 - 8;
                    let vg_b = (b1 & 0x0F) as i8 - 8;
                    let vr = vg + vg_r;
                    let vb = vg + vg_b;
                    px = [
                        px[0].wrapping_add(vr as u8),
                        px[1].wrapping_add(vg as u8),
                        px[2].wrapping_add(vb as u8),
                        px[3],
                    ];
                }
                QOI_OP_RUN => {
                    let run = (b0 & 0x3F) + 1;
                    for _ in 0..run {
                        if pixels.len() < n_pixels {
                            pixels.push(px);
                        }
                    }
                    index[hash(px)] = px;
                    continue;
                }
                _ => unreachable!(),
            }
        }
        index[hash(px)] = px;
        pixels.push(px);
    }

    if pixels.len() != n_pixels {
        return Err(QoiError::DimensionMismatch);
    }
    let _ = channels;
    Ok(Image { width, height, pixels })
}

/// Error kinds when decoding a PPM/PGM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PpmError {
    BadMagic,
    BadHeader,
    Truncated,
    Unsupported,
}

/// Decode a **Netpbm** image (PPM `P3`/`P6` or PGM `P2`/`P5`) into an [`Image`].
/// A simple, header-readable format — handy alongside QOI because much tooling
/// writes it directly. Supports 8-bit (maxval ≤ 255). No `unsafe`.
pub fn decode_ppm(data: &[u8]) -> Result<Image, PpmError> {
    if data.len() < 2 || data[0] != b'P' {
        return Err(PpmError::BadMagic);
    }
    let magic = data[1];
    let (ascii, gray) = match magic {
        b'2' => (true, true),   // PGM ascii
        b'3' => (true, false),  // PPM ascii
        b'5' => (false, true),  // PGM binary
        b'6' => (false, false), // PPM binary
        _ => return Err(PpmError::BadMagic),
    };
    // Read header tokens (width, height, maxval) — whitespace + #-comments.
    let mut pos = 2usize;
    let next_token = |pos: &mut usize| -> Option<u32> {
        loop {
            while *pos < data.len() && (data[*pos] as char).is_ascii_whitespace() {
                *pos += 1;
            }
            if *pos < data.len() && data[*pos] == b'#' {
                while *pos < data.len() && data[*pos] != b'\n' {
                    *pos += 1;
                }
                continue;
            }
            break;
        }
        let start = *pos;
        while *pos < data.len() && (data[*pos] as char).is_ascii_digit() {
            *pos += 1;
        }
        if *pos == start {
            return None;
        }
        core::str::from_utf8(&data[start..*pos]).ok()?.parse().ok()
    };
    let width = next_token(&mut pos).ok_or(PpmError::BadHeader)?;
    let height = next_token(&mut pos).ok_or(PpmError::BadHeader)?;
    let maxval = next_token(&mut pos).ok_or(PpmError::BadHeader)?;
    if width == 0 || height == 0 || maxval == 0 || maxval > 255 {
        return Err(PpmError::Unsupported);
    }
    let n = (width * height) as usize;
    let mut pixels = Vec::with_capacity(n);
    if ascii {
        // ASCII samples: 1 (gray) or 3 (rgb) numbers per pixel.
        for _ in 0..n {
            if gray {
                let v = next_token(&mut pos).ok_or(PpmError::Truncated)? as u8;
                pixels.push([v, v, v, 255]);
            } else {
                let r = next_token(&mut pos).ok_or(PpmError::Truncated)? as u8;
                let g = next_token(&mut pos).ok_or(PpmError::Truncated)? as u8;
                let b = next_token(&mut pos).ok_or(PpmError::Truncated)? as u8;
                pixels.push([r, g, b, 255]);
            }
        }
    } else {
        // Binary: exactly one whitespace after maxval, then raw bytes.
        pos += 1; // the separating whitespace
        let per = if gray { 1 } else { 3 };
        if pos + n * per > data.len() {
            return Err(PpmError::Truncated);
        }
        for i in 0..n {
            if gray {
                let v = data[pos + i];
                pixels.push([v, v, v, 255]);
            } else {
                let o = pos + i * 3;
                pixels.push([data[o], data[o + 1], data[o + 2], 255]);
            }
        }
    }
    Ok(Image { width, height, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Image {
        // An image that triggers all chunk types: flat areas (RUN), small
        // differences (DIFF/LUMA), large jumps (RGB/RGBA) and repetition (INDEX).
        let mut img = Image::new(8, 8, [10, 20, 30, 255]);
        for y in 0..8 {
            for x in 0..8 {
                let r = (x * 16) as u8;
                let g = (y * 16) as u8;
                let b = ((x + y) * 8) as u8;
                let a = if (x + y) % 5 == 0 { 128 } else { 255 };
                img.set(x, y, [r, g, b, a]);
            }
        }
        // A flat region for RUN.
        for x in 0..8 {
            img.set(x, 3, [77, 77, 77, 255]);
        }
        img
    }

    #[test]
    fn roundtrip_lossless() {
        let img = sample();
        let enc = encode(&img);
        assert_eq!(&enc[0..4], b"qoif");
        let dec = decode(&enc).unwrap();
        assert_eq!(dec.width, 8);
        assert_eq!(dec.height, 8);
        assert_eq!(dec, img, "QOI must be lossless");
    }

    #[test]
    fn roundtrip_solid_uses_runs() {
        let img = Image::new(32, 32, [200, 100, 50, 255]);
        let enc = encode(&img);
        // A solid area compresses strongly (header 14 + a few runs + 8 end).
        assert!(enc.len() < 60, "solid area too large: {}", enc.len());
        assert_eq!(decode(&enc).unwrap(), img);
    }

    #[test]
    fn bad_magic_rejected() {
        assert_eq!(decode(b"nope............"), Err(QoiError::BadMagic));
        assert_eq!(decode(&[]), Err(QoiError::BadMagic));
    }

    #[test]
    fn truncated_rejected() {
        let enc = encode(&Image::new(4, 4, [1, 2, 3, 255]));
        // Cut off the stream after the header → too little data.
        assert!(matches!(decode(&enc[0..15]), Err(QoiError::Truncated)));
    }

    #[test]
    fn image_ops() {
        let mut img = Image::new(3, 2, [0, 0, 0, 255]);
        img.set(0, 0, [1, 1, 1, 255]);
        img.set(2, 1, [9, 9, 9, 255]);
        assert_eq!(img.get(0, 0), Some([1, 1, 1, 255]));
        assert_eq!(img.get(3, 0), None);
        let f = img.flip_vertical();
        assert_eq!(f.get(0, 1), Some([1, 1, 1, 255])); // row 0 → row 1
        let c = img.crop(1, 0, 2, 2);
        assert_eq!((c.width, c.height), (2, 2));
    }

    #[test]
    fn alpha_transitions_roundtrip() {
        // Alternating alpha triggers QOI_OP_RGBA.
        let mut img = Image::new(4, 1, [0, 0, 0, 255]);
        img.set(0, 0, [255, 0, 0, 255]);
        img.set(1, 0, [255, 0, 0, 64]);
        img.set(2, 0, [255, 0, 0, 200]);
        img.set(3, 0, [0, 255, 0, 200]);
        assert_eq!(decode(&encode(&img)).unwrap(), img);
    }

    #[test]
    fn ppm_p3_ascii() {
        // 2×2 PPM with a comment + extra whitespace.
        let src = b"P3\n# a comment\n2 2\n255\n 255 0 0  0 255 0\n0 0 255  255 255 0\n";
        let img = decode_ppm(src).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.get(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.get(1, 0), Some([0, 255, 0, 255]));
        assert_eq!(img.get(0, 1), Some([0, 0, 255, 255]));
        assert_eq!(img.get(1, 1), Some([255, 255, 0, 255]));
    }

    #[test]
    fn ppm_p6_binary() {
        // 2×1 binary PPM: red, green.
        let mut src = Vec::from(&b"P6\n2 1\n255\n"[..]);
        src.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
        let img = decode_ppm(&src).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.get(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.get(1, 0), Some([0, 255, 0, 255]));
    }

    #[test]
    fn ppm_p5_gray_and_errors() {
        let mut src = Vec::from(&b"P5\n2 1\n255\n"[..]);
        src.extend_from_slice(&[40, 200]);
        let img = decode_ppm(&src).unwrap();
        assert_eq!(img.get(0, 0), Some([40, 40, 40, 255]));
        assert_eq!(img.get(1, 0), Some([200, 200, 200, 255]));
        assert_eq!(decode_ppm(b"PX\n1 1\n255\n\0"), Err(PpmError::BadMagic));
        assert_eq!(decode_ppm(b"P6\n2 2\n255\n\0"), Err(PpmError::Truncated));
    }
}

// ── BMP decoder (Windows BITMAPINFOHEADER, 24/32-bit uncompressed) ───────────

/// Errors from [`decode_bmp`].
#[derive(Debug, PartialEq, Eq)]
pub enum BmpError {
    NotBmp,
    Unsupported,
    Truncated,
}

fn u16le(b: &[u8], o: usize) -> u32 {
    b[o] as u32 | ((b[o + 1] as u32) << 8)
}
fn u32le_(b: &[u8], o: usize) -> u32 {
    b[o] as u32 | ((b[o + 1] as u32) << 8) | ((b[o + 2] as u32) << 16) | ((b[o + 3] as u32) << 24)
}

/// Decode a Windows BMP (24- or 32-bit, uncompressed BI_RGB). Rows are stored
/// bottom-up and padded to 4 bytes; we flip and unpad into top-down RGBA.
pub fn decode_bmp(data: &[u8]) -> Result<Image, BmpError> {
    if data.len() < 54 || &data[0..2] != b"BM" {
        return Err(BmpError::NotBmp);
    }
    let pix_off = u32le_(data, 10) as usize;
    let header = u32le_(data, 14); // DIB header size
    if header < 40 {
        return Err(BmpError::Unsupported); // only BITMAPINFOHEADER+
    }
    let width = u32le_(data, 18) as i32;
    let height_raw = u32le_(data, 22) as i32;
    let bpp = u16le(data, 28);
    let compression = u32le_(data, 30);
    if compression != 0 || (bpp != 24 && bpp != 32) || width <= 0 {
        return Err(BmpError::Unsupported);
    }
    let top_down = height_raw < 0;
    let height = height_raw.unsigned_abs();
    let width = width as u32;
    let bytes_pp = (bpp / 8) as usize;
    let row_size = (width as usize * bytes_pp).div_ceil(4) * 4; // padded to 4 bytes
    let mut img = Image::new(width, height, [0, 0, 0, 255]);
    for row in 0..height {
        let src_row = if top_down { row } else { height - 1 - row };
        let base = pix_off + src_row as usize * row_size;
        if base + width as usize * bytes_pp > data.len() {
            return Err(BmpError::Truncated);
        }
        for x in 0..width {
            let o = base + x as usize * bytes_pp;
            // BMP stores BGR(A).
            let b = data[o];
            let g = data[o + 1];
            let r = data[o + 2];
            let a = if bytes_pp == 4 { data[o + 3] } else { 255 };
            img.set(x, row, [r, g, b, a]);
        }
    }
    Ok(img)
}

// ── PNG decoder (baseline: 8-bit greyscale / RGB / RGBA, no interlace) ───────

/// Errors from [`decode_png`].
#[derive(Debug, PartialEq, Eq)]
pub enum PngError {
    NotPng,
    Unsupported,
    Truncated,
    BadFilter,
    Inflate,
}

/// Decode a baseline PNG (bit depth 8; colour type 0 grey, 2 RGB, 6 RGBA;
/// no interlace) into RGBA. This is the format the overwhelming majority of
/// screenshots and simple graphics use; palette/16-bit/interlaced are honestly
/// rejected as Unsupported rather than mis-decoded.
pub fn decode_png(data: &[u8]) -> Result<Image, PngError> {
    const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if data.len() < 8 || data[0..8] != SIG {
        return Err(PngError::NotPng);
    }
    let mut pos = 8;
    let (mut width, mut height, mut colour) = (0u32, 0u32, 0u8);
    let mut depth;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let len = ((data[pos] as usize) << 24)
            | ((data[pos + 1] as usize) << 16)
            | ((data[pos + 2] as usize) << 8)
            | data[pos + 3] as usize;
        let ctype = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        if body_start + len + 4 > data.len() {
            return Err(PngError::Truncated);
        }
        let body = &data[body_start..body_start + len];
        match ctype {
            b"IHDR" => {
                if len < 13 {
                    return Err(PngError::Truncated);
                }
                width = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                height = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                depth = body[8];
                colour = body[9];
                let interlace = body[12];
                if depth != 8 || interlace != 0 || !matches!(colour, 0 | 2 | 6) {
                    return Err(PngError::Unsupported);
                }
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = body_start + len + 4; // skip body + CRC
    }
    if width == 0 || height == 0 {
        return Err(PngError::Unsupported);
    }
    let channels = match colour {
        0 => 1usize,
        2 => 3,
        6 => 4,
        _ => return Err(PngError::Unsupported),
    };
    let raw = euroflate::zlib_decompress(&idat).map_err(|_| PngError::Inflate)?;
    let bpp = channels; // bytes per pixel at depth 8
    let stride = width as usize * bpp;
    if raw.len() < (stride + 1) * height as usize {
        return Err(PngError::Truncated);
    }
    // Reverse the PNG scanline filters into a flat RGB(A) buffer.
    let mut prev: Vec<u8> = vec![0; stride];
    let mut img = Image::new(width, height, [0, 0, 0, 255]);
    let mut off = 0usize;
    for y in 0..height as usize {
        let filter = raw[off];
        off += 1;
        let mut line = raw[off..off + stride].to_vec();
        off += stride;
        for i in 0..stride {
            let a = if i >= bpp { line[i - bpp] as i32 } else { 0 };
            let b = prev[i] as i32;
            let c = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
            let recon = match filter {
                0 => line[i] as i32,
                1 => line[i] as i32 + a,
                2 => line[i] as i32 + b,
                3 => line[i] as i32 + (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    let pred = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                    line[i] as i32 + pred
                }
                _ => return Err(PngError::BadFilter),
            };
            line[i] = (recon & 0xff) as u8;
        }
        for x in 0..width as usize {
            let o = x * bpp;
            let px = match colour {
                0 => [line[o], line[o], line[o], 255],
                2 => [line[o], line[o + 1], line[o + 2], 255],
                6 => [line[o], line[o + 1], line[o + 2], line[o + 3]],
                _ => unreachable!(),
            };
            img.set(x as u32, y as u32, px);
        }
        prev = line;
    }
    Ok(img)
}

/// Encode an image as a baseline PNG (8-bit RGBA, filter 0). Round-trips with
/// [`decode_png`]; small and correct rather than maximally compressed.
pub fn encode_png(img: &Image) -> Vec<u8> {
    let mut raw = Vec::with_capacity((img.width as usize * 4 + 1) * img.height as usize);
    for y in 0..img.height {
        raw.push(0); // filter: none
        for x in 0..img.width {
            let p = img.get(x, y).unwrap_or([0, 0, 0, 255]);
            raw.extend_from_slice(&p);
        }
    }
    let idat = euroflate::zlib_compress(&raw);
    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    let chunk = |out: &mut Vec<u8>, name: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(body);
        let mut crc_in = Vec::with_capacity(4 + body.len());
        crc_in.extend_from_slice(name);
        crc_in.extend_from_slice(body);
        out.extend_from_slice(&euroflate::crc32(&crc_in).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // depth 8, colour 6 (RGBA), no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
include!("imgtests.rs");

#[cfg(test)]
include!("jpegtests.rs");
