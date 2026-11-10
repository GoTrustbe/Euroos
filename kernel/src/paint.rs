//! EuroPaint — a full raster editor (not a demo). HSV colour picker, nine tools
//! (brush, pencil, line, rectangle outline/filled, ellipse, flood fill, colour
//! picker, eraser), a brush-size slider, undo/redo history, live shape preview,
//! recent colours, and Save to PNG/QOI/BMP on EuroFS via the sovereign codecs.

use crate::graphics::{Color, FrameBuffer};
use crate::text;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

const TITLEBAR_H: usize = 44;
const TOOLBAR_W: usize = 236;
const CANVAS_W: u32 = 640;
const CANVAS_H: u32 = 470;
const HISTORY: usize = 24; // undo/redo depth

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Brush,
    Pencil,
    Line,
    Rect,
    RectFill,
    Ellipse,
    Bucket,
    Picker,
    Eraser,
}
const TOOLS: [(Tool, &str); 9] = [
    (Tool::Brush, "Brush"),
    (Tool::Pencil, "Pencil"),
    (Tool::Line, "Line"),
    (Tool::Rect, "Rect"),
    (Tool::RectFill, "Fill\u{25AF}"),
    (Tool::Ellipse, "Ellipse"),
    (Tool::Bucket, "Bucket"),
    (Tool::Picker, "Picker"),
    (Tool::Eraser, "Eraser"),
];

struct Paint {
    canvas: euromedia::Image,
    tool: Tool,
    hue: u16,   // 0..360
    sat: u8,    // 0..255
    val: u8,    // 0..255
    brush: u32, // 1..64
    recent: Vec<[u8; 4]>,
    undo: Vec<euromedia::Image>,
    redo: Vec<euromedia::Image>,
    // Stroke / shape drag state (canvas coordinates).
    start: Option<(i32, i32)>,
    cur: Option<(i32, i32)>,
    last: Option<(i32, i32)>,
    committed_start: bool, // pushed an undo snapshot for this drag?
    status: String,
}

static PAINT: Mutex<Option<Paint>> = Mutex::new(None);
static PENDING_SAVE: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);

fn ensure(p: &mut Option<Paint>) -> &mut Paint {
    p.get_or_insert_with(|| Paint {
        canvas: euromedia::Image::new(CANVAS_W, CANVAS_H, [255, 255, 255, 255]),
        tool: Tool::Brush,
        hue: 0,
        sat: 200,
        val: 220,
        brush: 4,
        recent: Vec::new(),
        undo: Vec::new(),
        redo: Vec::new(),
        start: None,
        cur: None,
        last: None,
        committed_start: false,
        status: String::from("EuroPaint \u{00B7} pick a tool and draw"),
    })
}

fn hsv_to_rgb(h: u16, s: u8, v: u8) -> [u8; 4] {
    let h = (h % 360) as i32;
    let s = s as i32;
    let v = v as i32;
    let c = v * s / 255;
    let x = c * (60 - (h % 120 - 60).abs()) / 60;
    let m = v - c;
    let (r, g, b) = match h / 60 {
        0 => (c, x, 0),
        1 => (x, c, 0),
        2 => (0, c, x),
        3 => (0, x, c),
        4 => (x, 0, c),
        _ => (c, 0, x),
    };
    [(r + m) as u8, (g + m) as u8, (b + m) as u8, 255]
}

fn fg(p: &Paint) -> [u8; 4] {
    hsv_to_rgb(p.hue, p.sat, p.val)
}

/// Memory the undo history may hold: 24 steps of the default canvas are 29 MiB;
/// an opened photo is larger, and 24 copies of it are not a reasonable price.
const UNDO_BYTES: usize = 48 * 1024 * 1024;

/// Push the canvas on the undo stack. The history is bounded in steps AND in
/// bytes (and by a quarter of the free heap), and the copy is made fallibly:
/// when there is no room the oldest steps go, and if even one copy does not
/// fit the stroke simply cannot be undone. It never aborts the kernel.
fn snapshot(p: &mut Paint) {
    p.redo.clear();
    let need = p.canvas.byte_len();
    let budget = UNDO_BYTES.min(crate::allocator::stats().1 / 4);
    while !p.undo.is_empty()
        && (p.undo.len() >= HISTORY || p.undo.iter().map(|i| i.byte_len()).sum::<usize>() + need > budget)
    {
        p.undo.remove(0);
    }
    match p.canvas.try_clone().filter(|_| need <= budget) {
        Some(copy) => p.undo.push(copy),
        None => p.status = String::from("not enough memory to keep an undo step"),
    }
}

