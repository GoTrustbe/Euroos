//! Transparante, fault-gedreven swap (plan J3, slot 2) — de paging-helft van de
//! swap-cyclus die [`euromm::swap`] (CLOCK + SwapArea) al host-getest levert.
//!
//! Waar de `[j3]`-zelftest de mechaniek (slachtofferkeuze + schijf-I/O) bewijst,
//! maakt deze module het **transparant**: een uitgeswapte pagina krijgt z'n PTE
//! op *niet-present* met de swap-slot in de bovenste bits gecodeerd. Raakt iets
//! die pagina later aan, dan vuurt een **page fault** — en de fault-handler
//! ([`crate::interrupts`]) roept [`try_swap_in`] aan, die het frame terugleest van
//! schijf, de PTE weer present maakt en de instructie hervat. Het proces merkt er
//! niets van: de pagina "was er altijd".
//!
//! Alle page-tables zijn identity-mapped (virtueel = fysiek < 512 GiB), dus we
//! lopen ze direct met fysieke pointers. De swap-I/O gaat via [`crate::virtio_blk`]
//! (busy-poll, werkt ook met interrupts uit — precies wat een fault-handler nodig
//! heeft).

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
/// Marker-bit in een NIET-present PTE: "deze pagina is uitgeswapt". Omdat present=0
/// negeert de CPU alle overige bits, dus we mogen er vrij de swap-slot in coderen.
const SWAPPED: u64 = 1 << 9;
const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const SECTORS_PER_PAGE: u64 = 8; // 4096 / 512

struct SwapMgr {
    area: SwapArea,
    /// Vrije fysieke frames voor swap-in (gevuld bij swap-out, geleegd bij swap-in).
    pool: Vec<u64>,
    base_lba: u64,
    swap_ins: u64,
    swap_outs: u64,
}

static MGR: Mutex<Option<SwapMgr>> = Mutex::new(None);

/// Fysiek basisadres van de actieve PML4 (uit CR3).
fn active_pml4() -> *mut u64 {
    Cr3::read().0.start_address().as_u64() as *mut u64
}

/// Loop de 4 niveaus naar de PTE (4 KiB) van `virt`. Geeft None bij een huge-page
/// of een ontbrekend tussenniveau (dan is het geen swappable 4 KiB-pagina).
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

/// Zorg dat de tabel-entry naar een present sub-tabel wijst; alloceer er anders een.
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

/// Map één 4 KiB-pagina `virt` → `frame` in de actieve adresruimte, en bouw daarbij
/// de PDPT/PD/PT-keten zo nodig op (fijnmazig, zodat de pagina swappable is).
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

/// Initialiseer de swap-manager: `slots` swap-slots vanaf `base_lba` op schijf 0,
/// met een kleine pool vrije frames voor de swap-in.
pub fn init(base_lba: u64, slots: usize, pool: Vec<u64>) {
    *MGR.lock() = Some(SwapMgr {
        area: SwapArea::new(slots),
        pool,
        base_lba,
        swap_ins: 0,
        swap_outs: 0,
    });
}

/// Swap de pagina op `virt` UIT: schrijf 'm naar een swap-slot, maak de PTE niet-
/// present (met de slot gecodeerd), en geef het frame vrij aan de pool. Geeft true
/// bij succes. Hierna faultt elke toegang tot `virt` → [`try_swap_in`].
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
            return false; // niet (meer) present → niets om uit te swappen
        }
        let frame = e & PHYS_MASK;
        let slot = match mgr.area.alloc() {
            Some(s) => s,
            None => return false, // swap vol
        };
        // 4 KiB (8 sectoren) van het frame naar de swap-slot schrijven.
        for s in 0..SECTORS_PER_PAGE {
            let mut sec = [0u8; 512];
            core::ptr::copy_nonoverlapping((frame + s * 512) as *const u8, sec.as_mut_ptr(), 512);
            crate::virtio_blk::write_sector(mgr.base_lba + slot as u64 * SECTORS_PER_PAGE + s, &sec);
        }
        crate::virtio_blk::flush();
        // PTE niet-present + slot gecodeerd in de bovenste bits + SWAPPED-marker.
        *pte = ((slot as u64) << 12) | SWAPPED;
        tlb::flush(VirtAddr::new(virt));
        mgr.pool.push(frame);
        mgr.swap_outs += 1;
    }
    true
}

/// Door de page-fault-handler aangeroepen: als `virt` een uitgeswapte pagina is,
/// lees 'm terug in een vers frame, maak de PTE weer present en geef true (de
/// instructie wordt hervat). False = geen swap-pagina → een echte fault.
pub fn try_swap_in(virt: u64) -> bool {
    // De fault-handler draait op de PF-IST-stack; een gewone try_lock voorkomt een
    // deadlock mocht er ooit genest gefault worden terwijl we de lock al houden.
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
            return false; // present of niet-onze-marker → echte fault
        }
        let slot = ((e >> 12) & 0xFFFFF) as usize;
        let frame = match mgr.pool.pop() {
            Some(f) => f,
            None => return false, // geen vrij frame → kan niet inswappen
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

/// (swap-ins, swap-outs) — diagnostiek voor de zelftest.
pub fn stats() -> (u64, u64) {
    match MGR.lock().as_ref() {
        Some(m) => (m.swap_ins, m.swap_outs),
        None => (0, 0),
    }
}
