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
    // The layout of the launchers today (kernel/src/ring3.rs): brk from +32 MiB,
    // the mmap window from +64 MiB, the stack at the top, whatever the span.
    let brk_start = base + 32 * MIB + sl.brk;
    let mmap_start = base + 64 * MIB + sl.mmap;
    let stack_top = base + span - MIB - sl.stack;
    Some(Layout {
        exe: base,
        ldso: base + 8 * MIB,
        brk_start,
        brk_end: mmap_start,
        mmap_start,
        heap_end: stack_top - MIB,
        stack_top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The largest slides `aslr_slides` gives, none, and one in between.
    const SLIDES: [Slides; 3] = [
        Slides { brk: 0, mmap: 0, stack: 0 },
        Slides { brk: 0xF << 16, mmap: 0x1F << 16, stack: 0xFF << 12 },
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

    #[test]
    fn an_arena_without_room_is_refused() {
        assert!(glibc_layout(0, 16 * MIB, SLIDES[1]).is_none());
    }
}