fn push_recent(p: &mut Paint, c: [u8; 4]) {
    p.recent.retain(|x| *x != c);
    p.recent.insert(0, c);
    p.recent.truncate(10);
}

// ── Drawing primitives on the canvas ────────────────────────────────────────

fn dot(img: &mut euromedia::Image, cx: i32, cy: i32, r: u32, col: [u8; 4], square: bool) {
    let r = r as i32;
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if square || dx * dx + dy * dy <= r2 {
                let (x, y) = (cx + dx, cy + dy);
                if x >= 0 && y >= 0 && (x as u32) < img.width && (y as u32) < img.height {
                    img.set(x as u32, y as u32, col);
                }
            }
        }
    }
}

fn line(img: &mut euromedia::Image, x0: i32, y0: i32, x1: i32, y1: i32, r: u32, col: [u8; 4], square: bool) {
    let (mut x0, mut y0) = (x0, y0);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        dot(img, x0, y0, r, col, square);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn rect(img: &mut euromedia::Image, x0: i32, y0: i32, x1: i32, y1: i32, r: u32, col: [u8; 4], filled: bool) {
    let (lx, hx) = (x0.min(x1), x0.max(x1));
    let (ly, hy) = (y0.min(y1), y0.max(y1));
    if filled {
        for y in ly..=hy {
            for x in lx..=hx {
                if x >= 0 && y >= 0 && (x as u32) < img.width && (y as u32) < img.height {
                    img.set(x as u32, y as u32, col);
                }
            }
        }
    } else {
        line(img, lx, ly, hx, ly, r, col, true);
        line(img, lx, hy, hx, hy, r, col, true);
        line(img, lx, ly, lx, hy, r, col, true);
        line(img, hx, ly, hx, hy, r, col, true);
    }
}

fn ellipse(img: &mut euromedia::Image, x0: i32, y0: i32, x1: i32, y1: i32, r: u32, col: [u8; 4]) {
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let ax = (x1 - x0).abs() / 2;
    let by = (y1 - y0).abs() / 2;
    if ax == 0 || by == 0 {
        line(img, x0, y0, x1, y1, r, col, true);
        return;
    }
    // Sample the parametric ellipse densely enough for any size.
    let steps = (ax + by) * 4 + 16;
    let mut prev: Option<(i32, i32)> = None;
    for i in 0..=steps {
        // angle = 2*pi*i/steps, using integer-friendly cos/sin via a small table.
        let (c, s) = cos_sin(i as i32 * 360 / steps);
        let x = cx + ax * c / 1000;
        let y = cy + by * s / 1000;
        if let Some((px, py)) = prev {
            line(img, px, py, x, y, r, col, true);
        }
        prev = Some((x, y));
    }
}

// cos/sin in 1/1000 fixed point for integer angle degrees.
fn cos_sin(deg: i32) -> (i32, i32) {
    let d = ((deg % 360) + 360) % 360;
    let sin_tbl = |a: i32| -> i32 {
        // 0..90 degrees sine table, ×1000.
        const T: [i32; 91] = [
            0, 17, 35, 52, 70, 87, 105, 122, 139, 156, 174, 191, 208, 225, 242, 259, 276, 292, 309,
            326, 342, 358, 375, 391, 407, 423, 438, 454, 469, 485, 500, 515, 530, 545, 559, 574,
            588, 602, 616, 629, 643, 656, 669, 682, 695, 707, 719, 731, 743, 755, 766, 777, 788,
            799, 809, 819, 829, 839, 848, 857, 866, 875, 883, 891, 899, 906, 914, 921, 927, 934,
            940, 946, 951, 956, 961, 966, 970, 974, 978, 982, 985, 988, 990, 993, 995, 996, 998,
            999, 999, 1000, 1000,
        ];
        let a = a.clamp(0, 90) as usize;
        T[a]
    };
    let sin = match d {
        0..=90 => sin_tbl(d),
        91..=180 => sin_tbl(180 - d),
        181..=270 => -sin_tbl(d - 180),
        _ => -sin_tbl(360 - d),
    };
    let cos = match d {
        0..=90 => sin_tbl(90 - d),
        91..=180 => -sin_tbl(d - 90),
        181..=270 => -sin_tbl(270 - d),
        _ => sin_tbl(d - 270),
    };
    (cos, sin)
}

// ── Public open / save ──────────────────────────────────────────────────────

/// The largest image FILE EuroPaint reads (the decoded image has its own limit).
const MAX_OPEN_BYTES: u64 = 32 * 1024 * 1024;

pub fn open(fs: &mut dyn eurofs::fs::FileSystem, path: &str) {
    let free = crate::allocator::stats().1;
    // The decoded image must fit a few times: canvas, the copy for the screen, undo.
    open_limited(fs, path, eurofs::read_budget(free as u64, MAX_OPEN_BYTES, 2), free / 8);
}

/// [`open`] with explicit limits for the file and for the decoded image. Over
/// either limit the canvas stays as it is and the status line says why.
fn open_limited(fs: &mut dyn eurofs::fs::FileSystem, path: &str, file_limit: u64, image_limit: usize) -> bool {
    let bytes = match eurofs::read_file_bounded(&*fs, path, file_limit) {
        Ok(b) => b,
        Err(e) => {
            if let eurofs::BoundedReadError::TooLarge { .. } = e {
                crate::serial_println!("[paint] {path}: {e}");
                let mut g = PAINT.lock();
                ensure(&mut g).status = alloc::format!("{path}: {e}");
            }
            return false;
        }
    };
    {
        let img = if bytes.len() >= 8 && bytes[0..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
            euromedia::decode_png(&bytes).ok()
        } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
            euromedia::decode_bmp(&bytes).ok()
        } else if bytes.len() >= 4 && &bytes[0..4] == b"qoif" {
            euromedia::decode(&bytes).ok()
        } else if bytes.len() >= 2 && bytes[0..2] == [0xFF, 0xD8] {
            euromedia::decode_jpeg(&bytes).ok()
        } else {
            None
        };
        if let Some(im) = img {
            let mut g = PAINT.lock();
            let p = ensure(&mut g);
            if im.byte_len() > image_limit {
                p.status = alloc::format!("{path}: image too large to edit ({}x{})", im.width, im.height);
                return false;
            }
            p.canvas = im;
            p.undo.clear();
            p.redo.clear();
            p.status = alloc::format!("editing {path}");
            return true;
        }
    }
    false
}

