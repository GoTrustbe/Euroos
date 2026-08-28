//! EuroView — the image viewer app. Opens PNG / BMP / QOI / PPM from EuroFS,
//! decodes it with the sovereign `euromedia` codecs, and blits it fitted into
//! the window. No browser, no foreign image library.

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

const TITLEBAR_H: usize = 44;

struct View {
    img: Option<euromedia::Image>,
    path: String,
    status: String, // "PNG 800x600" or an honest error
    zoom: Zoom,
    rot: u8, // quarter turns clockwise (0..3)
    siblings: Vec<String>, // image files in the same directory (for prev/next)
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit,
    X1,
    X2,
}

static VIEW: Mutex<View> = Mutex::new(View {
    img: None,
    path: String::new(),
    status: String::new(),
    zoom: Zoom::Fit,
    rot: 0,
    siblings: Vec::new(),
});

/// Open a file: sniff the format by content, decode, and remember it (or a
/// clear, honest status if the format is not one we decode).
pub fn open(fs: &mut dyn eurofs::fs::FileSystem, path: &str) {
    let mut v = VIEW.lock();
    v.path = String::from(path);
    v.zoom = Zoom::Fit;
    v.rot = 0;
    // Collect the image files in the same directory for Prev/Next browsing.
    v.siblings.clear();
    let dir = match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    };
    if let Ok(entries) = fs.list_dir(&dir) {
        for e in entries {
            if e.kind == eurofs::EntryKind::File {
                let full = if dir == "/" {
                    alloc::format!("/{}", e.name)
                } else {
                    alloc::format!("{dir}/{}", e.name)
                };
                if handles(&full) {
                    v.siblings.push(full);
                }
            }
        }
        v.siblings.sort();
    }
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

/// Toolbar buttons across the top of the viewer body.
const TOOLBAR: [&str; 6] = ["Fit", "100%", "200%", "\u{21BB} Rotate", "\u{2190} Prev", "Next \u{2192}"];

/// A click in the viewer. Toolbar hits change zoom/rotation; Prev/Next return
/// the path to open (the kernel loop re-opens it with FS access).
pub fn click(win_x: usize, win_y: usize, mx: usize, my: usize) -> Option<String> {
    let tb_y = win_y + TITLEBAR_H + 6;
    if my < tb_y || my >= tb_y + 26 {
        return None;
    }
    let i = mx.checked_sub(win_x + 10)? / 86;
    if i >= TOOLBAR.len() {
        return None;
    }
    let mut v = VIEW.lock();
    match i {
        0 => v.zoom = Zoom::Fit,
        1 => v.zoom = Zoom::X1,
        2 => v.zoom = Zoom::X2,
        3 => v.rot = (v.rot + 1) % 4,
        4 | 5 => {
            // Prev/Next in the sorted sibling ring.
            if v.siblings.is_empty() {
                return None;
            }
            let cur = v.siblings.iter().position(|p| *p == v.path).unwrap_or(0);
            let n = v.siblings.len();
            let next = if i == 5 { (cur + 1) % n } else { (cur + n - 1) % n };
            return Some(v.siblings[next].clone());
        }
        _ => {}
    }
    Some(String::new()) // handled in-place (empty = no re-open needed)
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
    // Toolbar: Fit / 100% / 200% / Rotate / Prev / Next.
    let tb_y = by + 6;
    for (i, label) in TOOLBAR.iter().enumerate() {
        let bxp = bx + 10 + i * 86;
        if bxp + 82 > bx + bw {
            break;
        }
        let active = matches!((i, v.zoom), (0, Zoom::Fit) | (1, Zoom::X1) | (2, Zoom::X2));
        let bg = if active { Color::rgb(0x8B, 0x5C, 0xF6) } else { Color::rgb(0x3A, 0x40, 0x4A) };
        fb.fill_rounded_rect(bxp, tb_y, 82, 26, 6, bg);
        let lw = text::width_px(label, 12.0);
        text::draw_px(fb, bxp + (82usize.saturating_sub(lw)) / 2, tb_y + 6, label, Color::WHITE, 12.0);
    }
    let body_y = by + 38; // below the toolbar
    let body_h = bh.saturating_sub(38);
    if let Some(img) = &v.img {
        // Rotation swaps the effective dimensions for 90/270 degrees; zoom is
        // Fit (capped 1x), exact 100% or exact 200%.
        let iw0 = img.width.max(1) as usize;
        let ih0 = img.height.max(1) as usize;
        let (iw, ih) = if v.rot % 2 == 1 { (ih0, iw0) } else { (iw0, ih0) };
        let margin = 12usize;
        let avail_w = bw.saturating_sub(margin * 2).max(1);
        let avail_h = body_h.saturating_sub(margin * 2 + 24).max(1);
        let scale = match v.zoom {
            Zoom::Fit => ((avail_w * 1024) / iw).min((avail_h * 1024) / ih).min(1024).max(1),
            Zoom::X1 => 1024,
            Zoom::X2 => 2048,
        };
        let dw = ((iw * scale) / 1024).min(avail_w);
        let dh = ((ih * scale) / 1024).min(avail_h);
        let ox = bx + (bw.saturating_sub(dw)) / 2;
        let oy = body_y + (body_h.saturating_sub(dh + 24)) / 2;
        for dy in 0..dh {
            for dx in 0..dw {
                let rx = (dx * 1024) / scale;
                let ry = (dy * 1024) / scale;
                let (sxi, syi) = match v.rot {
                    1 => (ry, iw.saturating_sub(1 + rx)),
                    2 => (iw0.saturating_sub(1 + rx), ih0.saturating_sub(1 + ry)),
                    3 => (ih.saturating_sub(1 + ry), rx),
                    _ => (rx, ry),
                };
                if let Some(p) = img.get(sxi.min(iw0 - 1) as u32, syi.min(ih0 - 1) as u32) {
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
    // Status line: format + size + position in the folder ring.
    let pos = v.siblings.iter().position(|p| *p == v.path).map(|i| i + 1).unwrap_or(0);
    let stat = if pos > 0 {
        alloc::format!("{}  \u{00B7}  {pos}/{}", v.status, v.siblings.len())
    } else {
        v.status.clone()
    };
    text::draw_px(fb, bx + 12, by + bh - 20, &stat, Color::rgb(0xE6, 0xE8, 0xEC), 12.5);
}
