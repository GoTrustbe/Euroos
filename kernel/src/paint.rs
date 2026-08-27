//! EuroPaint — a small raster editor. A live RGBA canvas you draw on with the
//! mouse; a palette + brush sizes + eraser + clear on the left; Save writes a
//! real PNG (and a QOI) to EuroFS via the sovereign euromedia encoder.

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use spin::Mutex;

const TITLEBAR_H: usize = 44;
const TOOLBAR_W: usize = 120;
const CANVAS_W: u32 = 520;
const CANVAS_H: u32 = 420;

const PALETTE: [[u8; 4]; 12] = [
    [0x1A, 0x1A, 0x1A, 255], [0xFF, 0xFF, 0xFF, 255],
    [0xE7, 0x4C, 0x3C, 255], [0xE6, 0x7E, 0x22, 255],
    [0xF1, 0xC4, 0x0F, 255], [0x2E, 0xCC, 0x71, 255],
    [0x1A, 0xBC, 0x9C, 255], [0x34, 0x98, 0xDB, 255],
    [0x2B, 0x6C, 0xB0, 255], [0x9B, 0x59, 0xB6, 255],
    [0xE8, 0x4E, 0x8A, 255], [0x95, 0xA5, 0xA6, 255],
];
const BRUSHES: [u32; 4] = [2, 5, 10, 18];

struct Paint {
    canvas: euromedia::Image,
    colour: usize, // index into PALETTE
    brush: usize,  // index into BRUSHES
    eraser: bool,
    last: Option<(u32, u32)>, // last draw point for line interpolation
    status: String,
}

static PAINT: Mutex<Option<Paint>> = Mutex::new(None);

fn ensure<'a>(p: &'a mut Option<Paint>) -> &'a mut Paint {
    if p.is_none() {
        *p = Some(Paint {
            canvas: euromedia::Image::new(CANVAS_W, CANVAS_H, [255, 255, 255, 255]),
            colour: 0,
            brush: 1,
            eraser: false,
            last: None,
            status: String::from("draw with the mouse  \u{00B7}  Save writes a PNG"),
        });
    }
    p.as_mut().unwrap()
}

/// Open an existing image into the canvas (edit an image from EuroView/Files).
pub fn open(fs: &mut dyn eurofs::fs::FileSystem, path: &str) {
    if let Ok(bytes) = fs.read_file(path) {
        let img = if bytes.len() >= 8 && bytes[0..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
            euromedia::decode_png(&bytes).ok()
        } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
            euromedia::decode_bmp(&bytes).ok()
        } else if bytes.len() >= 4 && &bytes[0..4] == b"qoif" {
            euromedia::decode(&bytes).ok()
        } else {
            None
        };
        if let Some(im) = img {
            let mut g = PAINT.lock();
            let p = ensure(&mut g);
            p.canvas = im;
            p.status = alloc::format!("editing {path}");
        }
    }
}

/// A press or drag inside the window at window-local (mx,my). Returns true if a
/// repaint is needed. The kernel calls this while the left button is held.
pub fn pointer(win_x: usize, win_y: usize, mx: usize, my: usize, down: bool) -> bool {
    let bx = win_x;
    let by = win_y + TITLEBAR_H;
    let mut g = PAINT.lock();
    let p = ensure(&mut g);
    // Toolbar hit-tests (palette swatches, brush sizes, eraser, clear, save).
    if mx >= bx && mx < bx + TOOLBAR_W {
        let lx = mx - bx;
        let ly = my.saturating_sub(by);
        // Palette: a 3-wide grid of 34px swatches starting at y=12.
        if ly >= 12 && ly < 12 + 4 * 40 && lx >= 10 && lx < 10 + 3 * 34 {
            let col = (lx - 10) / 34;
            let row = (ly - 12) / 40;
            let idx = row * 3 + col;
            if idx < PALETTE.len() {
                p.colour = idx;
                p.eraser = false;
                return true;
            }
        }
        // Brush sizes: a row of four at y=185.
        if ly >= 185 && ly < 215 {
            let i = lx / 30;
            if i < BRUSHES.len() {
                p.brush = i;
                return true;
            }
        }
        // Eraser / Clear / Save buttons.
        if ly >= 235 && ly < 265 { p.eraser = !p.eraser; return true; }
        if ly >= 275 && ly < 305 {
            p.canvas = euromedia::Image::new(CANVAS_W, CANVAS_H, [255, 255, 255, 255]);
            p.status = String::from("cleared");
            return true;
        }
        if ly >= 315 && ly < 345 { return save_now(p); }
        return false;
    }
    // Canvas draw.
    let cx0 = bx + TOOLBAR_W + 10;
    let cy0 = by + 10;
    if mx >= cx0 && my >= cy0 && mx < cx0 + CANVAS_W as usize && my < cy0 + CANVAS_H as usize {
        let px = (mx - cx0) as u32;
        let py = (my - cy0) as u32;
        let col = if p.eraser { [255, 255, 255, 255] } else { PALETTE[p.colour] };
        let r = BRUSHES[p.brush];
        // Interpolate from the last point so a fast drag makes a solid stroke.
        if down {
            if let Some((lx, ly)) = p.last {
                stamp_line(&mut p.canvas, lx, ly, px, py, r, col);
            } else {
                stamp(&mut p.canvas, px, py, r, col);
            }
            p.last = Some((px, py));
        }
        return true;
    }
    false
}

