//! EuroOS SVG-icon-renderer (EDS) — tekent de `euicons`-set (24px stroke-grid)
//! software in het framebuffer. Geen bitmaps: de iconen zijn echte vector-paden
//! (rect/circle/path met M/L/H/V/C-commando's), getekend als strokes met een
//! lijndikte. Schaalbaar naar elke grootte (resolutie-onafhankelijk, EDS-eis).

use crate::graphics::{Color, FrameBuffer};

// ── De icon-set: mini-SVG element-strings (uit euicons.js). currentColor. ──
fn icon_svg(name: &str) -> &'static str {
    match name {
        "files" => "<path d=\"M3 8 V18.5 H21 V8 Z M3 8 V6 H8.5 L10.5 8\"/>",
        "browser" => "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M3 12 H21\"/><path d=\"M12 3 L8 7 L7 12 L8 17 L12 21 L16 17 L17 12 L16 7 Z\"/>",
        "mail" => "<rect x=\"3\" y=\"5.5\" width=\"18\" height=\"13\" rx=\"2.2\"/><path d=\"m4 7 7.3 5.2a1.2 1.2 0 0 0 1.4 0L20 7\"/>",
        "settings" => "<path d=\"M7 4 V20 M17 4 V20\"/><circle cx=\"7\" cy=\"9\" r=\"2.4\"/><circle cx=\"17\" cy=\"15\" r=\"2.4\"/>",
        "store" => "<path d=\"M5 8h14l-1 11.5H6zM8.5 8V6.5a3.5 3.5 0 0 1 7 0V8\"/>",
        "terminal" => "<rect x=\"3\" y=\"4.5\" width=\"18\" height=\"15\" rx=\"2.4\"/><path d=\"m7 10 3 2.5L7 15M13 15h4\"/>",
        "photos" => "<rect x=\"3\" y=\"5\" width=\"18\" height=\"14\" rx=\"2.4\"/><circle cx=\"8.5\" cy=\"9.5\" r=\"1.6\"/><path d=\"m4 17 5-4.5 3.5 3L16 12l4 4\"/>",
        "shieldCheck" => "<path d=\"M12 3.2 5 6v5.2c0 4.4 3 7.6 7 9.6 4-2 7-5.2 7-9.6V6z\"/><path d=\"m9 11.7 2.1 2.1L15 9.9\"/>",
        "lock" => "<rect x=\"5\" y=\"10.5\" width=\"14\" height=\"9.5\" rx=\"2.2\"/><path d=\"M8 10.5V8a4 4 0 0 1 8 0v2.5\"/>",
        "wifi" => "<path d=\"M2.5 9.2a14 14 0 0 1 19 0M6 12.7a9 9 0 0 1 12 0M9.3 16.1a4 4 0 0 1 5.4 0\"/><circle cx=\"12\" cy=\"19\" r=\"1.1\"/>",
        "sun" => "<circle cx=\"12\" cy=\"12\" r=\"4\"/><path d=\"M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4\"/>",
        "moon" => "<path d=\"M15 3.5 C8.5 5 5.5 9.5 6 14 C6.5 18.5 10.5 21 15.5 20.5 C11 19 8.5 15 9 11 C9.4 7.5 11.5 5 15 3.5 Z\"/>",
        "search" => "<circle cx=\"11\" cy=\"11\" r=\"6.5\"/><path d=\"m20 20-3.6-3.6\"/>",
        "folder" => "<path d=\"M3 7.5A1.5 1.5 0 0 1 4.5 6H9l2 2.2h8.5A1.5 1.5 0 0 1 21 9.7V18a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18z\"/>",
        "grid" => "<rect x=\"4\" y=\"4\" width=\"7\" height=\"7\" rx=\"1.6\"/><rect x=\"13\" y=\"4\" width=\"7\" height=\"7\" rx=\"1.6\"/><rect x=\"4\" y=\"13\" width=\"7\" height=\"7\" rx=\"1.6\"/><rect x=\"13\" y=\"13\" width=\"7\" height=\"7\" rx=\"1.6\"/>",
        "user" => "<circle cx=\"12\" cy=\"8.5\" r=\"3.5\"/><path d=\"M5.5 19.5a6.5 6.5 0 0 1 13 0z\"/>",
        "bell" => "<path d=\"M6.5 10a5.5 5.5 0 0 1 11 0c0 4 1.5 5 1.5 5H5s1.5-1 1.5-5z\"/><path d=\"M10 18.5a2 2 0 0 0 4 0\"/>",
        "clock" => "<circle cx=\"12\" cy=\"12\" r=\"8.5\"/><path d=\"M12 7.5 V12 L15.5 14\"/>",
        "plus" => "<path d=\"M12 5 V19 M5 12 H19\"/>",
        "home" => "<path d=\"M3.5 11.5 L12 4 L20.5 11.5 M6 10 V19.5 H18 V10\"/>",
        "doc" => "<path d=\"M6 3 H14 L18 7 V21 H6 Z M14 3 V7 H18\"/>",
        "download" => "<path d=\"M12 4 V14 M8 11 L12 15 L16 11 M5 19 H19\"/>",
        "star" => "<path d=\"M12 4 L14 9.3 L20 10 L15.5 14 L17 20 L12 16.8 L7 20 L8.5 14 L4 10 L10 9.3 Z\"/>",
        "chevron" => "<path d=\"M14 6.5 L9 12 L14 17.5\"/>",
        "arrow" => "<path d=\"M19 12 H5 M11 6 L5 12 L11 18\"/>",
        _ => "",
    }
}

