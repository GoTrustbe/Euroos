//! The layout of a glibc process's arena: one identity-mapped span that holds
//! the executable, ld.so, the brk heap, the mmap window and the stack.
//!
//! BUG-136: the launchers fall back to smaller arenas when RAM is fragmented
//! (down to 64 MiB), but the layout was fixed: brk from +32 MiB, the mmap
//! window from +64 MiB, the stack at the top. In a 64 MiB arena the mmap window
//! started past the end of the arena and the brk region ran past it (into the
//! stack and beyond the last mapped byte). Every region must lie inside the
//! arena, whatever its size, or the arena is refused.

pub const MIB: u64 = 1 << 20;

/// The random slides of one start (ASLR).
#[derive(Clone, Copy, Debug)]
pub struct Slides {
    pub brk: u64,
    pub mmap: u64,
    pub stack: u64,
}

/// Where the regions of an arena at `base` lie (absolute addresses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The executable (PIE, first PT_LOAD at 0).
    pub exe: u64,
    /// ld.so.
    pub ldso: u64,
    /// The brk heap: [brk_start, brk_end).
    pub brk_start: u64,
    pub brk_end: u64,
    /// The mmap window: [mmap_start, heap_end).
    pub mmap_start: u64,
    pub heap_end: u64,
    /// The initial stack pointer region ends here.
    pub stack_top: u64,
}

/// The layout of an arena of `span` bytes at `base`, or None when no layout
/// with every region inside the arena exists.
pub fn glibc_layout(base: u64, span: u64, sl: Slides) -> Option<Layout> {
    // From 128 MiB the layout of every earlier release: brk from +32 MiB, the
    // mmap window from +64 MiB. A smaller arena keeps the same order in the same
    // proportions (a quarter, three eighths): 64 MiB gives brk from +16 MiB and
    // the mmap window from +24 MiB, ~35 MiB of it.
    let (brk_off, mmap_off) = if span >= 128 * MIB { (32 * MIB, 64 * MIB) } else { (span / 4, span * 3 / 8) };
    let brk_start = base + brk_off + sl.brk;
    let mmap_start = base + mmap_off + sl.mmap;
    let stack_top = (base + span).checked_sub(MIB + sl.stack)?;
    let heap_end = stack_top.checked_sub(MIB)?;
    let l = Layout { exe: base, ldso: base + 8 * MIB, brk_start, brk_end: mmap_start, mmap_start, heap_end, stack_top };
    // Every region inside the arena and in order, with room in each.
    let ok = l.brk_start >= l.ldso + MIB
        && l.brk_end >= l.brk_start + 4 * MIB
        && l.heap_end >= l.mmap_start + 8 * MIB
        && l.stack_top <= base + span;
    ok.then_some(l)
}

