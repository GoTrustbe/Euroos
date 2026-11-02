//! Own 4-level page tables (Track 3 paging).
//!
//! We build a fresh PML4 + PDPT that map the lower 512 GiB **identically**
//! (virtual = physical) with 1 GiB huge pages. Because everything stays
//! identity-mapped, all existing pointers (kernel code, stack, heap, framebuffer)
//! keep working after loading our CR3 — but now on OUR tables instead of UEFI's.
//!
//! Small and robust: 2 frames (PML4 + PDPT), 512×1 GiB. The fine-grained
//! (4 KiB, User-bit) mappings for ring-3 are layered on top in a later step.

use euromm::FrameAllocator;

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE: u64 = 1 << 7; // 1 GiB page in a PDPT entry
const NX: u64 = 1 << 63; // No-Execute (requires EFER.NXE) — W^X
const GIB: u64 = 1 << 30;
const MIB2: u64 = 1 << 21; // 2 MiB page in a PD entry
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
/// A non-present PTE that holds a swap slot (see swapmgr): bit 9, slot in bits 12+.
const SWAPPED_PTE: u64 = 1 << 9;

/// The shared high PDPT (PML4[1] → 512 GiB..1 TiB), set up at boot. Every
/// process PML4 shares the same PDPT so that guarded kernel stacks (A2) + high
/// MMIO BARs are valid in ALL address spaces.
pub static HIGH_PDPT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn high_pdpt_entry() -> u64 {
    let p = HIGH_PDPT.load(core::sync::atomic::Ordering::Relaxed);
    if p != 0 {
        p | PRESENT | WRITABLE
    } else {
        0
    }
}

/// Build an ISOLATED address space (PML4) for one process. The kernel is mapped
/// supervisor everywhere (identity), so that syscalls/interrupts/kernel stacks
/// keep working after a CR3 switch. Only the 2 MiB block at `arena_phys`
/// (2 MiB-aligned, < 1 GiB) gets the USER bit — that is where THIS process's
/// user frames live. Other processes' arenas stay supervisor-only -> ring 3
/// cannot reach them. Returns the physical PML4 address (does NOT load CR3).
pub fn build_address_space(
    falloc: &mut FrameAllocator,
    arena_phys: u64,
    exec_pages: &[u64; 8],
    writ_pages: &[u64; 8],
) -> u64 {
    let pml4 = falloc.allocate().expect("frame for process PML4");
    let pdpt = falloc.allocate().expect("frame for process PDPT");
    let pd = falloc.allocate().expect("frame for process PD");
    // Fourth frame: the PT that maps the 2 MiB arena block fine-grained (4 KiB), so
    // we can enforce W^X per page (executable XOR writable).
    let pt = falloc.allocate().expect("frame for arena PT");
    // SAFETY: four fresh, identity-mapped frames; we fill them completely.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        core::ptr::write_bytes(pt as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        let ptp = pt as *mut u64;
        // PML4[0] -> PDPT (USER at every level; the PT entry gates per 4 KiB).
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Share the high region (PML4[1]) with the boot address space (A2/high MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        // PDPT[0] -> PD (lower 1 GiB, fine-grained 2 MiB).
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        // PDPT[1..512] = identity 1 GiB supervisor (kernel/MMIO/framebuffer high).
        // NO NX: kernel code in the lower 1 GiB runs ring-0 (syscalls/interrupts)
        // UNDER this process CR3, so those pages must stay executable. W^X applies
        // only to the USER arena (the PT below).
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // PD[0..512] = identity 2 MiB supervisor (kernel — executable), except the
        // arena block which points to the fine-grained W^X PT.
        let arena_idx = (arena_phys / MIB2) as usize;
        for i in 0..512usize {
            if i == arena_idx {
                pdp.add(i).write_volatile(pt | PRESENT | WRITABLE | USER);
            } else {
                pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | HUGE);
            }
        }
        // PT[0..512]: the 512 4 KiB pages of the arena. W^X per page:
        //   exec & !writ → R-X  (code, read-only + executable)
        //   exec &  writ → RWX  (binary with a mixed RWE segment; W^X not enforceable)
        //   !exec        → RW+NX (data/bss/heap/stack: writable, not executable)
        for p in 0..512usize {
            let bit = 1u64 << (p % 64);
            let exec = exec_pages[p / 64] & bit != 0;
            let writ = writ_pages[p / 64] & bit != 0;
            let flags = if exec && writ {
                PRESENT | USER | WRITABLE // RWX — no other option for an RWE segment
            } else if exec {
                PRESENT | USER // R-X
            } else {
                PRESENT | USER | WRITABLE | NX // RW + NX
            };
            ptp.add(p).write_volatile((arena_phys + p as u64 * 4096) | flags);
        }
    }
    pml4
}

/// Like [`build_address_space`] but for a LARGE arena spanning `nblocks`
/// consecutive 2 MiB blocks (the DOOM port needs ~32 MiB for code + WAD + heap).
/// Block 0 keeps the fine-grained W^X PT (code/data/bss must fit in its 2 MiB);
/// the remaining blocks are mapped as RW+NX 2 MiB huge pages with the USER bit —
/// exactly right for heap + stack (pure data, never executable). Falls back to
/// the single-block layout when `nblocks == 1`.
pub fn build_address_space_big(
    falloc: &mut FrameAllocator,
    arena_phys: u64,
    nblocks: u64,
    exec_pages: &[u64; 8],
    writ_pages: &[u64; 8],
) -> u64 {
    let pml4 = build_address_space(falloc, arena_phys, exec_pages, writ_pages);
    if nblocks <= 1 {
        return pml4;
    }
    // Reach into the freshly built PD (PDPT[0] -> PD) and promote the arena's
    // extra blocks from supervisor-huge to USER RW+NX huge pages.
    let arena_idx = (arena_phys / MIB2) as usize;
    // SAFETY: identity-mapped page tables we just allocated; single-threaded here.
    unsafe {
        let pdpt = (*(pml4 as *const u64)) & 0x000f_ffff_ffff_f000;
        let pd = (*(pdpt as *const u64)) & 0x000f_ffff_ffff_f000;
        let pdp = pd as *mut u64;
        for b in 1..nblocks as usize {
            let i = arena_idx + b;
            if i >= 512 {
                break; // stays within the lower 1 GiB PD
            }
            pdp.add(i)
                .write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | USER | HUGE | NX);
        }
    }
    pml4
}

/// Like [`build_address_space`], but maps the arena block as a single 2 MiB USER
/// page with RWX (no W^X). For the counter demo (`spawn_counter_task`): it runs a
/// raw machine-code blob with a counter variable IN the same page, so code and
/// data cannot be separated — W^X is not applicable there. Real ELF programs
/// always use the W^X variant above.
pub fn build_address_space_rwx(falloc: &mut FrameAllocator, arena_phys: u64) -> u64 {
    let pml4 = falloc.allocate().expect("frame for process PML4");
    let pdpt = falloc.allocate().expect("frame for process PDPT");
    let pd = falloc.allocate().expect("frame for process PD");
    // SAFETY: three fresh, identity-mapped frames; we fill them completely.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Share the high region (PML4[1]) with the boot address space (A2/high MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        let arena_idx = (arena_phys / MIB2) as usize;
        for i in 0..512usize {
            let user = if i == arena_idx { USER } else { 0 };
            pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | HUGE | user);
        }
    }
    pml4
}

