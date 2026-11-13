//! A file that lives in memory, made of 4 KiB frames: the bytes of a memfd or
//! a shared-memory file are the frames its mappings map, once, instead of a
//! byte vector in the kernel heap with a second copy in frames for every
//! mapping (BUG-132, BUG-152).
//!
//! [`PageFile`] is the file: a length and one frame per page, 0 for a hole
//! (a page nobody has written or touched reads as zeros and costs nothing).
//! [`Budget`] is the limit: how many frames all such files together may hold,
//! derived from the size of the pool they come from, which follows the RAM of
//! the machine. The frames themselves come through the [`Frames`] trait, so
//! the same code runs on the host against a model (tests below) and in the
//! kernel against the frame pool.

use alloc::vec::Vec;

pub const PAGE: usize = 4096;

/// Where frames come from and how their bytes are reached.
pub trait Frames {
    /// A frame, contents undefined. None: no memory, or over the budget.
    fn alloc(&mut self) -> Option<u64>;
    fn free(&mut self, frame: u64);
    /// The 4096 bytes of `frame`.
    fn page(&mut self, frame: u64) -> &mut [u8];
}

/// A write, a grow or a touch that could not get memory. The caller answers
/// ENOSPC (write, ftruncate, fallocate) or treats the fault as out of memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoSpace;

/// The largest file this structure will index (1 TiB): a length a program
/// asks for is never trusted to size the frame list.
pub const MAX_LEN: u64 = 1 << 40;

#[derive(Debug, Default)]
pub struct PageFile {
    len: u64,
    /// Frame of page i, 0 = hole. Never longer than the pages of `len` except
    /// for pages a mapping touched past the end (kept until the file shrinks).
    frames: Vec<u64>,
    held: usize,
}

impl PageFile {
    pub const fn new() -> Self {
        Self { len: 0, frames: Vec::new(), held: 0 }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Frames this file holds.
    pub fn held(&self) -> usize {
        self.held
    }

    /// The frame of page `idx`, 0 when it is a hole.
    pub fn frame_at(&self, idx: usize) -> u64 {
        self.frames.get(idx).copied().unwrap_or(0)
    }

    /// Every frame of the file (holes left out).
    pub fn frames(&self) -> impl Iterator<Item = u64> + '_ {
        self.frames.iter().copied().filter(|&f| f != 0)
    }

    fn index(&mut self, idx: usize) -> Result<(), NoSpace> {
        if idx >= self.frames.len() {
            let more = idx + 1 - self.frames.len();
            self.frames.try_reserve(more).map_err(|_| NoSpace)?;
            self.frames.resize(idx + 1, 0);
        }
        Ok(())
    }

    /// The frame of page `idx`, allocated and zeroed if it was a hole. This is
    /// what a page fault in a shared mapping of the file asks for.
    pub fn ensure_frame(&mut self, fr: &mut dyn Frames, idx: usize) -> Result<u64, NoSpace> {
        if (idx as u64) >= MAX_LEN / PAGE as u64 {
            return Err(NoSpace);
        }
        self.index(idx)?;
        if self.frames[idx] == 0 {
            let f = fr.alloc().ok_or(NoSpace)?;
            fr.page(f).fill(0);
            self.frames[idx] = f;
            self.held += 1;
        }
        Ok(self.frames[idx])
    }

    /// Read at `off` into `buf`; the number of bytes read (0 at or past the end).
    pub fn read(&self, fr: &mut dyn Frames, off: u64, buf: &mut [u8]) -> usize {
        if off >= self.len {
            return 0;
        }
        let n = buf.len().min((self.len - off) as usize);
        let mut done = 0usize;
        while done < n {
            let pos = off as usize + done;
            let (idx, po) = (pos / PAGE, pos % PAGE);
            let k = (PAGE - po).min(n - done);
            match self.frame_at(idx) {
                0 => buf[done..done + k].fill(0),
                f => buf[done..done + k].copy_from_slice(&fr.page(f)[po..po + k]),
            }
            done += k;
        }
        n
    }