/// ASLR S6: the layout of an arena whose segments are `segs`. One piece: as
/// [`glibc_layout`] at its base. Three segments (image, heap, stack,
/// `layout::split_segs`): every region is the arena offset of `glibc_layout`
/// seen through its segment, the mmap window ends where the heap segment ends,
/// and each region must lie in its own segment (ld.so with its largest slide in
/// the image, the brk heap and at least 8 MiB of mmap window in the heap, the
/// stack top in the stack segment), or None.
pub fn glibc_layout_segs(segs: &crate::layout::Segs, sl: Slides) -> Option<Layout> {
    let span = segs.span();
    if !segs.split() {
        return glibc_layout(segs.base(), span, sl);
    }
    let o = glibc_layout(0, span, sl)?;
    let x = segs.list();
    let (img, heap, stk) = (x[0], x[1], x[2]);
    let ok = o.ldso + crate::layout::SLIDE_WINDOW + MIB <= img.len
        && o.brk_start >= heap.off
        && o.mmap_start + 8 * MIB <= heap.off + heap.len
        && o.stack_top > stk.off
        && o.stack_top <= stk.off + stk.len;
    if !ok {
        return None;
    }
    Some(Layout {
        exe: img.virt,
        ldso: segs.virt(o.ldso),
        brk_start: segs.virt(o.brk_start),
        brk_end: segs.virt(o.brk_end),
        mmap_start: segs.virt(o.mmap_start),
        heap_end: heap.end(),
        stack_top: segs.virt(o.stack_top),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest slides `aslr_slides` gives, none, and one in between.
    const SLIDES: [Slides; 3] = [
        Slides { brk: 0, mmap: 0, stack: 0 },
        Slides { brk: 0xFF << 12, mmap: 0x1FF << 12, stack: 0xFF << 12 },
        Slides { brk: 7 << 16, mmap: 3 << 16, stack: 100 << 12 },
    ];

    fn check(span_mib: u64) {
        let base = 0x0620_0000;
        let end = base + span_mib * MIB;
        for sl in SLIDES {
            let l = glibc_layout(base, span_mib * MIB, sl).expect("an arena of 64 MiB or more has a layout");
            // exe below +8 MiB, ld.so from +8 MiB (well under 1 MiB): the heap starts above both.
            assert!(l.brk_start >= l.ldso + MIB, "{span_mib} MiB {sl:?}: brk {:#x} over ld.so", l.brk_start);
            assert!(l.brk_end >= l.brk_start + 4 * MIB, "{span_mib} MiB {sl:?}: brk region under 4 MiB");
            assert!(l.mmap_start >= l.brk_end, "{span_mib} MiB {sl:?}: mmap window inside the brk region");
            assert!(l.heap_end >= l.mmap_start + 8 * MIB, "{span_mib} MiB {sl:?}: mmap window {:#x}..{:#x} under 8 MiB", l.mmap_start, l.heap_end);
            assert!(l.stack_top > l.heap_end, "{span_mib} MiB {sl:?}: stack under the mmap window");
            assert!(l.stack_top <= end, "{span_mib} MiB {sl:?}: stack past the arena");
            assert!(l.brk_end <= end && l.heap_end <= end, "{span_mib} MiB {sl:?}: a region past the arena end {end:#x}");
        }
    }

    #[test]
    fn every_arena_size_the_launchers_take() {
        for span in [64, 96, 128, 192, 256, 384, 512, 1024] {
            check(span);
        }
    }

    #[test]
    fn large_arenas_keep_their_layout() {
        let l = glibc_layout(0, 256 * MIB, SLIDES[0]).unwrap();
        assert_eq!((l.brk_start, l.mmap_start, l.stack_top), (32 * MIB, 64 * MIB, 255 * MIB));
    }

    /// ASLR S6: with three segments every region lies in its own segment, the
    /// brk heap and the mmap window are those of one piece (same offsets), the
    /// mmap window ends with the heap segment; one piece is the layout of before.
    #[test]
    fn segments_hold_their_regions() {
        use crate::layout::{seg_cuts, Seg, Segs};
        for span_mib in [64u64, 96, 128, 192, 256, 384, 512, 1024] {
            let span = span_mib * MIB;
            let (h, s) = seg_cuts(span);
            let segs = Segs::three([
                Seg { virt: 9 << 40, off: 0, len: h },
                Seg { virt: 13 << 40, off: h, len: s - h },
                Seg { virt: 17 << 40, off: s, len: span - s },
            ])
            .unwrap();
            for sl in SLIDES {
                let one = glibc_layout(0, span, sl).unwrap();
                let l = glibc_layout_segs(&segs, sl).unwrap_or_else(|| panic!("{span_mib} MiB {sl:?}: no layout"));
                let x = segs.list();
                assert_eq!(l.exe, x[0].virt);
                assert!(x[0].has(l.ldso) && l.ldso - x[0].virt == one.ldso);
                assert!(x[1].has(l.brk_start) && x[1].has(l.mmap_start));
                assert_eq!((l.brk_start - x[1].virt + h, l.mmap_start - x[1].virt + h), (one.brk_start, one.mmap_start));
                assert_eq!(l.brk_end, l.mmap_start);
                assert_eq!(l.heap_end, x[1].end());
                assert!(l.heap_end >= l.mmap_start + 8 * MIB);
                assert!(l.stack_top > x[2].virt && l.stack_top <= x[2].end());
                assert_eq!(l.stack_top - x[2].virt + s, one.stack_top);
                // The stack keeps at least 2 MiB below its top inside its segment.
                assert!(l.stack_top - x[2].virt >= 2 * MIB);
            }
            assert_eq!(glibc_layout_segs(&Segs::one(0x0620_0000, span), SLIDES[1]), glibc_layout(0x0620_0000, span, SLIDES[1]));
        }
    }

    #[test]
    fn an_arena_without_room_is_refused() {
        assert!(glibc_layout(0, 16 * MIB, SLIDES[1]).is_none());
    }
}
