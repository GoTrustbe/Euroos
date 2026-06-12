//! Eigen 4-niveau page tables (Track 3 paging).
//!
//! We bouwen een verse PML4 + PDPT die de onderste 512 GiB **identiek** mappen
//! (virtueel = fysiek) met 1 GiB huge-pages. Omdat alles identity-mapped blijft,
//! werken alle bestaande pointers (kernelcode, stack, heap, framebuffer) na het
//! laden van onze CR3 gewoon door — maar nu op ONZE tabellen i.p.v. die van UEFI.
//!
//! Klein en robuust: 2 frames (PML4 + PDPT), 512×1 GiB. De fijnmazige
//! (4 KiB, User-bit) mappings voor ring-3 komen hierbovenop in een volgende stap.

use euromm::FrameAllocator;

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE: u64 = 1 << 7; // 1 GiB-pagina in een PDPT-entry
const NX: u64 = 1 << 63; // No-Execute (vereist EFER.NXE) — W^X
const GIB: u64 = 1 << 30;
const MIB2: u64 = 1 << 21; // 2 MiB-pagina in een PD-entry
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// De gedeelde hoge PDPT (PML4[1] → 512 GiB..1 TiB), bij boot ingesteld. Elke
/// proces-PML4 deelt dezelfde PDPT zodat guarded kernel-stacks (A2) + hoge MMIO-
/// BARs in álle adresruimten geldig zijn.
pub static HIGH_PDPT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn high_pdpt_entry() -> u64 {
    let p = HIGH_PDPT.load(core::sync::atomic::Ordering::Relaxed);
    if p != 0 {
        p | PRESENT | WRITABLE
    } else {
        0
    }
}