/// **[robust] paint** — bucket clicks on, at the edge of and far outside the
/// canvas (`flood_fill` used to `unwrap()` the clicked pixel), on a fresh
/// state; the user's painting is put back afterwards.
#[cfg(feature = "selftest")]
pub fn robust_selftest() -> bool {
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    let saved = PAINT.lock().take();
    let saved_override = RECENT_OVERRIDE.lock().take();
    {
        let mut g = PAINT.lock();
        ensure(&mut g).tool = Tool::Bucket;
    }
    let (wx, wy) = (100usize, 100usize);
    let (cx0, cy0) = (wx + TOOLBAR_W + 8, wy + TITLEBAR_H + 8);
    // Outside first: nothing may change, nothing may panic.
    for (mx, my) in [
        (0, 0),
        (usize::MAX, usize::MAX),
        (cx0 + CANVAS_W as usize, cy0),
        (cx0, cy0 + CANVAS_H as usize),
        (cx0 - 1, cy0),
        (cx0, usize::MAX),
        (usize::MAX / 2, cy0),
    ] {
        let _ = pointer(wx, wy, mx, my, true);
        release();
    }
    let untouched = {
        let mut g = PAINT.lock();
        ensure(&mut g).canvas.pixels.iter().all(|p| *p == WHITE)
    };
    // The last pixel of the canvas: fills all of it.
    let hit = pointer(wx, wy, cx0 + CANVAS_W as usize - 1, cy0 + CANVAS_H as usize - 1, true);
    release();
    let (filled, undo_steps) = {
        let mut g = PAINT.lock();
        let p = ensure(&mut g);
        (p.canvas.pixels.iter().all(|px| *px != WHITE), p.undo.len())
    };
    let mut scratch = euromedia::Image::new(4, 4, WHITE);
    let direct = euromedia::flood_fill(&mut scratch, -1, 2, [0, 0, 0, 255]) == 0
        && euromedia::flood_fill(&mut scratch, 2, i64::MAX, [0, 0, 0, 255]) == 0
        && euromedia::flood_fill(&mut scratch, 4, 4, [0, 0, 0, 255]) == 0;
    *PAINT.lock() = saved;
    *RECENT_OVERRIDE.lock() = saved_override;
    untouched && hit && filled && undo_steps == 1 && direct
}

