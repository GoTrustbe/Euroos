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
pub fn build_address_space_rwx_big(falloc: &mut FrameAllocator, arena_phys: u64, nblocks: u64) -> u64 {
    let pml4 = build_address_space_rwx(falloc, arena_phys);
    if nblocks <= 1 {
        return pml4;
    }
    let arena_idx = (arena_phys / MIB2) as usize;
    // SAFETY: identity-mapped tables we just built; single-threaded here.
    unsafe {
        let pdpt = (*(pml4 as *const u64)) & 0x000f_ffff_ffff_f000;
        let pd = (*(pdpt as *const u64)) & 0x000f_ffff_ffff_f000;
        let pdp = pd as *mut u64;
        for b in 1..nblocks as usize {
            let i = arena_idx + b;
            if i >= 512 {
                break;
            }
            pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | USER | HUGE);
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

/// Free the page-table frames of a process address space (PML4+PDPT+PD+arena PT).
/// The arena data pages themselves are freed separately by the caller.
pub fn free_address_space(falloc: &mut FrameAllocator, pml4: u64) {
    // SAFETY: pml4 is identity-mapped; we walk the table chain.
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        let pd = (pdpt as *const u64).read_volatile() & ADDR_MASK;
        // The arena PD entry is the only PRESENT-without-HUGE: it points to the 4 KiB PT.
        let pdp = pd as *const u64;
        for i in 0..512usize {
            let e = pdp.add(i).read_volatile();
            if e & PRESENT != 0 && e & HUGE == 0 {
                let _ = falloc.free(e & ADDR_MASK); // the arena PT
                break;
            }
        }
        let _ = falloc.free(pd);
        let _ = falloc.free(pdpt);
        let _ = falloc.free(pml4);
    }
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