/// A large all-RWX USER arena spanning `nblocks` 2 MiB huge pages, for running a
/// real glibc dynamic program: the loader (ld-linux) mmaps libc + the exe's code
/// at addresses IT chooses, scattered across the arena, so code and data cannot
/// be separated into fixed W^X regions. Every arena block is PRESENT|WRITABLE|
/// USER|HUGE with NO NX — W^X is deliberately relaxed for this compatibility
/// sandbox (the arena is still isolated: only its blocks carry the USER bit).
/// Map a user arena of `nblocks` 2 MiB huge blocks at `virt_arena` -> `phys_arena`
/// into an address space whose PDPT is at `pdpt`, WHEREVER the range lies.
///
/// Every filler used to assume the arena fits the first 1 GiB window: one PD,
/// indices 0..512, blocks past the boundary silently skipped. The tail of an
/// arena that crossed 1 GiB then stayed on the identity SUPERVISOR mapping,
/// and the process died on its first stack write with a protection violation -
/// which is exactly what happened the day the fork pool grew to 896 MiB and
/// pushed the browser arena across the boundary. `get_pd` supplies a fresh
/// frame when a 1 GiB window needs its huge entry split into a PD; windows the
/// arena does not touch keep their 1 GiB identity entries.
///
/// Returns false when a needed table frame could not be allocated.
pub fn map_user_arena(
    pdpt: u64,
    virt_arena: u64,
    phys_arena: u64,
    nblocks: u64,
    get_pd: &mut dyn FnMut() -> Option<u64>,
) -> bool {
    // SAFETY: identity-mapped tables; single-threaded during address-space build.
    unsafe {
        let pdptp = pdpt as *mut u64;
        for b in 0..nblocks {
            let virt = virt_arena + b * MIB2;
            let i3 = ((virt >> 30) & 0x1FF) as usize;
            let i2 = ((virt >> 21) & 0x1FF) as usize;
            let e3 = pdptp.add(i3).read_volatile();
            let pd = if e3 & PRESENT != 0 && e3 & HUGE == 0 {
                e3 & 0x000f_ffff_ffff_f000
            } else {
                // A 1 GiB huge entry (or an empty slot): split into a PD of 2 MiB
                // identity supervisor entries, then hang it USER-reachable - the
                // USER bit on intermediate tables gates nothing by itself, the
                // leaf entries decide.
                let Some(pd) = get_pd() else { return false };
                let pdp = pd as *mut u64;
                for i in 0..512u64 {
                    pdp.add(i as usize)
                        .write_volatile(((i3 as u64) * GIB + i * MIB2) | PRESENT | WRITABLE | HUGE);
                }
                pdptp.add(i3).write_volatile(pd | PRESENT | WRITABLE | USER);
                pd
            };
            (pd as *mut u64)
                .add(i2)
                .write_volatile((phys_arena + b * MIB2) | PRESENT | WRITABLE | HUGE | USER);
        }
    }
    true
}

pub fn build_address_space_rwx_big(falloc: &mut FrameAllocator, arena_phys: u64, nblocks: u64) -> u64 {
    let pml4 = build_address_space_rwx(falloc, arena_phys);
    if nblocks <= 1 {
        return pml4;
    }
    // SAFETY: identity-mapped tables we just built; single-threaded here.
    unsafe {
        let pdpt = (*(pml4 as *const u64)) & 0x000f_ffff_ffff_f000;
        let mut get_pd = || falloc.allocate().ok();
        if !map_user_arena(pdpt, arena_phys, arena_phys, nblocks, &mut get_pd) {
            crate::serial_println!("[paging] map_user_arena: table frame allocation failed");
        }
    }
    pml4
}

/// Like [`build_address_space`], but for a FORKED child: the USER arena lies at a
/// DIFFERENT physical address than where it appears virtually. We map the VIRTUAL
/// arena (= the 2 MiB block of the PARENT, because the copied stack/code carry
/// absolute pointers there) to the PHYSICAL frames of the child. So the child is
/// an exact copy at the same virtual addresses, but with its own memory.
/// (S3 fork: child pml4 = build_address_space_remap(parent_virt_arena, child_phys_arena).)
pub fn build_address_space_remap(falloc: &mut FrameAllocator, virt_arena: u64, phys_arena: u64) -> u64 {
    let pml4 = falloc.allocate().expect("frame for child PML4");
    let pdpt = falloc.allocate().expect("frame for child PDPT");
    let pd = falloc.allocate().expect("frame for child PD");
    fill_remap_tables(pml4, pdpt, pd, virt_arena, phys_arena);
    pml4
}

/// Fill PRE-allocated table frames (pml4/pdpt/pd) for a remapped child address
/// space. fork() allocates the three frames from the process pool and calls this.
pub fn fill_remap_tables(pml4: u64, pdpt: u64, pd: u64, virt_arena: u64, phys_arena: u64) {
    // SAFETY: three fresh, identity-mapped frames; we fill them completely.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Share the high region (PML4[1]) with the boot address space (A2/high MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // PD[0..512] = identity supervisor 2 MiB, EXCEPT the virtual arena slot:
        // it points (USER) to the PHYSICAL frames of the child.
        let virt_idx = (virt_arena / MIB2) as usize;
        for i in 0..512usize {
            let entry = if i == virt_idx {
                phys_arena | PRESENT | WRITABLE | HUGE | USER
            } else {
                (i as u64 * MIB2) | PRESENT | WRITABLE | HUGE
            };
            pdp.add(i).write_volatile(entry);
        }
    }
}

/// W^X variant for fork: like [`fill_remap_tables`], but the arena block is mapped
/// FINE-GRAINED (4 KiB) via `pt`, with per-page the SAME permissions as the PARENT
/// (cloned from `parent_pt`). So the child inherits exactly the W^X layout (R-X code,
/// RW+NX data/stack) on its OWN physical frames. `parent_pt` None → everything RWX
/// (fallback for a parent without a fine-grained PT). Requires 4 table frames.
pub fn fill_remap_tables_wx(pml4: u64, pdpt: u64, pd: u64, pt: u64,
                            virt_arena: u64, phys_arena: u64, parent_pt: Option<u64>) {
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        core::ptr::write_bytes(pt as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        let ptp = pt as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Share the high region (PML4[1]) with the boot address space (A2/high MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        let virt_idx = (virt_arena / MIB2) as usize;
        for i in 0..512usize {
            if i == virt_idx {
                pdp.add(i).write_volatile(pt | PRESENT | WRITABLE | USER);
            } else {
                pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | HUGE);
            }
        }
        let par = parent_pt.map(|x| x as *const u64);
        for i in 0..512usize {
            let flags = match par {
                Some(p) => p.add(i).read_volatile() & !ADDR_MASK, // clone parent permissions
                None => PRESENT | USER | WRITABLE,                // fallback: RWX
            };
            ptp.add(i).write_volatile((phys_arena + i as u64 * 4096) | flags);
        }
    }
}

// ── A2/G1: guarded kernel stacks in the shared high region ─────────────────
// A POOL of guarded stacks (IST/AP/scheduler tasks). Each "unit" = 1 unmapped
// guard page + 4 stack pages (16 KiB) = 5 pages, back to back in the first 2 MiB
// of the high region (512 GiB..512 GiB+2 MiB, one shared PT of 512 entries → max
// ~102 units). A stack overflow lands on the guard page → an immediate #PF
// instead of silent corruption of the neighboring stack.
use core::sync::atomic::{AtomicU64, Ordering as AtOrd};

