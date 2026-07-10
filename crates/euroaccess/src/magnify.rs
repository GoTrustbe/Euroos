//! 3F-3 — **screen magnification** (follow-focus lens).
//!
//! EN 301 549 / EAA require low-vision support. This is the pure mapping: given a
//! zoom factor and the focused element's bounds, compute which region of the
//! framebuffer to sample, and nearest-neighbour-scale it into a destination
//! buffer. The compositor calls this on its backbuffer before presenting.

use crate::Rect;

/// A follow-focus magnifier with an integer zoom factor.
#[derive(Clone, Copy, Debug)]
pub struct Magnifier {
    pub zoom: u32,
}

impl Magnifier {
    /// `zoom` is clamped to at least 1 (1 = magnification off).
    pub fn new(zoom: u32) -> Self {
        Magnifier { zoom: zoom.max(1) }
    }

    pub fn enabled(&self) -> bool {
        self.zoom > 1
    }

    /// The source region of a `screen_w`×`screen_h` framebuffer to sample so the
    /// magnified view is centred on `focus`, clamped to stay on-screen.
    pub fn source_rect(&self, focus: Rect, screen_w: u32, screen_h: u32) -> Rect {
        let vw = (screen_w / self.zoom).max(1);
        let vh = (screen_h / self.zoom).max(1);
        let (cx, cy) = focus.center();
        let x = (cx - vw as i32 / 2).clamp(0, (screen_w.saturating_sub(vw)) as i32);
        let y = (cy - vh as i32 / 2).clamp(0, (screen_h.saturating_sub(vh)) as i32);
        Rect::new(x, y, vw, vh)
    }

    /// Nearest-neighbour magnify: sample `region` of `src` (a row-major u32
    /// framebuffer of width `src_w`) into `dst` (width `dst_w`, height `dst_h`),
    /// each source pixel expanded to a `zoom`×`zoom` block.
    pub fn blit(&self, src: &[u32], src_w: u32, region: Rect, dst: &mut [u32], dst_w: u32, dst_h: u32) {
        let z = self.zoom;
        for dy in 0..dst_h {
            let sy = region.y as u32 + dy / z;
            let srow = (sy * src_w) as usize;
            let drow = (dy * dst_w) as usize;
            for dx in 0..dst_w {
                let sx = region.x as u32 + dx / z;
                let sidx = srow + sx as usize;
                let didx = drow + dx as usize;
                if sidx < src.len() && didx < dst.len() {
                    dst[didx] = src[sidx];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_rect_centers_and_clamps() {
        let m = Magnifier::new(2);
        // Focus near the top-left corner → clamped to (0,0); view = half the screen.
        let r = m.source_rect(Rect::new(0, 0, 10, 10), 200, 100);
        assert_eq!((r.w, r.h), (100, 50));
        assert_eq!((r.x, r.y), (0, 0));
        // Focus in the centre → centred view.
        let r2 = m.source_rect(Rect::new(100, 50, 0, 0), 200, 100);
        assert_eq!((r2.x, r2.y), (50, 25));
    }

    #[test]
    fn blit_doubles_each_pixel() {
        // A 2×2 source magnified 2× → a 4×4 where each pixel is a 2×2 block.
        let src = [1u32, 2, 3, 4]; // row-major 2×2
        let m = Magnifier::new(2);
        let mut dst = [0u32; 16]; // 4×4
        m.blit(&src, 2, Rect::new(0, 0, 2, 2), &mut dst, 4, 4);
        assert_eq!(
            dst,
            [
                1, 1, 2, 2, //
                1, 1, 2, 2, //
                3, 3, 4, 4, //
                3, 3, 4, 4,
            ]
        );
    }
}
