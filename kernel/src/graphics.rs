//! Framebuffer-primitieven voor de UEFI GOP.
//!
//! Best-choice details die de spec-MVP oversloeg:
//! - **Pixelformaat-detectie** (RGB vs BGR) i.p.v. hardcoded BGR.
//! - **Stride** wordt gerespecteerd: de scanline-breedte in geheugen kan
//!   groter zijn dan de zichtbare breedte (`stride >= width`).
//! - Alle writes zijn `write_volatile` zodat de compiler ze niet wegoptimaliseert.

use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

/// Kies de beste GOP-mode: voorkeur 1024x768, anders grootste ≤1920x1080.
/// Best-effort — bij geen geschikte mode blijft de huidige mode staan.
pub fn set_best_mode(gop: &mut GraphicsOutput) {
    let modes: alloc::vec::Vec<_> = gop.modes().collect();
    let mut best: Option<usize> = None;
    let mut best_score: i64 = -1;

    for (i, mode) in modes.iter().enumerate() {
        let (w, h) = mode.info().resolution();
        if w < 800 || h < 600 {
            continue;
        }
        let score = if w == 1024 && h == 768 {
            1_000_000
        } else if w <= 1920 && h <= 1080 {
            (w * h) as i64
        } else {
            continue;
        };
        if score > best_score {
            best_score = score;
            best = Some(i);
        }
    }

    if let Some(i) = best {
        let _ = gop.set_mode(&modes[i]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Pak naar 0x00RRGGBB (backbuffer-formaat, los van het GOP-pixelformaat).
    #[inline]
    pub const fn pack(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    #[inline]
    pub const fn from_u32(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// Lineaire interpolatie tussen twee kleuren (t = 0..1).
    #[inline]
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        Color::rgb(
            (self.r as f32 + (other.r as f32 - self.r as f32) * t) as u8,
            (self.g as f32 + (other.g as f32 - self.g as f32) * t) as u8,
            (self.b as f32 + (other.b as f32 - self.b as f32) * t) as u8,
        )
    }

    /// Alpha-mengen: `self` over `dst` met dekking `a` (0..=255).
    #[inline]
    pub fn over(self, dst: Color, a: u8) -> Color {
        let a = a as u32;
        let ia = 255 - a;
        Color::rgb(
            ((self.r as u32 * a + dst.r as u32 * ia) / 255) as u8,
            ((self.g as u32 * a + dst.g as u32 * ia) / 255) as u8,
            ((self.b as u32 * a + dst.b as u32 * ia) / 255) as u8,
        )
    }

    // EuroOS-palet — EDS light-thema (euro.css). Warme zand-desktop, witte
    // oppervlakken, Europees blauw als accent.
    pub const BACKGROUND: Self = Self::rgb(0xF4, 0xF1, 0xEB); // --paper (warme zand)
    pub const PAPER_2: Self = Self::rgb(0xEF, 0xEA, 0xE1); // --paper-2
    pub const SURFACE: Self = Self::rgb(0xFF, 0xFF, 0xFF); // --surface (wit)
    pub const CARD: Self = Self::rgb(0xFB, 0xF9, 0xF5); // --surface-2
    pub const SURFACE_3: Self = Self::rgb(0xF5, 0xF2, 0xEC); // --surface-3
    pub const BORDER: Self = Self::rgb(0xE7, 0xE1, 0xD6); // --line
    pub const TASKBAR: Self = Self::rgb(0xFF, 0xFF, 0xFF);
    pub const HOVER: Self = Self::rgb(0xF5, 0xF2, 0xEC); // --surface-3
    pub const ACCENT: Self = Self::rgb(0x2D, 0x6B, 0xE0); // Europees blauw --accent
    pub const ACCENT_SOFT: Self = Self::rgb(0xEA, 0xF1, 0xFD); // --accent-soft
    pub const BLUE: Self = Self::rgb(0x1E, 0x4F, 0xB0); // --accent-deep
    pub const WHITE: Self = Self::rgb(0xFF, 0xFF, 0xFF); // echt wit (tekst op kleur)
    pub const INK: Self = Self::rgb(0x24, 0x30, 0x3B); // --ink (primaire tekst, donker)
    pub const TEXT_SEC: Self = Self::rgb(0x5C, 0x66, 0x72); // --ink-soft
    pub const TEXT_DIM: Self = Self::rgb(0x8E, 0x96, 0xA1); // --ink-faint
    pub const GOLD: Self = Self::rgb(0xE2, 0xA3, 0x3A); // --gold (EU-sterren)
    pub const SUCCESS: Self = Self::rgb(0x2E, 0x9E, 0x5B); // --ok (geverifieerd)
    pub const SUCCESS_SOFT: Self = Self::rgb(0xE4, 0xF4, 0xEA); // --ok-soft
    pub const YELLOW: Self = Self::rgb(0xD9, 0x98, 0x2B); // --warn
    pub const RED: Self = Self::rgb(0xD6, 0x45, 0x3D); // --danger
}

/// Een bezeten (owned) view op de UEFI framebuffer.
///
/// Tekenen gaat naar een RAM-**backbuffer** (`buf`, 0x00RRGGBB) zodat we
/// alpha-mengen en anti-aliasing kunnen doen zonder trage read-modify-write op
/// MMIO; `present()` blit de backbuffer in één keer naar de GOP (geen tearing).
/// Als `buf` null is werkt de FrameBuffer in *directe* modus (schrijft meteen
/// naar MMIO) — zo blijft de panic-handler werken zonder te alloceren.
pub struct FrameBuffer {
    base: *mut u8,
    buf: *mut u32,
    width: usize,
    height: usize,
    /// Scanline-breedte in *pixels* (kan > width zijn).
    stride: usize,
    format: PixelFormat,
}

impl FrameBuffer {
    /// De RAM-backbuffer (0x00RRGGBB-pixels) + afmetingen, voor consumenten die het
    /// beeld elders heen kopiëren (bv. de virtio-gpu-scanout, BB-2). `None` in
    /// directe modus (geen backbuffer).
    pub fn backbuffer(&self) -> Option<(*const u32, usize, usize, usize)> {
        if self.buf.is_null() {
            None
        } else {
            Some((self.buf as *const u32, self.width, self.height, self.stride))
        }
    }

    /// Directe modus (geen backbuffer) — voor de panic-handler.
    /// # Safety
    /// `base` moet wijzen naar een geldige GOP-framebuffer van minstens
    /// `stride * height * 4` bytes, geldig voor de duur van dit object.
    pub unsafe fn new(
        base: *mut u8,
        width: usize,
        height: usize,
        stride: usize,
        format: PixelFormat,
    ) -> Self {
        Self { base, buf: core::ptr::null_mut(), width, height, stride, format }
    }

    /// Gebufferde modus: alloceert een RAM-backbuffer (geleakt — de FrameBuffer
    /// leeft zo lang als de kernel). Tekenen gaat hierheen; `present()` blit.
    /// # Safety
    /// Zie `new`.
    pub unsafe fn new_buffered(
        base: *mut u8,
        width: usize,
        height: usize,
        stride: usize,
        format: PixelFormat,
    ) -> Self {
        let owned: &'static mut [u32] = alloc::vec![0u32; width * height].leak();
        Self { base, buf: owned.as_mut_ptr(), width, height, stride, format }
    }

    /// Schrijf één pixel rechtstreeks naar de GOP-MMIO (met pixelformaat).
    #[inline]
    fn write_mmio(&self, x: usize, y: usize, c: Color) {
        let offset = (y * self.stride + x) * 4;
        unsafe {
            let p = self.base.add(offset);
            match self.format {
                PixelFormat::Rgb => {
                    p.write_volatile(c.r);
                    p.add(1).write_volatile(c.g);
                    p.add(2).write_volatile(c.b);
                }
                _ => {
                    p.write_volatile(c.b);
                    p.add(1).write_volatile(c.g);
                    p.add(2).write_volatile(c.r);
                }
            }
            p.add(3).write_volatile(0);
        }
    }

    /// Blit de backbuffer volledig naar de GOP.
    pub fn present(&self) {
        self.present_rect(0, 0, self.width, self.height);
    }

    /// Blit alleen het gebied (x,y,w,h). Schrijft één `u32` per pixel i.p.v. drie
    /// losse bytes — fors sneller. De backbuffer (0x00RRGGBB) heeft in LE de
    /// byte-volgorde B,G,R,0 = precies het BGR-formaat, dus voor BGR is het een
    /// directe u32-kopie; voor RGB wisselen we R/B om.
    pub fn present_rect(&self, x: usize, y: usize, w: usize, h: usize) {
        if self.buf.is_null() {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        let dst = self.base as *mut u32;
        let rgb = matches!(self.format, PixelFormat::Rgb);
        for row in y..y_end {
            let src_row = row * self.width;
            let dst_row = row * self.stride;
            for col in x..x_end {
                let v = unsafe { *self.buf.add(src_row + col) };
                let out = if rgb {
                    ((v & 0xFF) << 16) | (v & 0x0000_FF00) | ((v >> 16) & 0xFF)
                } else {
                    v
                };
                unsafe { dst.add(dst_row + col).write_volatile(out) };
            }
        }
    }

    /// Meng `c` met dekking `a` over de bestaande pixel (anti-aliasing).
    #[inline]
    pub fn blend(&self, x: usize, y: usize, c: Color, a: u8) {
        if a == 0 || x >= self.width || y >= self.height {
            return;
        }
        if a == 255 {
            self.put_pixel(x, y, c);
            return;
        }
        let dst = self.get_pixel(x, y);
        self.put_pixel(x, y, c.over(dst, a));
    }

    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }

    #[inline]
    pub fn put_pixel(&self, x: usize, y: usize, c: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        if self.buf.is_null() {
            self.write_mmio(x, y, c); // directe modus (panic)
        } else {
            // SAFETY: bounds gecheckt; index < width*height.
            unsafe { *self.buf.add(y * self.width + x) = c.pack() };
        }
    }

    /// Lees een pixel terug (cursor save-under + alpha-mengen).
    pub fn get_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width || y >= self.height {
            return Color::BACKGROUND;
        }
        if self.buf.is_null() {
            let offset = (y * self.stride + x) * 4;
            // SAFETY: bounds gecheckt.
            unsafe {
                let p = self.base.add(offset);
                let b0 = p.read_volatile();
                let b1 = p.add(1).read_volatile();
                let b2 = p.add(2).read_volatile();
                match self.format {
                    PixelFormat::Rgb => Color::rgb(b0, b1, b2),
                    _ => Color::rgb(b2, b1, b0),
                }
            }
        } else {
            Color::from_u32(unsafe { *self.buf.add(y * self.width + x) })
        }
    }

    pub fn fill_rect(&self, x: usize, y: usize, w: usize, h: usize, c: Color) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for row in y..y_end {
            for col in x..x_end {
                self.put_pixel(col, row, c);
            }
        }
    }

    pub fn clear(&self, c: Color) {
        self.fill_rect(0, 0, self.width, self.height, c);
    }

    /// Gevulde rechthoek met afgeronde hoeken (radius `r`), **anti-aliased**.
    /// In de hoeken wordt de dekking 4×4 ge-supersampled tegen de hoekcirkel,
    /// zodat de randen vloeiend in de achtergrond overlopen i.p.v. getrapt.
    pub fn fill_rounded_rect(&self, x: usize, y: usize, w: usize, h: usize, r: usize, c: Color) {
        let r = r.min(w / 2).min(h / 2);
        if r == 0 {
            self.fill_rect(x, y, w, h, c);
            return;
        }
        let rf = r as f32;
        let r2 = rf * rf;
        for row in 0..h {
            // Verticale hoekkernen: alleen de boven/onderranden buigen.
            let cy = if row < r {
                Some(rf)
            } else if row >= h - r {
                Some((h - r) as f32)
            } else {
                None
            };
            for col in 0..w {
                let cx = if col < r {
                    Some(rf)
                } else if col >= w - r {
                    Some((w - r) as f32)
                } else {
                    None
                };
                match (cx, cy) {
                    (Some(ccx), Some(ccy)) => {
                        // Echte hoek: supersample de dekking.
                        let mut inside = 0u32;
                        let mut sy = 0;
                        while sy < 4 {
                            let py = row as f32 + (sy as f32 + 0.5) * 0.25;
                            let dy = py - ccy;
                            let mut sx = 0;
                            while sx < 4 {
                                let px = col as f32 + (sx as f32 + 0.5) * 0.25;
                                let dx = px - ccx;
                                if dx * dx + dy * dy <= r2 {
                                    inside += 1;
                                }
                                sx += 1;
                            }
                            sy += 1;
                        }
                        if inside == 16 {
                            self.put_pixel(x + col, y + row, c);
                        } else if inside > 0 {
                            self.blend(x + col, y + row, c, (inside * 255 / 16) as u8);
                        }
                    }
                    // Rechte randstrook: vol.
                    _ => self.put_pixel(x + col, y + row, c),
                }
            }
        }
    }

    /// Afgeronde rechthoek met een 150°-lineair verloop (`c0`→`c1`), AA-hoeken.
    /// Voor de kleurrijke app-icoontegels (squircles) uit het EDS.
    pub fn fill_rounded_rect_grad(&self, x: usize, y: usize, w: usize, h: usize, r: usize, c0: Color, c1: Color) {
        let r = r.min(w / 2).min(h / 2);
        let rf = r as f32;
        let r2 = rf * rf;
        // CSS linear-gradient(150deg): richting (sin150°, -cos150°) = (0.5, 0.866).
        let (gdx, gdy) = (0.5f32, 0.866f32);
        let maxp = (w as f32) * gdx + (h as f32) * gdy;
        for row in 0..h {
            let cyc = if row < r {
                Some(rf)
            } else if row >= h - r {
                Some((h - r) as f32)
            } else {
                None
            };
            for col in 0..w {
                let t = ((col as f32) * gdx + (row as f32) * gdy) / maxp;
                let c = c0.lerp(c1, t);
                let cxc = if col < r {
                    Some(rf)
                } else if col >= w - r {
                    Some((w - r) as f32)
                } else {
                    None
                };
                match (cxc, cyc) {
                    (Some(ccx), Some(ccy)) => {
                        let mut inside = 0u32;
                        let mut sy = 0;
                        while sy < 4 {
                            let py = row as f32 + (sy as f32 + 0.5) * 0.25;
                            let dy = py - ccy;
                            let mut sx = 0;
                            while sx < 4 {
                                let px = col as f32 + (sx as f32 + 0.5) * 0.25;
                                let dx = px - ccx;
                                if dx * dx + dy * dy <= r2 {
                                    inside += 1;
                                }
                                sx += 1;
                            }
                            sy += 1;
                        }
                        if inside == 16 {
                            self.put_pixel(x + col, y + row, c);
                        } else if inside > 0 {
                            self.blend(x + col, y + row, c, (inside * 255 / 16) as u8);
                        }
                    }
                    _ => self.put_pixel(x + col, y + row, c),
                }
            }
        }
    }

    /// Anti-aliased dik lijnsegment met ronde uiteinden (afstand-tot-segment).
    pub fn aa_seg(&self, x0: f32, y0: f32, x1: f32, y1: f32, half: f32, c: Color) {
        let half = half.max(0.5);
        let minx = (x0.min(x1) - half - 1.0).max(0.0) as usize;
        let miny = (y0.min(y1) - half - 1.0).max(0.0) as usize;
        let maxx = ((x0.max(x1) + half + 1.0) as usize).min(self.width.saturating_sub(1));
        let maxy = ((y0.max(y1) + half + 1.0) as usize).min(self.height.saturating_sub(1));
        let vx = x1 - x0;
        let vy = y1 - y0;
        let len2 = vx * vx + vy * vy;
        for py in miny..=maxy {
            for px in minx..=maxx {
                let fx = px as f32 + 0.5;
                let fy = py as f32 + 0.5;
                // Projecteer (fx,fy) op het segment → afstand d.
                let t = if len2 > 0.0 {
                    (((fx - x0) * vx + (fy - y0) * vy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let dx = fx - (x0 + t * vx);
                let dy = fy - (y0 + t * vy);
                let d = sqrtf(dx * dx + dy * dy);
                let cov = (half + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, c, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// Anti-aliased cirkelring (omtrek) met dikte `2*half` rond (cx,cy), straal `r`.
    pub fn aa_ring(&self, cx: f32, cy: f32, r: f32, half: f32, c: Color) {
        let outer = r + half + 1.0;
        let minx = (cx - outer).max(0.0) as usize;
        let miny = (cy - outer).max(0.0) as usize;
        let maxx = ((cx + outer) as usize).min(self.width.saturating_sub(1));
        let maxy = ((cy + outer) as usize).min(self.height.saturating_sub(1));
        for py in miny..=maxy {
            for px in minx..=maxx {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let dist = sqrtf(dx * dx + dy * dy);
                let cov = (half + 0.5 - (dist - r).abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, c, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// Zachte slagschaduw rond een (afgeronde) rechthoek: dekking valt vloeiend
    /// af met de afstand tot de rand. Het interieur wordt overgeslagen (het
    /// venster tekent daar zelf overheen) zodat alleen de halo gemengd wordt.
    pub fn drop_shadow(&self, wx: usize, wy: usize, w: usize, h: usize, spread: i32, dyoff: i32, c: Color) {
        let x0 = wx as i32;
        let y0 = wy as i32;
        let x1 = x0 + w as i32;
        let y1 = y0 + h as i32;
        // De schaduw-rechthoek staat iets lager (licht van bovenaf).
        let (rx0, ry0, rx1, ry1) = (x0, y0 + dyoff, x1, y1 + dyoff);
        let py_start = (y0 - spread).max(0);
        let py_end = (y1 + spread + dyoff).min(self.height as i32);
        let px_start = (x0 - spread).max(0);
        let px_end = (x1 + spread).min(self.width as i32);
        let sp = spread as f32;
        for py in py_start..py_end {
            for px in px_start..px_end {
                if px >= x0 && px < x1 && py >= y0 && py < y1 {
                    continue; // interieur — venster overschrijft dit
                }
                let ddx = (rx0 - px).max(px - rx1).max(0) as f32;
                let ddy = (ry0 - py).max(py - ry1).max(0) as f32;
                let d = sqrtf(ddx * ddx + ddy * ddy);
                if d >= sp {
                    continue;
                }
                let t = 1.0 - d / sp;
                let a = (70.0 * t * t) as u8;
                self.blend(px as usize, py as usize, c, a);
            }
        }
    }

    pub fn draw_border(&self, x: usize, y: usize, w: usize, h: usize, thick: usize, c: Color) {
        self.fill_rect(x, y, w, thick, c);
        self.fill_rect(x, y + h.saturating_sub(thick), w, thick, c);
        self.fill_rect(x, y, thick, h, c);
        self.fill_rect(x + w.saturating_sub(thick), y, thick, h, c);
    }
}

/// Vierkantswortel zonder libm (geen std/float-intrinsics in no_std).
/// Bit-truc voor een ruwe schatting + twee Newton-stappen → ruim genoeg
/// nauwkeurig voor anti-aliasing-dekking.
#[inline]
pub fn sqrtf(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let i = x.to_bits();
    let mut y = f32::from_bits((i >> 1) + 0x1fbd_1df5);
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y
}