/// The guard address of the FIRST unit — kept for the A2 self-test log.
pub static STACK_GUARD_ADDR: AtomicU64 = AtomicU64::new(0);

const STACK_REGION_BASE: u64 = 512 * GIB; // start of the high region (PML4[1] slot 0)
const UNIT_STACK_PAGES: u64 = 4; // 16 KiB usable stack per unit
const UNIT_PAGES: u64 = 1 + UNIT_STACK_PAGES; // + 1 guard page
const UNIT_BYTES: u64 = UNIT_PAGES * 4096;
const MAX_UNITS: u64 = 512 / UNIT_PAGES; // the PT has 512 entries

static GUARD_REGION_BASE: AtomicU64 = AtomicU64::new(0); // 0 = not yet set up
static GUARDED_UNITS: AtomicU64 = AtomicU64::new(0); // number of granted units
static GUARD_PT: AtomicU64 = AtomicU64::new(0); // phys of the PT that maps the region

/// True if `addr` falls in a guard page of the guarded-stack pool. Uniform
/// units → O(1) region+modulo check (the guard is the first page of each unit).
pub fn is_stack_guard(addr: u64) -> bool {
    let base = GUARD_REGION_BASE.load(AtOrd::Relaxed);
    if base == 0 {
        return false;
    }
    let n = GUARDED_UNITS.load(AtOrd::Relaxed);
    if addr < base || addr >= base + n * UNIT_BYTES {
        return false;
    }
    (addr - base) % UNIT_BYTES < 4096
}

/// Set up (idempotent) the fine-grained PD→PT that maps the guarded-stack region:
/// replace the 1 GiB huge mapping of 512..513 GiB in the SHARED high PDPT with a
/// PD→PT. Returns the PT phys (0 if the high PDPT is missing).
fn ensure_guard_pt(falloc: &mut FrameAllocator) -> u64 {
    let existing = GUARD_PT.load(AtOrd::Relaxed);
    if existing != 0 {
        return existing;
    }
    let pdpt_hi = HIGH_PDPT.load(AtOrd::Relaxed);
    if pdpt_hi == 0 {
        return 0;
    }
    let pd = falloc.allocate().expect("frame for guard PD");
    let pt = falloc.allocate().expect("frame for guard PT");
    unsafe {
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        core::ptr::write_bytes(pt as *mut u8, 0, 4096);
        // Only PD[0] -> PT (the first 2 MiB of the high region, fine-grained).
        (pd as *mut u64).write_volatile(pt | PRESENT | WRITABLE);
        // Replace the 1 GiB huge PDPT entry with this PD (shared across all PML4s).
        (pdpt_hi as *mut u64).write_volatile(pd | PRESENT | WRITABLE);
        flush_tlb();
    }
    GUARD_PT.store(pt, AtOrd::Relaxed);
    GUARD_REGION_BASE.store(STACK_REGION_BASE, AtOrd::Relaxed);
    pt
}

/// Allocate the NEXT guarded stack unit: map its 4 stack pages onto real frames
/// and leave its guard page unmapped. Returns the stack TOP (grows downward to
/// the guard), or 0 on failure. Works in ALL address spaces (the high PDPT is
/// shared) — suitable for IST, AP and scheduler-task stacks.
pub fn guarded_stack_alloc(falloc: &mut FrameAllocator) -> u64 {
    let pt = ensure_guard_pt(falloc);
    if pt == 0 {
        return 0;
    }
    let idx = GUARDED_UNITS.load(AtOrd::Relaxed);
    if idx >= MAX_UNITS {
        return 0; // PT full
    }
    let pt_base = (idx * UNIT_PAGES) as usize;
    unsafe {
        let ptp = pt as *mut u64;
        ptp.add(pt_base).write_volatile(0); // guard page: absent
        for p in 1..=UNIT_STACK_PAGES as usize {
            let frame = falloc.allocate().expect("frame for guarded stack");
            core::ptr::write_bytes(frame as *mut u8, 0, 4096);
            ptp.add(pt_base + p).write_volatile(frame | PRESENT | WRITABLE);
        }
        flush_tlb();
    }
    GUARDED_UNITS.store(idx + 1, AtOrd::Relaxed);
    // Top = above the unit (grows downward through the 4 stack pages to the guard).
    STACK_REGION_BASE + (idx + 1) * UNIT_BYTES
}

/// Number of granted guarded stacks (for diagnostics).
pub fn guarded_stack_count() -> u64 {
    GUARDED_UNITS.load(AtOrd::Relaxed)
}

/// A2-compat: allocate the first guarded stack + remember its guard address for the
/// existing self-test log. Returns the stack TOP.
pub fn setup_guarded_stack(falloc: &mut FrameAllocator) -> u64 {
    let top = guarded_stack_alloc(falloc);
    if top != 0 {
        STACK_GUARD_ADDR.store(STACK_REGION_BASE, AtOrd::Relaxed);
    }
    top
}

/// Non-destructive verification: walk the shared high PDPT → PD → PT and return
/// `true` if the guard page (PT[0]) is NOT present (as it should be).
pub fn guard_page_unmapped() -> bool {
    let pdpt_hi = HIGH_PDPT.load(core::sync::atomic::Ordering::Relaxed);
    if pdpt_hi == 0 {
        return false;
    }
    unsafe {
        let e0 = (pdpt_hi as *const u64).read_volatile();
        if e0 & PRESENT == 0 || e0 & HUGE != 0 {
            return false; // still a huge mapping (no fine-grained guard PD)
        }
        let pd = e0 & ADDR_MASK;
        let pde = (pd as *const u64).read_volatile();
        if pde & PRESENT == 0 || pde & HUGE != 0 {
            return false;
        }
        let pt = pde & ADDR_MASK;
        let guard = (pt as *const u64).read_volatile(); // PT[0] = guard
        guard & PRESENT == 0
    }
}

/// Find the 4 KiB PT that maps the USER arena in `pml4` (None for a HUGE/old mapping).
pub fn arena_pt(pml4: u64, virt_arena: u64) -> Option<u64> {
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        let pd = (pdpt as *const u64).read_volatile() & ADDR_MASK;
        let idx = (virt_arena / MIB2) as usize;
        let e = (pd as *const u64).add(idx).read_volatile();
        if e & PRESENT != 0 && e & HUGE == 0 {
            Some(e & ADDR_MASK)
        } else {
            None
        }
    }
}

/// Set all 512 arena pages temporarily RW (to load a new image during execve)
/// and flush the TLB. Address bits remain; only the permissions widen.
pub fn arena_set_writable(pt: u64) {
    unsafe {
        let p = pt as *mut u64;
        for i in 0..512usize {
            let addr = p.add(i).read_volatile() & ADDR_MASK;
            p.add(i).write_volatile(addr | PRESENT | USER | WRITABLE);
        }
    }
    flush_tlb();
}

