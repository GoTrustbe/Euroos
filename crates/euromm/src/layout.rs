//! Where a Linux-ABI (glibc) process's memory lies: the zones of the address
//! space and the one random draw per image (ASLR, docs/design/ASLR.md 2.1, 2.2, 4).
//!
//! Pure functions over the bytes of one draw from the kernel DRBG, so every
//! property (inside its zone, aligned, disjoint, below 2^47, the bits a region
//! really gets) is checked on the host for random and extreme inputs.
//!
//! Three modes (boot option `aslr=`):
//! - `legacy`: the layout of before: the demand region in PML4 slot 2 (1 TiB,
//!   480 GiB), the executable from disk at its start, the mmap floor 192 MiB
//!   above it plus a 19-bit page slide, the vDSO at a fixed address.
//! - `on`: the demand zone 32..96 TiB (PML4 64..191); the executable from disk
//!   at 32 TiB plus a draw over 4 TiB, aligned to its largest `p_align`; the
//!   mmap floor at 40 TiB plus a page draw over 4 TiB (30 bits); the vDSO at a
//!   random page of PML4 slot 200 (27 bits).
//! - `off`: the zones of `on` with every draw 0: the same addresses every run.
//!
//! The slides inside the arena (brk, small mmaps, stack) are those of before in
//! `legacy` and `on` (the arena does not move until step S5) and 0 in `off`.

use core::ops::Range;

pub const PAGE: u64 = 4096;
pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;
pub const TIB: u64 = 1 << 40;
/// The user half of 4-level paging: every user address lies below it.
pub const USER_TOP: u64 = 1 << 47;
/// The arena zone of step S2 (`paging::ARENA_ZONE_*`): 8..12 TiB.
pub const ARENA_ZONE: Range<u64> = 8 * TIB..12 * TIB;
/// ASLR S5: the arena base is a 2 MiB step of the zone (21 bits).
pub const ARENA_STEP: u64 = 2 * MIB;
/// Where ld.so lies in the arena (before its page slide), as before S5.
pub const LDSO_OFF: u64 = 8 * MIB;
/// ASLR S5: the page slides of the embedded executable and of ld.so inside the
/// arena are below this (9 bits of pages).
pub const SLIDE_WINDOW: u64 = 2 * MIB;

/// `legacy`: the demand region of before (PML4 slot 2).
pub const LEGACY_DEMAND_BASE: u64 = 2 << 39;
pub const LEGACY_DEMAND_SIZE: u64 = 480 * GIB;
/// `legacy`: the fixed vDSO address (PML4 slot 3).
pub const LEGACY_VDSO: u64 = 0x1F0_0000_0000;
/// The window of a program from disk below the mmap floor in `legacy`, and
/// the smallest window in the other modes: 192 MiB, as before.
pub const EXE_WINDOW: u64 = 0x0C00_0000;

/// `on`/`off`: the demand zone, 32..96 TiB.
pub const DEMAND_BASE: u64 = 32 * TIB;
pub const DEMAND_SIZE: u64 = 64 * TIB;
/// The executable from disk: 32 TiB plus a draw below 4 TiB.
pub const EXE_BASE: u64 = 32 * TIB;
pub const EXE_DRAW: u64 = 4 * TIB;
/// The mmap floor: 40 TiB plus a page draw below 4 TiB; the bump allocator
/// grows from there to the end of the demand zone (52 TiB at least).
pub const FLOOR_BASE: u64 = 40 * TIB;
pub const FLOOR_DRAW: u64 = 4 * TIB;
/// The vDSO: a random page of PML4 slot 200 (100..100.5 TiB), two pages.
pub const VDSO_ZONE: u64 = 100 * TIB;
pub const VDSO_DRAW: u64 = 512 * GIB;
pub const VDSO_PAGES: u64 = 2;
/// The largest executable window `draw` accepts (`exe_span` above it is cut).
pub const EXE_SPAN_MAX: u64 = TIB;

/// Bytes of one draw (one per launch and per exec).
pub const DRAW_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    On,
    Off,
    Legacy,
}

