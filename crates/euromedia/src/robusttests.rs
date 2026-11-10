//! Hostile input for the decoders and the paint bucket: nothing a file or a
//! click can contain may panic (in the kernel a panic is a halt) or make a
//! decoder allocate for a size that only the header claims.

use super::*;
use std::panic::catch_unwind;

const WHITE: Rgba = [255, 255, 255, 255];
const RED: Rgba = [255, 0, 0, 255];

// ── flood fill ──────────────────────────────────────────────────────────────

#[test]
fn fill_click_outside_the_canvas_changes_nothing() {
    let mut img = Image::new(8, 6, WHITE);
    let before = img.clone();
    for (x, y) in [(-1, 0), (0, -1), (8, 0), (0, 6), (8, 6), (i64::MAX, 3), (3, i64::MIN), (1 << 40, 1 << 40)] {
        assert_eq!(flood_fill(&mut img, x, y, RED), 0, "click at ({x},{y})");
    }
    assert_eq!(img, before);
}

#[test]
fn fill_whole_canvas_and_same_colour() {
    let mut img = Image::new(640, 470, WHITE);
    assert_eq!(flood_fill(&mut img, 639, 469, RED), 640 * 470);
    assert!(img.pixels.iter().all(|p| *p == RED));
    assert_eq!(flood_fill(&mut img, 0, 0, RED), 0);
}

/// The straightforward 4-neighbour fill the scanline version replaces.
fn reference_fill(img: &mut Image, sx: u32, sy: u32, col: Rgba) -> usize {
    let target = img.get(sx, sy).unwrap();
    if target == col {
        return 0;
    }
    let mut n = 0;
    let mut stack = vec![(sx, sy)];
    while let Some((x, y)) = stack.pop() {
        if img.get(x, y) != Some(target) {
            continue;
        }
        img.set(x, y, col);
        n += 1;
        if x > 0 { stack.push((x - 1, y)); }
        if x + 1 < img.width { stack.push((x + 1, y)); }
        if y > 0 { stack.push((x, y - 1)); }
        if y + 1 < img.height { stack.push((x, y + 1)); }
    }
    n
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn fill_matches_the_reference_on_random_images() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for round in 0..400 {
        let (w, h) = (1 + rng.below(24) as u32, 1 + rng.below(24) as u32);
        let mut img = Image::new(w, h, WHITE);
        // Random walls: sparse to dense, so both big areas and mazes occur.
        let density = 1 + rng.below(4);
        for p in img.pixels.iter_mut() {
            if rng.below(5) < density {
                *p = [0, 0, 0, 255];
            }
        }
        let (sx, sy) = (rng.below(w as usize) as u32, rng.below(h as usize) as u32);
        let mut want = img.clone();
        let n_want = reference_fill(&mut want, sx, sy, RED);
        let n = flood_fill(&mut img, sx as i64, sy as i64, RED);
        assert_eq!(n, n_want, "round {round}: {w}x{h} from ({sx},{sy})");
        assert_eq!(img, want, "round {round}");
    }
}

#[test]
fn fill_survives_an_image_whose_size_lies() {
    let mut img = Image { width: 100, height: 100, pixels: vec![WHITE; 10] };
    assert_eq!(flood_fill(&mut img, 5, 0, RED), 0);
    let mut img = Image { width: u32::MAX, height: u32::MAX, pixels: vec![WHITE; 10] };
    assert_eq!(flood_fill(&mut img, 5, 5, RED), 0);
}

// ── header bombs: a few bytes that claim a huge image ──────────────────────

#[test]
fn qoi_header_claiming_4_gigapixels_is_refused() {
    let mut d = b"qoif".to_vec();
    d.extend_from_slice(&65535u32.to_be_bytes());
    d.extend_from_slice(&65535u32.to_be_bytes());
    d.extend_from_slice(&[4, 0]);
    assert_eq!(decode(&d), Err(QoiError::DimensionMismatch));
    // Within the pixel limit, but the 14 bytes cannot hold the pixels.
    let mut d = b"qoif".to_vec();
    d.extend_from_slice(&4000u32.to_be_bytes());
    d.extend_from_slice(&4000u32.to_be_bytes());
    d.extend_from_slice(&[4, 0]);
    assert_eq!(decode(&d), Err(QoiError::Truncated));
}

#[test]
fn ppm_header_claiming_a_huge_image_is_refused() {
    assert_eq!(decode_ppm(b"P6 65535 65535 255\n"), Err(PpmError::Unsupported));
    assert_eq!(decode_ppm(b"P6 4294967295 2 255\n"), Err(PpmError::Unsupported));
    assert_eq!(decode_ppm(b"P6 4000 4000 255\nabc"), Err(PpmError::Truncated));
    assert_eq!(decode_ppm(b"P3 4000 4000 255\n1 2 3"), Err(PpmError::Truncated));
}

fn bmp_header(width: i32, height: i32) -> Vec<u8> {
    let mut d = vec![0u8; 54];
    d[0] = b'B';
    d[1] = b'M';
    d[10..14].copy_from_slice(&54u32.to_le_bytes());
    d[14..18].copy_from_slice(&40u32.to_le_bytes());
    d[18..22].copy_from_slice(&width.to_le_bytes());
    d[22..26].copy_from_slice(&height.to_le_bytes());
    d[26] = 1;
    d[28] = 24;
    d
}