/// Apply W^X to the arena PT (after loading a new image): exec&!writ→R-X,
/// exec&writ→RWX, rest→RW+NX. Flush the TLB.
pub fn arena_set_wx(pt: u64, exec_pages: &[u64; 8], writ_pages: &[u64; 8]) {
    unsafe {
        let p = pt as *mut u64;
        for i in 0..512usize {
            let addr = p.add(i).read_volatile() & ADDR_MASK;
            let bit = 1u64 << (i % 64);
            let exec = exec_pages[i / 64] & bit != 0;
            let writ = writ_pages[i / 64] & bit != 0;
            let flags = if exec && writ {
                PRESENT | USER | WRITABLE
            } else if exec {
                PRESENT | USER
            } else {
                PRESENT | USER | WRITABLE | NX
            };
            p.add(i).write_volatile(addr | flags);
        }
    }
    flush_tlb();
}

/// Reload CR3 → flush the (non-global) TLB entries of the current address space.
fn flush_tlb() {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

/// The three page-table frames (pml4, pdpt, pd) of a process address space — so the
/// reaper can return them to the right allocator (main OR process pool).
pub fn table_frames(pml4: u64) -> (u64, u64, u64) {
    const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        let pd = (pdpt as *const u64).read_volatile() & ADDR_MASK;
        (pml4, pdpt, pd)
    }
}

/// DEMAND PAGING: map a single 4 KiB USER RW page `phys` at `virt` in process
/// `pml4`, creating the PDPT/PD/PT levels on demand (frames from the global process
/// pool, zeroed). This backs a large SPARSE mmap region committed one page at a time
/// on fault — the basis for Chromium-scale address spaces (reserve huge virtual,
/// commit little physical). Returns false if the pool is exhausted. All table frames
/// live in low RAM (identity-mapped in the process PML4[0]), so this is reachable
/// with the process CR3 active (i.e. from the page-fault handler).
/// Is `virt` already present in this address space? Read-ahead must skip mapped
/// pages: `map_demand_4k` overwrites the PTE unconditionally, and replacing a page a
/// thread has already written to would corrupt it silently.
pub fn demand_page_mapped(pml4: u64, virt: u64) -> bool {
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let e = (table as *const u64).add(((virt >> shift) & 0x1FF) as usize).read_volatile();
            if e & PRESENT == 0 {
                return false;
            }
            table = e & ADDR_MASK;
        }
        (table as *const u64).add(((virt >> 12) & 0x1FF) as usize).read_volatile() & PRESENT != 0
    }
}

/// Map one user page with table frames from the FRAME ALLOCATOR — for mappings made
/// at process-build time (the vDSO), before any demand pool exists. `map_demand_4k`
/// pulls its tables from the demand pool and silently cannot run that early: the
/// auxv then promises a vDSO whose pages fault on first touch (ld.so died at
/// VDSO_BASE+0x20 reading e_phoff).
/// Translate a user VA through an arbitrary PML4 to its physical address (None if
/// unmapped). For diagnostics that must read another process's memory — e.g. the
/// futex lock WORD of a deadlocked glibc process, dumped from the launcher.
/// Is `virt` mapped WRITABLE for user mode in the address space `pml4`? Every
/// level must carry PRESENT|USER|WRITABLE (x86 ANDs the permissions down the
/// walk). A kernel write to a page that is mapped read-only for the process
/// (its R-X code) would fault in ring 0 with CR0.WP set, so every kernel write
/// into app memory is checked here first (security review of the Euro API).
pub fn user_writable_in(pml4: u64, virt: u64) -> bool {
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let e = (table as *const u64).add(((virt >> shift) & 0x1FF) as usize).read_volatile();
            if e & (PRESENT | USER | WRITABLE) != (PRESENT | USER | WRITABLE) {
                return false;
            }
            if shift == 21 && e & HUGE != 0 {
                return true;
            }
            table = e & ADDR_MASK;
        }
        let e = (table as *const u64).add(((virt >> 12) & 0x1FF) as usize).read_volatile();
        e & (PRESENT | USER | WRITABLE) == (PRESENT | USER | WRITABLE)
    }
}

/// Is the whole range `[ptr, ptr+len)` user-writable in `pml4`? (Page by page.)
pub fn user_range_writable_in(pml4: u64, ptr: u64, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = ptr.checked_add(len as u64) else { return false };
    let mut page = ptr & !0xFFF;
    while page < end {
        if !user_writable_in(pml4, page) {
            return false;
        }
        page += 4096;
    }
    true
}

pub fn translate_in(pml4: u64, virt: u64) -> Option<u64> {
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let e = (table as *const u64).add(((virt >> shift) & 0x1FF) as usize).read_volatile();
            if e & PRESENT == 0 {
                return None;
            }
            if shift == 21 && e & HUGE != 0 {
                return Some((e & ADDR_MASK) + (virt & 0x1F_FFFF));
            }
            table = e & ADDR_MASK;
        }
        let e = (table as *const u64).add(((virt >> 12) & 0x1FF) as usize).read_volatile();
        if e & PRESENT == 0 {
            return None;
        }
        Some((e & ADDR_MASK) + (virt & 0xFFF))
    }
}

pub fn map_user_4k_falloc(falloc: &mut euromm::FrameAllocator, pml4: u64, virt: u64, phys: u64) -> bool {
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let slot = (table as *mut u64).add(((virt >> shift) & 0x1FF) as usize);
            let e = slot.read_volatile();
            table = if e & PRESENT != 0 {
                e & ADDR_MASK
            } else {
                let frame = match falloc.allocate() {
                    Ok(f) => f,
                    Err(_) => return false,
                };
                core::ptr::write_bytes(frame as *mut u8, 0, 4096);
                slot.write_volatile(frame | PRESENT | WRITABLE | USER);
                frame
            };
        }
        (table as *mut u64)
            .add(((virt >> 12) & 0x1FF) as usize)
            .write_volatile(phys | PRESENT | WRITABLE | USER);
    }
    true
}

/// map_demand_4k, but READ-ONLY: used for pages backed by the shared disk page
/// cache — a write then faults (protection violation) and the CoW breaker gives
/// the writer a private copy.
pub fn map_demand_4k_ro(pml4: u64, virt: u64, phys: u64) -> bool {
    // SAFETY: identical to map_demand_4k minus the WRITABLE bit.
    unsafe {
        let i4 = ((virt >> 39) & 0x1FF) as usize;
        let i3 = ((virt >> 30) & 0x1FF) as usize;
        let i2 = ((virt >> 21) & 0x1FF) as usize;
        let i1 = ((virt >> 12) & 0x1FF) as usize;
        let pdpt = match ensure_table((pml4 as *mut u64).add(i4)) { Some(p) => p, None => return false };
        let pd = match ensure_table((pdpt as *mut u64).add(i3)) { Some(p) => p, None => return false };
        let pt = match ensure_table((pd as *mut u64).add(i2)) { Some(p) => p, None => return false };
        (pt as *mut u64).add(i1).write_volatile(phys | PRESENT | USER);
    }
    true
}

