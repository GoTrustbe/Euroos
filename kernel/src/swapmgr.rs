//! Transparent, fault-driven swap (plan J3, slot 2) — the paging half of the
//! swap cycle that [`euromm::swap`] (CLOCK + SwapArea) already provides host-tested.
//!
//! Where the `[j3]` self-test proves the mechanism (victim selection + disk I/O),
//! this module makes it **transparent**: a swapped-out page gets its PTE
//! set to *non-present* with the swap slot encoded in the upper bits. If something
//! later touches that page, a **page fault** fires — and the fault handler
//! ([`crate::interrupts`]) calls [`try_swap_in`], which reads the frame back from
//! disk, makes the PTE present again, and resumes the instruction. The process
//! notices nothing: the page "was always there".
//!
//! All page tables are identity-mapped (virtual = physical < 512 GiB), so we
//! walk them directly with physical pointers. The swap I/O goes via [`crate::virtio_blk`]
//! (busy-poll, works even with interrupts off — exactly what a fault handler
//! needs).

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::tlb;
use x86_64::registers::control::Cr3;
use x86_64::VirtAddr;

use euromm::swap::SwapArea;
use euromm::FrameAllocator;

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
/// Marker bit in a NON-present PTE: "this page is swapped out". Because present=0
/// the CPU ignores all the other bits, so we may freely encode the swap slot in them.
const SWAPPED: u64 = 1 << 9;
const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const SECTORS_PER_PAGE: u64 = 8; // 4096 / 512

struct SwapMgr {
    area: SwapArea,
    /// Free physical frames for swap-in (filled on swap-out, emptied on swap-in).
    pool: Vec<u64>,
    base_lba: u64,
    swap_ins: u64,
    swap_outs: u64,
    /// CLOCK registry of swappable user pages (virtual addresses) + the hand.
    swappable: Vec<u64>,
    hand: usize,
    /// Swap-in reserve to keep in `pool` so an evicted page can always fault back in.
    reserve: usize,
}

static MGR: Mutex<Option<SwapMgr>> = Mutex::new(None);

/// Physical base address of the active PML4 (from CR3).
fn active_pml4() -> *mut u64 {
    Cr3::read().0.start_address().as_u64() as *mut u64
}

/// Walk the 4 levels down to the PTE (4 KiB) of `virt`. Returns None for a huge page
/// or a missing intermediate level (then it is not a swappable 4 KiB page).
unsafe fn walk_pte(virt: u64) -> Option<*mut u64> {
    let i4 = ((virt >> 39) & 0x1FF) as isize;
    let i3 = ((virt >> 30) & 0x1FF) as isize;
    let i2 = ((virt >> 21) & 0x1FF) as isize;
    let i1 = ((virt >> 12) & 0x1FF) as isize;
    let e4 = *active_pml4().offset(i4);
    if e4 & PRESENT == 0 {
        return None;
    }
    let pdpt = (e4 & PHYS_MASK) as *mut u64;
    let e3 = *pdpt.offset(i3);
    if e3 & PRESENT == 0 || e3 & HUGE != 0 {
        return None;
    }
    let pd = (e3 & PHYS_MASK) as *mut u64;
    let e2 = *pd.offset(i2);
    if e2 & PRESENT == 0 || e2 & HUGE != 0 {
        return None;
    }
    let pt = (e2 & PHYS_MASK) as *mut u64;
    Some(pt.offset(i1))
}

/// Ensure the table entry points to a present sub-table; otherwise allocate one.
unsafe fn ensure_table(entry: *mut u64, falloc: &mut FrameAllocator) -> u64 {
    let e = *entry;
    if e & PRESENT != 0 {
        return e & PHYS_MASK;
    }
    let f = falloc.allocate().expect("swapmgr page-table frame");
    core::ptr::write_bytes(f as *mut u8, 0, 4096);
    *entry = f | PRESENT | WRITABLE;
    f
}

/// Map one 4 KiB page `virt` → `frame` in the active address space, building up
/// the PDPT/PD/PT chain as needed (fine-grained, so the page is swappable).
pub fn map_one_page(falloc: &mut FrameAllocator, virt: u64, frame: u64) {
    unsafe {
        let i4 = ((virt >> 39) & 0x1FF) as isize;
        let i3 = ((virt >> 30) & 0x1FF) as isize;
        let i2 = ((virt >> 21) & 0x1FF) as isize;
        let i1 = ((virt >> 12) & 0x1FF) as isize;
        let pml4 = active_pml4();
        let pdpt = ensure_table(pml4.offset(i4), falloc) as *mut u64;
        let pd = ensure_table(pdpt.offset(i3), falloc) as *mut u64;
        let pt = ensure_table(pd.offset(i2), falloc) as *mut u64;
        *pt.offset(i1) = (frame & PHYS_MASK) | PRESENT | WRITABLE;
        tlb::flush(VirtAddr::new(virt));
    }
}

/// Initialize the swap manager: `slots` swap slots starting at `base_lba` on disk 0,
/// with a small pool of free frames for swap-in.
pub fn init(base_lba: u64, slots: usize, pool: Vec<u64>) {
    let reserve = pool.len();
    *MGR.lock() = Some(SwapMgr {
        area: SwapArea::new(slots),
        pool,
        base_lba,
        swap_ins: 0,
        swap_outs: 0,
        swappable: Vec::new(),
        hand: 0,
        reserve,
    });
}