/// Bouw een GEÏSOLEERDE adresruimte (PML4) voor één proces. De kernel wordt
/// overal supervisor gemapt (identity), zodat syscalls/interrupts/kernel-stacks
/// blijven werken na een CR3-wissel. Alleen het 2 MiB-blok op `arena_phys`
/// (2 MiB-uitgelijnd, < 1 GiB) krijgt de USER-bit — daar leven de user-frames
/// van DIT proces. Andermans arena's blijven supervisor-only -> ring 3 kan er
/// niet bij. Geeft het fysieke PML4-adres terug (laadt CR3 NIET).
pub fn build_address_space(
    falloc: &mut FrameAllocator,
    arena_phys: u64,
    exec_pages: &[u64; 8],
    writ_pages: &[u64; 8],
) -> u64 {
    let pml4 = falloc.allocate().expect("frame voor proces-PML4");
    let pdpt = falloc.allocate().expect("frame voor proces-PDPT");
    let pd = falloc.allocate().expect("frame voor proces-PD");
    // Vierde frame: de PT die het 2 MiB-arena-blok fijnmazig (4 KiB) mapt, zodat we
    // per pagina W^X kunnen afdwingen (uitvoerbaar XOR schrijfbaar).
    let pt = falloc.allocate().expect("frame voor arena-PT");
    // SAFETY: vier verse, identity-mapped frames; we vullen ze volledig.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        core::ptr::write_bytes(pt as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        let ptp = pt as *mut u64;
        // PML4[0] -> PDPT (USER op elk niveau; de PT-entry gate't per 4 KiB).
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Deel de hoge regio (PML4[1]) met de boot-adresruimte (A2/hoge MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        // PDPT[0] -> PD (onderste 1 GiB, fijnmazig 2 MiB).
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        // PDPT[1..512] = identity 1 GiB supervisor (kernel/MMIO/framebuffer hoog).
        // GEEN NX: kernelcode in de onderste 1 GiB draait ring-0 (syscalls/interrupts)
        // ONDER deze proces-CR3, dus die pagina's moeten uitvoerbaar blijven. W^X geldt
        // alleen voor de USER-arena (de PT hieronder).
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // PD[0..512] = identity 2 MiB supervisor (kernel — uitvoerbaar), behalve het
        // arena-blok dat naar de fijnmazige W^X-PT wijst.
        let arena_idx = (arena_phys / MIB2) as usize;
        for i in 0..512usize {
            if i == arena_idx {
                pdp.add(i).write_volatile(pt | PRESENT | WRITABLE | USER);
            } else {
                pdp.add(i).write_volatile((i as u64 * MIB2) | PRESENT | WRITABLE | HUGE);
            }
        }
        // PT[0..512]: de 512 4 KiB-pagina's van de arena. W^X per pagina:
        //   exec & !writ → R-X  (code, read-only + uitvoerbaar)
        //   exec &  writ → RWX  (binary met gemengd RWE-segment; W^X niet afdwingbaar)
        //   !exec        → RW+NX (data/bss/heap/stack: schrijfbaar, niet uitvoerbaar)
        for p in 0..512usize {
            let bit = 1u64 << (p % 64);
            let exec = exec_pages[p / 64] & bit != 0;
            let writ = writ_pages[p / 64] & bit != 0;
            let flags = if exec && writ {
                PRESENT | USER | WRITABLE // RWX — kan niet anders voor een RWE-segment
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

/// Zoals [`build_address_space`], maar mapt het arena-blok als één 2 MiB USER-pagina
/// met RWX (geen W^X). Voor de teller-demo (`spawn_counter_task`): die draait een
/// rauw machinecode-blob met een teller-variabele IN dezelfde pagina, dus code en
/// data zijn niet te scheiden — W^X is daar niet toepasbaar. Echte ELF-programma's
/// gebruiken altijd de W^X-variant hierboven.
pub fn build_address_space_rwx(falloc: &mut FrameAllocator, arena_phys: u64) -> u64 {
    let pml4 = falloc.allocate().expect("frame voor proces-PML4");
    let pdpt = falloc.allocate().expect("frame voor proces-PDPT");
    let pd = falloc.allocate().expect("frame voor proces-PD");
    // SAFETY: drie verse, identity-mapped frames; we vullen ze volledig.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Deel de hoge regio (PML4[1]) met de boot-adresruimte (A2/hoge MMIO).
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

/// Zoals [`build_address_space`], maar voor een GEFORKT kind: de USER-arena ligt op
/// een ANDER fysiek adres dan waar het virtueel verschijnt. We mappen de VIRTUELE
/// arena (= het 2 MiB-blok van de OUDER, want de gekopieerde stack/code dragen
/// absolute pointers daarheen) naar de FYSIEKE frames van het kind. Zo is het kind
/// een exacte kopie op dezelfde virtuele adressen, maar met eigen geheugen.
/// (S3 fork: child pml4 = build_address_space_remap(parent_virt_arena, child_phys_arena).)
pub fn build_address_space_remap(falloc: &mut FrameAllocator, virt_arena: u64, phys_arena: u64) -> u64 {
    let pml4 = falloc.allocate().expect("frame voor child-PML4");
    let pdpt = falloc.allocate().expect("frame voor child-PDPT");
    let pd = falloc.allocate().expect("frame voor child-PD");
    fill_remap_tables(pml4, pdpt, pd, virt_arena, phys_arena);
    pml4
}

/// Vul VOORAF-gealloceerde tabelframes (pml4/pdpt/pd) voor een geremapt kind-
/// adresruimte. fork() alloceert de drie frames uit de proces-pool en roept dit aan.
pub fn fill_remap_tables(pml4: u64, pdpt: u64, pd: u64, virt_arena: u64, phys_arena: u64) {
    // SAFETY: drie verse, identity-mapped frames; we vullen ze volledig.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;
        let pdp = pd as *mut u64;
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE | USER);
        // Deel de hoge regio (PML4[1]) met de boot-adresruimte (A2/hoge MMIO).
        pml4p.add(1).write_volatile(high_pdpt_entry());
        pdptp.write_volatile(pd | PRESENT | WRITABLE | USER);
        for i in 1..512u64 {
            pdptp.add(i as usize).write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // PD[0..512] = identity supervisor 2 MiB, BEHALVE de virtuele arena-slot:
        // die wijst (USER) naar de FYSIEKE frames van het kind.
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

/// W^X-variant voor fork: zoals [`fill_remap_tables`], maar het arena-blok wordt
/// FIJNMAZIG (4 KiB) gemapt via `pt`, met per pagina DEZELFDE rechten als de OUDER
/// (gekloond uit `parent_pt`). Zo erft het kind exact de W^X-layout (R-X code,
/// RW+NX data/stack) op z'n EIGEN fysieke frames. `parent_pt` None → alles RWX
/// (fallback voor een ouder zonder fijnmazige PT). Vereist 4 tabelframes.
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
        // Deel de hoge regio (PML4[1]) met de boot-adresruimte (A2/hoge MMIO).
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
                Some(p) => p.add(i).read_volatile() & !ADDR_MASK, // kloon ouder-rechten
                None => PRESENT | USER | WRITABLE,                // fallback: RWX
            };
            ptp.add(i).write_volatile((phys_arena + i as u64 * 4096) | flags);
        }
    }
}

// ── A2/G1: guarded kernel-stacks in de gedeelde hoge regio ─────────────────
// Een POOL van guarded stacks (IST/AP/scheduler-taken). Elke "unit" = 1 niet-
// gemapte guard-pagina + 4 stack-pagina's (16 KiB) = 5 pagina's, achter elkaar in
// de eerste 2 MiB van de hoge regio (512 GiB..512 GiB+2 MiB, één gedeelde PT van
// 512 entries → max ~102 units). Een stack-overflow landt op de guard-pagina →
// onmiddellijk #PF i.p.v. stille corruptie van de buur-stack.
use core::sync::atomic::{AtomicU64, Ordering as AtOrd};

/// Het guard-adres van de EERSTE unit — bewaard voor de A2-zelftest-log.
pub static STACK_GUARD_ADDR: AtomicU64 = AtomicU64::new(0);

const STACK_REGION_BASE: u64 = 512 * GIB; // begin van de hoge regio (PML4[1] slot 0)
const UNIT_STACK_PAGES: u64 = 4; // 16 KiB bruikbare stack per unit
const UNIT_PAGES: u64 = 1 + UNIT_STACK_PAGES; // + 1 guard-pagina
const UNIT_BYTES: u64 = UNIT_PAGES * 4096;
const MAX_UNITS: u64 = 512 / UNIT_PAGES; // de PT heeft 512 entries

static GUARD_REGION_BASE: AtomicU64 = AtomicU64::new(0); // 0 = nog niet opgezet
static GUARDED_UNITS: AtomicU64 = AtomicU64::new(0); // aantal toegekende units
static GUARD_PT: AtomicU64 = AtomicU64::new(0); // phys van de PT die de regio mapt

/// True als `addr` in een guard-pagina van de guarded-stack-pool valt. Uniforme
/// units → O(1) regio+modulo-check (de guard is de eerste pagina van elke unit).
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

/// Zet (idempotent) de fijnmazige PD→PT op die de guarded-stack-regio mapt:
/// vervang de 1 GiB-huge-mapping van 512..513 GiB in de GEDEELDE hoge PDPT door
/// een PD→PT. Geeft de PT-phys terug (0 als de hoge PDPT ontbreekt).
fn ensure_guard_pt(falloc: &mut FrameAllocator) -> u64 {
    let existing = GUARD_PT.load(AtOrd::Relaxed);
    if existing != 0 {
        return existing;
    }
    let pdpt_hi = HIGH_PDPT.load(AtOrd::Relaxed);
    if pdpt_hi == 0 {
        return 0;
    }
    let pd = falloc.allocate().expect("frame voor guard-PD");
    let pt = falloc.allocate().expect("frame voor guard-PT");
    unsafe {
        core::ptr::write_bytes(pd as *mut u8, 0, 4096);
        core::ptr::write_bytes(pt as *mut u8, 0, 4096);
        // Alleen PD[0] -> PT (de eerste 2 MiB van de hoge regio, fijnmazig).
        (pd as *mut u64).write_volatile(pt | PRESENT | WRITABLE);
        // Vervang de 1 GiB-huge PDPT-entry door deze PD (gedeeld over alle PML4's).
        (pdpt_hi as *mut u64).write_volatile(pd | PRESENT | WRITABLE);
        flush_tlb();
    }
    GUARD_PT.store(pt, AtOrd::Relaxed);
    GUARD_REGION_BASE.store(STACK_REGION_BASE, AtOrd::Relaxed);
    pt
}

/// Alloceer de VOLGENDE guarded stack-unit: map zijn 4 stack-pagina's op echte
/// frames en laat zijn guard-pagina onbemapt. Geeft de stack-TOP terug (groeit
/// naar beneden tot de guard), of 0 bij falen. Werkt in álle adresruimten (de hoge
/// PDPT is gedeeld) — geschikt voor IST-, AP- en scheduler-taak-stacks.
pub fn guarded_stack_alloc(falloc: &mut FrameAllocator) -> u64 {
    let pt = ensure_guard_pt(falloc);
    if pt == 0 {
        return 0;
    }
    let idx = GUARDED_UNITS.load(AtOrd::Relaxed);
    if idx >= MAX_UNITS {
        return 0; // PT vol
    }
    let pt_base = (idx * UNIT_PAGES) as usize;
    unsafe {
        let ptp = pt as *mut u64;
        ptp.add(pt_base).write_volatile(0); // guard-pagina: afwezig
        for p in 1..=UNIT_STACK_PAGES as usize {
            let frame = falloc.allocate().expect("frame voor guarded stack");
            core::ptr::write_bytes(frame as *mut u8, 0, 4096);
            ptp.add(pt_base + p).write_volatile(frame | PRESENT | WRITABLE);
        }
        flush_tlb();
    }
    GUARDED_UNITS.store(idx + 1, AtOrd::Relaxed);
    // Top = boven de unit (groeit naar beneden door de 4 stack-pagina's tot de guard).
    STACK_REGION_BASE + (idx + 1) * UNIT_BYTES
}

/// Aantal toegekende guarded stacks (voor diagnose).
pub fn guarded_stack_count() -> u64 {
    GUARDED_UNITS.load(AtOrd::Relaxed)
}

/// A2-compat: alloceer de eerste guarded stack + onthoud zijn guard-adres voor de
/// bestaande zelftest-log. Geeft de stack-TOP terug.
pub fn setup_guarded_stack(falloc: &mut FrameAllocator) -> u64 {
    let top = guarded_stack_alloc(falloc);
    if top != 0 {
        STACK_GUARD_ADDR.store(STACK_REGION_BASE, AtOrd::Relaxed);
    }
    top
}

/// Non-destructieve verificatie: loop de gedeelde hoge PDPT → PD → PT af en geef
/// `true` als de guard-pagina (PT[0]) NIET present is (zoals het hoort).
pub fn guard_page_unmapped() -> bool {
    let pdpt_hi = HIGH_PDPT.load(core::sync::atomic::Ordering::Relaxed);
    if pdpt_hi == 0 {
        return false;
    }
    unsafe {
        let e0 = (pdpt_hi as *const u64).read_volatile();
        if e0 & PRESENT == 0 || e0 & HUGE != 0 {
            return false; // nog een huge-mapping (geen fijnmazige guard-PD)
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

/// Vind de 4 KiB-PT die de USER-arena mapt in `pml4` (None bij een HUGE/oude mapping).
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

/// Zet alle 512 arena-pagina's tijdelijk RW (om tijdens execve een nieuw image te
/// laden) en flush de TLB. Adresbits blijven; alleen de rechten verruimen.
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

/// Pas W^X toe op de arena-PT (na het laden van een nieuw image): exec&!writ→R-X,
/// exec&writ→RWX, rest→RW+NX. Flush de TLB.
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

/// Herlaad CR3 → flush de (niet-globale) TLB-entries van de huidige adresruimte.
fn flush_tlb() {
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

/// De drie page-table-frames (pml4, pdpt, pd) van een proces-adresruimte — zodat de
/// reaper ze terug kan geven aan de juiste allocator (hoofd óf proces-pool).
pub fn table_frames(pml4: u64) -> (u64, u64, u64) {
    const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        let pd = (pdpt as *const u64).read_volatile() & ADDR_MASK;
        (pml4, pdpt, pd)
    }
}

/// Geef de page-table-frames van een proces-adresruimte vrij (PML4+PDPT+PD+arena-PT).
/// De arena-datapagina's zelf worden apart vrijgegeven door de aanroeper.
pub fn free_address_space(falloc: &mut FrameAllocator, pml4: u64) {
    // SAFETY: pml4 is identity-mapped; we lopen de tabelketen af.
    unsafe {
        let pdpt = (pml4 as *const u64).read_volatile() & ADDR_MASK;
        let pd = (pdpt as *const u64).read_volatile() & ADDR_MASK;
        // De arena-PD-entry is de enige PRESENT-zonder-HUGE: die wijst naar de 4 KiB-PT.
        let pdp = pd as *const u64;
        for i in 0..512usize {
            let e = pdp.add(i).read_volatile();
            if e & PRESENT != 0 && e & HUGE == 0 {
                let _ = falloc.free(e & ADDR_MASK); // de arena-PT
                break;
            }
        }
        let _ = falloc.free(pd);
        let _ = falloc.free(pdpt);
        let _ = falloc.free(pml4);
    }
}

/// Bouw de page tables en laad CR3. Geeft het fysieke PML4-adres terug.
pub fn init(falloc: &mut FrameAllocator) -> u64 {
    let pml4 = falloc.allocate().expect("frame voor PML4");
    let pdpt = falloc.allocate().expect("frame voor PDPT");

    // SAFETY: beide frames zijn vrij, geldig en (UEFI-)identity-mapped RAM.
    unsafe {
        core::ptr::write_bytes(pml4 as *mut u8, 0, 4096);
        core::ptr::write_bytes(pdpt as *mut u8, 0, 4096);

        let pml4p = pml4 as *mut u64;
        let pdptp = pdpt as *mut u64;

        // De BOOT-adresruimte is volledig SUPERVISOR: kernelcode, -stacks, -heap en
        // de page tables leven in de onderste 1 GiB en mogen GÉÉN User-bit dragen —
        // anders zou het inschakelen van SMEP (geen ring-0-fetch van user-pagina's)
        // of SMAP (geen ring-0-toegang tot user-pagina's) meteen faulten op de
        // eerstvolgende instructie/stacktoegang. Elk ring-3 proces krijgt z'n EIGEN
        // PML4 (build_address_space) met precies één User-arena; geen enkele ring-3
        // taak draait nog op deze boot-CR3. Zie ring3::enable_smep_smap.
        pml4p.write_volatile(pdpt | PRESENT | WRITABLE);
        // PDPT[i] = identity 1 GiB-pagina (0..512 GiB), supervisor.
        for i in 0..512u64 {
            pdptp
                .add(i as usize)
                .write_volatile((i * GIB) | PRESENT | WRITABLE | HUGE);
        }

        // PML4[1] -> tweede PDPT: identity 512 GiB..1 TiB, supervisor. Sommige
        // PCI 64-bit BARs liggen hoog — QEMU q35 plaatst de NVMe-controller-MMIO
        // bv. op ≈768 GiB. Zonder deze mapping faultt elke MMIO-toegang daarheen.
        let pdpt_hi = falloc.allocate().expect("frame voor hoge PDPT");
        core::ptr::write_bytes(pdpt_hi as *mut u8, 0, 4096);
        let pdpt_hip = pdpt_hi as *mut u64;
        pml4p.add(1).write_volatile(pdpt_hi | PRESENT | WRITABLE);
        for i in 0..512u64 {
            pdpt_hip
                .add(i as usize)
                .write_volatile(((512 + i) * GIB) | PRESENT | WRITABLE | HUGE);
        }
        // Deel deze hoge PDPT met elke proces-PML4 (build_address_space), zodat de
        // 512 GiB..1 TiB-regio overal identiek gemapt is — fundament voor gedeelde
        // guarded kernel-stacks (A2) en hoge MMIO-BARs onder een proces-CR3.
        HIGH_PDPT.store(pdpt_hi, core::sync::atomic::Ordering::Relaxed);

        // Laad onze CR3. Dit flusht meteen de TLB.
        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack, preserves_flags));
    }
    pml4
}
