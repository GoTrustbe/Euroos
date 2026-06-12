//! Local APIC + APIC-timer (Track 3.5) — vervangt de 8259-PIT als scheduler-tick.
//!
//! De LAPIC-MMIO ligt op fysiek `0xFEE00000` (identity-mapped supervisor, in de
//! 3–4 GiB 1-GiB-huge-page van [`crate::paging`]). We:
//!   1. zetten de LAPIC aan (IA32_APIC_BASE bit 11 + software-enable),
//!   2. houden de 8259-IRQ's (toetsenbord/muis) levend via LINT0=ExtINT
//!      (virtual-wire), zodat PS/2 blijft werken,
//!   3. kalibreren de timer tegen PIT-kanaal 2 en zetten 'm periodiek op `hz` Hz.
//!
//! Dit is de eerste stap richting SMP (meerdere cores): de Local APIC is per-CPU
//! en de IO-APIC + AP-bring-up bouwen hierop voort.

use x86_64::instructions::port::Port;
use x86_64::registers::model_specific::Msr;

const LAPIC_BASE: u64 = 0xFEE0_0000;

// Register-offsets (bytes vanaf de basis).
const REG_ID: u64 = 0x020;
const REG_EOI: u64 = 0x0B0;
const REG_SPURIOUS: u64 = 0x0F0;
const REG_LVT_LINT0: u64 = 0x350;
const REG_LVT_LINT1: u64 = 0x360;
const REG_LVT_TIMER: u64 = 0x320;
const REG_TIMER_INIT: u64 = 0x380;
const REG_TIMER_CUR: u64 = 0x390;
const REG_TIMER_DIV: u64 = 0x3E0;
const REG_ICR_LOW: u64 = 0x300;
const REG_ICR_HIGH: u64 = 0x310;

const IA32_APIC_BASE: u32 = 0x1B;

const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const APIC_SW_ENABLE: u32 = 1 << 8;
const DELIVERY_EXTINT: u32 = 0b111 << 8;
const DELIVERY_NMI: u32 = 0b100 << 8;

#[inline]
unsafe fn rd(off: u64) -> u32 {
    ((LAPIC_BASE + off) as *const u32).read_volatile()
}
#[inline]
unsafe fn wr(off: u64, v: u32) {
    ((LAPIC_BASE + off) as *mut u32).write_volatile(v);
}

/// Local-APIC-ID van de huidige CPU (de BSP bij single-core).
pub fn lapic_id() -> u32 {
    unsafe { rd(REG_ID) >> 24 }
}

// ── IO-APIC (IRQ-routering, vervangt de 8259-PIC) ──────────────────────────
unsafe fn ioapic_write(base: u64, reg: u32, val: u32) {
    (base as *mut u32).write_volatile(reg); // IOREGSEL
    ((base + 0x10) as *mut u32).write_volatile(val); // IOWIN
}

/// Route een GSI (global system interrupt) naar `vector` op core `dest_apic`.
/// ISA-IRQs zijn edge-triggered, active-high; we zetten de redirection-entry
/// (low = vector/fixed/physical/unmasked, high = bestemmings-APIC-id).
pub fn ioapic_route(ioapic_base: u32, gsi: u32, vector: u8, dest_apic: u8) {
    let base = ioapic_base as u64;
    let low = 0x10 + 2 * gsi;
    let high = 0x11 + 2 * gsi;
    unsafe {
        ioapic_write(base, low, 1 << 16); // eerst maskeren
        ioapic_write(base, high, (dest_apic as u32) << 24);
        ioapic_write(base, low, vector as u32); // fixed, physical, edge, active-high, unmasked
    }
}

/// End-Of-Interrupt naar de Local APIC (de timer-vector EOI't hierheen i.p.v. PIC).
#[inline]
pub fn eoi() {
    unsafe { wr(REG_EOI, 0) };
}

/// Aantal LAPIC-timerticks per `hz`-periode (resultaat van de kalibratie).
static mut CAL_COUNT: u32 = 0;

/// Zet de LAPIC aan en start de periodieke timer op `hz` Hz, interrupt-`vector`.
/// Geeft het gekalibreerde initiële-tellingsgetal terug (diagnostiek).
pub fn init(hz: u32, vector: u8) -> u32 {
    unsafe {
        // 1. Global enable via IA32_APIC_BASE (bit 11) — firmware zet 'm meestal al.
        let mut base_msr = Msr::new(IA32_APIC_BASE);
        let base = base_msr.read();
        base_msr.write(base | (1 << 11));

        // 2. Software-enable + spurious-vector 0xFF.
        wr(REG_SPURIOUS, APIC_SW_ENABLE | 0xFF);

        // 3. Virtual-wire: LINT0 = ExtINT (laat 8259 kbd/muis door), LINT1 = NMI.
        wr(REG_LVT_LINT0, DELIVERY_EXTINT);
        wr(REG_LVT_LINT1, DELIVERY_NMI);

        // 4. Kalibreer tegen PIT-kanaal 2 en start de periodieke timer.
        let count = calibrate(hz);
        CAL_COUNT = count;
        wr(REG_TIMER_DIV, 0x3); // 0b011 = delen door 16
        wr(REG_LVT_TIMER, LVT_PERIODIC | vector as u32);
        wr(REG_TIMER_INIT, count);
        count
    }
}