/// Teken icon `name` in het vak (`x`,`y`) met grootte `size` px en kleur `color`.
pub fn draw(fb: &FrameBuffer, name: &str, x: usize, y: usize, size: usize, color: Color) {
    let svg = icon_svg(name);
    if svg.is_empty() {
        return;
    }
    let s = size as f32 / 24.0; // viewBox = 24
    let ox = x as f32;
    let oy = y as f32;
    // Lijndikte schaalt mee (min 1).
    let thick = ((1.7 * s + 0.5) as i32).max(1);
    let tx = |v: f32| ox + v * s;
    let ty = |v: f32| oy + v * s;

    let mut rest = svg;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt + 1..];
        let gt = match rest.find('>') {
            Some(g) => g,
            None => break,
        };
        let elem = &rest[..gt];
        rest = &rest[gt + 1..];
        let tag = elem.split([' ', '/']).next().unwrap_or("");
        match tag {
            "circle" => {
                let cx = tx(attr(elem, "cx"));
                let cy = ty(attr(elem, "cy"));
                let r = attr(elem, "r") * s;
                stroke_circle(fb, cx, cy, r, thick, color);
            }
            "rect" => {
                let rx = tx(attr(elem, "x"));
                let ry = ty(attr(elem, "y"));
                let w = attr(elem, "width") * s;
                let h = attr(elem, "height") * s;
                let rr = attr(elem, "rx") * s;
                stroke_round_rect(fb, rx, ry, w, h, rr, thick, color);
            }
            "path" => {
                if let Some(d) = attr_str(elem, "d") {
                    draw_path(fb, d, &tx, &ty, s, thick, color);
                }
            }
            _ => {}
        }
    }
}

// ── Mini-attribuut-parsers ────────────────────────────────────────────────
fn attr(elem: &str, key: &str) -> f32 {
    attr_str(elem, key).and_then(parse_first_num).unwrap_or(0.0)
}

fn attr_str<'a>(elem: &'a str, key: &str) -> Option<&'a str> {
    // zoek key="..."
    let pat = key;
    let mut i = 0;
    let b = elem.as_bytes();
    while i + pat.len() + 2 < b.len() {
        if &elem[i..i + pat.len()] == pat && b[i + pat.len()] == b'=' && b[i + pat.len() + 1] == b'"' {
            // grens-check: voorafgaand teken is spatie of begin
            let ok_before = i == 0 || b[i - 1] == b' ';
            if ok_before {
                let start = i + pat.len() + 2;
                if let Some(end) = elem[start..].find('"') {
                    return Some(&elem[start..start + end]);
                }
            }
        }
        i += 1;
    }
    None
}

fn parse_first_num(s: &str) -> Option<f32> {
    let s = s.trim();
    let mut end = 0;
    let bytes = s.as_bytes();
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'+' {
            end += 1;
        } else {
            break;
        }
    }
    parse_f32(&s[..end])
}

/// Eenvoudige f32-parser (geen exponenten nodig voor SVG-paden).
fn parse_f32(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut int_part: f32 = 0.0;
    let mut frac: f32 = 0.0;
    let mut scale: f32 = 1.0;
    let mut seen_dot = false;
    let mut any = false;
    for c in s.bytes() {
        match c {
            b'0'..=b'9' => {
                any = true;
                let d = (c - b'0') as f32;
                if seen_dot {
                    scale *= 0.1;
                    frac += d * scale;
                } else {
                    int_part = int_part * 10.0 + d;
                }
            }
            b'.' => seen_dot = true,
            _ => break,
        }
    }
    if !any {
        return None;
    }
    let v = int_part + frac;
    Some(if neg { -v } else { v })
}