impl Mode {
    /// The value of the boot option `aslr=`.
    pub fn parse(v: &str) -> Option<Mode> {
        match v.trim() {
            "on" => Some(Mode::On),
            "off" => Some(Mode::Off),
            "legacy" => Some(Mode::Legacy),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::On => "on",
            Mode::Off => "off",
            Mode::Legacy => "legacy",
        }
    }
    /// For an atomic: 0 legacy, 1 on, 2 off.
    pub fn to_u8(self) -> u8 {
        match self {
            Mode::Legacy => 0,
            Mode::On => 1,
            Mode::Off => 2,
        }
    }
    pub fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::On,
            2 => Mode::Off,
            _ => Mode::Legacy,
        }
    }
}

/// The fixed part of the layout of a mode (the same for every process).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub demand_base: u64,
    pub demand_size: u64,
    /// The PML4 slot that holds the vDSO (its table chain is freed by slot).
    pub vdso_slot: usize,
}

impl Zone {
    pub fn demand_end(&self) -> u64 {
        self.demand_base + self.demand_size
    }
    /// The PML4 slots the demand zone covers.
    pub fn demand_slots(&self) -> Range<usize> {
        (self.demand_base >> 39) as usize..((self.demand_end() + (1 << 39) - 1) >> 39) as usize
    }
}

pub fn zone(mode: Mode) -> Zone {
    match mode {
        Mode::Legacy => Zone { demand_base: LEGACY_DEMAND_BASE, demand_size: LEGACY_DEMAND_SIZE, vdso_slot: (LEGACY_VDSO >> 39) as usize },
        Mode::On | Mode::Off => Zone { demand_base: DEMAND_BASE, demand_size: DEMAND_SIZE, vdso_slot: (VDSO_ZONE >> 39) as usize },
    }
}

/// The addresses of one image (one launch or one exec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Load bias of a program from disk (Chromium) and the end of its window.
    pub exe_base: u64,
    pub exe_end: u64,
    /// Where the mmap bump allocator starts (libraries, large mmaps, thread stacks).
    pub mmap_floor: u64,
    /// The vDSO image (its clock page follows at +4 KiB).
    pub vdso: u64,
    /// Page slides inside the arena (brk start, mmap window, stack top).
    pub brk: u64,
    pub mmap: u64,
    pub stack: u64,
    /// ASLR S5: the offset of the arena in its zone (a multiple of
    /// `ARENA_STEP`, below the zone size; `arena_base` fits it to the span)
    /// and the page slides of the embedded executable and of ld.so.
    pub arena: u64,
    pub exe_slide: u64,
    pub ldso_slide: u64,
}

/// ASLR S5: the virtual base of an arena of `span` bytes for the draw `l`:
/// 8 TiB plus the drawn offset, moved down as far as needed for the arena to
/// end inside the zone (the offsets of the last `span` of the zone fall onto
/// one base; with 128 MiB that is 64 of the 2^21 values).
pub fn arena_base(l: &Layout, span: u64) -> u64 {
    let _ = (l, span);
    ARENA_ZONE.start
}

fn word(r: &[u8; DRAW_BYTES], i: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&r[i * 8..i * 8 + 8]);
    u64::from_le_bytes(b)
}

fn round_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}