    /// Write `data` at `off`, growing the file. The number of bytes written:
    /// all of them, or fewer when memory ran out part-way (what was written
    /// stays and the length covers it). Err when not one byte could be written.
    pub fn write(&mut self, fr: &mut dyn Frames, off: u64, data: &[u8]) -> Result<usize, NoSpace> {
        if data.is_empty() {
            return Ok(0);
        }
        if off.checked_add(data.len() as u64).map(|e| e > MAX_LEN).unwrap_or(true) {
            return Err(NoSpace);
        }
        let mut done = 0usize;
        while done < data.len() {
            let pos = off as usize + done;
            let (idx, po) = (pos / PAGE, pos % PAGE);
            let k = (PAGE - po).min(data.len() - done);
            let f = match self.ensure_frame(fr, idx) {
                Ok(f) => f,
                Err(e) => {
                    if done == 0 {
                        return Err(e);
                    }
                    break;
                }
            };
            fr.page(f)[po..po + k].copy_from_slice(&data[done..done + k]);
            done += k;
        }
        self.len = self.len.max(off + done as u64);
        Ok(done)
    }

    /// ftruncate. Growing costs nothing (holes). Shrinking frees every frame
    /// wholly past the new end and zeroes the rest of the last page, so that
    /// growing again reads zeros there.
    pub fn set_len(&mut self, fr: &mut dyn Frames, len: u64) -> Result<(), NoSpace> {
        if len > MAX_LEN {
            return Err(NoSpace);
        }
        if len < self.len {
            let keep = ((len + PAGE as u64 - 1) / PAGE as u64) as usize;
            if keep < self.frames.len() {
                for i in keep..self.frames.len() {
                    if self.frames[i] != 0 {
                        fr.free(self.frames[i]);
                        self.held -= 1;
                    }
                }
                self.frames.truncate(keep);
                self.frames.shrink_to_fit();
            }
            let po = (len % PAGE as u64) as usize;
            if po != 0 {
                let f = self.frame_at((len / PAGE as u64) as usize);
                if f != 0 {
                    fr.page(f)[po..].fill(0);
                }
            }
        }
        self.len = len;
        Ok(())
    }

    /// Give every frame back; the file is empty afterwards.
    pub fn release(&mut self, fr: &mut dyn Frames) {
        for &f in self.frames.iter() {
            if f != 0 {
                fr.free(f);
            }
        }
        self.forget();
    }

    /// Drop the frame list without freeing the frames: the pool they came
    /// from is going away as a whole. The file is empty afterwards.
    pub fn forget(&mut self) {
        self.frames = Vec::new();
        self.held = 0;
        self.len = 0;
    }
}

/// How many frames the files in memory may hold together, and how many they
/// hold. The limit follows the pool, and the pool follows the RAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub limit: usize,
    pub used: usize,
    /// Frames of the pool that files leave alone (for page faults of programs).
    pub reserve: usize,
    /// Charges refused so far.
    pub refused: u64,
}

impl Budget {
    pub const fn none() -> Self {
        Self { limit: 0, used: 0, reserve: 0, refused: 0 }
    }

    /// The budget for a pool of `pool_frames`: half of the pool for files, and
    /// files stop while a sixteenth of the pool (at most 256 MiB) is still free
    /// for everything else. A 4 GiB guest with a 1.5 GiB pool gets 768 MiB of
    /// files; a 16 GiB guest with 11 GiB, 5.5 GiB.
    pub fn for_pool(pool_frames: usize) -> Self {
        Self { limit: pool_frames / 2, used: 0, reserve: (pool_frames / 16).min(65536), refused: 0 }
    }

    /// May a file take `n` more frames, with `pool_free` frames free in the pool?
    pub fn try_charge(&mut self, n: usize, pool_free: usize) -> bool {
        if self.used + n > self.limit || pool_free < n + self.reserve {
            self.refused += 1;
            return false;
        }
        self.used += n;
        true
    }

