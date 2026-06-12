//! SMP-bring-up (Track 3.5): start de application-processors (APs).
//!
//! De BSP kopieert de [`asm/trampoline.S`]-blob naar physiek `0x8000`, patcht de
//! boot-PML4 (CR3), een per-AP stack en de `ap_main`-entry, en stuurt per core
//! de INIT-SIPI-SIPI-sequentie via de Local-APIC. Elke AP komt in long mode, telt
//! zichzelf op in [`AP_ONLINE`] en parkeert (`hlt`). SMP-scheduling (per-CPU
//! run-queues) is de volgende stap; dit bewijst dat alle cores kernelcode draaien.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use alloc::vec::Vec;

use crate::serial_println;

/// Grootte van de parallelle-som-demo (0..N), verdeeld over de cores. Bewust
/// bescheiden: de demo bewijst de parallelle verdeling + correcte som, maar een
/// gigantische N kost vooral onder TCG-emulatie nodeloos veel boot-tijd (SPERF).
const WORK_N: u64 = 2_000_000;

/// Door build.rs geassembleerde flat-binary trampoline (org 0x8000).
static TRAMPOLINE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trampoline.bin"));

const TRAMP: u64 = 0x8000;
const OFF_CR3: u64 = 0xF00;
const OFF_STACK: u64 = 0xF08;
const OFF_ENTRY: u64 = 0xF10;

const MAX_AP: usize = 7;
const AP_STACK_SIZE: usize = 16 * 1024;
static mut AP_STACKS: [[u8; AP_STACK_SIZE]; MAX_AP] = [[0; AP_STACK_SIZE]; MAX_AP];

/// G1: per-AP GUARDED kernel-stacktop (0 = niet gezet → val terug op de BSS-stack).
/// main.rs vult deze vóór `init()` uit de guarded-stack-pool, zodat een AP-stack-
/// overflow op een niet-gemapte guard-pagina faultt i.p.v. de buur-AP-stack te
/// overschrijven. De guarded regio zit in de gedeelde hoge PDPT → ook geldig onder
/// de boot-PML4 waarop de AP's draaien.
static AP_GUARDED_TOP: [AtomicU64; MAX_AP] = [const { AtomicU64::new(0) }; MAX_AP];

/// Zet de guarded stacktop voor AP-slot `idx` (aanroepen vóór [`init`]).
pub fn set_ap_guarded_top(idx: usize, top: u64) {
    if idx < MAX_AP {
        AP_GUARDED_TOP[idx].store(top, Ordering::Release);
    }
}

/// G1: alloceer guarded kernel-stacks voor álle AP-slots uit de guarded-stack-pool.
/// Aanroepen vóór [`init`] (de hoofd-frame-allocator is dan nog beschikbaar). Geeft
/// het aantal gezette stacks terug.
pub fn setup_guarded_stacks(falloc: &mut euromm::FrameAllocator) -> usize {
    let mut n = 0;
    for idx in 0..MAX_AP {
        let top = crate::paging::guarded_stack_alloc(falloc);
        if top == 0 {
            break;
        }
        set_ap_guarded_top(idx, top);
        n += 1;
    }
    n
}

/// Aantal APs dat long mode bereikte en Rust-code draait.
pub static AP_ONLINE: AtomicU32 = AtomicU32::new(0);

// ── Parallelle-werk-demo: elke core sommeert een eigen bereik [lo,hi). ──
// Geïndexeerd op (LAPIC-id & 7); de BSP vult de bereiken vóór de bring-up.
const MAX_CPU: usize = 8;
static WORK_LO: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_HI: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_SUM: [AtomicU64; MAX_CPU] = [const { AtomicU64::new(0) }; MAX_CPU];
static WORK_DONE: [AtomicBool; MAX_CPU] = [const { AtomicBool::new(false) }; MAX_CPU];

/// Sommeer `lo..hi` (de kernwerklast — bewijst dat een core écht rekent).
fn compute_range(lo: u64, hi: u64) -> u64 {
    let mut s = 0u64;
    let mut i = lo;
    while i < hi {
        s = s.wrapping_add(i);
        i += 1;
    }
    s
}