/// Part of **[robust-fs]**: a file over the limit, and a decoded image over the
/// image limit, leave the canvas alone and set the status line.
#[cfg(feature = "selftest")]
pub fn robust_open_selftest(fs: &mut dyn eurofs::fs::FileSystem, big: &str) -> bool {
    let saved = PAINT.lock().take();
    let refused_file = !open_limited(fs, big, 4096, usize::MAX);
    let status_file = {
        let mut g = PAINT.lock();
        ensure(&mut g).status.clone()
    };
    // A real 32x32 PNG, but only 1 KiB of decoded image allowed.
    let png = euromedia::encode_png(&euromedia::Image::new(32, 32, [1, 2, 3, 255]));
    let path = "/tmp/robust-32.png";
    let wrote = fs.write_file(path, &png).is_ok();
    let refused_image = !open_limited(fs, path, 1 << 20, 1024);
    let (status_image, canvas_kept) = {
        let mut g = PAINT.lock();
        let p = ensure(&mut g);
        (p.status.clone(), p.canvas.width == CANVAS_W && p.canvas.height == CANVAS_H)
    };
    let accepted = open_limited(fs, path, 1 << 20, 1 << 20);
    let _ = fs.remove_file(path);
    *PAINT.lock() = saved;
    refused_file && status_file.contains("too large") && wrote && refused_image && status_image.contains("too large to edit") && canvas_kept && accepted
}

pub fn flush_save(fs: &mut dyn eurofs::fs::FileSystem) {
    let job = PENDING_SAVE.lock().take();
    if let Some((path, bytes)) = job {
        let _ = fs.create_dir("/home/euro/pictures");
        let ok = fs.write_file(&path, &bytes).is_ok();
        crate::serial_println!("[paint] saved {path} ({} B) ok={ok}", bytes.len());
    }
}

// ── Toolbar layout (y offsets inside the body) ──────────────────────────────
const TOOL_Y: usize = 10;
const TOOL_ROWS: usize = 3;
const SLIDER_Y: usize = 132;
const SV_Y: usize = 178;
const SV_W: usize = 176;
const SV_H: usize = 130;
const HUE_X: usize = 190;
const HUE_W: usize = 20;
const RECENT_Y: usize = 322;
const BTN_Y: usize = 360;

fn tool_at(lx: usize, ly: usize) -> Option<Tool> {
    if ly < TOOL_Y || ly >= TOOL_Y + TOOL_ROWS * 38 {
        return None;
    }
    let col = lx.checked_sub(10)? / 72;
    let row = (ly - TOOL_Y) / 38;
    if col >= 3 {
        return None;
    }
    let i = row * 3 + col;
    TOOLS.get(i).map(|(t, _)| *t)
}

