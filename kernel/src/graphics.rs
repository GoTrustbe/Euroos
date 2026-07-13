//! Framebuffer primitives for the UEFI GOP.
//!
//! Best-choice details the spec-MVP skipped:
//! - **Pixel-format detection** (RGB vs BGR) instead of hardcoded BGR.
//! - **Stride** is respected: the scanline width in memory can be
//!   larger than the visible width (`stride >= width`).
//! - All writes are `write_volatile` so the compiler does not optimize them away.

use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

/// Pick the best GOP mode: prefer 1024x768, otherwise the largest ≤1920x1080.
/// Best-effort — if no suitable mode is found, the current mode stays.
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

    /// Pack into 0x00RRGGBB (backbuffer format, independent of the GOP pixel format).
    #[inline]
    pub const fn pack(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    #[inline]
    pub const fn from_u32(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// Linear interpolation between two colors (t = 0..1).
    #[inline]
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        Color::rgb(
            (self.r as f32 + (other.r as f32 - self.r as f32) * t) as u8,
            (self.g as f32 + (other.g as f32 - self.g as f32) * t) as u8,
            (self.b as f32 + (other.b as f32 - self.b as f32) * t) as u8,
        )
    }

    /// Alpha-blend: `self` over `dst` with opacity `a` (0..=255).
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

    // EuroOS palette — EDS light theme (euro.css). Warm sand desktop, white
    // surfaces, European blue as accent.
    pub const BACKGROUND: Self = Self::rgb(0xF4, 0xF1, 0xEB); // --paper (warm sand)
    pub const PAPER_2: Self = Self::rgb(0xEF, 0xEA, 0xE1); // --paper-2
    pub const SURFACE: Self = Self::rgb(0xFF, 0xFF, 0xFF); // --surface (white)
    pub const CARD: Self = Self::rgb(0xFB, 0xF9, 0xF5); // --surface-2
    pub const SURFACE_3: Self = Self::rgb(0xF5, 0xF2, 0xEC); // --surface-3
    pub const BORDER: Self = Self::rgb(0xE7, 0xE1, 0xD6); // --line
    pub const TASKBAR: Self = Self::rgb(0xFF, 0xFF, 0xFF);
    pub const HOVER: Self = Self::rgb(0xF5, 0xF2, 0xEC); // --surface-3
    pub const ACCENT: Self = Self::rgb(0x2D, 0x6B, 0xE0); // European blue --accent
    pub const ACCENT_SOFT: Self = Self::rgb(0xEA, 0xF1, 0xFD); // --accent-soft
    pub const BLUE: Self = Self::rgb(0x1E, 0x4F, 0xB0); // --accent-deep
    pub const WHITE: Self = Self::rgb(0xFF, 0xFF, 0xFF); // true white (text on color)
    pub const INK: Self = Self::rgb(0x24, 0x30, 0x3B); // --ink (primary text, dark)
    pub const TEXT_SEC: Self = Self::rgb(0x5C, 0x66, 0x72); // --ink-soft
    pub const TEXT_DIM: Self = Self::rgb(0x8E, 0x96, 0xA1); // --ink-faint
    pub const GOLD: Self = Self::rgb(0xE2, 0xA3, 0x3A); // --gold (EU stars)
    pub const SUCCESS: Self = Self::rgb(0x2E, 0x9E, 0x5B); // --ok (verified)
    pub const SUCCESS_SOFT: Self = Self::rgb(0xE4, 0xF4, 0xEA); // --ok-soft
    pub const YELLOW: Self = Self::rgb(0xD9, 0x98, 0x2B); // --warn
    pub const RED: Self = Self::rgb(0xD6, 0x45, 0x3D); // --danger
}

/// An owned view onto the UEFI framebuffer.
///
/// Drawing goes to a RAM **backbuffer** (`buf`, 0x00RRGGBB) so we can
/// alpha-blend and anti-alias without slow read-modify-write on
/// MMIO; `present()` blits the backbuffer all at once to the GOP (no tearing).
/// If `buf` is null the FrameBuffer works in *direct* mode (writes straight
/// to MMIO) — this keeps the panic handler working without allocating.
pub struct FrameBuffer {
    base: *mut u8,
    buf: *mut u32,
    width: usize,
    height: usize,
    /// Scanline width in *pixels* (can be > width).
    stride: usize,
    format: PixelFormat,
}

impl FrameBuffer {
    /// The RAM backbuffer (0x00RRGGBB pixels) + dimensions, for consumers that copy the
    /// image elsewhere (e.g. the virtio-gpu scanout, BB-2). `None` in
    /// direct mode (no backbuffer).
    pub fn backbuffer(&self) -> Option<(*const u32, usize, usize, usize)> {
        if self.buf.is_null() {
            None
        } else {
            Some((self.buf as *const u32, self.width, self.height, self.stride))
        }
    }