/// Read the PTE for `virt` in `pml4`'s demand region: (physical frame, writable).
/// None when any level is absent.
/// Print the RAW page-table chain for `virt` in `pml4`, huge pages included.
/// Diagnostic for a protection fault: the entry flags say which level denies
/// the access, which no amount of reasoning from the allocator layout does.
pub fn dump_walk(pml4: u64, virt: u64) {
    // SAFETY: identity-mapped table chain, read-only walk.
    unsafe {
        let i4 = ((virt >> 39) & 0x1FF) as usize;
        let i3 = ((virt >> 30) & 0x1FF) as usize;
        let i2 = ((virt >> 21) & 0x1FF) as usize;
        let i1 = ((virt >> 12) & 0x1FF) as usize;
        let e4 = (pml4 as *const u64).add(i4).read_volatile();
        crate::serial_println!("[walk] pml4[{i4}]={e4:#x}");
        if e4 & PRESENT == 0 { return; }
        let e3 = ((e4 & ADDR_MASK) as *const u64).add(i3).read_volatile();
        crate::serial_println!("[walk] pdpt[{i3}]={e3:#x}");
        if e3 & PRESENT == 0 || e3 & (1 << 7) != 0 { return; }
        let e2 = ((e3 & ADDR_MASK) as *const u64).add(i2).read_volatile();
        crate::serial_println!("[walk] pd[{i2}]={e2:#x}{}", if e2 & (1 << 7) != 0 { " (2 MiB huge)" } else { "" });
        if e2 & PRESENT == 0 || e2 & (1 << 7) != 0 { return; }
        let e1 = ((e2 & ADDR_MASK) as *const u64).add(i1).read_volatile();
        crate::serial_println!("[walk] pt[{i1}]={e1:#x}");
    }
}

/// The raw PTE of `virt` in `pml4` (0 when no table covers it): the swap code
/// reads the SWAPPED marker of a non-present entry through this.
pub fn demand_pte_raw(pml4: u64, virt: u64) -> u64 {
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let e = (table as *const u64).add(((virt >> shift) & 0x1FF) as usize).read_volatile();
            if e & PRESENT == 0 { return 0; }
            table = e & ADDR_MASK;
        }
        (table as *const u64).add(((virt >> 12) & 0x1FF) as usize).read_volatile()
    }
}

pub fn demand_pte(pml4: u64, virt: u64) -> Option<(u64, bool)> {
    // SAFETY: identity-mapped table chain, read-only walk.
    unsafe {
        let i4 = ((virt >> 39) & 0x1FF) as usize;
        let i3 = ((virt >> 30) & 0x1FF) as usize;
        let i2 = ((virt >> 21) & 0x1FF) as usize;
        let i1 = ((virt >> 12) & 0x1FF) as usize;
        let e4 = (pml4 as *const u64).add(i4).read_volatile();
        if e4 & PRESENT == 0 { return None; }
        let e3 = ((e4 & ADDR_MASK) as *const u64).add(i3).read_volatile();
        if e3 & PRESENT == 0 { return None; }
        let e2 = ((e3 & ADDR_MASK) as *const u64).add(i2).read_volatile();
        if e2 & PRESENT == 0 { return None; }
        let e1 = ((e2 & ADDR_MASK) as *const u64).add(i1).read_volatile();
        if e1 & PRESENT == 0 { return None; }
        Some((e1 & ADDR_MASK, e1 & WRITABLE != 0))
    }
}

pub fn map_demand_4k(pml4: u64, virt: u64, phys: u64) -> bool {
    // SAFETY: pml4 + all table frames are identity-mapped low RAM; page-aligned.
    unsafe {
        let i4 = ((virt >> 39) & 0x1FF) as usize;
        let i3 = ((virt >> 30) & 0x1FF) as usize;
        let i2 = ((virt >> 21) & 0x1FF) as usize;
        let i1 = ((virt >> 12) & 0x1FF) as usize;
        let pdpt = match ensure_table((pml4 as *mut u64).add(i4)) { Some(p) => p, None => return false };
        let pd = match ensure_table((pdpt as *mut u64).add(i3)) { Some(p) => p, None => return false };
        let pt = match ensure_table((pd as *mut u64).add(i2)) { Some(p) => p, None => return false };
        (pt as *mut u64).add(i1).write_volatile(phys | PRESENT | WRITABLE | USER);
    }
    true
}

/// Ensure the table entry at `slot` points to a present next-level table, creating a
/// fresh zeroed one (from the process pool) if absent. Returns its physical address.
unsafe fn ensure_table(slot: *mut u64) -> Option<u64> {
    let e = slot.read_volatile();
    if e & PRESENT != 0 {
        return Some(e & ADDR_MASK);
    }
    let frame = crate::procpool::demand_alloc()?;
    core::ptr::write_bytes(frame as *mut u8, 0, 4096);
    slot.write_volatile(frame | PRESENT | WRITABLE | USER);
    Some(frame)
}

/// fork() for the demand-paged glibc model — MULTI-BLOCK arena remap. Like
/// [`fill_remap_tables`] but remaps `nblocks` consecutive 2 MiB arena blocks
/// (`virt_arena`..) to the child's own physical blocks (`phys_arena`.., must be
/// `nblocks`*2 MiB contiguous). Kernel identity + high region are preserved so the
/// child can run ring-0 (syscalls/faults). Used by do_glibc_fork.
pub fn fill_remap_tables_multiblock(pml4: u64, pdpt: u64, pd: u64,
                                    virt_arena: u64, phys_arena: u64, nblocks: u64) {
    // SAFETY: three fresh identity-mapped frames; filled completely.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        pml4p.add(1).write_volatile(high_pdpt_entry()); // share high region (MMIO)
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // PD[0..512] identity supervisor for the first GiB; the arena blocks are
        // written by map_user_arena below, in WHICHEVER 1 GiB windows they fall.
        for i in 0..512usize {
            pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | HUGE);
        }
        let mut get_pd = || {
            // fork runs in syscall context: extra table frames come from the pool.
            let f = crate::procpool::alloc();
            if let Some(fr) = f {
                core::ptr::write_bytes(fr as *mut u8, 0, 4096);
            }
            f
        };
        if !map_user_arena(pdpt, virt_arena, phys_arena, nblocks, &mut get_pd) {
            crate::serial_println!("[paging] multiblock map_user_arena: pool frame failed");
        }
    }
}