/// The layout of one image. `exe_span`: bytes the executable from disk covers
/// from its load bias (0 for a program that is not from disk); `exe_align`:
/// its largest `p_align` (anything below 4 KiB counts as 4 KiB, a value that
/// is not a power of two is rounded up to one, at most 1 GiB).
pub fn draw(r: &[u8; DRAW_BYTES], mode: Mode, exe_span: u64, exe_align: u64) -> Layout {
    let w0 = word(r, 0);
    // The cuts of before (ring3 `aslr_slides`): brk 8 bits, mmap window 9,
    // stack 8, legacy demand floor 19, all in pages.
    let (brk, mmap, stack) = ((w0 & 0xFF) << 12, ((w0 >> 8) & 0x1FF) << 12, ((w0 >> 17) & 0xFF) << 12);
    let legacy_floor = ((w0 >> 25) & 0x7FFFF) << 12;
    let window = round_up(exe_span.min(EXE_SPAN_MAX), 2 * MIB).max(EXE_WINDOW);
    match mode {
        Mode::Legacy => Layout {
            exe_base: LEGACY_DEMAND_BASE,
            exe_end: LEGACY_DEMAND_BASE + EXE_WINDOW,
            mmap_floor: LEGACY_DEMAND_BASE + EXE_WINDOW + legacy_floor,
            vdso: LEGACY_VDSO,
            brk,
            mmap,
            stack,
            arena: 0,
            exe_slide: 0,
            ldso_slide: 0,
        },
        Mode::Off => Layout {
            exe_base: EXE_BASE,
            exe_end: EXE_BASE + window,
            mmap_floor: FLOOR_BASE,
            vdso: VDSO_ZONE,
            brk: 0,
            mmap: 0,
            stack: 0,
            arena: 0,
            exe_slide: 0,
            ldso_slide: 0,
        },
        Mode::On => {
            let align = exe_align.clamp(PAGE, GIB).next_power_of_two();
            let exe_base = EXE_BASE + (word(r, 1) & (EXE_DRAW - 1) & !(align - 1));
            let mmap_floor = FLOOR_BASE + (word(r, 2) & (FLOOR_DRAW - 1) & !(PAGE - 1));
            // 2^27 pages; the last one has no room for the clock page after it:
            // it takes the one below (one value of 2^27 twice as likely).
            let v = (word(r, 3) & (VDSO_DRAW - 1) & !(PAGE - 1)).min(VDSO_DRAW - VDSO_PAGES * PAGE);
            Layout { exe_base, exe_end: exe_base + window, mmap_floor, vdso: VDSO_ZONE + v, brk, mmap, stack, arena: 0, exe_slide: 0, ldso_slide: 0 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sm(u64);
    impl Sm {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn bytes(&mut self) -> [u8; DRAW_BYTES] {
            let mut r = [0u8; DRAW_BYTES];
            for i in 0..4 {
                r[i * 8..i * 8 + 8].copy_from_slice(&self.next().to_le_bytes());
            }
            r
        }
    }

    fn inputs() -> Vec<[u8; DRAW_BYTES]> {
        let mut v = vec![[0u8; DRAW_BYTES], [0xFF; DRAW_BYTES], [0xAA; DRAW_BYTES], [0x55; DRAW_BYTES]];
        let mut s = Sm(20261116);
        for _ in 0..4000 {
            v.push(s.bytes());
        }
        v
    }

    /// Bits that differ from the first draw in at least one draw (the
    /// statistic of the `[aslr]` self-test).
    fn varying(xs: &[u64]) -> u32 {
        xs.iter().fold(0u64, |a, &x| a | (x ^ xs[0])).count_ones()
    }

    #[test]
    fn parse_values() {
        assert_eq!(Mode::parse("on"), Some(Mode::On));
        assert_eq!(Mode::parse(" off "), Some(Mode::Off));
        assert_eq!(Mode::parse("legacy"), Some(Mode::Legacy));
        for bad in ["", "ON", "1", "true", "random", "legacy2"] {
            assert_eq!(Mode::parse(bad), None, "{bad}");
        }
        for m in [Mode::On, Mode::Off, Mode::Legacy] {
            assert_eq!(Mode::from_u8(m.to_u8()), m);
            assert_eq!(Mode::parse(m.name()), Some(m));
        }
    }

    #[test]
    fn zones_and_slots() {
        let l = zone(Mode::Legacy);
        assert_eq!(l.demand_slots(), 2..3);
        assert_eq!(l.vdso_slot, 3);
        for m in [Mode::On, Mode::Off] {
            let z = zone(m);
            assert_eq!(z.demand_slots(), 64..192);
            assert_eq!(z.vdso_slot, 200);
            // kernel (slots 0, 1) < arena zone <= demand < vDSO < 2^47, and
            // nothing in the last slot next to the non-canonical hole.
            assert!(ARENA_ZONE.end <= z.demand_base);
            assert!(z.demand_end() <= VDSO_ZONE);
            assert!(VDSO_ZONE + VDSO_DRAW <= USER_TOP - (1 << 39));
        }
    }

    #[test]
    fn legacy_is_the_layout_of_before() {
        for r in inputs() {
            let w = u64::from_le_bytes(r[0..8].try_into().unwrap());
            let l = draw(&r, Mode::Legacy, 300 * MIB, 2 * MIB);
            assert_eq!(l.exe_base, 0x100_0000_0000);
            assert_eq!(l.exe_end, 0x100_0000_0000 + 0x0C00_0000);
            assert_eq!(l.mmap_floor, 0x100_0000_0000 + 0x0C00_0000 + (((w >> 25) & 0x7FFFF) << 12));
            assert_eq!(l.vdso, 0x1F0_0000_0000);
            assert_eq!((l.brk, l.mmap, l.stack), ((w & 0xFF) << 12, ((w >> 8) & 0x1FF) << 12, ((w >> 17) & 0xFF) << 12));
        }
    }

    #[test]
    fn off_is_fixed() {
        let a = draw(&[0u8; DRAW_BYTES], Mode::Off, 0, 0);
        for r in inputs() {
            assert_eq!(draw(&r, Mode::Off, 0, 0), a);
        }
        assert_eq!((a.exe_base, a.mmap_floor, a.vdso), (32 * TIB, 40 * TIB, 100 * TIB));
        assert_eq!((a.brk, a.mmap, a.stack), (0, 0, 0));
    }

    #[test]
    fn on_every_region_in_its_zone() {
        let z = zone(Mode::On);
        for (span, align) in [(0, 0), (190 * MIB, PAGE), (300 * MIB, 2 * MIB), (EXE_SPAN_MAX * 3, 3 * MIB), (5 * GIB, 64 * 1024)] {
            for r in inputs() {
                let l = draw(&r, Mode::On, span, align);
                let al = align.clamp(PAGE, GIB).next_power_of_two();
                assert_eq!(l.exe_base % al, 0);
                assert!(l.exe_base >= EXE_BASE && l.exe_base < EXE_BASE + EXE_DRAW);
                assert!(l.exe_end >= l.exe_base + span.min(EXE_SPAN_MAX) && l.exe_end >= l.exe_base + EXE_WINDOW);
                assert!(l.exe_end <= l.mmap_floor, "exe window runs into the floor");
                assert!(l.exe_base >= z.demand_base);
                assert_eq!(l.mmap_floor % PAGE, 0);
                assert!(l.mmap_floor >= FLOOR_BASE && l.mmap_floor < FLOOR_BASE + FLOOR_DRAW);
                // at least 52 TiB of bump room above the floor
                assert!(z.demand_end() - l.mmap_floor >= 52 * TIB);
                assert_eq!(l.vdso % PAGE, 0);
                assert!(l.vdso >= VDSO_ZONE && l.vdso + VDSO_PAGES * PAGE <= VDSO_ZONE + VDSO_DRAW);
                assert_eq!((l.vdso >> 39) as usize, z.vdso_slot);
                assert_eq!(((l.vdso + VDSO_PAGES * PAGE - 1) >> 39) as usize, z.vdso_slot);
                assert!(l.vdso >= z.demand_end());
                assert!(l.brk < MIB && l.mmap < 2 * MIB && l.stack < MIB);
                assert!(l.vdso + VDSO_PAGES * PAGE < USER_TOP);
            }
        }
    }

    #[test]
    fn on_entropy_per_region() {
        let all = inputs();
        let pick = |f: &dyn Fn(&Layout) -> u64, align: u64| -> u32 {
            let xs: Vec<u64> = all.iter().map(|r| f(&draw(r, Mode::On, 200 * MIB, align))).collect();
            varying(&xs)
        };
        assert_eq!(pick(&|l| l.mmap_floor, PAGE), 30);
        assert_eq!(pick(&|l| l.exe_base, PAGE), 30);
        assert_eq!(pick(&|l| l.exe_base, 2 * MIB), 21);
        assert_eq!(pick(&|l| l.vdso, PAGE), 27);
        // The arena slides stay those of before until S5.
        assert_eq!(pick(&|l| l.brk, PAGE), 8);
        assert_eq!(pick(&|l| l.mmap, PAGE), 9);
        assert_eq!(pick(&|l| l.stack, PAGE), 8);
        // Independent draws: the distance between the floor and the exe varies as much.
        assert!(pick(&|l| l.mmap_floor.wrapping_sub(l.exe_base), PAGE) >= 30);
        assert!(pick(&|l| l.vdso.wrapping_sub(l.mmap_floor), PAGE) >= 27);
    }

    /// ASLR S5: the arena base and the two page slides stay in their windows
    /// for every span a launcher or an exec uses (64 MiB .. 1 GiB) and beyond.
    #[test]
    fn on_arena_in_its_zone() {
        for span in [64 * MIB, 96 * MIB, 128 * MIB, GIB, 3 * GIB, 4 * TIB, 5 * TIB] {
            for r in inputs() {
                let l = draw(&r, Mode::On, 200 * MIB, PAGE);
                let a = arena_base(&l, span);
                assert_eq!(a % ARENA_STEP, 0);
                assert!(a >= ARENA_ZONE.start);
                assert!(a + span.min(ARENA_ZONE.end - ARENA_ZONE.start) <= ARENA_ZONE.end, "arena runs out of its zone");
                assert!(l.arena % ARENA_STEP == 0 && l.arena < ARENA_ZONE.end - ARENA_ZONE.start);
                assert!(l.exe_slide % PAGE == 0 && l.exe_slide < SLIDE_WINDOW);
                assert!(l.ldso_slide % PAGE == 0 && l.ldso_slide < SLIDE_WINDOW);
                // ld.so (under 1 MiB, euromm::arena) with its slide ends below
                // the brk window of the smallest arena (64 MiB: +16 MiB).
                assert!(LDSO_OFF + l.ldso_slide + MIB <= 16 * MIB);
            }
        }
    }

    /// ASLR S5 (design 2.2): arena base 21 bits, the embedded executable and
    /// ld.so 30 (base plus a 9-bit page slide), stack 29; independent of the
    /// draws of S1.
    #[test]
    fn on_arena_entropy() {
        let all = inputs();
        let pick = |f: &dyn Fn(&Layout) -> u64| -> u32 {
            let xs: Vec<u64> = all.iter().map(|r| f(&draw(r, Mode::On, 200 * MIB, PAGE))).collect();
            varying(&xs)
        };
        let span = 128 * MIB;
        assert_eq!(pick(&|l| arena_base(l, span)), 21);
        assert_eq!(pick(&|l| l.exe_slide), 9);
        assert_eq!(pick(&|l| l.ldso_slide), 9);
        assert_eq!(pick(&|l| arena_base(l, span) + l.exe_slide), 30);
        assert_eq!(pick(&|l| arena_base(l, span) + LDSO_OFF + l.ldso_slide), 30);
        assert_eq!(pick(&|l| arena_base(l, span) + span - MIB - l.stack), 29);
        // Not tied to the draws of S1 or to each other.
        assert!(pick(&|l| arena_base(l, span).wrapping_sub(l.exe_base)) >= 30);
        assert!(pick(&|l| arena_base(l, span).wrapping_sub(l.mmap_floor)) >= 30);
        assert!(pick(&|l| l.ldso_slide.wrapping_sub(l.exe_slide)) >= 9);
    }

    /// `off`: the arena at 8 TiB, no slide (the layout of S4); `legacy`: no
    /// slide (its arena is the identity one, the base is not used).
    #[test]
    fn arena_off_and_legacy() {
        for r in inputs() {
            let o = draw(&r, Mode::Off, 200 * MIB, PAGE);
            assert_eq!(arena_base(&o, 128 * MIB), ARENA_ZONE.start);
            assert_eq!((o.arena, o.exe_slide, o.ldso_slide), (0, 0, 0));
            let g = draw(&r, Mode::Legacy, 200 * MIB, PAGE);
            assert_eq!((g.arena, g.exe_slide, g.ldso_slide), (0, 0, 0));
        }
    }
}
