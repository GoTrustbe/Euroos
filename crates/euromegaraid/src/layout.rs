//! How the driver's DMA window is divided. Both the driver process and the
//! kernel's block layer use this one definition: the kernel places write data
//! into a data slot and reads read data from it; the driver points the
//! controller at the same slot. Neither side passes raw addresses for slots.
//!
//! ```text
//! 0x00000  frames      MAX_FRAMES x 64 B (32-byte aligned; QEMU maps 2 KiB
//!                      past every frame address, all of it inside the window)
//! 0x00800  queue info  32 B
//! 0x00840  producer index (u32), 0x00844 consumer index (u32)
//! 0x01000  reply ring  up to RING_MAX u32 contexts (2 pages)
//! 0x03000  sense       MAX_FRAMES x 128 B
//! 0x04000  DCMD/pass-through data buffer, 1 page (controller info is 2 KiB)
//! 0x05000  data slots  SLOTS x SLOT_BYTES
//! ```

use crate::frame::{FRAME_SIZE, SENSE_LEN};

pub const PAGE: u64 = 4096;
/// Commands the driver keeps in flight at most (frames in the window).
pub const MAX_FRAMES: usize = 32;
/// Largest reply ring (contexts) the window holds. QEMU's maximum is 2048.
pub const RING_MAX: u32 = 2048;
/// Data slots for block I/O and the bytes per slot (128 sectors).
pub const SLOTS: usize = 4;
pub const SLOT_BYTES: u32 = 64 * 1024;

const OFF_FRAMES: u64 = 0x0000;
const OFF_QINFO: u64 = 0x0800;
const OFF_PI: u64 = 0x0840;
const OFF_CI: u64 = 0x0844;
const OFF_RING: u64 = 0x1000;
const OFF_SENSE: u64 = 0x3000;
const OFF_DCMD: u64 = 0x4000;
pub const DCMD_BYTES: u32 = 4096;
const OFF_SLOTS: u64 = 0x5000;

/// Total window size in bytes and pages.
pub const WINDOW_BYTES: u64 = OFF_SLOTS + SLOTS as u64 * SLOT_BYTES as u64;
pub const WINDOW_PAGES: usize = (WINDOW_BYTES / PAGE) as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    TooSmall,
    Unaligned,
    /// Frames are posted through a 32-bit queue port: the window must lie below 4 GiB.
    Above4G,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub base: u64,
    pub len: u64,
}

impl Layout {
    pub fn in_window(base: u64, len: u64) -> Result<Layout, LayoutError> {
        if base % PAGE != 0 {
            return Err(LayoutError::Unaligned);
        }
        if len < WINDOW_BYTES {
            return Err(LayoutError::TooSmall);
        }
        match base.checked_add(len) {
            Some(end) if end <= 1 << 32 => Ok(Layout { base, len }),
            _ => Err(LayoutError::Above4G),
        }
    }

    pub fn frame(&self, i: usize) -> u64 {
        debug_assert!(i < MAX_FRAMES);
        self.base + OFF_FRAMES + (i * FRAME_SIZE) as u64
    }
    pub fn qinfo(&self) -> u64 {
        self.base + OFF_QINFO
    }
    pub fn producer(&self) -> u64 {
        self.base + OFF_PI
    }
    pub fn consumer(&self) -> u64 {
        self.base + OFF_CI
    }
    pub fn ring(&self) -> u64 {
        self.base + OFF_RING
    }
    pub fn sense(&self, i: usize) -> u64 {
        debug_assert!(i < MAX_FRAMES);
        self.base + OFF_SENSE + (i * SENSE_LEN) as u64
    }
    pub fn dcmd_buf(&self) -> u64 {
        self.base + OFF_DCMD
    }
    /// Data slot `i`, or `None` for an index the window does not have.
    pub fn slot(&self, i: usize) -> Option<u64> {
        (i < SLOTS).then(|| self.base + OFF_SLOTS + i as u64 * SLOT_BYTES as u64)
    }
    /// Is `[addr, addr + len)` inside the window?
    pub fn contains(&self, addr: u64, len: u64) -> bool {
        match (addr.checked_add(len), self.base.checked_add(self.len)) {
            (Some(e), Some(we)) => len > 0 && addr >= self.base && e <= we,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_do_not_overlap_and_fit() {
        let l = Layout::in_window(0x10_0000, WINDOW_BYTES).unwrap();
        assert!(l.frame(MAX_FRAMES - 1) + FRAME_SIZE as u64 <= l.qinfo());
        // QEMU maps 2 KiB from every frame address: still inside the window.
        assert!(l.contains(l.frame(MAX_FRAMES - 1), 2048));
        assert!(l.qinfo() + 32 <= l.producer());
        assert!(l.consumer() + 4 <= l.ring());
        assert!(l.ring() + RING_MAX as u64 * 4 <= l.sense(0));
        assert!(l.sense(MAX_FRAMES - 1) + SENSE_LEN as u64 <= l.dcmd_buf());
        assert!(l.dcmd_buf() + DCMD_BYTES as u64 <= l.slot(0).unwrap());
        assert_eq!(l.slot(SLOTS - 1).unwrap() + SLOT_BYTES as u64, l.base + WINDOW_BYTES);
        assert_eq!(l.slot(SLOTS), None);
        assert_eq!(l.frame(1) % 32, 0);
        assert_eq!(WINDOW_PAGES, 69);
    }

    #[test]
    fn window_checks() {
        assert_eq!(Layout::in_window(0x10_0001, WINDOW_BYTES), Err(LayoutError::Unaligned));
        assert_eq!(Layout::in_window(0x10_0000, WINDOW_BYTES - 1), Err(LayoutError::TooSmall));
        assert_eq!(Layout::in_window(0xFFFF_F000, WINDOW_BYTES), Err(LayoutError::Above4G));
        assert_eq!(Layout::in_window(u64::MAX & !0xFFF, WINDOW_BYTES), Err(LayoutError::Above4G));
        let l = Layout::in_window(0x1000, WINDOW_BYTES).unwrap();
        assert!(!l.contains(0x0FFF, 2));
        assert!(!l.contains(l.base, 0));
        assert!(!l.contains(u64::MAX, 2));
        assert!(l.contains(l.base + WINDOW_BYTES - 1, 1));
        assert!(!l.contains(l.base + WINDOW_BYTES - 1, 2));
    }
}
