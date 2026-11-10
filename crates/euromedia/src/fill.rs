//! Flood fill (the paint bucket), written for coordinates that come straight
//! from a mouse click: any `(x, y)` is accepted, a point outside the image
//! fills nothing.

use crate::{Image, Rgba};
use alloc::vec::Vec;

/// Replace the 4-connected area of equal colour around `(sx, sy)` with `col`.
/// Returns the number of pixels changed (0 for a point outside the image, or
/// when the area already has that colour).
///
/// Scanline fill: the work list holds one entry per run of pixels, not four
/// per pixel, and it grows fallibly. If the memory for it is not there the
/// fill stops where it is instead of aborting the kernel.
pub fn flood_fill(img: &mut Image, sx: i64, sy: i64, col: Rgba) -> usize {
    let (w, h) = (img.width as usize, img.height as usize);
    if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
        return 0;
    }
    // An Image is plain data with public fields: do not trust width x height
    // to match the pixel buffer.
    if w.checked_mul(h).is_none_or(|n| n > img.pixels.len()) {
        return 0;
    }
    let (sx, sy) = (sx as usize, sy as usize);
    let target = img.pixels[sy * w + sx];
    if target == col {
        return 0;
    }
    let mut filled = 0usize;
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if stack.try_reserve(64).is_err() {
        return 0;
    }
    stack.push((sx, sy));
    while let Some((x, y)) = stack.pop() {
        let row = y * w;
        if img.pixels[row + x] != target {
            continue;
        }
        // Widen to the whole run on this row and paint it.
        let mut l = x;
        while l > 0 && img.pixels[row + l - 1] == target {
            l -= 1;
        }
        let mut r = x;
        while r + 1 < w && img.pixels[row + r + 1] == target {
            r += 1;
        }
        for p in &mut img.pixels[row + l..=row + r] {
            *p = col;
        }
        filled += r - l + 1;
        // Queue one seed per run of target pixels in the rows above and below.
        for ny in [y.wrapping_sub(1), y + 1] {
            if ny >= h {
                continue; // also catches the wrap of row 0 - 1
            }
            let nrow = ny * w;
            let mut i = l;
            while i <= r {
                if img.pixels[nrow + i] == target {
                    if stack.len() == stack.capacity() && stack.try_reserve(stack.len()).is_err() {
                        return filled;
                    }
                    stack.push((i, ny));
                    while i <= r && img.pixels[nrow + i] == target {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    filled
}