#[test]
fn bmp_header_claiming_a_huge_image_is_refused() {
    assert_eq!(decode_bmp(&bmp_header(i32::MAX, i32::MAX)), Err(BmpError::Truncated));
    assert_eq!(decode_bmp(&bmp_header(60000, i32::MIN)), Err(BmpError::Truncated));
    assert_eq!(decode_bmp(&bmp_header(3000, 3000)), Err(BmpError::Truncated));
    // A complete file over the pixel limit is refused as unsupported.
    let mut big = bmp_header(1, 16 * 1024 * 1024 + 1);
    big.resize(54 + 4 * (16 * 1024 * 1024 + 1), 0);
    assert_eq!(decode_bmp(&big), Err(BmpError::Unsupported));
}

#[test]
fn png_and_jpeg_claiming_65535_squared_are_refused() {
    // IHDR 65535 x 65535, 8-bit RGBA, then an empty IDAT.
    let mut d = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
    d.extend_from_slice(b"IHDR");
    d.extend_from_slice(&65535u32.to_be_bytes());
    d.extend_from_slice(&65535u32.to_be_bytes());
    d.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(decode_png(&d), Err(PngError::Unsupported));

    let mut j = crate::jpegtests::JGRAY.to_vec();
    let sof = j.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    j[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // height, width
    assert_eq!(decode_jpeg(&j), Err(JpegError::Unsupported));
}

#[test]
fn jpeg_cut_off_in_the_scan_is_reported_not_padded() {
    let j = crate::jpegtests::J420;
    let sos = j.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
    assert!(decode_jpeg(j).is_ok());
    assert_eq!(decode_jpeg(&j[..sos + 20]), Err(JpegError::Truncated));
}

#[test]
fn try_new_and_pixel_count_limits() {
    assert_eq!(pixel_count(4096, 4096), Some(16 * 1024 * 1024));
    assert_eq!(pixel_count(4097, 4096), None);
    assert_eq!(pixel_count(u32::MAX, u32::MAX), None);
    assert!(Image::try_new(4097, 4096, WHITE).is_none());
    let img = Image::try_new(3, 2, RED).unwrap();
    assert_eq!(img, Image::new(3, 2, RED));
    assert_eq!(img.try_clone().unwrap(), img);
    assert_eq!(img.byte_len(), 24);
}

// ── mutation run: every truncation and many corruptions of real files ──────

fn decode_all(d: &[u8]) {
    let _ = decode(d);
    let _ = decode_ppm(d);
    let _ = decode_bmp(d);
    let _ = decode_png(d);
    let _ = decode_jpeg(d);
}

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let img = {
        let mut i = Image::new(9, 7, WHITE);
        for (n, p) in i.pixels.iter_mut().enumerate() {
            *p = [(n * 37) as u8, (n * 11) as u8, (n / 3) as u8, if n % 5 == 0 { 128 } else { 255 }];
        }
        i
    };
    vec![
        ("qoi", encode(&img)),
        ("ppm-p6", b"P6\n3 2\n255\nabcdefghijklmnopqr".to_vec()),
        ("ppm-p3", b"P3 2 2 255 1 2 3 4 5 6 7 8 9 10 11 12".to_vec()),
        ("ppm-p2", b"P2\n# c\n2 2\n15\n1 2 3 4".to_vec()),
        ("png-rgba", crate::imgtests::PNG_RGBA.to_vec()),
        ("png-rgb", crate::imgtests::PNG_RGB.to_vec()),
        ("png-grey", crate::imgtests::PNG_GREY.to_vec()),
        ("png-enc", encode_png(&img)),
        ("bmp24", crate::imgtests::BMP24.to_vec()),
        ("jpeg-444", crate::jpegtests::J444.to_vec()),
        ("jpeg-420", crate::jpegtests::J420.to_vec()),
        ("jpeg-gray", crate::jpegtests::JGRAY.to_vec()),
        ("jpeg-rst", crate::jpegtests::JRST.to_vec()),
    ]
}

#[test]
fn no_truncation_of_a_real_file_panics() {
    let mut panics = Vec::new();
    for (name, data) in samples() {
        for cut in 0..data.len() {
            let d = data[..cut].to_vec();
            if catch_unwind(move || decode_all(&d)).is_err() {
                panics.push(format!("{name} cut at {cut}"));
            }
        }
    }
    assert!(panics.is_empty(), "{} truncations panicked, first: {:?}", panics.len(), &panics[..panics.len().min(5)]);
}

#[test]
fn no_corruption_of_a_real_file_panics() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut panics = Vec::new();
    let mut runs = 0u32;
    for (name, data) in samples() {
        // Every single-byte change in the first 700 bytes (all headers and
        // tables) to four values, then random 1-4 byte corruptions anywhere.
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for pos in 0..data.len().min(700) {
            for v in [0x00, 0xFF, data[pos] ^ 0x01, data[pos] ^ 0x80] {
                let mut d = data.clone();
                d[pos] = v;
                cases.push(d);
            }
        }
        for _ in 0..3000 {
            let mut d = data.clone();
            for _ in 0..1 + rng.below(4) {
                let pos = rng.below(d.len());
                d[pos] = rng.next() as u8;
            }
            cases.push(d);
        }
        for (i, d) in cases.into_iter().enumerate() {
            runs += 1;
            if catch_unwind(move || decode_all(&d)).is_err() {
                panics.push(format!("{name} case {i}"));
            }
        }
    }
    assert!(panics.is_empty(), "{} of {runs} corruptions panicked, first: {:?}", panics.len(), &panics[..panics.len().min(5)]);
}