// ── Path-tessellatie (M/L/H/V/C + relatieve varianten; A/Q -> rechte lijn) ──
fn draw_path(fb: &FrameBuffer, d: &str, tx: &dyn Fn(f32) -> f32, ty: &dyn Fn(f32) -> f32, _s: f32, thick: i32, color: Color) {
    let mut nums = NumIter::new(d);
    let mut cmds = CmdIter::new(d);
    let (mut x, mut y) = (0f32, 0f32);
    let (mut sx, mut sy) = (0f32, 0f32);
    let (mut px, mut py) = (tx(0.0), ty(0.0));
    let mut started = false;
    let mut cmd = b' ';

    while let Some(c) = cmds.next_cmd(&mut nums) {
        cmd = c;
        let rel = c.is_ascii_lowercase();
        match c.to_ascii_uppercase() {
            b'M' => {
                let nx = nums.next().unwrap_or(0.0);
                let ny = nums.next().unwrap_or(0.0);
                x = if rel { x + nx } else { nx };
                y = if rel { y + ny } else { ny };
                sx = x;
                sy = y;
                px = tx(x);
                py = ty(y);
                started = true;
                // volgende impliciete coords zijn lineto's; CmdIter regelt dat
            }
            b'L' => {
                let nx = nums.next().unwrap_or(0.0);
                let ny = nums.next().unwrap_or(0.0);
                x = if rel { x + nx } else { nx };
                y = if rel { y + ny } else { ny };
                let (cx, cy) = (tx(x), ty(y));
                if started {
                    line(fb, px, py, cx, cy, thick, color);
                }
                px = cx;
                py = cy;
            }
            b'H' => {
                let nx = nums.next().unwrap_or(0.0);
                x = if rel { x + nx } else { nx };
                let (cx, cy) = (tx(x), ty(y));
                line(fb, px, py, cx, cy, thick, color);
                px = cx;
                py = cy;
            }
            b'V' => {
                let ny = nums.next().unwrap_or(0.0);
                y = if rel { y + ny } else { ny };
                let (cx, cy) = (tx(x), ty(y));
                line(fb, px, py, cx, cy, thick, color);
                px = cx;
                py = cy;
            }
            b'C' => {
                let x1 = adj(rel, x, nums.next().unwrap_or(0.0));
                let y1 = adj(rel, y, nums.next().unwrap_or(0.0));
                let x2 = adj(rel, x, nums.next().unwrap_or(0.0));
                let y2 = adj(rel, y, nums.next().unwrap_or(0.0));
                let ex = adj(rel, x, nums.next().unwrap_or(0.0));
                let ey = adj(rel, y, nums.next().unwrap_or(0.0));
                // tessellatie van de cubic bezier
                let steps = 14;
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    let (bx, by) = cubic(x, y, x1, y1, x2, y2, ex, ey, t);
                    let (cx, cy) = (tx(bx), ty(by));
                    line(fb, px, py, cx, cy, thick, color);
                    px = cx;
                    py = cy;
                }
                x = ex;
                y = ey;
            }
            b'A' | b'Q' | b'S' | b'T' => {
                // Niet volledig ondersteund: trek een rechte lijn naar het eindpunt.
                // (De dock-iconen gebruiken deze niet; zo blijft het robuust.)
                let last_two = nums.skip_to_last_pair();
                if let Some((ex_r, ey_r)) = last_two {
                    x = adj(rel, x, ex_r);
                    y = adj(rel, y, ey_r);
                    let (cx, cy) = (tx(x), ty(y));
                    line(fb, px, py, cx, cy, thick, color);
                    px = cx;
                    py = cy;
                }
            }
            b'Z' => {
                let (cx, cy) = (tx(sx), ty(sy));
                line(fb, px, py, cx, cy, thick, color);
                x = sx;
                y = sy;
                px = cx;
                py = cy;
            }
            _ => {}
        }
    }
    let _ = cmd;
}

fn adj(rel: bool, base: f32, v: f32) -> f32 {
    if rel {
        base + v
    } else {
        v
    }
}

fn cubic(x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let dd = t * t * t;
    (a * x0 + b * x1 + c * x2 + dd * x3, a * y0 + b * y1 + c * y2 + dd * y3)
}

// ── Stroke-primitieven — anti-aliased via de framebuffer (afstand-tot-vorm) ──
fn half(thick: i32) -> f32 {
    (thick as f32 * 0.5).max(0.6)
}

fn line(fb: &FrameBuffer, x0: f32, y0: f32, x1: f32, y1: f32, thick: i32, c: Color) {
    fb.aa_seg(x0, y0, x1, y1, half(thick), c);
}

fn stroke_circle(fb: &FrameBuffer, cx: f32, cy: f32, r: f32, thick: i32, c: Color) {
    if r <= 0.0 {
        return;
    }
    fb.aa_ring(cx, cy, r, half(thick), c);
}