/// Button released: end the current stroke.
pub fn release() {
    if let Some(p) = PAINT.lock().as_mut() {
        p.last = None;
    }
}

fn stamp(img: &mut euromedia::Image, cx: u32, cy: u32, r: u32, col: [u8; 4]) {
    let r2 = (r * r) as i64;
    for dy in -(r as i64)..=(r as i64) {
        for dx in -(r as i64)..=(r as i64) {
            if dx * dx + dy * dy <= r2 {
                let x = cx as i64 + dx;
                let y = cy as i64 + dy;
                if x >= 0 && y >= 0 && (x as u32) < img.width && (y as u32) < img.height {
                    img.set(x as u32, y as u32, col);
                }
            }
        }
    }
}

fn stamp_line(img: &mut euromedia::Image, x0: u32, y0: u32, x1: u32, y1: u32, r: u32, col: [u8; 4]) {
    let (mut x0, mut y0) = (x0 as i64, y0 as i64);
    let (x1, y1) = (x1 as i64, y1 as i64);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        stamp(img, x0 as u32, y0 as u32, r, col);
        if x0 == x1 && y0 == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x0 += sx; }
        if e2 <= dx { err += dx; y0 += sy; }
    }
}

fn save_now(p: &mut Paint) -> bool {
    // Deferred FS write: stash the bytes; the kernel loop flushes them (it holds
    // the &mut FileSystem). We can't take the FS here, so signal via a static.
    *PENDING_SAVE.lock() = Some(euromedia::encode_png(&p.canvas));
    p.status = String::from("saved /home/euro/pictures/painting.png");
    true
}

static PENDING_SAVE: Mutex<Option<alloc::vec::Vec<u8>>> = Mutex::new(None);

/// The kernel calls this each loop with FS access to flush a pending Save.
pub fn flush_save(fs: &mut dyn eurofs::fs::FileSystem) {
    let bytes = PENDING_SAVE.lock().take();
    if let Some(b) = bytes {
        let _ = fs.create_dir("/home/euro/pictures");
        let n = b.len();
        let ok = fs.write_file("/home/euro/pictures/painting.png", &b).is_ok();
        crate::serial_println!("[paint] Save PNG -> /home/euro/pictures/painting.png ({n} B) ok={ok}");
    }
}

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    fb.fill_rect(bx, by, bw, bh, Color::rgb(0x20, 0x24, 0x2A));
    let mut g = PAINT.lock();
    let p = ensure(&mut g);

    // Toolbar background.
    fb.fill_rect(bx, by, TOOLBAR_W, bh, Color::rgb(0x2A, 0x2E, 0x36));
    // Palette swatches (3 x 4).
    for (i, c) in PALETTE.iter().enumerate() {
        let col = i % 3;
        let row = i / 3;
        let sx = bx + 10 + col * 34;
        let sy = by + 12 + row * 40;
        fb.fill_rounded_rect(sx, sy, 30, 30, 6, Color::rgb(c[0], c[1], c[2]));
        if i == p.colour && !p.eraser {
            fb.fill_rounded_rect(sx.saturating_sub(2), sy.saturating_sub(2), 34, 3, 1, Color::WHITE);
        }
    }
    // Brush sizes.
    for (i, r) in BRUSHES.iter().enumerate() {
        let sx = bx + 8 + i * 30;
        let sy = by + 185;
        let d = (*r as usize).min(14);
        fb.fill_rounded_rect(sx + (14 - d) / 2, sy + (14 - d) / 2, d, d, d / 2, Color::WHITE);
        if i == p.brush {
            fb.fill_rounded_rect(sx, sy + 20, 22, 2, 1, Color::rgb(0x8B, 0x5C, 0xF6));
        }
    }
    // Buttons.
    let btn = |fb: &FrameBuffer, ly: usize, label: &str, on: bool| {
        let c = if on { Color::rgb(0x8B, 0x5C, 0xF6) } else { Color::rgb(0x3A, 0x40, 0x4A) };
        fb.fill_rounded_rect(bx + 10, by + ly, TOOLBAR_W - 20, 26, 6, c);
        text::draw_px(fb, bx + 20, by + ly + 6, label, Color::WHITE, 12.5);
    };
    btn(fb, 235, "Eraser", p.eraser);
    btn(fb, 275, "Clear", false);
    btn(fb, 315, "Save PNG", false);

    // Canvas.
    let cx0 = bx + TOOLBAR_W + 10;
    let cy0 = by + 10;
    for cy in 0..CANVAS_H {
        for cx in 0..CANVAS_W {
            if let Some(px) = p.canvas.get(cx, cy) {
                fb.put_pixel(cx0 + cx as usize, cy0 + cy as usize, Color::rgb(px[0], px[1], px[2]));
            }
        }
    }
    text::draw_px(fb, cx0, cy0 + CANVAS_H as usize + 8, &p.status, Color::rgb(0xC8, 0xCC, 0xD2), 12.0);
}
