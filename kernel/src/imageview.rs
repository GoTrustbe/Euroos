//! EuroView — the image viewer app. Opens PNG / BMP / QOI / PPM from EuroFS,
//! decodes it with the sovereign `euromedia` codecs, and blits it fitted into
//! the window. No browser, no foreign image library.

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use spin::Mutex;

const TITLEBAR_H: usize = 44;

struct View {
    img: Option<euromedia::Image>,
    path: String,
    status: String, // "PNG 800x600" or an honest error
}

static VIEW: Mutex<View> = Mutex::new(View {
    img: None,
    path: String::new(),
    status: String::new(),
});

/// Open a file: sniff the format by content, decode, and remember it (or a
/// clear, honest status if the format is not one we decode).
pub fn open(fs: &mut dyn eurofs::fs::FileSystem, path: &str) {
    let mut v = VIEW.lock();
    v.path = String::from(path);
    let bytes = match fs.read_file(path) {
        Ok(b) => b,
        Err(_) => {
            v.img = None;
            v.status = alloc::format!("could not read {path}");
            return;
        }
    };
    let (img, kind) = decode_any(&bytes);
    match img {
        Some(im) => {
            v.status = alloc::format!("{kind}  {}x{}", im.width, im.height);
            v.img = Some(im);
        }
        None => {
            v.img = None;
            v.status = alloc::format!("{kind} — not supported yet");
        }
    }
}

/// Content sniff + decode. Returns the decoded image (if any) and a format label.
fn decode_any(b: &[u8]) -> (Option<euromedia::Image>, &'static str) {
    if b.len() >= 8 && b[0..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
        return (euromedia::decode_png(b).ok(), "PNG");
    }
    if b.len() >= 2 && &b[0..2] == b"BM" {
        return (euromedia::decode_bmp(b).ok(), "BMP");
    }
    if b.len() >= 4 && &b[0..4] == b"qoif" {
        return (euromedia::decode(b).ok(), "QOI");
    }
    if b.len() >= 2 && b[0] == b'P' && (b'1'..=b'6').contains(&b[1]) {
        return (euromedia::decode_ppm(b).ok(), "PPM");
    }
    if b.len() >= 3 && &b[0..3] == [0xFF, 0xD8, 0xFF] {
        return (None, "JPEG"); // honestly not decoded yet
    }
    (None, "unknown format")
}

/// Is a path something EuroView can show? (used by the file-open routing.)
pub fn handles(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".png", ".bmp", ".qoi", ".ppm", ".pgm", ".jpg", ".jpeg"]
        .iter()
        .any(|e| p.ends_with(e))
}

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    // Checkerboard so transparency is visible (like every image viewer).
    for ty in 0..bh {
        for tx in 0..bw {
            let c = if ((tx / 12) + (ty / 12)) % 2 == 0 {
                Color::rgb(0x2A, 0x2E, 0x36)
            } else {
                Color::rgb(0x22, 0x26, 0x2C)
            };
            fb.put_pixel(bx + tx, by + ty, c);
        }
    }
    let v = VIEW.lock();
    if let Some(img) = &v.img {
        // Fit the image into the body with an integer-friendly float scale,
        // preserving aspect ratio, centred.
        let iw = img.width.max(1) as usize;
        let ih = img.height.max(1) as usize;
        let margin = 12usize;
        let avail_w = bw.saturating_sub(margin * 2).max(1);
        let avail_h = bh.saturating_sub(margin * 2 + 24).max(1);
        // scale = min(avail/size), in 1/1024 fixed point, capped at 1x (no upscaling blur).
        let sx = (avail_w * 1024) / iw;
        let sy = (avail_h * 1024) / ih;
        let scale = sx.min(sy).min(1024).max(1);
        let dw = (iw * scale) / 1024;
        let dh = (ih * scale) / 1024;
        let ox = bx + (bw.saturating_sub(dw)) / 2;
        let oy = by + (bh.saturating_sub(dh + 24)) / 2;
        for dy in 0..dh {
            let syi = (dy * 1024) / scale;
            for dx in 0..dw {
                let sxi = (dx * 1024) / scale;
                if let Some(p) = img.get(sxi.min(iw - 1) as u32, syi.min(ih - 1) as u32) {
                    let col = Color::rgb(p[0], p[1], p[2]);
                    if p[3] == 255 {
                        fb.put_pixel(ox + dx, oy + dy, col);
                    } else {
                        fb.blend(ox + dx, oy + dy, col, p[3]);
                    }
                }
            }
        }
    }
    // Status line at the bottom (format + size, or the honest error).
    text::draw_px(fb, bx + 12, by + bh - 20, &v.status, Color::rgb(0xE6, 0xE8, 0xEC), 12.5);
}