/// A press or drag inside the window. `down` = button held. Returns need-repaint.
pub fn pointer(win_x: usize, win_y: usize, mx: usize, my: usize, down: bool) -> bool {
    let bx = win_x;
    let by = win_y + TITLEBAR_H;
    let mut g = PAINT.lock();
    let p = ensure(&mut g);

    // Toolbar interactions (only on the initial press, not while dragging the canvas).
    if mx >= bx && mx < bx + TOOLBAR_W && p.start.is_none() {
        let lx = mx - bx;
        let ly = my.saturating_sub(by);
        if let Some(t) = tool_at(lx, ly) {
            p.tool = t;
            p.status = String::from(match t {
                Tool::Brush => "Brush",
                Tool::Pencil => "Pencil (1px)",
                Tool::Line => "Line: drag from A to B",
                Tool::Rect => "Rectangle: drag",
                Tool::RectFill => "Filled rectangle: drag",
                Tool::Ellipse => "Ellipse: drag",
                Tool::Bucket => "Bucket: click an area",
                Tool::Picker => "Picker: click to grab a colour",
                Tool::Eraser => "Eraser",
            });
            return true;
        }
        // Brush-size slider.
        if ly >= SLIDER_Y && ly < SLIDER_Y + 24 && lx >= 12 && lx < 12 + 200 {
            let v = ((lx - 12) * 64 / 200).clamp(1, 64) as u32;
            p.brush = v;
            return true;
        }
        // SV field.
        if lx >= 8 && lx < 8 + SV_W && ly >= SV_Y && ly < SV_Y + SV_H {
            p.sat = (((lx - 8) * 255) / SV_W) as u8;
            p.val = (255 - ((ly - SV_Y) * 255) / SV_H) as u8;
            return true;
        }
        // Hue strip.
        if lx >= HUE_X && lx < HUE_X + HUE_W && ly >= SV_Y && ly < SV_Y + SV_H {
            p.hue = (((ly - SV_Y) * 360) / SV_H) as u16;
            return true;
        }
        // Recent colours.
        if ly >= RECENT_Y && ly < RECENT_Y + 24 {
            let i = lx.saturating_sub(8) / 22;
            if let Some(c) = p.recent.get(i).copied() {
                // Reverse HSV so the sliders follow the recent colour roughly.
                p.status = alloc::format!("colour {:02x}{:02x}{:02x}", c[0], c[1], c[2]);
                // store as a direct-RGB override via recent (simplest: set sat/val to
                // pick nearest is overkill; keep the picker as source of truth and just
                // draw with the recent by remembering it):
                RECENT_OVERRIDE.lock().replace(c);
            }
            return true;
        }
        // Buttons: Undo, Redo, Clear, Save row.
        if ly >= BTN_Y && ly < BTN_Y + 30 {
            let i = lx.saturating_sub(8) / 56;
            match i {
                0 => {
                    if let Some(c) = p.undo.pop() {
                        p.redo.push(core::mem::replace(&mut p.canvas, c));
                        p.status = String::from("undo");
                    }
                }
                1 => {
                    if let Some(c) = p.redo.pop() {
                        p.undo.push(core::mem::replace(&mut p.canvas, c));
                        p.status = String::from("redo");
                    }
                }
                2 => {
                    snapshot(p);
                    p.canvas = euromedia::Image::new(CANVAS_W, CANVAS_H, [255, 255, 255, 255]);
                    p.status = String::from("cleared");
                }
                _ => {
                    let bytes = euromedia::encode_png(&p.canvas);
                    *PENDING_SAVE.lock() = Some((String::from("/home/euro/pictures/painting.png"), bytes));
                    p.status = String::from("saved painting.png");
                }
            }
            return true;
        }
        return false;
    }

    // Canvas area.
    let cx0 = bx + TOOLBAR_W + 8;
    let cy0 = by + 8;
    let inside = mx >= cx0 && my >= cy0 && (mx - cx0) < CANVAS_W as usize && (my - cy0) < CANVAS_H as usize;
    if !inside && p.start.is_none() {
        return false;
    }
    let px = (mx as i32 - cx0 as i32).clamp(0, CANVAS_W as i32 - 1);
    let py = (my as i32 - cy0 as i32).clamp(0, CANVAS_H as i32 - 1);
    if !down {
        return false;
    }
    let colour = RECENT_OVERRIDE.lock().take().unwrap_or_else(|| fg(p));
    match p.tool {
        Tool::Brush | Tool::Pencil | Tool::Eraser => {
            if !p.committed_start {
                snapshot(p);
                p.committed_start = true;
            }
            let c = if p.tool == Tool::Eraser { [255, 255, 255, 255] } else { colour };
            let r = if p.tool == Tool::Pencil { 0 } else { p.brush };
            if let Some((lx, ly)) = p.last {
                line(&mut p.canvas, lx, ly, px, py, r, c, false);
            } else {
                dot(&mut p.canvas, px, py, r, c, false);
            }
            p.last = Some((px, py));
            if p.tool != Tool::Eraser {
                push_recent(p, c);
            }
        }
        Tool::Bucket => {
            snapshot(p);
            // euromedia::flood_fill takes any coordinate (a click outside the
            // canvas fills nothing) and cannot run the heap dry.
            euromedia::flood_fill(&mut p.canvas, px as i64, py as i64, colour);
            push_recent(p, colour);
        }
        Tool::Picker => {
            if let Some(c) = p.canvas.get(px as u32, py as u32) {
                p.status = alloc::format!("picked {:02x}{:02x}{:02x}", c[0], c[1], c[2]);
                RECENT_OVERRIDE.lock().replace(c);
                push_recent(p, c);
            }
        }
        Tool::Line | Tool::Rect | Tool::RectFill | Tool::Ellipse => {
            // Shapes: remember the drag; commit on release. Live preview in render().
            if p.start.is_none() {
                p.start = Some((px, py));
            }
            p.cur = Some((px, py));
        }
    }
    true
}

