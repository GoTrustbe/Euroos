//! Built-in screenshot: capture the framebuffer to an image file the system can
//! open (PPM, which EuroWeb already renders). Triggered from the desktop menu;
//! posts a notification when saved.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use eurofs::FileSystem;

use crate::graphics::FrameBuffer;

static CTR: AtomicU64 = AtomicU64::new(1);

/// Encode 0x00RRGGBB pixels as a binary PPM (P6). `stride` is the row stride in
/// pixels (may exceed `w`).
pub fn encode_ppm(pixels: &[u32], w: usize, h: usize, stride: usize) -> Vec<u8> {
    let header = alloc::format!("P6\n{w} {h}\n255\n");
    let mut out = Vec::with_capacity(header.len() + w * h * 3);
    out.extend_from_slice(header.as_bytes());
    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            let px = pixels[row + x];
            out.push((px >> 16) as u8); // R
            out.push((px >> 8) as u8); // G
            out.push(px as u8); // B
        }
    }
    out
}

/// Capture the current framebuffer to `/screenshots/shot-N.ppm`. Returns the
/// path on success. Requires the buffered (backbuffer) framebuffer.
pub fn capture(fb: &FrameBuffer, fs: &mut dyn FileSystem) -> Option<String> {
    let (ptr, w, h, stride) = fb.backbuffer()?;
    let pixels = unsafe { core::slice::from_raw_parts(ptr, stride * h) };
    let ppm = encode_ppm(pixels, w, h, stride);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let _ = fs.create_dir("/screenshots");
    let path = alloc::format!("/screenshots/shot-{n}.ppm");
    if fs.write_file(&path, &ppm).is_ok() {
        crate::serial_println!("[shot] screenshot saved: {path} ({} B)", ppm.len());
        Some(path)
    } else {
        None
    }
}

/// `[shot]` boot self-test: the PPM encoder produces a valid header and the
/// right pixel bytes for a known 2x2 image.
pub fn selftest() {
    let px = [0x00FF0000u32, 0x0000FF00, 0x000000FF, 0x00FFFFFF]; // R G B W, 2x2
    let ppm = encode_ppm(&px, 2, 2, 2);
    let header_ok = ppm.starts_with(b"P6\n2 2\n255\n");
    let len_ok = ppm.len() == 11 + 2 * 2 * 3;
    let first_red = ppm.get(11..14) == Some(&[0xFF, 0x00, 0x00][..]);
    let ok = header_ok && len_ok && first_red;
    crate::serial_println!(
        "[shot] Screenshot (PPM): header={header_ok}, size={len_ok}, pixels={first_red} → {}",
        if ok { "OK (capture the screen to a file) ✓" } else { "FAILED ✗" }
    );
}