/// Entry voor elke AP (vanuit de trampoline, 64-bit): meld je online, reken je
/// toegewezen stuk van de parallelle som, en parkeer dan (`hlt`).
#[no_mangle]
pub extern "sysv64" fn ap_main() -> ! {
    let id = (crate::apic::lapic_id() & 7) as usize;
    AP_ONLINE.fetch_add(1, Ordering::SeqCst);
    // 1) Toegewezen stuk van de parallelle som.
    let lo = WORK_LO[id].load(Ordering::Acquire);
    let hi = WORK_HI[id].load(Ordering::Acquire);
    let sum = compute_range(lo, hi);
    WORK_SUM[id].store(sum, Ordering::Release);
    WORK_DONE[id].store(true, Ordering::Release);
    // 2) Per-CPU scheduler opzetten: gedeelde GDT-segmenten + gedeelde IDT + een
    //    eigen run-queue (idle + 2 workers) + eigen LAPIC-timer (vector 0x41).
    //    Na sti tikt deze core zelf en wisselt z'n eigen taken af. Deze hlt-lus
    //    is de idle-taak: de eerste tick bewaart 'm en switcht naar een worker.
    crate::gdt::init_ap();
    crate::interrupts::init();
    crate::sched::ap_setup(id);
    crate::apic::start_timer_on_this_cpu(0x41);
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// Start alle ingeschakelde AP-cores. Aanroepen ná `init_timer` (APIC + timer
/// gekalibreerd) en vóór `interrupts::enable()` — dan staat de BSP nog op de
/// boot-PML4 en switcht de scheduler de CR3 nog niet.
pub fn init() {
    let madt = match crate::acpi::parse() {
        Some(m) => m,
        None => {
            serial_println!("[smp] geen MADT — single-core");
            return;
        }
    };
    let bsp = crate::apic::lapic_id() as u8;
    let cr3 = crate::sched::boot_pml4();
    if cr3 == 0 {
        serial_println!("[smp] boot-PML4 onbekend — bring-up overgeslagen");
        return;
    }

    // Verdeel een parallelle som 0..N over álle ingeschakelde cores (BSP + APs).
    let enabled: Vec<u8> = madt.cores.iter().filter(|c| c.enabled).map(|c| c.apic_id).collect();
    let ncores = enabled.len().max(1) as u64;
    let chunk = WORK_N / ncores;
    for (i, &id) in enabled.iter().enumerate() {
        let lo = i as u64 * chunk;
        let hi = if i as u64 + 1 == ncores { WORK_N } else { lo + chunk };
        let s = (id & 7) as usize;
        WORK_LO[s].store(lo, Ordering::Release);
        WORK_HI[s].store(hi, Ordering::Release);
        WORK_DONE[s].store(false, Ordering::Release);
    }

    // Trampoline naar 0x8000 kopiëren + CR3/entry patchen (per-AP stack volgt).
    unsafe {
        core::ptr::copy_nonoverlapping(TRAMPOLINE.as_ptr(), TRAMP as *mut u8, TRAMPOLINE.len());
        ((TRAMP + OFF_CR3) as *mut u64).write_volatile(cr3);
        ((TRAMP + OFF_ENTRY) as *mut u64).write_volatile(ap_main as usize as u64);
    }

    let mut idx = 0usize;
    let mut started = 0u32;
    for c in &madt.cores {
        if !c.enabled || c.apic_id == bsp {
            continue;
        }
        if idx >= MAX_AP {
            break;
        }
        // Per-AP stacktop (16-uitgelijnd); APs starten één voor één, dus geen race.
        // G1: gebruik een GUARDED stacktop als die gezet is (overflow → guard-#PF
        // i.p.v. stille corruptie); anders val terug op de BSS-stack.
        let guarded = AP_GUARDED_TOP[idx].load(Ordering::Acquire);
        let sp = if guarded != 0 {
            guarded & !0xF
        } else {
            unsafe {
                let base = core::ptr::addr_of_mut!(AP_STACKS[idx]) as u64;
                (base + AP_STACK_SIZE as u64) & !0xF
            }
        };
        unsafe { ((TRAMP + OFF_STACK) as *mut u64).write_volatile(sp) };

        let before = AP_ONLINE.load(Ordering::SeqCst);
        // INIT-SIPI-SIPI (Intel MP-protocol).
        crate::apic::send_init(c.apic_id);
        crate::apic::busy_wait_us(10_000);
        crate::apic::send_sipi(c.apic_id, (TRAMP >> 12) as u8);
        crate::apic::busy_wait_us(200);
        crate::apic::send_sipi(c.apic_id, (TRAMP >> 12) as u8);

        // Wacht tot deze AP zich meldt (timeout ~100 ms).
        let mut ok = false;
        for _ in 0..20 {
            crate::apic::busy_wait_us(5_000);
            if AP_ONLINE.load(Ordering::SeqCst) > before {
                ok = true;
                break;
            }
        }
        if ok {
            started += 1;
            serial_println!("[smp] core APIC-id {} online", c.apic_id);
        } else {
            serial_println!("[smp] core APIC-id {} kwam NIET online (timeout)", c.apic_id);
        }
        idx += 1;
    }
    serial_println!(
        "[smp] {}/{} AP-core(s) online (BSP = APIC-id {}, {} cores totaal)",
        started,
        idx,
        bsp,
        madt.cores.len()
    );

    // De BSP rekent z'n eigen stuk, wacht op de APs en verifieert de totale som —
    // bewijs dat alle cores écht (parallel) kernel-rekenwerk deden.
    let bid = (bsp & 7) as usize;
    let blo = WORK_LO[bid].load(Ordering::Acquire);
    let bhi = WORK_HI[bid].load(Ordering::Acquire);
    WORK_SUM[bid].store(compute_range(blo, bhi), Ordering::Release);
    WORK_DONE[bid].store(true, Ordering::Release);

    for &id in &enabled {
        let s = (id & 7) as usize;
        let mut g = 0u32;
        while !WORK_DONE[s].load(Ordering::Acquire) {
            crate::apic::busy_wait_us(1_000);
            g += 1;
            if g > 5_000 {
                serial_println!("[smp] core APIC-id {} maakte z'n werk niet af (timeout)", id);
                break;
            }
        }
    }
    let mut total = 0u64;
    for &id in &enabled {
        total = total.wrapping_add(WORK_SUM[(id & 7) as usize].load(Ordering::Acquire));
    }
    let expected = if WORK_N % 2 == 0 {
        (WORK_N / 2).wrapping_mul(WORK_N - 1)
    } else {
        WORK_N.wrapping_mul((WORK_N - 1) / 2)
    };
    serial_println!(
        "[smp] parallelle som 0..{} over {} core(s) = {} (verwacht {}) -> {}",
        WORK_N,
        ncores,
        total,
        expected,
        if total == expected { "OK" } else { "FOUT" }
    );

    // Geef de APs ~80 ms en lees hun per-CPU werk-tellers: bewijs dat elke core
    // onafhankelijk z'n EIGEN run-queue draait en taken afwisselt (SMP-scheduling).
    crate::apic::busy_wait_us(200_000);
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        let s = (id & 7) as usize;
        let a = crate::sched::AP_WORK_A[s].load(Ordering::Relaxed);
        let b = crate::sched::AP_WORK_B[s].load(Ordering::Relaxed);
        serial_println!(
            "[smp] core APIC-id {} per-CPU scheduler: worker-A={} worker-B={} (eigen run-queue)",
            id, a, b
        );
    }

    // ── Run 2: cross-CPU IPIs + TLB-shootdown ──
    // Ping elke AP 5× en doe één TLB-shootdown; verifieer via de per-CPU tellers.
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        for _ in 0..5 {
            crate::apic::send_ipi(id, crate::interrupts::IPI_PING_VECTOR);
            crate::apic::busy_wait_us(300);
        }
    }
    tlb_shootdown(&enabled, bsp);
    crate::apic::busy_wait_us(5_000);
    for &id in &enabled {
        if id == bsp {
            continue;
        }
        let s = (id & 7) as usize;
        serial_println!(
            "[smp] core APIC-id {} cross-CPU: {} ping-IPIs ontvangen, {} TLB-shootdown(s)",
            id,
            crate::interrupts::IPI_COUNT[s].load(Ordering::Relaxed),
            crate::interrupts::TLB_COUNT[s].load(Ordering::Relaxed)
        );
    }

    // ── Run 2: load-balancing — plaats een extra taak op de minst belaste AP ──
    let aps: Vec<u8> = enabled.iter().copied().filter(|&id| id != bsp).collect();
    if let Some(&target) = aps.iter().min_by_key(|&&id| crate::sched::ap_load((id & 7) as usize)) {
        let s = (target & 7) as usize;
        if crate::sched::ap_enqueue_worker(s) {
            serial_println!("[smp] load-balance: extra taak geplaatst op minst-belaste core APIC-id {}", target);
            crate::apic::busy_wait_us(40_000);
            serial_println!(
                "[smp] core APIC-id {} draait de gebalanceerde taak: worker-C={}",
                target,
                crate::sched::AP_WORK_C[s].load(Ordering::Relaxed)
            );
        }
    }
}

/// Stuur een TLB-shootdown naar alle andere cores (aanroepen ná het wijzigen van
/// gedeelde kernel-page-tables, zodat geen core met stale TLB-entries verder draait).
pub fn tlb_shootdown(cores: &[u8], self_id: u8) {
    for &id in cores {
        if id != self_id {
            crate::apic::send_ipi(id, crate::interrupts::IPI_TLB_VECTOR);
        }
    }
}

/// Stop alle andere cores (bv. vóór een shutdown of in de panic-handler).
pub fn halt_others(cores: &[u8], self_id: u8) {
    for &id in cores {
        if id != self_id {
            crate::apic::send_ipi(id, crate::interrupts::IPI_HALT_VECTOR);
        }
    }
}