    /// Copy the whole backbuffer (width*height pixels) out to `dst` — used to
    /// cache an expensive-to-draw layer (e.g. the wallpaper) so later frames can
    /// restore it with a cheap memcpy instead of recomputing it.
    pub fn snapshot(&self, dst: &mut alloc::vec::Vec<u32>) {
        if self.buf.is_null() {
            return;
        }
        let n = self.width * self.height;
        dst.clear();
        dst.reserve(n);
        unsafe {
            dst.set_len(n);
            core::ptr::copy_nonoverlapping(self.buf, dst.as_mut_ptr(), n);
        }
    }

    /// Short tag for the GOP pixel format (diagnostics).
    pub fn format_tag(&self) -> &'static str {
        match self.format {
            PixelFormat::Rgb => "RGB",
            PixelFormat::Bgr => "BGR",
            PixelFormat::Bitmask => "MASK",
            PixelFormat::BltOnly => "BLT",
        }
    }

    /// Restore a previously snapshotted layer into the backbuffer (cheap memcpy).
    pub fn restore(&self, src: &[u32]) {
        if self.buf.is_null() || src.len() != self.width * self.height {
            return;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.buf, src.len());
        }
    }

    /// Direct mode (no backbuffer) — for the panic handler.
    /// # Safety
    /// `base` must point to a valid GOP framebuffer of at least
    /// `stride * height * 4` bytes, valid for the lifetime of this object.
    pub unsafe fn new(
        base: *mut u8,
        width: usize,
        height: usize,
        stride: usize,
        format: PixelFormat,
    ) -> Self {
        Self { base, buf: core::ptr::null_mut(), width, height, stride, format }
    }

    /// Buffered mode: allocates a RAM backbuffer (leaked — the FrameBuffer
    /// lives as long as the kernel). Drawing goes here; `present()` blits.
    /// # Safety
    /// See `new`.
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

    /// Write one pixel straight to the GOP MMIO (with pixel format).
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

    /// Blit the entire backbuffer to the GOP.
    pub fn present(&self) {
        self.present_rect(0, 0, self.width, self.height);
    }

    /// Blit only the region (x,y,w,h). Writes one `u32` per pixel instead of three
    /// separate bytes — much faster. The backbuffer (0x00RRGGBB) has, in LE, the
    /// byte order B,G,R,0 = exactly the BGR format, so for BGR it is a
    /// direct u32 copy; for RGB we swap R/B.
    pub fn present_rect(&self, x: usize, y: usize, w: usize, h: usize) {
        if self.buf.is_null() {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x_end <= x || y_end <= y {
            return;
        }
        let dst = self.base as *mut u32;
        let rgb = matches!(self.format, PixelFormat::Rgb);
        let run = x_end - x;
        for row in y..y_end {
            let src_row = row * self.width;
            let dst_row = row * self.stride;
            if !rgb {
                // BGR framebuffer: the backbuffer u32 is already the exact byte
                // order — copy the whole row segment at once (a memcpy is orders
                // of magnitude faster than per-pixel volatile writes, which is
                // what made full-screen presents crawl under TCG emulation).
                unsafe {
                    core::ptr::copy_nonoverlapping(self.buf.add(src_row + x), dst.add(dst_row + x), run);
                }
            } else {
                for col in x..x_end {
                    let v = unsafe { *self.buf.add(src_row + col) };
                    let out = ((v & 0xFF) << 16) | (v & 0x0000_FF00) | ((v >> 16) & 0xFF);
                    unsafe { dst.add(dst_row + col).write_volatile(out) };
                }
            }
        }
    }

    /// Blend `c` with opacity `a` over the existing pixel (anti-aliasing).
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
            self.write_mmio(x, y, c); // direct mode (panic)
        } else {
            // SAFETY: bounds checked; index < width*height.
            unsafe { *self.buf.add(y * self.width + x) = c.pack() };
        }
    }

    /// Read a pixel back (cursor save-under + alpha-blend).
    pub fn get_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width || y >= self.height {
            return Color::BACKGROUND;
        }
        if self.buf.is_null() {
            let offset = (y * self.stride + x) * 4;
            // SAFETY: bounds checked.
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
        if x_end <= x || y_end <= y {
            return;
        }
        if self.buf.is_null() {
            // Direct (panic) mode: no backbuffer — write straight to MMIO.
            for row in y..y_end {
                for col in x..x_end {
                    self.write_mmio(col, row, c);
                }
            }
            return;
        }
        // Buffered mode: pack the color ONCE and fill each row with a bulk
        // memset-style write (no per-pixel bounds check / repack), which is
        // orders of magnitude faster than per-pixel put_pixel under TCG.
        let packed = c.pack();
        let run = x_end - x;
        for row in y..y_end {
            let base = row * self.width + x;
            let slice = unsafe { core::slice::from_raw_parts_mut(self.buf.add(base), run) };
            slice.fill(packed);
        }
    }

    pub fn clear(&self, c: Color) {
        self.fill_rect(0, 0, self.width, self.height, c);
    }

    /// Blit an XRGB8888 (`0x00RRGGBB`) source image into the backbuffer at
    /// `(dx,dy)`, integer-scaled by `scale`. The source pixel format is IDENTICAL
    /// to the backbuffer format (see [`Color::pack`]), so pixels are copied
    /// verbatim — no per-pixel repack. Used by the app-graphics bridge: a
    /// scheduled userspace app (the DOOM port) hands over frames this way, and
    /// `present_rect` does the final BGR conversion at scan-out.
    pub fn blit_xrgb(&self, dx: usize, dy: usize, src: &[u32], sw: usize, sh: usize, scale: usize) {
        if self.buf.is_null() || sw == 0 || sh == 0 {
            return;
        }
        let scale = scale.max(1);
        for sy in 0..sh {
            let src_row = &src[sy * sw..sy * sw + sw];
            for k in 0..scale {
                let ty = dy + sy * scale + k;
                if ty >= self.height {
                    return;
                }
                let maxrun = self.width.saturating_sub(dx);
                if maxrun == 0 {
                    continue;
                }
                // SAFETY: `ty < height`, and we clamp the run to `maxrun` below.
                let dst = unsafe { core::slice::from_raw_parts_mut(self.buf.add(ty * self.width + dx), maxrun) };
                let mut di = 0usize;
                for &v in src_row {
                    for _ in 0..scale {
                        if di >= maxrun {
                            break;
                        }
                        dst[di] = v;
                        di += 1;
                    }
                }
            }
        }
    }

    /// Filled rectangle with rounded corners (radius `r`), **anti-aliased**.
    /// In the corners the coverage is 4×4 supersampled against the corner circle,
    /// so the edges blend smoothly into the background instead of being stepped.
    pub fn fill_rounded_rect(&self, x: usize, y: usize, w: usize, h: usize, r: usize, c: Color) {
        let r = r.min(w / 2).min(h / 2);
        if r == 0 {
            self.fill_rect(x, y, w, h, c);
            return;
        }
        // Bulk-fill the solid interior with three fast rects; only the four
        // rounded corners need per-pixel supersampled coverage (was: per-pixel
        // over the ENTIRE window, the bulk of a window's draw cost under TCG).
        self.fill_rect(x, y + r, w, h - 2 * r, c); // middle band, full width
        self.fill_rect(x + r, y, w - 2 * r, r, c); // top straight strip
        self.fill_rect(x + r, y + h - r, w - 2 * r, r, c); // bottom straight strip

        let rf = r as f32;
        let r2 = rf * rf;
        // (corner-cell top-left in window coords, arc-center in window coords)
        let corners = [
            (0usize, 0usize, rf, rf),
            (w - r, 0, (w - r) as f32, rf),
            (0, h - r, rf, (h - r) as f32),
            (w - r, h - r, (w - r) as f32, (h - r) as f32),
        ];
        for (cx0, cy0, ccx, ccy) in corners {
            for row in cy0..cy0 + r {
                for col in cx0..cx0 + r {
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
            }
        }
    }

    /// Rounded rectangle with a 150° linear gradient (`c0`→`c1`), AA corners.
    /// For the colorful app icon tiles (squircles) from the EDS.
    pub fn fill_rounded_rect_grad(&self, x: usize, y: usize, w: usize, h: usize, r: usize, c0: Color, c1: Color) {
        let r = r.min(w / 2).min(h / 2);
        let rf = r as f32;
        let r2 = rf * rf;
        // CSS linear-gradient(150deg): direction (sin150°, -cos150°) = (0.5, 0.866).
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

    /// Anti-aliased thick line segment with round caps (distance-to-segment).
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
                // Project (fx,fy) onto the segment → distance d.
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

    /// Anti-aliased circle ring (outline) of thickness `2*half` around (cx,cy), radius `r`.
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

    /// Soft drop shadow around a (rounded) rectangle: coverage falls off smoothly
    /// with the distance to the edge. The interior is skipped (the
    /// window draws over it itself) so only the halo is blended.
    pub fn drop_shadow(&self, wx: usize, wy: usize, w: usize, h: usize, spread: i32, dyoff: i32, c: Color) {
        let x0 = wx as i32;
        let y0 = wy as i32;
        let x1 = x0 + w as i32;
        let y1 = y0 + h as i32;
        // The shadow rectangle sits slightly lower (light from above).
        let (rx0, ry0, rx1, ry1) = (x0, y0 + dyoff, x1, y1 + dyoff);
        let py_start = (y0 - spread).max(0);
        let py_end = (y1 + spread + dyoff).min(self.height as i32);
        let px_start = (x0 - spread).max(0);
        let px_end = (x1 + spread).min(self.width as i32);
        let sp = spread as f32;
        for py in py_start..py_end {
            for px in px_start..px_end {
                if px >= x0 && px < x1 && py >= y0 && py < y1 {
                    continue; // interior — window overwrites this
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

/// Square root without libm (no std/float intrinsics in no_std).
/// Bit trick for a rough estimate + two Newton steps → plenty
/// accurate for anti-aliasing coverage.
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