    pub fn uncharge(&mut self, n: usize) {
        self.used = self.used.saturating_sub(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Host frames: a map of pages, a capacity, and a ledger of every alloc
    /// and free so that a leak or a double free fails the test.
    struct Host {
        pages: BTreeMap<u64, Vec<u8>>,
        next: u64,
        cap: usize,
        double_free: usize,
    }

    impl Host {
        fn new(cap: usize) -> Self {
            Self { pages: BTreeMap::new(), next: 0x1000, cap, double_free: 0 }
        }
    }

    impl Frames for Host {
        fn alloc(&mut self) -> Option<u64> {
            if self.pages.len() >= self.cap {
                return None;
            }
            let f = self.next;
            self.next += 0x1000;
            self.pages.insert(f, vec![0xAA; PAGE]); // dirty on purpose: the file must zero it
            Some(f)
        }
        fn free(&mut self, frame: u64) {
            if self.pages.remove(&frame).is_none() {
                self.double_free += 1;
            }
        }
        fn page(&mut self, frame: u64) -> &mut [u8] {
            self.pages.get_mut(&frame).expect("a frame that is not allocated")
        }
    }

    #[test]
    fn holes_read_zero_and_cost_nothing() {
        let mut h = Host::new(64);
        let mut f = PageFile::new();
        f.set_len(&mut h, 2 << 20).unwrap();
        assert_eq!((f.len(), f.held()), (2 << 20, 0));
        let mut b = [1u8; 100];
        assert_eq!(f.read(&mut h, 4000, &mut b), 100);
        assert!(b.iter().all(|&x| x == 0));
        assert_eq!(f.write(&mut h, 4090, b"0123456789"), Ok(10));
        assert_eq!(f.held(), 2, "the write crosses a page boundary");
        let mut b = [9u8; 14];
        assert_eq!(f.read(&mut h, 4088, &mut b), 14);
        assert_eq!(&b, b"\0\x000123456789\0\0");
        assert_eq!(f.read(&mut h, 2 << 20, &mut b), 0);
    }

    #[test]
    fn shrink_frees_frames_and_zeroes_the_tail() {
        let mut h = Host::new(64);
        let mut f = PageFile::new();
        f.write(&mut h, 0, &[7u8; 3 * PAGE]).unwrap();
        assert_eq!(f.held(), 3);
        f.set_len(&mut h, 100).unwrap();
        assert_eq!((f.held(), h.pages.len()), (1, 1));
        f.set_len(&mut h, 3 * PAGE as u64).unwrap();
        let mut b = vec![1u8; 3 * PAGE];
        assert_eq!(f.read(&mut h, 0, &mut b), 3 * PAGE);
        assert!(b[..100].iter().all(|&x| x == 7) && b[100..].iter().all(|&x| x == 0));
        f.release(&mut h);
        assert_eq!((h.pages.len(), h.double_free, f.len()), (0, 0, 0));
    }

    #[test]
    fn out_of_frames_is_an_error_not_a_panic() {
        let mut h = Host::new(2);
        let mut f = PageFile::new();
        // Two pages fit, the third does not: a short write, then ENOSPC.
        assert_eq!(f.write(&mut h, 0, &[5u8; 3 * PAGE]), Ok(2 * PAGE));
        assert_eq!(f.len(), 2 * PAGE as u64);
        assert_eq!(f.write(&mut h, 2 * PAGE as u64, b"x"), Err(NoSpace));
        assert_eq!(f.len(), 2 * PAGE as u64, "a refused write does not grow the file");
        assert_eq!(f.ensure_frame(&mut h, 5), Err(NoSpace));
        assert_eq!(f.set_len(&mut h, MAX_LEN + 1), Err(NoSpace));
        assert_eq!(f.write(&mut h, u64::MAX - 2, b"abc"), Err(NoSpace));
        assert_eq!(f.ensure_frame(&mut h, usize::MAX), Err(NoSpace));
        // Overwriting what is there needs no frame and still works.
        assert_eq!(f.write(&mut h, 10, b"abc"), Ok(3));
    }

    #[test]
    fn budget_follows_the_pool() {
        // 4 GiB guest, 1.5 GiB pool; 16 GiB guest, 11 GiB pool.
        let small = Budget::for_pool(384 * 1024);
        let big = Budget::for_pool(2816 * 1024);
        assert_eq!((small.limit / 256, small.reserve / 256), (768, 96));
        assert_eq!((big.limit / 256, big.reserve / 256), (5632, 256));
        let mut b = Budget::for_pool(1024);
        assert_eq!((b.limit, b.reserve), (512, 64));
        assert!(b.try_charge(500, 1024));
        assert!(!b.try_charge(13, 1024), "over the limit");
        assert!(b.try_charge(12, 76));
        assert!(!b.try_charge(1, 64), "the reserve of the pool stays");
        b.uncharge(512);
        assert!(!b.try_charge(1, 64));
        assert!(b.try_charge(1, 65));
        assert_eq!(b.refused, 3);
    }

    #[test]
    fn model_against_a_byte_vector() {
        let mut seed = 0x0123_4567_89ab_cdefu64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        const MAXB: u64 = 40 * PAGE as u64;
        let mut h = Host::new(24); // fewer frames than the file can ask for
        let mut f = PageFile::new();
        let mut m: Vec<u8> = Vec::new();
        let (mut short, mut refused) = (0, 0);
        for step in 0..40000u32 {
            match rnd(8) {
                0..=2 => {
                    let off = rnd(MAXB);
                    let n = rnd(3 * PAGE as u64) as usize;
                    let data: Vec<u8> = (0..n).map(|i| (step as usize + i) as u8 | 1).collect();
                    match f.write(&mut h, off, &data) {
                        Ok(k) => {
                            if k > 0 {
                                if m.len() < off as usize + k {
                                    m.resize(off as usize + k, 0);
                                }
                                m[off as usize..off as usize + k].copy_from_slice(&data[..k]);
                            }
                            if k < n {
                                short += 1;
                            }
                        }
                        Err(NoSpace) => refused += 1,
                    }
                }
                3..=4 => {
                    let off = rnd(MAXB + PAGE as u64);
                    let mut buf = vec![0x55u8; rnd(3 * PAGE as u64) as usize];
                    let got = f.read(&mut h, off, &mut buf);
                    let want = if (off as usize) < m.len() { buf.len().min(m.len() - off as usize) } else { 0 };
                    assert_eq!(got, want, "step {step}");
                    if got > 0 { assert_eq!(&buf[..got], &m[off as usize..off as usize + got], "step {step}"); }
                }
                5 => {
                    let len = rnd(MAXB);
                    f.set_len(&mut h, len).unwrap();
                    m.resize(len as usize, 0);
                }
                6 => {
                    // A fault in a mapping: inside the file only (the kernel refuses the rest).
                    let pages = (m.len() + PAGE - 1) / PAGE;
                    if pages > 0 {
                        let idx = rnd(pages as u64) as usize;
                        if let Ok(fr) = f.ensure_frame(&mut h, idx) {
                            // The mapping sees the bytes of the file, zeros past its end.
                            let page = h.page(fr).to_vec();
                            let lo = idx * PAGE;
                            let hi = m.len().min(lo + PAGE);
                            assert_eq!(&page[..hi - lo], &m[lo..hi], "step {step}");
                            assert!(page[hi - lo..].iter().all(|&b| b == 0), "step {step}: bytes past the end of the file");
                        }
                    }
                }
                _ => {
                    assert_eq!(f.held(), h.pages.len(), "step {step}: frames held = frames allocated");
                    assert_eq!(f.frames().count(), f.held());
                }
            }
            assert_eq!(f.len(), m.len() as u64, "step {step}");
            assert!(h.pages.len() <= 24);
        }
        assert!(short > 0 && refused > 0, "the test must reach the out-of-frames paths ({short}, {refused})");
        f.release(&mut h);
        assert_eq!((h.pages.len(), h.double_free), (0, 0));
    }
}