/// Register a user virtual page as a candidate for CLOCK auto-eviction.
pub fn register_swappable(virt: u64) {
    if let Some(mgr) = MGR.lock().as_mut() {
        if !mgr.swappable.contains(&virt) {
            mgr.swappable.push(virt);
        }
    }
}

/// Auto-evict under memory pressure: pick the next registered page (CLOCK order),
/// swap it OUT, and hand its physical frame back to the global allocator `falloc`
/// (keeping a swap-in reserve in the pool so the page can later fault back in).
/// Returns the freed physical frame, or `None` if nothing could be reclaimed.
/// The page becomes non-present; the next access transparently swaps it in.
pub fn auto_evict(falloc: &mut FrameAllocator) -> Option<u64> {
    // Choose a victim (present, registered) using the CLOCK hand.
    let victim = {
        let mut guard = MGR.lock();
        let mgr = guard.as_mut()?;
        if mgr.swappable.is_empty() {
            return None;
        }
        let n = mgr.swappable.len();
        let mut chosen = None;
        for _ in 0..n {
            let v = mgr.swappable[mgr.hand % n];
            mgr.hand = mgr.hand.wrapping_add(1);
            // Is it present (worth evicting)?
            let present = unsafe { walk_pte(v).map(|p| *p & PRESENT != 0).unwrap_or(false) };
            if present {
                chosen = Some(v);
                break;
            }
        }
        chosen?
    };
    // swap_out takes the lock itself → call it outside the guard above.
    if !swap_out(victim) {
        return None;
    }
    // The freed frame is now in the pool; return it to the global allocator,
    // but never drop below the swap-in reserve.
    let mut guard = MGR.lock();
    let mgr = guard.as_mut()?;
    if mgr.pool.len() > mgr.reserve {
        let frame = mgr.pool.pop()?;
        let _ = falloc.free(frame);
        Some(frame)
    } else {
        // Pool at reserve floor: the page is swapped out (pressure relieved on the
        // swap device) but we keep the frame as swap-in headroom.
        None
    }
}

/// Swap the page at `virt` OUT: write it to a swap slot, make the PTE non-
/// present (with the slot encoded), and return the frame to the pool. Returns true
/// on success. After this, every access to `virt` faults → [`try_swap_in`].
pub fn swap_out(virt: u64) -> bool {
    let mut guard = MGR.lock();
    let mgr = match guard.as_mut() {
        Some(m) => m,
        None => return false,
    };
    unsafe {
        let pte = match walk_pte(virt) {
            Some(p) => p,
            None => return false,
        };
        let e = *pte;
        if e & PRESENT == 0 {
            return false; // not (any longer) present → nothing to swap out
        }
        let frame = e & PHYS_MASK;
        let slot = match mgr.area.alloc() {
            Some(s) => s,
            None => return false, // swap full
        };
        // Write 4 KiB (8 sectors) of the frame to the swap slot.
        for s in 0..SECTORS_PER_PAGE {
            let mut sec = [0u8; 512];
            core::ptr::copy_nonoverlapping((frame + s * 512) as *const u8, sec.as_mut_ptr(), 512);
            crate::virtio_blk::write_sector(mgr.base_lba + slot as u64 * SECTORS_PER_PAGE + s, &sec);
        }
        crate::virtio_blk::flush();
        // PTE non-present + slot encoded in the upper bits + SWAPPED marker.
        *pte = ((slot as u64) << 12) | SWAPPED;
        tlb::flush(VirtAddr::new(virt));
        mgr.pool.push(frame);
        mgr.swap_outs += 1;
    }
    true
}

/// Called by the page-fault handler: if `virt` is a swapped-out page,
/// read it back into a fresh frame, make the PTE present again and return true (the
/// instruction is resumed). False = not a swap page → a real fault.
pub fn try_swap_in(virt: u64) -> bool {
    // The fault handler runs on the PF IST stack; a plain try_lock prevents a
    // deadlock should a nested fault ever occur while we already hold the lock.
    let mut guard = match MGR.try_lock() {
        Some(g) => g,
        None => return false,
    };
    let mgr = match guard.as_mut() {
        Some(m) => m,
        None => return false,
    };
    unsafe {
        let pte = match walk_pte(virt) {
            Some(p) => p,
            None => return false,
        };
        let e = *pte;
        if e & PRESENT != 0 || e & SWAPPED == 0 {
            return false; // present or not-our-marker → real fault
        }
        let slot = ((e >> 12) & 0xFFFFF) as usize;
        let frame = match mgr.pool.pop() {
            Some(f) => f,
            None => return false, // no free frame → cannot swap in
        };
        for s in 0..SECTORS_PER_PAGE {
            let mut sec = [0u8; 512];
            crate::virtio_blk::read_sector(mgr.base_lba + slot as u64 * SECTORS_PER_PAGE + s, &mut sec);
            core::ptr::copy_nonoverlapping(sec.as_ptr(), (frame + s * 512) as *mut u8, 512);
        }
        *pte = (frame & PHYS_MASK) | PRESENT | WRITABLE;
        tlb::flush(VirtAddr::new(virt));
        mgr.area.free(slot);
        mgr.swap_ins += 1;
    }
    true
}

/// (swap-ins, swap-outs) — diagnostics for the self-test.
pub fn stats() -> (u64, u64) {
    match MGR.lock().as_ref() {
        Some(m) => (m.swap_ins, m.swap_outs),
        None => (0, 0),
    }
}