fn stroke_round_rect(fb: &FrameBuffer, x: f32, y: f32, w: f32, h: f32, r: f32, thick: i32, c: Color) {
    let r = r.min(w / 2.0).min(h / 2.0);
    // Vier rechte zijden (ingekort met de radius).
    line(fb, x + r, y, x + w - r, y, thick, c); // boven
    line(fb, x + r, y + h, x + w - r, y + h, thick, c); // onder
    line(fb, x, y + r, x, y + h - r, thick, c); // links
    line(fb, x + w, y + r, x + w, y + h - r, thick, c); // rechts
    // Vier hoeken als kwart-cirkels (kwart van een midpoint-cirkel).
    quarter(fb, x + r, y + r, r, thick, c, 2); // links-boven
    quarter(fb, x + w - r, y + r, r, thick, c, 3); // rechts-boven
    quarter(fb, x + r, y + h - r, r, thick, c, 1); // links-onder
    quarter(fb, x + w - r, y + h - r, r, thick, c, 0); // rechts-onder
}

/// Kwart-cirkelboog rond (cx,cy); `q`: 0=RB,1=LB,2=LB-boven,3=RB-boven kwadrant.
fn quarter(fb: &FrameBuffer, cx: f32, cy: f32, r: f32, thick: i32, c: Color, q: u8) {
    let cx = cx as i32;
    let cy = cy as i32;
    let r = r as i32;
    if r <= 0 {
        return;
    }
    for ring in 0..thick {
        let rr = (r - thick / 2 + ring).max(0);
        let mut x = rr;
        let mut y = 0;
        let mut err = 1 - x;
        while x >= y {
            let pts = match q {
                0 => [(cx + x, cy + y), (cx + y, cy + x)], // rechts-onder
                1 => [(cx - x, cy + y), (cx - y, cy + x)], // links-onder
                2 => [(cx - x, cy - y), (cx - y, cy - x)], // links-boven
                _ => [(cx + x, cy - y), (cx + y, cy - x)], // rechts-boven
            };
            for &(px, py) in &pts {
                if px >= 0 && py >= 0 {
                    fb.put_pixel(px as usize, py as usize, c);
                }
            }
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }
}

// ── Tokenizers voor het pad: commando's en getallen ───────────────────────
struct NumIter<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> NumIter<'a> {
    fn new(s: &'a str) -> Self {
        NumIter { bytes: s.as_bytes(), pos: 0 }
    }
    fn next(&mut self) -> Option<f32> {
        let b = self.bytes;
        // sla scheidingstekens over
        while self.pos < b.len() && (b[self.pos] == b' ' || b[self.pos] == b',') {
            self.pos += 1;
        }
        if self.pos >= b.len() || b[self.pos].is_ascii_alphabetic() {
            return None;
        }
        let start = self.pos;
        if b[self.pos] == b'-' || b[self.pos] == b'+' {
            self.pos += 1;
        }
        let mut seen_dot = false;
        while self.pos < b.len() {
            let c = b[self.pos];
            if c.is_ascii_digit() {
                self.pos += 1;
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        parse_f32(core::str::from_utf8(&b[start..self.pos]).ok()?)
    }
    /// Lees alle resterende getallen tot het volgende commando; geef het laatste paar.
    fn skip_to_last_pair(&mut self) -> Option<(f32, f32)> {
        let mut last = None;
        let mut prev = None;
        while let Some(n) = self.peek_num() {
            prev = last;
            last = Some(n);
            self.next();
        }
        match (prev, last) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }
    fn peek_num(&self) -> Option<f32> {
        let mut p = self.pos;
        let b = self.bytes;
        while p < b.len() && (b[p] == b' ' || b[p] == b',') {
            p += 1;
        }
        if p >= b.len() || b[p].is_ascii_alphabetic() {
            return None;
        }
        Some(0.0)
    }
}

struct CmdIter<'a> {
    bytes: &'a [u8],
    pos: usize,
    repeat: u8, // herhaal-commando voor impliciete coords
}
impl<'a> CmdIter<'a> {
    fn new(s: &'a str) -> Self {
        CmdIter { bytes: s.as_bytes(), pos: 0, repeat: 0 }
    }
    /// Geef het volgende commando-letter. Synchroniseert met `nums` via de pos.
    fn next_cmd(&mut self, nums: &mut NumIter) -> Option<u8> {
        // synchroniseer onze positie met de getallen-iterator
        self.pos = nums.pos;
        let b = self.bytes;
        while self.pos < b.len() && (b[self.pos] == b' ' || b[self.pos] == b',') {
            self.pos += 1;
        }
        if self.pos >= b.len() {
            return None;
        }
        let c = b[self.pos];
        if c.is_ascii_alphabetic() {
            self.pos += 1;
            nums.pos = self.pos;
            // M wordt na de eerste coords impliciet L (SVG-regel)
            self.repeat = match c {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            };
            Some(c)
        } else {
            // impliciete herhaling van het vorige commando
            if self.repeat != 0 {
                Some(self.repeat)
            } else {
                None
            }
        }
    }
}