pub fn release() {
    let mut g = PAINT.lock();
    let p = ensure(&mut g);
    // Commit a pending shape.
    if let (Some((sx, sy)), Some((cx, cy))) = (p.start.take(), p.cur.take()) {
        let colour = fg(p);
        snapshot(p);
        let r = p.brush;
        match p.tool {
            Tool::Line => line(&mut p.canvas, sx, sy, cx, cy, r, colour, false),
            Tool::Rect => rect(&mut p.canvas, sx, sy, cx, cy, r, colour, false),
            Tool::RectFill => rect(&mut p.canvas, sx, sy, cx, cy, r, colour, true),
            Tool::Ellipse => ellipse(&mut p.canvas, sx, sy, cx, cy, r, colour),
            _ => {}
        }
        push_recent(p, colour);
    }
    p.last = None;
    p.committed_start = false;
}

static RECENT_OVERRIDE: Mutex<Option<[u8; 4]>> = Mutex::new(None);

// ── Render ──────────────────────────────────────────────────────────────────

pub fn render(fb: &FrameBuffer, x: usize, y: usize, w: usize, h: usize) {
    let bx = x;
    let by = y + TITLEBAR_H;
    let bw = w;
    let bh = h.saturating_sub(TITLEBAR_H);
    fb.fill_rect(bx, by, bw, bh, Color::rgb(0x1C, 0x20, 0x26));
    fb.fill_rect(bx, by, TOOLBAR_W, bh, Color::rgb(0x2A, 0x2E, 0x36));
    let mut g = PAINT.lock();
    let p = ensure(&mut g);

    // Tools (3 columns).
    for (i, (t, label)) in TOOLS.iter().enumerate() {
        let col = i % 3;
        let row = i / 3;
        let tx = bx + 10 + col * 72;
        let ty = by + TOOL_Y + row * 38;
        let on = *t == p.tool;
        let c = if on { Color::rgb(0x8B, 0x5C, 0xF6) } else { Color::rgb(0x3A, 0x40, 0x4A) };
        fb.fill_rounded_rect(tx, ty, 66, 32, 6, c);
        text::draw_px(fb, tx + 8, ty + 9, label, Color::WHITE, 11.5);
    }
    // Brush slider.
    text::draw_px(fb, bx + 12, by + SLIDER_Y - 16, "Brush size", Color::rgb(0xB0, 0xB6, 0xC0), 11.0);
    fb.fill_rounded_rect(bx + 12, by + SLIDER_Y + 8, 200, 4, 2, Color::rgb(0x50, 0x56, 0x60));
    let knob = 12 + (p.brush as usize * 200 / 64);
    fb.fill_rounded_rect(bx + knob.saturating_sub(6), by + SLIDER_Y + 2, 12, 16, 6, Color::rgb(0x8B, 0x5C, 0xF6));
    let bs = alloc::format!("{}px", p.brush);
    text::draw_px(fb, bx + 214 - text::width_px(&bs, 11.0).min(30), by + SLIDER_Y + 3, &bs, Color::WHITE, 11.0);

    // SV field: saturation on x, value on y, at the current hue.
    for sy in 0..SV_H {
        for sx in 0..SV_W {
            let s = (sx * 255 / SV_W) as u8;
            let v = (255 - sy * 255 / SV_H) as u8;
            let c = hsv_to_rgb(p.hue, s, v);
            fb.put_pixel(bx + 8 + sx, by + SV_Y + sy, Color::rgb(c[0], c[1], c[2]));
        }
    }
    // SV cursor.
    let sxp = bx + 8 + (p.sat as usize * SV_W / 255);
    let syp = by + SV_Y + ((255 - p.val as usize) * SV_H / 255);
    fb.fill_rounded_rect(sxp.saturating_sub(3), syp.saturating_sub(3), 6, 6, 3, Color::WHITE);

    // Hue strip.
    for hy in 0..SV_H {
        let hue = (hy * 360 / SV_H) as u16;
        let c = hsv_to_rgb(hue, 255, 255);
        for hx in 0..HUE_W {
            fb.put_pixel(bx + HUE_X + hx, by + SV_Y + hy, Color::rgb(c[0], c[1], c[2]));
        }
    }
    let hyp = by + SV_Y + (p.hue as usize * SV_H / 360);
    fb.fill_rect(bx + HUE_X, hyp.saturating_sub(1), HUE_W, 2, Color::WHITE);

    // Current colour + hex.
    let cur = fg(p);
    fb.fill_rounded_rect(bx + 8, by + SV_Y - 22, 40, 16, 4, Color::rgb(cur[0], cur[1], cur[2]));
    let hex = alloc::format!("#{:02X}{:02X}{:02X}", cur[0], cur[1], cur[2]);
    text::draw_px(fb, bx + 54, by + SV_Y - 21, &hex, Color::WHITE, 12.0);

    // Recent colours.
    for (i, c) in p.recent.iter().enumerate() {
        fb.fill_rounded_rect(bx + 8 + i * 22, by + RECENT_Y, 18, 18, 4, Color::rgb(c[0], c[1], c[2]));
    }

    // Buttons.
    for (i, label) in ["Undo", "Redo", "Clear", "Save"].iter().enumerate() {
        let tx = bx + 8 + i * 56;
        fb.fill_rounded_rect(tx, by + BTN_Y, 52, 26, 6, Color::rgb(0x3A, 0x40, 0x4A));
        text::draw_px(fb, tx + 8, by + BTN_Y + 6, label, Color::WHITE, 11.5);
    }

    // Canvas.
    let cx0 = bx + TOOLBAR_W + 8;
    let cy0 = by + 8;
    for cy in 0..CANVAS_H {
        for cx in 0..CANVAS_W {
            if let Some(px) = p.canvas.get(cx, cy) {
                fb.put_pixel(cx0 + cx as usize, cy0 + cy as usize, Color::rgb(px[0], px[1], px[2]));
            }
        }
    }
    // Live shape preview (drawn on a throwaway copy so the canvas is untouched).
    if let (Some((sx, sy)), Some((cx, cy))) = (p.start, p.cur) {
        let mut prev = p.canvas.clone();
        let colour = fg(p);
        let r = p.brush;
        match p.tool {
            Tool::Line => line(&mut prev, sx, sy, cx, cy, r, colour, false),
            Tool::Rect => rect(&mut prev, sx, sy, cx, cy, r, colour, false),
            Tool::RectFill => rect(&mut prev, sx, sy, cx, cy, r, colour, true),
            Tool::Ellipse => ellipse(&mut prev, sx, sy, cx, cy, r, colour),
            _ => {}
        }
        // Only repaint the bounding box of the shape for speed.
        let (lx, hx) = (sx.min(cx).max(0), sx.max(cx).min(CANVAS_W as i32 - 1));
        let (ly, hy) = (sy.min(cy).max(0), sy.max(cy).min(CANVAS_H as i32 - 1));
        for yy in ly..=hy {
            for xx in lx..=hx {
                if let Some(px) = prev.get(xx as u32, yy as u32) {
                    fb.put_pixel(cx0 + xx as usize, cy0 + yy as usize, Color::rgb(px[0], px[1], px[2]));
                }
            }
        }
    }
    text::draw_px(fb, cx0, cy0 + CANVAS_H as usize + 8, &p.status, Color::rgb(0xC8, 0xCC, 0xD2), 12.0);
}