/// fork(): copy every COMMITTED page of the demand region PML4[`idx`] from `parent`
/// into fresh pool frames mapped at the SAME virtual address in `child`. Read-only
/// exe/lib pages could be re-faulted from disk, but the child must observe the
/// parent's RUNTIME writes (heap, initialized data, chrome globals), so we copy every
/// committed page. Returns false on pool exhaustion (caller tears the child down).
pub fn clone_demand_region(parent: u64, child: u64, idx: usize) -> bool {
    // SAFETY: identity-mapped table chains for both address spaces; single-threaded
    // (IF=0 syscall). Frame copies read the parent's phys (identity) and write fresh
    // pool frames (identity), both reachable with the current CR3.
    unsafe {
        let e4 = (parent as *const u64).add(idx).read_volatile();
        if e4 & PRESENT == 0 {
            return true;
        }
        let pdpt = e4 & ADDR_MASK;
        for i3 in 0..512usize {
            let e3 = (pdpt as *const u64).add(i3).read_volatile();
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd = e3 & ADDR_MASK;
            for i2 in 0..512usize {
                let e2 = (pd as *const u64).add(i2).read_volatile();
                if e2 & PRESENT == 0 {
                    continue;
                }
                let pt = e2 & ADDR_MASK;
                for i1 in 0..512usize {
                    let e1 = (pt as *const u64).add(i1).read_volatile();
                    if e1 & PRESENT == 0 {
                        continue;
                    }
                    let src = e1 & ADDR_MASK;
                    let va = ((idx as u64) << 39)
                        | ((i3 as u64) << 30)
                        | ((i2 as u64) << 21)
                        | ((i1 as u64) << 12);
                    // A READ-ONLY page is a shared disk-cache frame: the child
                    // maps the SAME frame read-only (CoW on write) instead of
                    // copying it. This is what makes fork affordable with the
                    // page cache: copying the parent's whole RO-mapped text set
                    // (hundreds of MB) exhausted the pool and later cache-hit
                    // mappings failed silently (run 16 ifetch terminations).
                    if e1 & WRITABLE == 0 {
                        if !map_demand_4k_ro(child, va, src) {
                            return false;
                        }
                        continue;
                    }
                    let dst = match crate::procpool::demand_alloc() {
                        Some(f) => f,
                        None => return false,
                    };
                    core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, 4096);
                    if !map_demand_4k(child, va, dst) {
                        crate::procpool::demand_free(dst);
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Unmap every present 4 KiB page of [`start`, `end`) in `pml4` (the demand region),
/// flushing each from the TLB, and free the frame when `free_if(va, writable)` says
/// the process owns it (a shared or page-cache frame is only unmapped). Table
/// frames stay. Returns (pages unmapped, frames freed). This is what munmap and a
/// MAP_FIXED overlay need: V8 and PartitionAlloc "decommit" a range by mapping it
/// again with MAP_FIXED and expect fresh zero pages afterwards; keeping the old
/// frames handed them their stale heap back.
pub fn unmap_demand_range(pml4: u64, start: u64, end: u64,
                          free_if: &mut dyn FnMut(u64, bool) -> bool) -> (usize, usize) {
    let mut unmapped = 0usize;
    let mut freed = 0usize;
    let mut va = start & !0xFFF;
    // SAFETY: identity-mapped table chain; single-threaded (IF=0 syscall).
    unsafe {
        while va < end {
            let i4 = ((va >> 39) & 0x1FF) as usize;
            let i3 = ((va >> 30) & 0x1FF) as usize;
            let i2 = ((va >> 21) & 0x1FF) as usize;
            let e4 = (pml4 as *const u64).add(i4).read_volatile();
            if e4 & PRESENT == 0 { va = ((va >> 39) + 1) << 39; continue; }
            let e3 = ((e4 & ADDR_MASK) as *const u64).add(i3).read_volatile();
            if e3 & PRESENT == 0 { va = ((va >> 30) + 1) << 30; continue; }
            let e2 = ((e3 & ADDR_MASK) as *const u64).add(i2).read_volatile();
            if e2 & PRESENT == 0 { va = ((va >> 21) + 1) << 21; continue; }
            let pt = e2 & ADDR_MASK;
            // Walk this page table to the end of the range or of the table.
            let table_end = (((va >> 21) + 1) << 21).min(end);
            while va < table_end {
                let i1 = ((va >> 12) & 0x1FF) as usize;
                let slot = (pt as *mut u64).add(i1);
                let e1 = slot.read_volatile();
                if e1 & PRESENT != 0 {
                    slot.write_volatile(0);
                    core::arch::asm!("invlpg [{}]", in(reg) va, options(nostack, preserves_flags));
                    unmapped += 1;
                    if free_if(va, e1 & WRITABLE != 0) {
                        crate::procpool::demand_free(e1 & ADDR_MASK);
                        freed += 1;
                    }
                }
                va += 4096;
            }
        }
    }
    (unmapped, freed)
}

/// Free a demand-paged region: walk PML4[`idx`] and return every committed data page
/// AND every page-table frame to the process pool, then clear the PML4 entry. Mirrors
/// [`map_demand_4k`]. Called when a process that used demand paging exits.
/// free_demand_region, but frames present in `keep` (sorted) survive — they are
/// SHARED (memfd) frames other processes still map; freeing them with the dead
/// process is a use-after-free for every other mapper.
/// Every physical frame a PML4's demand region (slot `idx`) maps right now, sorted.
/// For the exit guard: a dead child must not free a frame the browser main still
/// maps, whatever list says it is private.
pub fn demand_phys_sorted(pml4: u64, idx: usize) -> alloc::vec::Vec<u64> {
    let mut v = alloc::vec::Vec::new();
    // SAFETY: identity-mapped table chain, read-only walk.
    unsafe {
        let e4 = (pml4 as *const u64).add(idx).read_volatile();
        if e4 & PRESENT == 0 {
            return v;
        }
        let pdpt = e4 & ADDR_MASK;
        for i3 in 0..512usize {
            let e3 = (pdpt as *const u64).add(i3).read_volatile();
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd = e3 & ADDR_MASK;
            for i2 in 0..512usize {
                let e2 = (pd as *const u64).add(i2).read_volatile();
                if e2 & PRESENT == 0 {
                    continue;
                }
                let pt = e2 & ADDR_MASK;
                for i1 in 0..512usize {
                    let e1 = (pt as *const u64).add(i1).read_volatile();
                    if e1 & PRESENT != 0 {
                        v.push(e1 & ADDR_MASK);
                    }
                }
            }
        }
    }
    v.sort_unstable();
    v
}

pub fn free_demand_region_except(pml4: u64, idx: usize, keep: &[u64]) {
    // SAFETY: identity-mapped table chain; called single-threaded at teardown.
    unsafe {
        let e4 = (pml4 as *const u64).add(idx).read_volatile();
        if e4 & PRESENT == 0 {
            return;
        }
        let pdpt = e4 & ADDR_MASK;
        for i3 in 0..512usize {
            let e3 = (pdpt as *const u64).add(i3).read_volatile();
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd = e3 & ADDR_MASK;
            for i2 in 0..512usize {
                let e2 = (pd as *const u64).add(i2).read_volatile();
                if e2 & PRESENT == 0 {
                    continue;
                }
                let pt = e2 & ADDR_MASK;
                for i1 in 0..512usize {
                    let e1 = (pt as *const u64).add(i1).read_volatile();
                    if e1 & PRESENT == 0 && e1 & SWAPPED_PTE != 0 {
                        crate::swapmgr::free_slot(((e1 >> 12) & 0xFFFFF) as usize);
                    }
                    if e1 & PRESENT != 0 {
                        let phys = e1 & ADDR_MASK;
                        if keep.binary_search(&phys).is_err() {
                            crate::procpool::demand_free(phys); // committed data page
                        }
                    }
                }
                crate::procpool::demand_free(pt);
            }
            crate::procpool::demand_free(pd);
        }
        crate::procpool::demand_free(pdpt);
        (pml4 as *mut u64).add(idx).write_volatile(0);
    }
}

pub fn free_demand_region(pml4: u64, idx: usize) {
    // SAFETY: identity-mapped table chain; called single-threaded at process teardown.
    unsafe {
        let e4 = (pml4 as *const u64).add(idx).read_volatile();
        if e4 & PRESENT == 0 {
            return;
        }
        let pdpt = e4 & ADDR_MASK;
        for i3 in 0..512usize {
            let e3 = (pdpt as *const u64).add(i3).read_volatile();
            if e3 & PRESENT == 0 {
                continue;
            }
            let pd = e3 & ADDR_MASK;
            for i2 in 0..512usize {
                let e2 = (pd as *const u64).add(i2).read_volatile();
                if e2 & PRESENT == 0 {
                    continue;
                }
                let pt = e2 & ADDR_MASK;
                for i1 in 0..512usize {
                    let e1 = (pt as *const u64).add(i1).read_volatile();
                    if e1 & PRESENT != 0 {
                        crate::procpool::demand_free(e1 & ADDR_MASK); // committed data page
                    } else if e1 & SWAPPED_PTE != 0 {
                        crate::swapmgr::free_slot(((e1 >> 12) & 0xFFFFF) as usize); // its swap slot
                    }
                }
                crate::procpool::demand_free(pt);
            }
            crate::procpool::demand_free(pd);
        }
        crate::procpool::demand_free(pdpt);
        (pml4 as *mut u64).add(idx).write_volatile(0);
    }
}

/// Free the page-table frames of a process address space (PML4+PDPT+PD+arena PT).
/// The arena data pages themselves are freed separately by the caller.
pub fn free_address_space(falloc: &mut FrameAllocator, pml4: u64) {
    // SAFETY: pml4 is identity-mapped; we walk the table chain.
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        // Under PML4[0] a PDPT entry is either an identity 1 GiB HUGE page (not a
        // frame of ours) or a page DIRECTORY of this process: the one
        // build_address_space_rwx made at index 0, and the one(s) map_user_arena
        // added at the index where the big arena physically lies (get_pd). Free
        // every such PD and, under it, every PRESENT-without-HUGE entry (a 4 KiB
        // page table). Freeing only pdpt[0] leaked exactly one PD frame per
        // glibc run ([churn-proc], 2026-10-07).
        let pdptp = pdpt as *const u64;
        for i3 in 0..512usize {
            let e3 = pdptp.add(i3).read_volatile();
            if e3 & PRESENT == 0 || e3 & HUGE != 0 {
                continue;
            }
            let pd = e3 & ADDR_MASK;
            let pdp = pd as *const u64;
            for i in 0..512usize {
                let e = pdp.add(i).read_volatile();
                if e & PRESENT != 0 && e & HUGE == 0 {
                    let _ = falloc.free(e & ADDR_MASK);
                }
            }
            let _ = falloc.free(pd);
        }
        let _ = falloc.free(pdpt);
        let _ = falloc.free(pml4);
    }
}

/// Free a fork child's own page tables under PML4[0] (pdpt, pd and every 4 KiB
/// page table, all from `procpool::alloc` in fill_remap_tables_multiblock) back
/// to the process pool. The leaf frames (the child's arena) are freed by the
/// caller as a range; the identity 1 GiB / 2 MiB HUGE entries are not frames
/// of ours. 3 table frames per fork child leaked without this ([churn-fork]).
///
/// CALLED FROM THE CHILD'S OWN EXIT PATH, i.e. while the CPU still runs on
/// these very tables (CR3 = this pml4, and PML4[0] maps the kernel itself).
/// So this only marks the frames free in the pool, exactly like the pml4
/// frame free next to it; it must NOT clear or zero any entry (doing so hung
/// the boot in a fault loop, churn6). Nothing reuses the frames before the
/// exit path switches away: single core, no allocation in between.
pub fn free_fork_tables(pml4: u64) -> usize {
    let mut freed = 0;
    // SAFETY: identity-mapped tables; read-only walk, frames only marked free.
    unsafe {
        let e4 = (pml4 as *const u64).read_volatile();
        if e4 & PRESENT == 0 { return 0; }
        let pdpt = e4 & ADDR_MASK;
        for i in 0..512usize {
            let e3 = (pdpt as *const u64).add(i).read_volatile();
            if e3 & PRESENT == 0 || e3 & HUGE != 0 { continue; }
            let pd = e3 & ADDR_MASK;
            for j in 0..512usize {
                let e2 = (pd as *const u64).add(j).read_volatile();
                if e2 & PRESENT != 0 && e2 & HUGE == 0 {
                    crate::procpool::free(e2 & ADDR_MASK); freed += 1;
                }
            }
            crate::procpool::free(pd); freed += 1;
        }
        crate::procpool::free(pdpt); freed += 1;
        // no write_volatile(0): see above, we are running on this table
    }
    freed
}

/// Free the page-TABLE frames under one PML4 slot back to the DEMAND POOL (the
/// tables of the demand region come from `demand_alloc`, see `map_demand_4k`),
/// and clear the slot. Leaf frames are not touched: the caller frees those
/// (free_demand_region_except) before. Returns the number of table frames.
pub fn free_demand_table_chain(pml4: u64, slot: usize) -> usize {
    let mut freed = 0;
    // SAFETY: identity-mapped tables of a process no task runs on anymore.
    unsafe {
        let pml4p = (pml4 as *mut u64).add(slot & 0x1FF);
        let e4 = pml4p.read_volatile();
        if e4 & PRESENT == 0 { return 0; }
        let pdpt = e4 & ADDR_MASK;
        for i in 0..512usize {
            let e3 = (pdpt as *const u64).add(i).read_volatile();
            if e3 & PRESENT == 0 || e3 & HUGE != 0 { continue; }
            let pd = e3 & ADDR_MASK;
            for j in 0..512usize {
                let e2 = (pd as *const u64).add(j).read_volatile();
                if e2 & PRESENT != 0 && e2 & HUGE == 0 {
                    crate::procpool::demand_free(e2 & ADDR_MASK); freed += 1;
                }
            }
            crate::procpool::demand_free(pd); freed += 1;
        }
        crate::procpool::demand_free(pdpt); freed += 1;
        pml4p.write_volatile(0);
    }
    freed
}

/// Free the page-TABLE frames (pdpt, pd, pt: never the leaf data frames) under
/// one PML4 slot of a process address space, and clear the slot. For the
/// per-process chains built with `map_user_4k_falloc` (the vDSO at its own
/// slot): those tables come from the frame allocator and `free_address_space`
/// does not reach them, which leaked 3 frames per glibc run ([churn-proc]).
/// Must be called BEFORE `free_address_space` (which frees the pml4 frame).
pub fn free_user_table_chain(falloc: &mut FrameAllocator, pml4: u64, slot: usize) -> usize {
    let mut freed = 0;
    // SAFETY: identity-mapped tables of a process no task runs on anymore.
    unsafe {
        let pml4p = (pml4 as *mut u64).add(slot & 0x1FF);
        let e4 = pml4p.read_volatile();
        if e4 & PRESENT == 0 { return 0; }
        let pdpt = e4 & ADDR_MASK;
        for i in 0..512usize {
            let e3 = (pdpt as *const u64).add(i).read_volatile();
            if e3 & PRESENT == 0 || e3 & HUGE != 0 { continue; }
            let pd = e3 & ADDR_MASK;
            for j in 0..512usize {
                let e2 = (pd as *const u64).add(j).read_volatile();
                if e2 & PRESENT != 0 && e2 & HUGE == 0 {
                    let _ = falloc.free(e2 & ADDR_MASK); freed += 1;
                }
            }
            let _ = falloc.free(pd); freed += 1;
        }
        let _ = falloc.free(pdpt); freed += 1;
        pml4p.write_volatile(0);
    }
    freed
}

/// Build the page tables and load CR3. Returns the physical PML4 address.
pub fn init(falloc: &mut FrameAllocator) -> u64 {
    let pml4 = falloc.allocate().expect("frame for PML4");
    let pdpt = falloc.allocate().expect("frame for PDPT");

    // SAFETY: both frames are free, valid and (UEFI-)identity-mapped RAM.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);

        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;

        // The BOOT address space is entirely SUPERVISOR: kernel code, stacks, heap and
        // the page tables live in the lower 1 GiB and must NOT carry the User bit —
        // otherwise enabling SMEP (no ring-0 fetch of user pages) or SMAP (no ring-0
        // access to user pages) would fault immediately on the very next
        // instruction/stack access. Each ring-3 process gets its OWN PML4
        // (build_address_space) with exactly one User arena; no ring-3 task runs on
        // this boot CR3 anymore. See ring3::enable_smep_smap.
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE);
        // PDPT[i] = identity 1 GiB page (0..512 GiB), supervisor.
        for i in 0..512u64 {
            pdptp
                .add(i as usize)
                .write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }

        // PML4[1] -> second PDPT: identity 512 GiB..1 TiB, supervisor. Some
        // PCI 64-bit BARs lie high — QEMU q35 places the NVMe controller MMIO
        // e.g. at ≈768 GiB. Without this mapping every MMIO access there faults.
        let pdpt_hi = falloc.allocate().expect("frame for high PDPT");
        core::ptr::write_bytes(pdpt_hi as *mut u8, 0, 4096);
        let pdpt_hip = pdpt_hi as *mut u64;
        pml4p.add(1).write_volatile(pdpt_hi | PRESENT | WRITABLE);
        for i in 0..512u64 {
            pdpt_hip
                .add(i as usize)
                .write_volatile(((512 + i) * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // Share this high PDPT with every process PML4 (build_address_space), so the
        // 512 GiB..1 TiB region is mapped identically everywhere — the foundation for
        // shared guarded kernel stacks (A2) and high MMIO BARs under a process CR3.
        HIGH_PDPT.store(pdpt_hi, core::sync::atomic::Ordering::Relaxed);

        // Load our CR3. This immediately flushes the TLB.
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
    }
    pml4
}

/// Userspace drivers (docs/SPRINT-PLAN-USERSPACE-DRIVERS.md): give the process
/// `pml4` access to exactly one 4 KiB page at virtual == physical `pa` (below
/// 512 GiB), `USER | RW | NX`, plus `PCD | PWT` (uncached) when `mmio`.
///
/// The process's lower 512 GiB is an identity map of supervisor huge pages
/// (1 GiB in the PDPT, 2 MiB in the PD). The huge page covering `pa` is split
/// into the next level with the very same supervisor mapping (address, flags,
/// executable for the kernel), so ring 0 sees no difference; only the one leaf
/// gets the USER bit, and U/S is ANDed down the walk, so everything around it
/// stays out of reach of ring 3. The new table frames come from `falloc` and
/// are freed with the address space (`free_address_space` frees every non-huge
/// table under PML4[0]). Call before the process first runs (no TLB shootdown).
pub fn map_user_identity_4k(falloc: &mut FrameAllocator, pml4: u64, pa: u64, mmio: bool) -> bool {
    map_user_4k_at(falloc, pml4, pa, pa, mmio)
}

/// Like [`map_user_identity_4k`], but the page at virtual `va` (below 512 GiB)
/// shows physical `pa`, which may lie anywhere below 2^52: a device BAR above
/// the identity map (QEMU's 64-bit NVMe BAR near 768 GiB) is given to its
/// userspace driver at a virtual address of the driver's own (eurodrv
/// `HIGH_MMIO_VA`). Only the leaf differs from the identity case; the split
/// tables above it keep the supervisor identity mapping for everything else.
pub fn map_user_4k_at(falloc: &mut FrameAllocator, pml4: u64, va: u64, pa: u64, mmio: bool) -> bool {
    const PCD_PWT: u64 = (1 << 4) | (1 << 3);
    const PAT_HUGE: u64 = 1 << 12; // PAT bit of a 1 GiB / 2 MiB entry
    const PAT_4K: u64 = 1 << 7; // PAT bit of a 4 KiB entry
    if va >= 512 * GIB || va & 0xFFF != 0 || pa & 0xFFF != 0 || pa >= 1 << 52 {
        return false;
    }
    // Flags of a huge entry that carry over to the smaller pages (P, RW, U/S,
    // PWT, PCD, G and NX); never the address, PS, A/D or the PAT position.
    let carry = |e: u64| e & (0x1F | (1 << 8) | NX);
    // SAFETY: every table touched is identity-mapped and belongs to this
    // process's private hierarchy (PML4[0] -> its own PDPT), or is a fresh frame.
    unsafe {
        let e4 = (pml4 as *const u64).read_volatile();
        if e4 & PRESENT == 0 {
            return false;
        }
        let pdpt = e4 & ADDR_MASK;
        let i3 = ((va >> 30) & 0x1FF) as usize;
        let s3 = (pdpt as *mut u64).add(i3);
        let mut e3 = s3.read_volatile();
        if e3 & PRESENT == 0 {
            return false;
        }
        if e3 & HUGE != 0 {
            let pd = match falloc.allocate() { Ok(f) => f, Err(_) => return false };
            let base = e3 & 0x000F_FFFF_C000_0000;
            let pat = e3 & PAT_HUGE;
            for j in 0..512u64 {
                (pd as *mut u64).add(j as usize).write_volatile((base + j * MIB2) | carry(e3) | HUGE | pat);
            }
            e3 = pd | PRESENT | WRITABLE | USER;
            s3.write_volatile(e3);
        } else if e3 & USER == 0 {
            s3.write_volatile(e3 | USER);
        }
        let pd = e3 & ADDR_MASK;
        let i2 = ((va >> 21) & 0x1FF) as usize;
        let s2 = (pd as *mut u64).add(i2);
        let mut e2 = s2.read_volatile();
        if e2 & PRESENT == 0 {
            return false;
        }
        if e2 & HUGE != 0 {
            let pt = match falloc.allocate() { Ok(f) => f, Err(_) => return false };
            let base = e2 & 0x000F_FFFF_FFE0_0000;
            let pat = if e2 & PAT_HUGE != 0 { PAT_4K } else { 0 };
            for k in 0..512u64 {
                (pt as *mut u64).add(k as usize).write_volatile((base + k * 4096) | carry(e2) | pat);
            }
            e2 = pt | PRESENT | WRITABLE | USER;
            s2.write_volatile(e2);
        } else if e2 & USER == 0 {
            s2.write_volatile(e2 | USER);
        }
        let pt = e2 & ADDR_MASK;
        let i1 = ((va >> 12) & 0x1FF) as usize;
        let leaf = pa | PRESENT | WRITABLE | USER | NX | if mmio { PCD_PWT } else { 0 };
        (pt as *mut u64).add(i1).write_volatile(leaf);
    }
    true
}

/// Is `virt` a page ring 3 may touch in `pml4` (present and USER at every level)?
pub fn user_accessible_in(pml4: u64, virt: u64) -> bool {
    // SAFETY: identity-mapped table chain, read-only walk.
    unsafe {
        let mut table = pml4;
        for shift in [39u64, 30, 21] {
            let e = (table as *const u64).add(((virt >> shift) & 0x1FF) as usize).read_volatile();
            if e & (PRESENT | USER) != (PRESENT | USER) {
                return false;
            }
            if shift != 39 && e & HUGE != 0 {
                return true;
            }
            table = e & ADDR_MASK;
        }
        let e = (table as *const u64).add(((virt >> 12) & 0x1FF) as usize).read_volatile();
        e & (PRESENT | USER) == (PRESENT | USER)
    }
}
