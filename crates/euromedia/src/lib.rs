//! EuroMedia — de afbeeldingsviewer-kern van EuroOS (Sprint AC-1).
//!
//! Een soevereine **QOI**-codec (Quite OK Image): een eenvoudig, modern,
//! patentvrij beeldformaat dat volledig van scratch te implementeren is — geen
//! `libpng`/`libjpeg`-afhankelijkheid. Decodeert en codeert lossless RGBA, plus
//! een [`Image`]-model met basisbewerkingen (pixeltoegang, bijsnijden, flip).
//! PNG/JPEG/WebP komen er als aparte decoders bij; QOI bewijst de pijplijn.
//!
//! Pure `no_std`-logica, host-getest. Geen `unsafe`.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// Een RGBA-pixel.
pub type Rgba = [u8; 4];

/// Een afbeelding: breedte × hoogte RGBA-pixels (rij na rij).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
}

impl Image {
    /// Maak een afbeelding gevuld met één kleur.
    pub fn new(width: u32, height: u32, fill: Rgba) -> Self {
        Image { width, height, pixels: vec![fill; (width * height) as usize] }
    }
    /// Lees een pixel (None buiten bereik).
    pub fn get(&self, x: u32, y: u32) -> Option<Rgba> {
        if x < self.width && y < self.height {
            Some(self.pixels[(y * self.width + x) as usize])
        } else {
            None
        }
    }
    /// Zet een pixel.
    pub fn set(&mut self, x: u32, y: u32, px: Rgba) {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize] = px;
        }
    }
    /// Verticaal spiegelen (op zijn kop).
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
    /// Een rechthoek uitsnijden (geclampt op de randen).
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

/// Foutsoorten bij het decoderen.
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

/// Codeer een afbeelding naar een QOI-bytestroom (RGBA, lineaire colorspace).
pub fn encode(img: &Image) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&img.width.to_be_bytes());
    out.extend_from_slice(&img.height.to_be_bytes());
    out.push(4); // channels
    out.push(0); // colorspace: 0 = sRGB met lineaire alpha

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
    // Einde-marker: 7× 0x00 + 0x01.
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

/// Decodeer een QOI-bytestroom naar een [`Image`].
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Image {
        // Een afbeelding die alle chunk-types uitlokt: vlakken (RUN), kleine
        // verschillen (DIFF/LUMA), grote sprongen (RGB/RGBA) en herhaling (INDEX).
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
        // Een vlak gebied voor RUN.
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
        assert_eq!(dec, img, "QOI moet lossless zijn");
    }

    #[test]
    fn roundtrip_solid_uses_runs() {
        let img = Image::new(32, 32, [200, 100, 50, 255]);
        let enc = encode(&img);
        // Een effen vlak comprimeert sterk (header 14 + paar runs + 8 eind).
        assert!(enc.len() < 60, "effen vlak te groot: {}", enc.len());
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
        // Kap de stream af na de header → te weinig data.
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
        assert_eq!(f.get(0, 1), Some([1, 1, 1, 255])); // rij 0 → rij 1
        let c = img.crop(1, 0, 2, 2);
        assert_eq!((c.width, c.height), (2, 2));
    }

    #[test]
    fn alpha_transitions_roundtrip() {
        // Wisselende alpha lokt QOI_OP_RGBA uit.
        let mut img = Image::new(4, 1, [0, 0, 0, 255]);
        img.set(0, 0, [255, 0, 0, 255]);
        img.set(1, 0, [255, 0, 0, 64]);
        img.set(2, 0, [255, 0, 0, 200]);
        img.set(3, 0, [0, 255, 0, 200]);
        assert_eq!(decode(&encode(&img)).unwrap(), img);
    }
}