/// Zet de Local APIC van DEZE cpu aan en start z'n periodieke timer op `vector`,
/// met de reeds (door de BSP) gekalibreerde tellingswaarde. Voor de APs.
pub fn start_timer_on_this_cpu(vector: u8) {
    unsafe {
        let mut base = Msr::new(IA32_APIC_BASE);
        let b = base.read();
        base.write(b | (1 << 11));
        wr(REG_SPURIOUS, APIC_SW_ENABLE | 0xFF);
        let count = core::ptr::addr_of!(CAL_COUNT).read();
        wr(REG_TIMER_DIV, 0x3);
        wr(REG_LVT_TIMER, LVT_PERIODIC | vector as u32);
        wr(REG_TIMER_INIT, if count == 0 { 1_000_000 } else { count });
    }
}

/// Stuur een gewone (fixed-delivery) inter-processor interrupt naar `apic_id` op
/// `vector`. Voor cross-CPU-signalering (reschedule, halt, TLB-shootdown).
pub fn send_ipi(apic_id: u8, vector: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4000 | vector as u32); // fixed, assert, edge, physical dest
        wait_icr_idle();
    }
}

/// Stuur een INIT-IPI naar core `apic_id` (physieke destination mode).
pub fn send_init(apic_id: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4500); // INIT, assert, edge
        wait_icr_idle();
    }
}

/// Stuur een Startup-IPI (SIPI) met trampoline-startpagina `vector` (phys >> 12).
pub fn send_sipi(apic_id: u8, vector: u8) {
    unsafe {
        wr(REG_ICR_HIGH, (apic_id as u32) << 24);
        wr(REG_ICR_LOW, 0x0000_4600 | vector as u32); // Startup
        wait_icr_idle();
    }
}

unsafe fn wait_icr_idle() {
    // ICR-bit 12 (delivery status) blijft 1 zolang de IPC onderweg is.
    let mut guard = 0u32;
    while rd(REG_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
    }
}

/// Busy-wait ~`us` microseconden via de lopende LAPIC-timer (current-count),
/// onafhankelijk van interrupts (die staan tijdens de SMP-bring-up nog uit).
pub fn busy_wait_us(us: u32) {
    let cal = unsafe { core::ptr::addr_of!(CAL_COUNT).read() };
    if cal == 0 {
        for _ in 0..(us as u64 * 300) {
            core::hint::spin_loop();
        }
        return;
    }
    // cal ticks = 1 periode = 10_000 us (100 Hz). want = cal * us / 10_000.
    let want = (cal as u64 * us as u64) / 10_000;
    unsafe {
        let mut last = rd(REG_TIMER_CUR);
        let mut elapsed = 0u64;
        let mut guard = 0u64;
        while elapsed < want {
            let cur = rd(REG_TIMER_CUR);
            // De teller telt AF; bij een herlaad (cur > last) is hij gewrapt.
            elapsed += if cur <= last {
                (last - cur) as u64
            } else {
                last as u64 + (cal as u64 - cur as u64)
            };
            last = cur;
            core::hint::spin_loop();
            guard += 1;
            if guard > 200_000_000 {
                break;
            }
        }
    }
}

/// Meet hoeveel LAPIC-timerticks er in één `hz`-periode passen via PIT-kanaal 2
/// (mode 0, ~`1/hz` s one-shot, gepolld op de OUT2-statusbit).
unsafe fn calibrate(hz: u32) -> u32 {
    let mut p61 = Port::<u8>::new(0x61);
    // Gate aan (bit0), speaker uit (bit1=0).
    let v = (p61.read() & 0xFC) | 0x01;
    p61.write(v);

    let pit_count = (1_193_182u32 / hz) as u16;
    let mut cmd = Port::<u8>::new(0x43);
    let mut ch2 = Port::<u8>::new(0x42);
    cmd.write(0xB0); // ch2, lobyte+hibyte, mode 0
    ch2.write((pit_count & 0xFF) as u8);
    ch2.write((pit_count >> 8) as u8);

    // LAPIC-timer op maximum laten lopen.
    wr(REG_TIMER_DIV, 0x3);
    wr(REG_TIMER_INIT, 0xFFFF_FFFF);

    // Wacht tot PIT-ch2 z'n terminal count haalt (OUT2 = 0x61 bit5 wordt hoog).
    let mut guard = 0u32;
    while (p61.read() & 0x20) == 0 {
        guard += 1;
        if guard > 50_000_000 {
            break; // veiligheidsklep tegen een hangende kalibratie
        }
    }

    wr(REG_LVT_TIMER, LVT_MASKED);
    let elapsed = 0xFFFF_FFFFu32 - rd(REG_TIMER_CUR);
    if elapsed < 1000 {
        1_000_000 // fallback bij mislukte kalibratie
    } else {
        elapsed
    }
}
